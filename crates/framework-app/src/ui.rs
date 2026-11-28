use crate::audio_out::{AudioEngine, AudioOutput};
use crate::playback::{self, PREVIEW_FPS, PreviewQuality, PreviewScale, Transport};
use crate::state::{EditorState, MAX_ZOOM, MIN_ZOOM};
use crate::theme;
use crate::worker::{ExportJob, ExportState, PreviewWorker};
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, Vec2};
use framework_core::{ClipId, MediaKind, PROJECT_EXTENSION, SaveMode, Time, TrackId, TrackKind};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

const TRACK_HEIGHT: f32 = 48.0;
const HEADER_WIDTH: f32 = 72.0;
const RULER_HEIGHT: f32 = 22.0;

/// Tabs of the left-hand browser panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserTab {
    Media,
    Effects,
    Generators,
}

/// Planned effects and generators shown (disabled) until their roadmap phase lands.
pub const PLANNED_EFFECTS: &[(&str, &str)] = &[
    ("Auto white balance", "Phase 4 · #9"),
    ("Colour correction", "Phase 4 · #9"),
    ("Transform / crop", "Phase 2 · #4"),
    ("Opacity & blend", "Phase 2 · #4"),
    ("Cross dissolve", "Phase 2 · #4"),
    ("Emphasis zoom", "Phase 5 · #12"),
];
pub const PLANNED_GENERATORS: &[(&str, &str)] = &[
    ("Text / title", "Phase 3 · #8"),
    ("Lower third", "Phase 3 · #8"),
    ("Solid colour", "Phase 2 · #20"),
    ("Gradient", "Phase 2 · #20"),
];

/// Actions requested by the UI that need platform services (file dialogs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    New,
    Open,
    Save,
    SaveAs,
    /// Embeds all media into a new `.fwk` file.
    CollectAndSave,
    Import,
    ChooseExportPath,
    StartExport,
    CancelExport,
    /// Closes the window (after the unsaved-changes check).
    Quit,
    /// Unsaved-changes prompt: drop the edits and run the pending action.
    DiscardChanges,
    /// Unsaved-changes prompt: save, then run the pending action.
    SaveAndContinue,
    /// Unsaved-changes prompt: keep editing.
    CancelPending,
}

pub struct FrameworkApp {
    pub state: EditorState,
    preview: Option<PreviewWorker>,
    preview_texture: Option<TextureHandle>,
    preview_requested: Option<(Time, u64)>,
    project_revision: u64,
    export_job: Option<ExportJob>,
    pub export_window: bool,
    pub browser_tab: BrowserTab,
    pub actions: Vec<UiAction>,
    /// Width of the timeline's time area in the last frame, for zoom steps and fitting.
    timeline_width: f32,
    drag: Option<ClipDrag>,
    pub snapping: bool,
    /// On-screen clip and lane-header rectangles of the last frame.
    pub clip_rects: Vec<(ClipId, Rect)>,
    pub lane_rects: Vec<(TrackId, Rect)>,
    /// Action waiting for the user to resolve unsaved changes.
    pub pending: Option<UiAction>,
    /// When false, file dialogs are skipped and no sound device is opened (tests).
    pub use_dialogs: bool,
    pub transport: Transport,
    last_tick: Option<Instant>,
    /// Playhead after the last playback tick, to notice seeks during playback.
    ticked_playhead: Time,
    /// Playback engine whose clock drives the playhead at normal speed.
    pub audio: Option<Arc<AudioEngine>>,
    audio_output: Option<AudioOutput>,
    audio_tried: bool,
    /// Last audio clock reading and when it last changed, to detect a stalled device.
    audio_clock: Option<(Time, Instant)>,
    /// Project revision the audio engine is playing.
    audio_revision: u64,
    pub preview_quality: PreviewQuality,
    wake: egui::Context,
}

impl FrameworkApp {
    pub fn new(ctx: &egui::Context) -> Self {
        theme::apply(ctx);
        // Cmd/Ctrl +/− zoom the timeline instead of the whole UI.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let mut app = Self {
            state: EditorState::default(),
            preview: None,
            preview_texture: None,
            preview_requested: None,
            project_revision: 0,
            export_job: None,
            export_window: false,
            browser_tab: BrowserTab::Media,
            actions: Vec::new(),
            timeline_width: 0.0,
            drag: None,
            snapping: true,
            clip_rects: Vec::new(),
            lane_rects: Vec::new(),
            pending: None,
            use_dialogs: true,
            transport: Transport::default(),
            last_tick: None,
            ticked_playhead: Time::ZERO,
            audio: None,
            audio_output: None,
            audio_tried: false,
            audio_clock: None,
            audio_revision: 0,
            preview_quality: PreviewQuality::default(),
            wake: ctx.clone(),
        };
        app.spawn_preview();
        app
    }

    fn spawn_preview(&mut self) {
        let (w, h) = self.preview_quality.size(&self.state.project.settings);
        let wake = self.wake.clone();
        self.preview = Some(PreviewWorker::spawn(w, h, move || wake.request_repaint()));
        self.preview_requested = None;
    }

    /// Changes the preview quality, restarting the renderer when the size changes.
    pub fn set_preview_quality(&mut self, quality: PreviewQuality) {
        let settings = &self.state.project.settings;
        let resize = quality.size(settings) != self.preview_quality.size(settings);
        self.preview_quality = quality;
        if resize {
            self.spawn_preview();
        }
    }

    /// Applies a transport command (play, pause, shuttle) and keeps audio in step.
    pub fn transport_action(&mut self, command: impl FnOnce(&mut Transport)) {
        let before = self.transport.speed;
        command(&mut self.transport);
        let end = self.state.project.duration();
        if self.transport.speed > 0.0 && before <= 0.0 && self.state.playhead >= end {
            self.state.set_playhead(Time::ZERO);
        }
        if self.transport.speed != before {
            self.sync_audio();
        }
    }

    /// Stops playback and moves the playhead.
    pub fn seek(&mut self, t: Time) {
        self.transport_action(Transport::pause);
        self.state.set_playhead(t);
    }

    /// False once the device has not consumed audio for a while (no working
    /// output); playback then continues on the wall clock without sound.
    fn audio_clock_running(&mut self, t: Time, now: Instant) -> bool {
        match self.audio_clock {
            Some((last, since)) if last == t => {
                if now.duration_since(since) < AUDIO_STALL {
                    return true;
                }
                self.audio = None;
                self.audio_output = None;
                self.audio_clock = None;
                self.state.status = "No working audio output; playing without sound".into();
                false
            }
            _ => {
                self.audio_clock = Some((t, now));
                true
            }
        }
    }

    fn sync_audio(&mut self) {
        self.audio_revision = self.project_revision;
        self.audio_clock = None;
        if self.transport.speed == 1.0 {
            if self.audio.is_none() && !self.audio_tried && self.use_dialogs {
                self.audio_tried = true;
                self.audio_output = AudioOutput::open();
                self.audio = self.audio_output.as_ref().map(|o| o.engine.clone());
            }
            if let Some(audio) = &self.audio {
                audio.play(&self.state.project, self.state.playhead);
            }
        } else if let Some(audio) = &self.audio {
            audio.stop();
        }
        self.ticked_playhead = self.state.playhead;
    }

    /// Advances the playhead while playing: follows the audio clock at normal
    /// speed, otherwise wall-clock time scaled by the shuttle speed.
    fn tick_playback(&mut self, ctx: &egui::Context) {
        if !self.transport.is_playing() {
            self.last_tick = None;
            return;
        }
        // Seeks and edits during playback restart audio from the playhead.
        let edited = self.transport.speed == 1.0 && self.audio_revision != self.project_revision;
        if self.state.playhead != self.ticked_playhead || edited {
            self.sync_audio();
        }
        let now = Instant::now();
        let dt = self
            .last_tick
            .map_or(0.0, |t| now.duration_since(t).as_secs_f64());
        self.last_tick = Some(now);
        let end = self.state.project.duration();
        let clock = (self.transport.speed == 1.0)
            .then(|| self.audio.as_ref().and_then(|a| a.position()))
            .flatten()
            .filter(|&t| self.audio_clock_running(t, now));
        self.state.playhead = match clock {
            Some(t) if t >= end => {
                self.transport.pause();
                end
            }
            Some(t) => t,
            None => self.transport.advance(self.state.playhead, dt, end),
        };
        if let Some(e) = self.audio.as_ref().and_then(|a| a.take_error()) {
            self.state.status = format!("Audio error: {e}");
        }
        if !self.transport.is_playing() {
            self.sync_audio();
        }
        self.ticked_playhead = self.state.playhead;
        ctx.request_repaint();
    }

    /// Opens a project file (e.g. passed on the command line).
    pub fn open_project(&mut self, path: &std::path::Path) -> bool {
        let ok = self.state.open(path);
        self.touch();
        ok
    }

    /// Marks the project as changed so the preview is re-rendered.
    pub fn touch(&mut self) {
        self.project_revision += 1;
    }

    pub fn export_job(&self) -> Option<&ExportJob> {
        self.export_job.as_ref()
    }

    /// Builds the whole UI for one frame.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.poll_preview(&ctx);
        self.menu_bar(ui);
        self.status_bar(ui);
        self.media_bin(ui);
        self.timeline(ui);
        self.preview_panel(ui);
        self.export_dialog(&ctx);
        self.unsaved_dialog(&ctx);
        self.handle_shortcuts(&ctx);
        let actions = std::mem::take(&mut self.actions);
        for action in actions {
            self.perform(action, &ctx);
        }
        self.tick_playback(&ctx);
        self.request_preview();
    }

    /// Turns a window close request with unsaved edits into the unsaved-changes prompt.
    pub fn handle_close_request(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && self.state.dirty {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending = Some(UiAction::Quit);
        }
    }

    fn request_preview(&mut self) {
        let key = (self.state.playhead, self.project_revision);
        if self.preview_requested == Some(key) {
            return;
        }
        // During playback, frames closer together than the preview frame rate are skipped.
        if self.transport.is_playing()
            && let Some((t, revision)) = self.preview_requested
            && revision == self.project_revision
        {
            let interval = self
                .preview_quality
                .frame_interval(self.state.project.settings.frame_rate);
            // At shuttle speeds the playhead covers `speed` timeline seconds per second.
            let interval = interval.flicks() as f64 * self.transport.speed.abs().max(1.0);
            if ((self.state.playhead.flicks() - t.flicks()).abs() as f64) < interval {
                return;
            }
        }
        if let Some(worker) = &self.preview {
            worker.request(&self.state.project, self.state.playhead);
            self.preview_requested = Some(key);
        }
    }

    fn poll_preview(&mut self, ctx: &egui::Context) {
        let Some(result) = self.preview.as_ref().and_then(PreviewWorker::poll) else {
            return;
        };
        match result {
            Ok(img) => {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [img.width as usize, img.height as usize],
                    &img.pixels,
                );
                match &mut self.preview_texture {
                    Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                    None => {
                        self.preview_texture =
                            Some(ctx.load_texture("preview", image, egui::TextureOptions::LINEAR));
                    }
                }
            }
            Err(e) => self.state.status = format!("Preview error: {e}"),
        }
    }

    pub fn perform(&mut self, action: UiAction, ctx: &egui::Context) {
        if matches!(action, UiAction::New | UiAction::Open | UiAction::Quit) && self.state.dirty {
            self.pending = Some(action);
            return;
        }
        match action {
            UiAction::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            UiAction::DiscardChanges => {
                if let Some(next) = self.pending.take() {
                    self.state.dirty = false;
                    self.perform(next, ctx);
                }
            }
            UiAction::SaveAndContinue => {
                self.perform(UiAction::Save, ctx);
                if !self.state.dirty {
                    self.perform(UiAction::DiscardChanges, ctx);
                }
            }
            UiAction::CancelPending => self.pending = None,
            UiAction::New => {
                self.seek(Time::ZERO);
                self.state = EditorState::default();
                self.touch();
            }
            UiAction::Open => {
                if let Some(path) = self.pick_file(&[PROJECT_EXTENSION, "json"]) {
                    self.seek(Time::ZERO);
                    self.state.open(&path);
                    self.touch();
                }
            }
            UiAction::Save => match self.state.project_path.clone() {
                Some(path) => self.save_project(&path, SaveMode::Reference),
                None => self.perform(UiAction::SaveAs, ctx),
            },
            UiAction::SaveAs => {
                if let Some(path) = self.save_project_file("project") {
                    self.save_project(&path, SaveMode::Reference);
                }
            }
            UiAction::CollectAndSave => {
                if let Some(path) = self.save_project_file("project-collected") {
                    self.save_project(&path, SaveMode::Collect);
                }
            }
            UiAction::Import => {
                let files = if self.use_dialogs {
                    rfd::FileDialog::new()
                        .add_filter(
                            "Media",
                            &[
                                "mp4", "mov", "mkv", "webm", "png", "jpg", "jpeg", "wav", "mp3",
                                "m4a", "aac", "flac",
                            ],
                        )
                        .pick_files()
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                self.state.import(&files);
            }
            UiAction::ChooseExportPath => {
                if let Some(path) = self.save_file("MP4 video", "mp4", "export") {
                    self.state.export_path = path.to_string_lossy().into_owned();
                }
            }
            UiAction::StartExport => {
                if self.export_job.as_ref().is_some_and(ExportJob::is_running) {
                    return;
                }
                let wake = ctx.clone();
                self.export_job = Some(ExportJob::start(
                    self.state.project.clone(),
                    encodable(self.state.selected_profile()),
                    PathBuf::from(&self.state.export_path),
                    move || wake.request_repaint(),
                ));
            }
            UiAction::CancelExport => {
                if let Some(job) = &self.export_job {
                    job.cancel();
                }
            }
        }
    }

    fn pick_file(&self, ext: &[&str]) -> Option<PathBuf> {
        self.use_dialogs
            .then(|| {
                rfd::FileDialog::new()
                    .add_filter("Project", ext)
                    .pick_file()
            })
            .flatten()
    }

    fn save_file(&self, filter: &str, ext: &str, stem: &str) -> Option<PathBuf> {
        self.use_dialogs
            .then(|| {
                rfd::FileDialog::new()
                    .add_filter(filter, &[ext])
                    .set_file_name(format!("{stem}.{ext}"))
                    .save_file()
            })
            .flatten()
    }

    fn save_project_file(&mut self, stem: &str) -> Option<PathBuf> {
        let chosen = self.save_file("Framework project", PROJECT_EXTENSION, stem)?;
        project_save_target(&chosen)
            .map_err(|e| self.state.status = e)
            .ok()
    }

    /// Saves the project; the preview first closes its media files because
    /// embedded media may live in the file being replaced.
    pub fn save_project(&mut self, path: &std::path::Path, mode: SaveMode) {
        self.transport_action(Transport::pause);
        if let Some(worker) = &self.preview {
            worker.release_media();
        }
        if let Some(audio) = &self.audio {
            audio.release();
        }
        self.state.save_to(path, mode);
        self.preview_requested = None;
        self.project_revision += 1;
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let fps = self.state.project.settings.frame_rate;
        let step = fps.frame_duration();
        let typing = ctx.egui_wants_keyboard_input();
        let (playhead, end) = (self.state.playhead, self.state.project.duration());
        let mut seek = None;
        let mut command: Option<fn(&mut Transport)> = None;
        let mut edits = Vec::new();
        ctx.input_mut(|i| {
            let plain =
                |i: &egui::InputState, key| !typing && i.modifiers.is_none() && i.key_pressed(key);
            if plain(i, egui::Key::ArrowRight) {
                seek = Some(playhead + step);
            }
            if plain(i, egui::Key::ArrowLeft) {
                seek = Some(playhead - step);
            }
            if plain(i, egui::Key::ArrowUp) {
                seek = playback::previous_edit(&self.state.project, playhead);
            }
            if plain(i, egui::Key::ArrowDown) {
                seek = playback::next_edit(&self.state.project, playhead);
            }
            if plain(i, egui::Key::Home) {
                seek = Some(Time::ZERO);
            }
            if plain(i, egui::Key::End) {
                seek = Some(end);
            }
            if plain(i, egui::Key::Space) {
                command = Some(Transport::toggle);
            }
            if plain(i, egui::Key::J) {
                command = Some(Transport::reverse);
            }
            if plain(i, egui::Key::K) {
                command = Some(Transport::pause);
            }
            if plain(i, egui::Key::L) {
                command = Some(Transport::forward);
            }
            if !typing {
                use TimelineCommand as C;
                let shift_cmd = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
                // Shifted variants first: consume_key ignores an unrequested Shift.
                let bindings = [
                    (shift_cmd, egui::Key::Z, C::Redo),
                    (shift_cmd, egui::Key::G, C::Ungroup),
                    (egui::Modifiers::COMMAND, egui::Key::Z, C::Undo),
                    (egui::Modifiers::COMMAND, egui::Key::Y, C::Redo),
                    (egui::Modifiers::COMMAND, egui::Key::G, C::Group),
                    (egui::Modifiers::COMMAND, egui::Key::B, C::Split),
                    (egui::Modifiers::COMMAND, egui::Key::A, C::SelectAll),
                    (egui::Modifiers::SHIFT, egui::Key::Delete, C::RippleDelete),
                    (
                        egui::Modifiers::SHIFT,
                        egui::Key::Backspace,
                        C::RippleDelete,
                    ),
                ];
                for (modifiers, key, c) in bindings {
                    if i.consume_key(modifiers, key) {
                        edits.push(c);
                    }
                }
                for (key, c) in [
                    (egui::Key::S, C::Split),
                    (egui::Key::Q, C::TrimStart),
                    (egui::Key::W, C::TrimEnd),
                    (egui::Key::Delete, C::Delete),
                    (egui::Key::Backspace, C::Delete),
                ] {
                    if plain(i, key) {
                        edits.push(c);
                    }
                }
            }
            if i.consume_key(egui::Modifiers::COMMAND, egui::Key::S) {
                self.actions.push(UiAction::Save);
            }
            if i.consume_key(egui::Modifiers::COMMAND, egui::Key::I) {
                self.actions.push(UiAction::Import);
            }
            // Cmd/Ctrl + and Cmd/Ctrl − zoom the timeline (egui's UI zoom is disabled).
            for key in [egui::Key::Plus, egui::Key::Equals] {
                if i.consume_key(egui::Modifiers::COMMAND, key) {
                    self.state.zoom_step(true, self.timeline_width);
                }
            }
            if i.consume_key(egui::Modifiers::COMMAND, egui::Key::Minus) {
                self.state.zoom_step(false, self.timeline_width);
            }
            if i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num0) {
                self.state.zoom_to_fit(self.timeline_width);
            }
        });
        if let Some(t) = seek {
            self.seek(t);
        }
        if let Some(command) = command {
            self.transport_action(command);
        }
        for edit in edits {
            self.timeline_command(edit);
        }
    }

    fn transport_bar(&mut self, ui: &mut egui::Ui) {
        let rate = self.state.project.settings.frame_rate;
        let end = self.state.project.duration();
        ui.horizontal(|ui| {
            if icon_button(ui, Icon::Start)
                .on_hover_text("Go to start (Home)")
                .clicked()
            {
                self.seek(Time::ZERO);
            }
            if icon_button(ui, Icon::Reverse)
                .on_hover_text("Play backwards, press again for faster (J)")
                .clicked()
            {
                self.transport_action(Transport::reverse);
            }
            let play = if self.transport.is_playing() {
                Icon::Pause
            } else {
                Icon::Play
            };
            if icon_button(ui, play)
                .on_hover_text("Play / pause (Space)")
                .clicked()
            {
                self.transport_action(Transport::toggle);
            }
            if icon_button(ui, Icon::Forward)
                .on_hover_text("Play forwards, press again for faster (L)")
                .clicked()
            {
                self.transport_action(Transport::forward);
            }
            if icon_button(ui, Icon::End)
                .on_hover_text("Go to end (End)")
                .clicked()
            {
                self.seek(end);
            }
            ui.monospace(format!(
                "{} / {}",
                playback::timecode(self.state.playhead, rate),
                playback::timecode(end, rate)
            ));
            if self.transport.is_playing() && self.transport.speed != 1.0 {
                ui.label(
                    egui::RichText::new(format!("{}×", self.transport.speed)).color(theme::ACCENT),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut quality = self.preview_quality;
                egui::ComboBox::from_id_salt("preview_fps")
                    .selected_text(playback::fps_label(quality.max_fps))
                    .show_ui(ui, |ui| {
                        for fps in PREVIEW_FPS {
                            ui.selectable_value(
                                &mut quality.max_fps,
                                fps,
                                playback::fps_label(fps),
                            );
                        }
                    });
                egui::ComboBox::from_id_salt("preview_scale")
                    .selected_text(quality.scale.label())
                    .show_ui(ui, |ui| {
                        for scale in PreviewScale::ALL {
                            ui.selectable_value(&mut quality.scale, scale, scale.label());
                        }
                    })
                    .response
                    .on_hover_text("Preview resolution");
                ui.label(egui::RichText::new("Preview").color(theme::TEXT_DIM));
                if quality != self.preview_quality {
                    self.set_preview_quality(quality);
                }
            });
        });
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    for (label, action) in [
                        ("New project", UiAction::New),
                        ("Open…", UiAction::Open),
                        ("Save", UiAction::Save),
                        ("Save as…", UiAction::SaveAs),
                        ("Collect and save…", UiAction::CollectAndSave),
                        ("Import media…", UiAction::Import),
                    ] {
                        if ui.button(label).clicked() {
                            self.actions.push(action);
                            ui.close();
                        }
                    }
                });
                ui.separator();
                ui.label(egui::RichText::new(self.state.title()).color(theme::TEXT_DIM));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new("Export").fill(theme::ACCENT))
                        .clicked()
                    {
                        self.export_window = true;
                    }
                });
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&self.state.status).color(theme::TEXT_DIM));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} / {}",
                            self.state.playhead,
                            self.state.project.duration()
                        ))
                        .monospace(),
                    );
                });
            });
        });
    }

    fn media_bin(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("browser")
            .resizable(true)
            .default_size(260.0)
            .size_range(180.0..=640.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (BrowserTab::Media, "Media"),
                        (BrowserTab::Effects, "Effects"),
                        (BrowserTab::Generators, "Generators"),
                    ] {
                        ui.selectable_value(&mut self.browser_tab, tab, label);
                    }
                });
                ui.separator();
                match self.browser_tab {
                    BrowserTab::Media => self.media_pool(ui),
                    BrowserTab::Effects => planned_list(ui, PLANNED_EFFECTS),
                    BrowserTab::Generators => planned_list(ui, PLANNED_GENERATORS),
                }
            });
    }

    fn media_pool(&mut self, ui: &mut egui::Ui) {
        if ui.button("Import…").clicked() {
            self.actions.push(UiAction::Import);
        }
        let mut add = false;
        egui::ScrollArea::vertical()
            .max_height(ui.available_height() - 40.0)
            .show(ui, |ui| {
                for asset in &self.state.project.media {
                    let selected = self.state.selected_media == Some(asset.id);
                    let detail = match (&asset.info.video, asset.info.duration) {
                        (Some(v), Some(d)) => format!("{}×{} · {}", v.width, v.height, d),
                        (Some(v), None) => format!("{}×{} · image", v.width, v.height),
                        (None, Some(d)) => format!("audio · {d}"),
                        (None, None) => String::new(),
                    };
                    let response = ui.selectable_label(
                        selected,
                        format!("{}\n{}", asset.display_name(), detail),
                    );
                    if response.clicked() {
                        self.state.selected_media = Some(asset.id);
                    }
                    if response.double_clicked() {
                        self.state.selected_media = Some(asset.id);
                        add = true;
                    }
                }
            });
        ui.separator();
        if ui
            .add_enabled(
                self.state.selected_media.is_some(),
                egui::Button::new("Add to timeline"),
            )
            .clicked()
        {
            add = true;
        }
        if add && self.state.append_selected_media().is_some() {
            self.touch();
        }
    }

    fn preview_panel(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::no_frame()
            .frame(egui::Frame::new().fill(theme::BG))
            .show(ui, |ui| {
                egui::Panel::bottom("transport").show(ui, |ui| self.transport_bar(ui));
                let rect = ui.available_rect_before_wrap();
                match &self.preview_texture {
                    Some(tex) => {
                        let size = fit_aspect(rect.size(), tex.size_vec2());
                        let target = Rect::from_center_size(rect.center(), size);
                        egui::Image::new((tex.id(), size)).paint_at(ui, target);
                    }
                    None => {
                        ui.centered_and_justified(|ui| {
                            ui.label(egui::RichText::new("No preview").color(theme::TEXT_DIM));
                        });
                    }
                }
            });
    }

    fn timeline(&mut self, ui: &mut egui::Ui) {
        let height = RULER_HEIGHT + TRACK_HEIGHT * self.state.project.tracks.len() as f32 + 48.0;
        egui::Panel::bottom("timeline")
            .resizable(true)
            .default_size(height.max(200.0))
            .size_range(120.0..=f32::INFINITY)
            .show(ui, |ui| {
                let mut commands = Vec::new();
                self.timeline_toolbar(ui, &mut commands);
                let rect = ui.available_rect_before_wrap();
                let response = ui.allocate_rect(rect, Sense::click_and_drag());
                self.timeline_input(ui, rect, &response);
                self.timeline_clips(ui, rect, &mut commands);
                for command in commands {
                    self.timeline_command(command);
                }
                self.paint_timeline(ui, rect);
            });
    }

    fn timeline_toolbar(&mut self, ui: &mut egui::Ui, commands: &mut Vec<TimelineCommand>) {
        let cmd = if cfg!(target_os = "macos") {
            "Cmd"
        } else {
            "Ctrl"
        };
        ui.horizontal(|ui| {
            let mut button = |ui: &mut egui::Ui, enabled: bool, label: &str, tip: String, c| {
                if ui
                    .add_enabled(enabled, egui::Button::new(label))
                    .on_hover_text(tip)
                    .clicked()
                {
                    commands.push(c);
                }
            };
            button(
                ui,
                self.state.can_undo(),
                "Undo",
                format!("Undo ({cmd} Z)"),
                TimelineCommand::Undo,
            );
            button(
                ui,
                self.state.can_redo(),
                "Redo",
                format!("Redo ({cmd} Shift Z)"),
                TimelineCommand::Redo,
            );
            ui.separator();
            button(
                ui,
                true,
                "Split",
                format!("Split at the playhead (S or {cmd} B)"),
                TimelineCommand::Split,
            );
            button(
                ui,
                true,
                "+ Video lane",
                "Add an empty video lane".into(),
                TimelineCommand::AddLane(TrackKind::Video),
            );
            button(
                ui,
                true,
                "+ Audio lane",
                "Add an empty audio lane".into(),
                TimelineCommand::AddLane(TrackKind::Audio),
            );
            ui.toggle_value(&mut self.snapping, "Snap")
                .on_hover_text("Snap clips to clip edges and the playhead");
            ui.separator();
            ui.label(egui::RichText::new("Zoom").color(theme::TEXT_DIM));
            let width = self.timeline_width;
            if ui
                .add_enabled(self.state.zoom > MIN_ZOOM, egui::Button::new("−"))
                .on_hover_text(format!("Zoom out ({cmd} − or {cmd} + scroll)"))
                .clicked()
            {
                self.state.zoom_step(false, width);
            }
            if ui
                .add_enabled(self.state.zoom < MAX_ZOOM, egui::Button::new("+"))
                .on_hover_text(format!("Zoom in ({cmd} + or {cmd} + scroll)"))
                .clicked()
            {
                self.state.zoom_step(true, width);
            }
            if ui
                .button("Fit")
                .on_hover_text(format!("Show the whole project ({cmd} 0)"))
                .clicked()
            {
                self.state.zoom_to_fit(width);
            }
        });
    }

    fn timeline_input(&mut self, ui: &egui::Ui, rect: Rect, response: &egui::Response) {
        let origin = rect.left() + HEADER_WIDTH;
        self.timeline_width = (rect.right() - origin).max(0.0);
        if let Some(pos) = response.interact_pointer_pos()
            && pos.x >= origin
        {
            self.state
                .set_playhead(self.state.x_to_time(pos.x - origin));
            if response.clicked() && pos.y > rect.top() + RULER_HEIGHT {
                self.state.selected_clips.clear();
            }
        }
        if response.hovered() {
            // Cmd/Ctrl + wheel and pinch arrive as zoom_delta.
            let (scroll, zoom, pointer) =
                ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
            if zoom != 1.0 {
                let x = pointer.map_or(0.0, |p| (p.x - origin).max(0.0));
                self.state.zoom_around(zoom, x);
            }
            if scroll.x != 0.0 || scroll.y != 0.0 {
                let delta = -(scroll.x + scroll.y) / self.state.zoom;
                self.state.scroll = (self.state.scroll + f64::from(delta)).max(0.0);
            }
        }
    }

    /// Track indices in on-screen order (highest video lane first, then the
    /// audio lanes) with their row rectangles.
    fn lane_layout(&self, rect: Rect) -> Vec<(usize, Rect)> {
        let mut order = self.state.project.lanes_of(TrackKind::Video);
        order.reverse();
        order.extend(self.state.project.lanes_of(TrackKind::Audio));
        order
            .into_iter()
            .enumerate()
            .map(|(row, i)| {
                let top = rect.top() + RULER_HEIGHT + row as f32 * TRACK_HEIGHT;
                (
                    i,
                    Rect::from_min_size(
                        Pos2::new(rect.left(), top),
                        Vec2::new(rect.width(), TRACK_HEIGHT - 2.0),
                    ),
                )
            })
            .collect()
    }

    /// Visible part of a clip in a lane, `None` when scrolled out of view.
    fn clip_rect(&self, rect: Rect, lane: Rect, clip: &framework_core::Clip) -> Option<Rect> {
        let origin = rect.left() + HEADER_WIDTH;
        let x0 = origin + self.state.time_to_x(clip.start);
        let x1 = origin + self.state.time_to_x(clip.end());
        (x1 >= origin && x0 <= rect.right()).then(|| {
            Rect::from_min_max(
                Pos2::new(x0.max(origin), lane.top() + 3.0),
                Pos2::new(x1.min(rect.right()), lane.top() + TRACK_HEIGHT - 5.0),
            )
        })
    }

    /// Mute and solo buttons in a lane header.
    fn lane_buttons(header: Rect) -> (Rect, Rect) {
        let m = Rect::from_min_size(
            Pos2::new(header.left() + 6.0, header.center().y + 1.0),
            Vec2::new(20.0, 15.0),
        );
        (m, m.translate(Vec2::new(24.0, 0.0)))
    }

    /// Lane headers and clip interactions: selection, drag to move, edge
    /// drags to trim, context menus.
    fn timeline_clips(&mut self, ui: &egui::Ui, rect: Rect, commands: &mut Vec<TimelineCommand>) {
        let modifiers = ui.input(|i| i.modifiers);
        let rows: Vec<_> = self
            .lane_layout(rect)
            .into_iter()
            .map(|(i, lane)| {
                let track = &self.state.project.tracks[i];
                let clips: Vec<_> = track
                    .clips
                    .iter()
                    .filter_map(|c| Some((c.id, self.clip_rect(rect, lane, c)?)))
                    .collect();
                (track.id, track.kind, lane, clips)
            })
            .collect();
        self.clip_rects.clear();
        self.lane_rects.clear();
        let mut select = None;
        let mut start_drag = None;
        for (track, kind, lane, clips) in rows {
            let header =
                Rect::from_min_size(lane.min, Vec2::new(HEADER_WIDTH - 4.0, lane.height()));
            self.lane_rects.push((track, header));
            ui.interact(header, ui.scope_id().with(("lane", track)), Sense::click())
                .context_menu(|ui| lane_menu(ui, track, kind, commands));
            let (mute, solo) = Self::lane_buttons(header);
            let tip = match kind {
                TrackKind::Video => "Mute: hide this lane",
                TrackKind::Audio => "Mute: silence this lane",
            };
            if ui
                .interact(mute, ui.scope_id().with(("mute", track)), Sense::click())
                .on_hover_text(tip)
                .clicked()
            {
                commands.push(TimelineCommand::Mute(track));
            }
            if ui
                .interact(solo, ui.scope_id().with(("solo", track)), Sense::click())
                .on_hover_text("Solo: play only soloed lanes of this kind")
                .clicked()
            {
                commands.push(TimelineCommand::Solo(track));
            }
            for (id, r) in clips {
                self.clip_rects.push((id, r));
                let body =
                    ui.interact(r, ui.scope_id().with(("clip", id)), Sense::click_and_drag());
                let edge = EDGE_PX.min(r.width() / 3.0);
                let left = ui.interact(
                    Rect::from_min_max(r.min, Pos2::new(r.left() + edge, r.bottom())),
                    ui.scope_id().with(("trim-start", id)),
                    Sense::drag(),
                );
                let right = ui.interact(
                    Rect::from_min_max(Pos2::new(r.right() - edge, r.top()), r.max),
                    ui.scope_id().with(("trim-end", id)),
                    Sense::drag(),
                );
                for (response, kind) in [
                    (&body, DragKind::Move),
                    (&left, DragKind::TrimStart),
                    (&right, DragKind::TrimEnd),
                ] {
                    if response.drag_started()
                        && let Some(pos) = response.interact_pointer_pos()
                    {
                        start_drag = Some(ClipDrag {
                            kind,
                            clip: id,
                            origin: pos,
                        });
                    }
                }
                if left.hovered() || right.hovered() || left.dragged() || right.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                } else if body.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                } else if body.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                }
                if body.clicked() {
                    select = Some((id, modifiers.shift || modifiers.command));
                }
                if body.secondary_clicked() && !self.state.selected_clips.contains(&id) {
                    select = Some((id, false));
                }
                body.context_menu(|ui| clip_menu(ui, commands));
            }
        }
        if let Some((id, additive)) = select {
            self.state.select_clip(id, additive);
        }
        if let Some(drag) = start_drag {
            if !self.state.selected_clips.contains(&drag.clip) {
                self.state
                    .select_clip(drag.clip, modifiers.shift || modifiers.command);
            }
            self.state.begin_gesture();
            self.drag = Some(drag);
        }
        if let Some(drag) = self.drag {
            if ui.input(|i| i.pointer.primary_down()) {
                self.update_drag(ui, drag);
            } else {
                let label = match drag.kind {
                    DragKind::Move => "Moved",
                    DragKind::TrimStart | DragKind::TrimEnd => "Trimmed",
                };
                self.state.end_gesture(label);
                self.drag = None;
            }
        }
    }

    /// Re-applies the ongoing drag to the project as it was when the drag started.
    fn update_drag(&mut self, ui: &egui::Ui, drag: ClipDrag) {
        let (Some(pos), Some(before)) = (
            ui.input(|i| i.pointer.interact_pos()),
            self.state.gesture_start().cloned(),
        ) else {
            return;
        };
        let Some(clip) = before.clip(drag.clip).cloned() else {
            return;
        };
        let zoom = f64::from(self.state.zoom);
        let dx = Time::from_seconds(f64::from(pos.x - drag.origin.x) / zoom);
        let ids = if self.state.selected_clips.contains(&clip.id) {
            self.state.selected_clips.clone()
        } else {
            vec![clip.id]
        };
        let moving = match drag.kind {
            DragKind::Move => before.with_groups(&ids),
            _ => vec![clip.id],
        };
        let mut points: Vec<Time> = before
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .filter(|c| !moving.contains(&c.id))
            .flat_map(|c| [c.start, c.end()])
            .collect();
        points.extend([Time::ZERO, self.state.playhead]);
        let threshold = Time::from_seconds(f64::from(SNAP_PX) / zoom);
        // Smallest correction that puts one of `edges` onto a snap point.
        let snap = |edges: &[Time]| -> Time {
            if !self.snapping {
                return Time::ZERO;
            }
            edges
                .iter()
                .flat_map(|e| points.iter().map(move |p| *p - *e))
                .filter(|d| d.flicks().abs() <= threshold.flicks())
                .min_by_key(|d| d.flicks().abs())
                .unwrap_or(Time::ZERO)
        };
        let min = self.state.min_clip_length();
        let mut project = before.clone();
        let result = match drag.kind {
            DragKind::Move => {
                let earliest = moving
                    .iter()
                    .filter_map(|id| before.clip(*id))
                    .map(|c| c.start)
                    .min()
                    .unwrap_or(Time::ZERO);
                let mut delta = dx + snap(&[clip.start + dx, clip.end() + dx]);
                delta = delta.max(-earliest);
                let rows = ((pos.y - drag.origin.y) / TRACK_HEIGHT).round() as isize;
                let video = before
                    .locate(clip.id)
                    .is_some_and(|(t, _)| before.tracks[t].kind == TrackKind::Video);
                // Video lanes are drawn top-down from the highest track index.
                let shift = if video { -rows } else { rows };
                project.move_clips(&ids, delta, shift).or_else(|_| {
                    project = before.clone();
                    project.move_clips(&ids, delta, 0)
                })
            }
            DragKind::TrimStart => {
                let t = clip.start + dx;
                project.trim_start(clip.id, t + snap(&[t]), min).map(|_| ())
            }
            DragKind::TrimEnd => {
                let t = clip.end() + dx;
                project.trim_end(clip.id, t + snap(&[t]), min).map(|_| ())
            }
        };
        if result.is_ok() && project != self.state.project {
            self.state.project = project;
            self.touch();
        }
    }

    /// Applies a timeline command from a shortcut, button or context menu.
    pub fn timeline_command(&mut self, command: TimelineCommand) {
        let st = &mut self.state;
        match command {
            TimelineCommand::Split => {
                st.split_at_playhead();
            }
            TimelineCommand::TrimStart => {
                st.trim_to_playhead(true);
            }
            TimelineCommand::TrimEnd => {
                st.trim_to_playhead(false);
            }
            TimelineCommand::Delete => {
                st.delete_selection(false);
            }
            TimelineCommand::RippleDelete => {
                st.delete_selection(true);
            }
            TimelineCommand::Group => {
                st.group_selection();
            }
            TimelineCommand::Ungroup => {
                st.ungroup_selection();
            }
            TimelineCommand::SelectAll => st.select_all(),
            TimelineCommand::Undo => {
                st.undo();
            }
            TimelineCommand::Redo => {
                st.redo();
            }
            TimelineCommand::AddLane(kind) => {
                st.add_lane(kind);
            }
            TimelineCommand::RemoveLane(id) => {
                st.remove_lane(id);
            }
            TimelineCommand::MoveLane(id, up) => {
                st.move_lane(id, up);
            }
            TimelineCommand::Mute(id) => st.toggle_mute(id),
            TimelineCommand::Solo(id) => st.toggle_solo(id),
        }
        self.touch();
    }

    fn paint_timeline(&self, ui: &egui::Ui, rect: Rect) {
        let painter = ui.painter_at(rect);
        let origin = rect.left() + HEADER_WIDTH;
        let font = FontId::proportional(11.0);

        // Ruler.
        let step = self.state.ruler_step(80.0);
        let first = (self.state.scroll / step).floor() as i64;
        let mut i = first;
        loop {
            let t = Time::from_seconds(i as f64 * step);
            let x = origin + self.state.time_to_x(t);
            if x > rect.right() {
                break;
            }
            if x >= origin {
                painter.line_segment(
                    [
                        Pos2::new(x, rect.top()),
                        Pos2::new(x, rect.top() + RULER_HEIGHT),
                    ],
                    Stroke::new(1.0, theme::TEXT_DIM),
                );
                painter.text(
                    Pos2::new(x + 3.0, rect.top() + 2.0),
                    Align2::LEFT_TOP,
                    crate::state::ruler_label(t, step),
                    font.clone(),
                    theme::TEXT_DIM,
                );
            }
            i += 1;
        }

        let project = &self.state.project;
        let linked = project.with_groups(&self.state.selected_clips);
        for (index, lane) in self.lane_layout(rect) {
            let track = &project.tracks[index];
            let active = project.lane_active(track);
            painter.rect_filled(lane, 4.0, theme::LANE);
            painter.text(
                Pos2::new(rect.left() + 8.0, lane.top() + 5.0),
                Align2::LEFT_TOP,
                &track.name,
                FontId::proportional(13.0),
                if active {
                    Color32::WHITE
                } else {
                    theme::TEXT_DIM
                },
            );
            let header =
                Rect::from_min_size(lane.min, Vec2::new(HEADER_WIDTH - 4.0, lane.height()));
            let (mute, solo) = Self::lane_buttons(header);
            for (r, label, on, color) in [
                (mute, "M", track.muted, MUTE_COLOR),
                (solo, "S", track.solo, SOLO_COLOR),
            ] {
                painter.rect_filled(r, 3.0, if on { color } else { theme::PANEL });
                painter.text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    label,
                    FontId::proportional(10.0),
                    if on { Color32::BLACK } else { theme::TEXT_DIM },
                );
            }
            for clip in &track.clips {
                let (Some(asset), Some(r)) =
                    (project.media(clip.media), self.clip_rect(rect, lane, clip))
                else {
                    continue;
                };
                let mut fill = match asset.kind() {
                    _ if track.kind == TrackKind::Audio => theme::AUDIO_CLIP,
                    MediaKind::Video => theme::VIDEO_CLIP,
                    MediaKind::Image => theme::IMAGE_CLIP,
                    MediaKind::Audio => theme::AUDIO_CLIP,
                };
                if !active {
                    fill = fill.gamma_multiply(0.4);
                }
                painter.rect_filled(r, 4.0, fill);
                let selected = self.state.selected_clips.contains(&clip.id);
                if selected || linked.contains(&clip.id) {
                    let width = if selected { 2.0 } else { 1.0 };
                    painter.rect_stroke(
                        r,
                        4.0,
                        Stroke::new(width, Color32::WHITE),
                        StrokeKind::Inside,
                    );
                }
                if selected && r.width() > 3.0 * EDGE_PX {
                    // Trim handles.
                    for x in [r.left() + 2.0, r.right() - 5.0] {
                        painter.rect_filled(
                            Rect::from_min_size(
                                Pos2::new(x, r.center().y - 8.0),
                                Vec2::new(3.0, 16.0),
                            ),
                            1.5,
                            Color32::WHITE,
                        );
                    }
                }
                let mut name = asset.display_name();
                if clip.group.is_some() {
                    name = format!("⛓ {name}");
                }
                painter.with_clip_rect(r.shrink(2.0)).text(
                    r.left_center() + Vec2::new(8.0, 0.0),
                    Align2::LEFT_CENTER,
                    name,
                    font.clone(),
                    Color32::WHITE,
                );
            }
        }

        // Playhead.
        let x = origin + self.state.time_to_x(self.state.playhead);
        if x >= origin && x <= rect.right() {
            painter.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(2.0, theme::PLAYHEAD),
            );
        }
    }

    fn unsaved_dialog(&mut self, ctx: &egui::Context) {
        if self.pending.is_none() {
            return;
        }
        egui::Window::new("Unsaved changes")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("The project has unsaved changes.");
                ui.horizontal(|ui| {
                    for (label, action) in [
                        ("Save", UiAction::SaveAndContinue),
                        ("Discard", UiAction::DiscardChanges),
                        ("Cancel", UiAction::CancelPending),
                    ] {
                        if ui.button(label).clicked() {
                            self.actions.push(action);
                        }
                    }
                });
            });
    }

    fn export_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.export_window;
        egui::Window::new("Export")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                let mut chosen = None;
                egui::ComboBox::from_label("Profile")
                    .selected_text(self.state.selected_profile().name.clone())
                    .show_ui(ui, |ui| {
                        for (i, p) in self.state.profiles.iter().enumerate() {
                            let selected = self.state.export_profile == Some(i);
                            if ui.selectable_label(selected, &p.name).clicked() {
                                chosen = Some(i);
                            }
                        }
                        ui.add_enabled(
                            false,
                            egui::Button::selectable(self.state.export_profile.is_none(), "Custom"),
                        );
                    });
                if let Some(i) = chosen {
                    self.state.choose_profile(i);
                }
                export_settings_editor(ui, &mut self.state.export_settings.video);
                self.state.sync_custom_profile();
                ui.label(egui::RichText::new("H.264 / AAC · MP4").color(theme::TEXT_DIM));
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.state.export_path);
                    if ui.button("Browse…").clicked() {
                        self.actions.push(UiAction::ChooseExportPath);
                    }
                });
                match self.export_job.as_ref().map(ExportJob::state) {
                    Some(ExportState::Running(p)) => {
                        ui.add(egui::ProgressBar::new(p.fraction()).show_percentage());
                        if ui.button("Cancel").clicked() {
                            self.actions.push(UiAction::CancelExport);
                        }
                    }
                    other => {
                        match other {
                            Some(ExportState::Done(path)) => {
                                ui.label(format!("Exported {}", path.display()));
                            }
                            Some(ExportState::Failed(e)) => {
                                ui.colored_label(theme::PLAYHEAD, e);
                            }
                            _ => {}
                        }
                        if ui
                            .add(egui::Button::new("Export").fill(theme::ACCENT))
                            .clicked()
                        {
                            self.actions.push(UiAction::StartExport);
                        }
                    }
                }
            });
        self.export_window = open;
    }
}

/// Free resolution, frame rate and bitrate fields of the export dialog.
fn export_settings_editor(ui: &mut egui::Ui, video: &mut framework_core::VideoSettings) {
    egui::Grid::new("export-settings")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Resolution");
            ui.horizontal(|ui| {
                for (value, label) in [(&mut video.width, "width"), (&mut video.height, "height")] {
                    ui.add(
                        egui::DragValue::new(value)
                            .range(16..=8192)
                            .speed(2.0)
                            .suffix(" px"),
                    )
                    .on_hover_text(label);
                }
            });
            ui.end_row();
            ui.label("Frame rate");
            let mut fps = video.frame_rate.as_f64();
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut fps)
                        .range(1.0..=240.0)
                        .max_decimals(3)
                        .suffix(" fps"),
                );
                egui::ComboBox::from_id_salt("fps-presets")
                    .selected_text("Presets")
                    .show_ui(ui, |ui| {
                        for preset in [23.976, 24.0, 25.0, 29.97, 30.0, 50.0, 59.94, 60.0] {
                            if ui.selectable_label(false, format!("{preset}")).clicked() {
                                fps = preset;
                            }
                        }
                    });
            });
            if (fps - video.frame_rate.as_f64()).abs() > 1e-6 {
                video.frame_rate = rate_from_fps(fps);
            }
            ui.end_row();
            ui.label("Video bitrate");
            ui.add(
                egui::DragValue::new(&mut video.bitrate_kbps)
                    .range(100..=500_000)
                    .speed(50.0)
                    .suffix(" kbit/s"),
            );
            ui.end_row();
        });
}

/// The profile with even dimensions, as H.264 with 4:2:0 chroma requires.
/// Applied at export time so typing a width digit by digit is not disturbed.
pub fn encodable(profile: &framework_core::ExportProfile) -> framework_core::ExportProfile {
    let mut p = profile.clone();
    p.video.width = (p.video.width & !1).max(2);
    p.video.height = (p.video.height & !1).max(2);
    p
}

/// Exact rational for a frame rate, recognising NTSC rates such as 29.97 (30000/1001).
pub fn rate_from_fps(fps: f64) -> framework_core::Rational {
    let ntsc = (fps * 1.001).round();
    if fps.fract() == 0.0 {
        framework_core::Rational::new(fps.max(1.0) as u32, 1)
    } else if (ntsc * 1000.0 / 1001.0 - fps).abs() < 0.005 {
        framework_core::Rational::new(ntsc as u32 * 1000, 1001)
    } else {
        framework_core::Rational::new((fps * 1000.0).round().max(1.0) as u32, 1000)
    }
}

fn planned_list(ui: &mut egui::Ui, items: &[(&str, &str)]) {
    for (name, when) in items {
        ui.add_enabled_ui(false, |ui| {
            ui.horizontal(|ui| {
                ui.label(*name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(*when).small().color(theme::TEXT_DIM));
                });
            });
        });
    }
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("Coming in later phases – see the roadmap.")
            .small()
            .color(theme::TEXT_DIM),
    );
}

/// Largest size with `content`'s aspect ratio that fits into `avail`.
pub fn fit_aspect(avail: Vec2, content: Vec2) -> Vec2 {
    if content.x <= 0.0 || content.y <= 0.0 || avail.x <= 0.0 || avail.y <= 0.0 {
        return Vec2::ZERO;
    }
    let scale = (avail.x / content.x).min(avail.y / content.y);
    content * scale
}

/// Appends `.fwk` unless the path already ends in it.
pub fn with_project_extension(path: &std::path::Path) -> PathBuf {
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(PROJECT_EXTENSION))
    {
        path.to_owned()
    } else {
        let mut p = path.as_os_str().to_owned();
        p.push(format!(".{PROJECT_EXTENSION}"));
        PathBuf::from(p)
    }
}

/// Path a project chosen in a save dialog is written to. The dialog only
/// confirmed replacing `chosen`, so an existing file that appending `.fwk`
/// would silently replace is refused.
pub fn project_save_target(chosen: &std::path::Path) -> Result<PathBuf, String> {
    let path = with_project_extension(chosen);
    if path != chosen && path.exists() {
        Err(format!(
            "{} already exists; select it in the dialog to replace it",
            path.display()
        ))
    } else {
        Ok(path)
    }
}

impl eframe::App for FrameworkApp {
    // `logic` also runs while the window is minimised, unlike `ui`.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_close_request(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        FrameworkApp::ui(self, ui);
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Title(self.state.title()));
    }
}

/// Width of the grab zone at each clip edge for trimming.
const EDGE_PX: f32 = 6.0;
/// Distance within which dragged clips snap to edges and the playhead.
const SNAP_PX: f32 = 8.0;
const MUTE_COLOR: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x4c);
const SOLO_COLOR: Color32 = Color32::from_rgb(0xe8, 0xc5, 0x47);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragKind {
    Move,
    TrimStart,
    TrimEnd,
}

#[derive(Debug, Clone, Copy)]
struct ClipDrag {
    kind: DragKind,
    clip: ClipId,
    origin: Pos2,
}

/// Timeline edits triggered by shortcuts, toolbar buttons and context menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineCommand {
    Split,
    TrimStart,
    TrimEnd,
    Delete,
    RippleDelete,
    Group,
    Ungroup,
    SelectAll,
    Undo,
    Redo,
    AddLane(TrackKind),
    RemoveLane(TrackId),
    /// Move a lane up (`true`) or down on screen.
    MoveLane(TrackId, bool),
    Mute(TrackId),
    Solo(TrackId),
}

fn shortcut_label(cmd: bool, shift: bool, key: &str) -> String {
    let mut s = String::new();
    if cmd {
        s.push_str(if cfg!(target_os = "macos") {
            "Cmd "
        } else {
            "Ctrl "
        });
    }
    if shift {
        s.push_str("Shift ");
    }
    s + key
}

fn menu_item(ui: &mut egui::Ui, label: &str, shortcut: String) -> bool {
    ui.add(egui::Button::new(label).shortcut_text(shortcut))
        .clicked()
}

fn clip_menu(ui: &mut egui::Ui, commands: &mut Vec<TimelineCommand>) {
    use TimelineCommand as C;
    let items = [
        (
            "Split at playhead",
            shortcut_label(false, false, "S"),
            C::Split,
        ),
        (
            "Trim start to playhead",
            shortcut_label(false, false, "Q"),
            C::TrimStart,
        ),
        (
            "Trim end to playhead",
            shortcut_label(false, false, "W"),
            C::TrimEnd,
        ),
        ("Delete", shortcut_label(false, false, "Del"), C::Delete),
        (
            "Ripple delete",
            shortcut_label(false, true, "Del"),
            C::RippleDelete,
        ),
        ("Group", shortcut_label(true, false, "G"), C::Group),
        ("Ungroup", shortcut_label(true, true, "G"), C::Ungroup),
    ];
    for (i, (label, shortcut, command)) in items.into_iter().enumerate() {
        if i == 3 || i == 5 {
            ui.separator();
        }
        if menu_item(ui, label, shortcut) {
            commands.push(command);
            ui.close();
        }
    }
}

fn lane_menu(
    ui: &mut egui::Ui,
    track: TrackId,
    kind: TrackKind,
    commands: &mut Vec<TimelineCommand>,
) {
    use TimelineCommand as C;
    let items = [
        ("Add video lane", C::AddLane(TrackKind::Video)),
        ("Add audio lane", C::AddLane(TrackKind::Audio)),
        ("Move lane up", C::MoveLane(track, true)),
        ("Move lane down", C::MoveLane(track, false)),
        (
            match kind {
                TrackKind::Video => "Delete video lane",
                TrackKind::Audio => "Delete audio lane",
            },
            C::RemoveLane(track),
        ),
    ];
    for (label, command) in items {
        if ui.button(label).clicked() {
            commands.push(command);
            ui.close();
        }
    }
}

/// How long the audio clock may stand still before playback falls back to the wall clock.
const AUDIO_STALL: std::time::Duration = std::time::Duration::from_millis(500);

/// Transport icons, painted as vector shapes so they look the same everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Icon {
    Start,
    Reverse,
    Play,
    Pause,
    Forward,
    End,
}

fn icon_button(ui: &mut egui::Ui, icon: Icon) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(26.0, 22.0), Sense::click());
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, 4.0, ui.visuals().widgets.hovered.bg_fill);
    }
    let color = if response.hovered() {
        Color32::WHITE
    } else {
        ui.visuals().text_color()
    };
    let c = rect.center();
    // Triangle pointing towards x = `tip`, with its flat side at x = `base`.
    let triangle = |tip: f32, base: f32| {
        egui::Shape::convex_polygon(
            vec![
                Pos2::new(base, c.y - 5.0),
                Pos2::new(tip, c.y),
                Pos2::new(base, c.y + 5.0),
            ],
            color,
            Stroke::NONE,
        )
    };
    let bar = |x: f32| {
        egui::Shape::rect_filled(
            Rect::from_min_max(Pos2::new(x, c.y - 5.0), Pos2::new(x + 2.0, c.y + 5.0)),
            0.0,
            color,
        )
    };
    let shapes = match icon {
        Icon::Play => vec![triangle(c.x + 5.0, c.x - 4.0)],
        Icon::Pause => vec![bar(c.x - 4.0), bar(c.x + 2.0)],
        Icon::Reverse => vec![triangle(c.x - 7.0, c.x), triangle(c.x, c.x + 7.0)],
        Icon::Forward => vec![triangle(c.x + 7.0, c.x), triangle(c.x, c.x - 7.0)],
        Icon::Start => vec![bar(c.x - 6.0), triangle(c.x - 4.0, c.x + 5.0)],
        Icon::End => vec![triangle(c.x + 4.0, c.x - 5.0), bar(c.x + 4.0)],
    };
    painter.extend(shapes);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, PointerButton, RawInput, Rect, pos2, vec2};
    use framework_core::{MediaAsset, Project};

    fn input(events: Vec<Event>) -> RawInput {
        RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 800.0))),
            events,
            ..Default::default()
        }
    }

    fn frame(app: &mut FrameworkApp, ctx: &egui::Context, events: Vec<Event>) {
        let mut out = ctx.run_ui(input(events), |ui| app.ui(ui));
        out.textures_delta.clear();
    }

    fn key(key: egui::Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn click(pos: egui::Pos2) -> Vec<Event> {
        vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ]
    }

    fn test_dir() -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("framework-app-ui-{}-{n}", std::process::id()))
    }

    fn app_with_project() -> (egui::Context, FrameworkApp) {
        let dir = test_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("ui.mp4");
        let image = dir.join("ui.png");
        let audio = dir.join("ui.wav");
        framework_media::synthetic::write_test_video(&video, (64, 36), 10, 10, true).unwrap();
        framework_media::synthetic::write_test_png(&image, (16, 16), [9, 9, 9]).unwrap();
        framework_media::synthetic::write_test_wav(&audio, 8000, 1, 0.5, 100).unwrap();
        let mut project = Project::new("ui");
        for path in [&video, &image, &audio] {
            let info = framework_media::probe(path).unwrap();
            let kind = info.kind;
            let id = project.add_media(MediaAsset::new(path.clone(), info));
            let track = project.default_track_for(kind);
            project.append_media(track, id).unwrap();
        }
        let ctx = egui::Context::default();
        let mut app = FrameworkApp::new(&ctx);
        app.use_dialogs = false;
        app.state = EditorState::new(project);
        app.state.export_path = dir.join("ui-export.mp4").to_string_lossy().into_owned();
        app.state.project_path = Some(dir.join("project.fwk"));
        app.touch();
        (ctx, app)
    }

    #[test]
    fn collect_and_save_embeds_media() {
        let (ctx, mut app) = app_with_project();
        let path = app
            .state
            .project_path
            .clone()
            .unwrap()
            .with_file_name("collected.fwk");
        app.save_project(&path, SaveMode::Collect);
        assert!(
            app.state.status.contains("embedded media"),
            "{}",
            app.state.status
        );
        assert!(!app.state.dirty);
        assert!(app.state.project.media.iter().all(|m| m.location.is_some()));
        // Re-saving over the open container works while the preview reads from it.
        frame(&mut app, &ctx, vec![]);
        app.save_project(&path, SaveMode::Reference);
        assert!(
            app.state.status.starts_with("Saved"),
            "{}",
            app.state.status
        );
        assert!(app.open_project(&path));
        assert!(app.state.project.media.iter().all(|m| m.embedded.is_some()));
    }

    #[test]
    fn timeline_zoom_shortcuts() {
        let (ctx, mut app) = app_with_project();
        frame(&mut app, &ctx, vec![]);
        assert!(app.timeline_width > 0.0);
        let zoom = app.state.zoom;
        frame(
            &mut app,
            &ctx,
            vec![key(egui::Key::Plus, Modifiers::COMMAND)],
        );
        assert_eq!(app.state.zoom, zoom * crate::state::ZOOM_STEP);
        frame(
            &mut app,
            &ctx,
            vec![key(egui::Key::Equals, Modifiers::COMMAND)],
        );
        frame(
            &mut app,
            &ctx,
            vec![key(egui::Key::Minus, Modifiers::COMMAND)],
        );
        frame(
            &mut app,
            &ctx,
            vec![key(egui::Key::Minus, Modifiers::COMMAND)],
        );
        assert!((app.state.zoom - zoom).abs() < 1e-3);
        frame(
            &mut app,
            &ctx,
            vec![key(egui::Key::Num0, Modifiers::COMMAND)],
        );
        let duration = app.state.project.duration().as_seconds() as f32;
        assert!((app.state.zoom * duration - app.timeline_width).abs() < 1.0);
        // The whole UI does not zoom.
        assert_eq!(ctx.zoom_factor(), 1.0);
    }

    #[test]
    fn export_dialog_with_custom_settings() {
        let (ctx, mut app) = app_with_project();
        app.export_window = true;
        frame(&mut app, &ctx, vec![]);
        app.state.choose_profile(2);
        app.state.export_settings.video.width = 1001;
        frame(&mut app, &ctx, vec![]);
        assert_eq!(app.state.export_settings.video.width, 1001);
        assert_eq!(encodable(app.state.selected_profile()).video.width, 1000);
        assert_eq!(app.state.export_profile, None);
        assert_eq!(app.state.selected_profile().name, "Custom");
    }

    #[test]
    fn frame_rates_become_exact_rationals() {
        use framework_core::Rational;
        assert_eq!(rate_from_fps(25.0), Rational::new(25, 1));
        assert_eq!(rate_from_fps(29.97), Rational::new(30000, 1001));
        assert_eq!(rate_from_fps(23.976), Rational::new(24000, 1001));
        assert_eq!(rate_from_fps(59.94), Rational::new(60000, 1001));
        assert_eq!(rate_from_fps(12.5), Rational::new(12500, 1000));
    }

    #[test]
    fn save_dialog_never_replaces_an_unconfirmed_project() {
        let dir = std::env::temp_dir().join(format!("framework-target-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("taken.fwk");
        std::fs::write(&existing, b"x").unwrap();
        assert!(project_save_target(&dir.join("taken")).is_err());
        assert_eq!(project_save_target(&existing).unwrap(), existing);
        assert_eq!(
            project_save_target(&dir.join("new")).unwrap(),
            dir.join("new.fwk")
        );
    }

    #[test]
    fn project_extension_is_appended() {
        use std::path::Path;
        assert_eq!(
            with_project_extension(Path::new("a/b")),
            Path::new("a/b.fwk")
        );
        assert_eq!(
            with_project_extension(Path::new("a/b.fwk")),
            Path::new("a/b.fwk")
        );
        assert_eq!(
            with_project_extension(Path::new("a/b.FWK")),
            Path::new("a/b.FWK")
        );
        assert_eq!(
            with_project_extension(Path::new("b.json")),
            Path::new("b.json.fwk")
        );
    }

    #[test]
    fn renders_all_panels_and_receives_preview() {
        let (ctx, mut app) = app_with_project();
        app.state.selected_clips = vec![app.state.project.tracks[0].clips[0].id];
        for _ in 0..200 {
            frame(&mut app, &ctx, vec![]);
            if app.preview_texture.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.preview_texture.is_some());
        // A second preview result updates the existing texture.
        app.state.set_playhead(Time::from_seconds(3.0));
        for _ in 0..100 {
            frame(&mut app, &ctx, vec![]);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        for tab in [
            BrowserTab::Effects,
            BrowserTab::Generators,
            BrowserTab::Media,
        ] {
            app.browser_tab = tab;
            frame(&mut app, &ctx, vec![]);
        }
        // Zoomed far out and scrolled: ruler/clip culling paths.
        app.state.zoom = 2.0;
        app.state.scroll = 0.5;
        frame(&mut app, &ctx, vec![]);
    }

    #[test]
    fn keyboard_shortcuts_move_playhead() {
        let (ctx, mut app) = app_with_project();
        frame(
            &mut app,
            &ctx,
            vec![key(egui::Key::ArrowRight, Modifiers::NONE)],
        );
        assert_eq!(
            app.state.playhead,
            Time::from_frames(1, app.state.project.settings.frame_rate)
        );
        frame(
            &mut app,
            &ctx,
            vec![key(egui::Key::ArrowRight, Modifiers::NONE)],
        );
        frame(
            &mut app,
            &ctx,
            vec![key(egui::Key::ArrowLeft, Modifiers::NONE)],
        );
        assert_eq!(
            app.state.playhead,
            Time::from_frames(1, app.state.project.settings.frame_rate)
        );
        frame(&mut app, &ctx, vec![key(egui::Key::Home, Modifiers::NONE)]);
        assert_eq!(app.state.playhead, Time::ZERO);
        frame(&mut app, &ctx, vec![key(egui::Key::S, Modifiers::COMMAND)]);
        assert!(
            app.state.status.starts_with("Saved"),
            "{}",
            app.state.status
        );
        frame(&mut app, &ctx, vec![key(egui::Key::I, Modifiers::COMMAND)]);
        assert_eq!(app.state.status, "Imported 0 file(s)");
    }

    #[test]
    fn timeline_click_scroll_and_zoom() {
        let (ctx, mut app) = app_with_project();
        frame(&mut app, &ctx, vec![]);
        // Click inside the timeline, right of the track headers.
        frame(&mut app, &ctx, click(pos2(600.0, 760.0)));
        assert!(app.state.playhead > Time::ZERO);
        let zoom = app.state.zoom;
        frame(
            &mut app,
            &ctx,
            vec![Event::PointerMoved(pos2(600.0, 760.0)), Event::Zoom(2.0)],
        );
        assert!(app.state.zoom > zoom);
        frame(
            &mut app,
            &ctx,
            vec![
                Event::PointerMoved(pos2(600.0, 760.0)),
                Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: vec2(-400.0, 0.0),
                    modifiers: Modifiers::NONE,
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Wheel,
                },
            ],
        );
        for _ in 0..30 {
            frame(
                &mut app,
                &ctx,
                vec![Event::PointerMoved(pos2(600.0, 760.0))],
            );
        }
        assert!(app.state.scroll > 0.0);
    }

    #[test]
    fn actions_open_save_export() {
        let (ctx, mut app) = app_with_project();
        let dir = app
            .state
            .project_path
            .clone()
            .unwrap()
            .parent()
            .unwrap()
            .to_owned();
        let path = dir.join("saved.fwk");
        assert!(app.state.save_to(&path, SaveMode::Reference));
        frame(&mut app, &ctx, vec![]);
        for action in [
            UiAction::Save,
            UiAction::SaveAs,
            UiAction::Open,
            UiAction::Import,
            UiAction::ChooseExportPath,
        ] {
            app.perform(action, &ctx);
        }
        assert!(app.open_project(&path));
        assert!(!app.open_project(&dir.join("missing.json")));

        app.export_window = true;
        frame(&mut app, &ctx, vec![]);
        app.perform(UiAction::StartExport, &ctx);
        // A second start while running is ignored.
        app.perform(UiAction::StartExport, &ctx);
        frame(&mut app, &ctx, vec![]);
        while app.export_job().is_some_and(ExportJob::is_running) {
            frame(&mut app, &ctx, vec![]);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(matches!(
            app.export_job().unwrap().state(),
            ExportState::Done(_)
        ));
        frame(&mut app, &ctx, vec![]);

        // Failure is shown in the dialog.
        app.state.export_path = "/no/such/dir/x.mp4".into();
        app.perform(UiAction::StartExport, &ctx);
        while app.export_job().is_some_and(ExportJob::is_running) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        frame(&mut app, &ctx, vec![]);
        assert!(matches!(
            app.export_job().unwrap().state(),
            ExportState::Failed(_)
        ));

        // Cancel a running export.
        app.state.export_path = dir.join("cancel.mp4").to_string_lossy().into_owned();
        app.perform(UiAction::StartExport, &ctx);
        frame(&mut app, &ctx, vec![]);
        app.perform(UiAction::CancelExport, &ctx);
        while app.export_job().is_some_and(ExportJob::is_running) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        app.state.dirty = false;
        app.perform(UiAction::New, &ctx);
        assert!(app.state.project.media.is_empty());
        app.perform(UiAction::CancelExport, &ctx);
    }

    #[test]
    fn preview_keeps_aspect_ratio() {
        let hd = vec2(960.0, 540.0);
        let close = |a: Vec2, b: Vec2| (a - b).length() < 1e-3;
        assert!(close(
            fit_aspect(vec2(1000.0, 1000.0), hd),
            vec2(1000.0, 562.5)
        ));
        assert!(close(
            fit_aspect(vec2(400.0, 100.0), hd),
            vec2(100.0 * 16.0 / 9.0, 100.0)
        ));
        assert_eq!(fit_aspect(vec2(0.0, 100.0), hd), Vec2::ZERO);
        assert_eq!(fit_aspect(vec2(100.0, 100.0), Vec2::ZERO), Vec2::ZERO);
    }

    #[test]
    fn unsaved_changes_prompt_before_discarding() {
        let (ctx, mut app) = app_with_project();
        app.state.dirty = true;

        app.perform(UiAction::New, &ctx);
        assert_eq!(app.pending, Some(UiAction::New));
        assert!(!app.state.project.media.is_empty());
        frame(&mut app, &ctx, vec![]);
        app.perform(UiAction::CancelPending, &ctx);
        assert_eq!(app.pending, None);
        assert!(!app.state.project.media.is_empty());

        app.perform(UiAction::New, &ctx);
        app.perform(UiAction::DiscardChanges, &ctx);
        assert_eq!(app.pending, None);
        assert!(app.state.project.media.is_empty());

        // Save & continue writes the project before running the action.
        let (ctx, mut app) = app_with_project();
        let path = app.state.project_path.clone().unwrap();
        app.state.dirty = true;
        app.perform(UiAction::Open, &ctx);
        assert_eq!(app.pending, Some(UiAction::Open));
        app.perform(UiAction::SaveAndContinue, &ctx);
        assert!(path.exists());
        assert_eq!(app.pending, None);
        assert!(!app.state.dirty);

        // Closing the window with edits asks first; without edits it closes.
        app.state.dirty = true;
        let mut raw = input(vec![]);
        raw.viewports.insert(
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                events: vec![egui::ViewportEvent::Close],
                ..Default::default()
            },
        );
        let mut out = ctx.run_ui(raw, |ui| {
            app.ui(ui);
            app.handle_close_request(ui.ctx());
        });
        out.textures_delta.clear();
        assert_eq!(app.pending, Some(UiAction::Quit));
        app.perform(UiAction::DiscardChanges, &ctx);
        assert_eq!(app.pending, None);
        app.perform(UiAction::DiscardChanges, &ctx);
    }

    #[test]
    fn preview_errors_are_reported() {
        let (ctx, mut app) = app_with_project();
        app.state.project.media[0].path = "/nope.mp4".into();
        app.touch();
        for _ in 0..200 {
            frame(&mut app, &ctx, vec![]);
            if app.state.status.starts_with("Preview error") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.state.status.starts_with("Preview error"));
    }

    #[test]
    fn media_pool_add_to_timeline_button() {
        let (ctx, mut app) = app_with_project();
        app.state.selected_media = Some(app.state.project.media[0].id);
        let before = app.state.project.tracks[0].clips.len();
        frame(&mut app, &ctx, vec![]);
        assert!(app.state.append_selected_media().is_some());
        app.touch();
        frame(&mut app, &ctx, vec![]);
        assert_eq!(app.state.project.tracks[0].clips.len(), before + 1);
    }

    fn press(app: &mut FrameworkApp, ctx: &egui::Context, k: egui::Key) {
        frame(app, ctx, vec![key(k, Modifiers::NONE)]);
    }

    #[test]
    fn transport_shortcuts_play_and_shuttle() {
        let (ctx, mut app) = app_with_project();
        let end = app.state.project.duration();
        press(&mut app, &ctx, egui::Key::Space);
        assert!(app.transport.is_playing());
        std::thread::sleep(std::time::Duration::from_millis(30));
        frame(&mut app, &ctx, vec![]);
        frame(&mut app, &ctx, vec![]);
        assert!(app.state.playhead > Time::ZERO);
        press(&mut app, &ctx, egui::Key::K);
        assert!(!app.transport.is_playing());
        press(&mut app, &ctx, egui::Key::L);
        press(&mut app, &ctx, egui::Key::L);
        assert_eq!(app.transport.speed, 2.0);
        // Reverse from the start pauses at zero.
        press(&mut app, &ctx, egui::Key::Home);
        assert!(!app.transport.is_playing());
        press(&mut app, &ctx, egui::Key::J);
        std::thread::sleep(std::time::Duration::from_millis(5));
        frame(&mut app, &ctx, vec![]);
        frame(&mut app, &ctx, vec![]);
        assert_eq!(app.state.playhead, Time::ZERO);
        assert!(!app.transport.is_playing());
        // Playing at the end starts over; fast playback stops at the end.
        press(&mut app, &ctx, egui::Key::End);
        assert_eq!(app.state.playhead, end);
        press(&mut app, &ctx, egui::Key::Space);
        assert_eq!(app.state.playhead, Time::ZERO);
        app.transport.speed = 1000.0;
        std::thread::sleep(std::time::Duration::from_millis(30));
        frame(&mut app, &ctx, vec![]);
        assert_eq!(app.state.playhead, end);
        assert!(!app.transport.is_playing());
        // Up/Down jump between edit points.
        press(&mut app, &ctx, egui::Key::ArrowUp);
        assert!(app.state.playhead < end);
        let back = app.state.playhead;
        press(&mut app, &ctx, egui::Key::ArrowDown);
        assert!(app.state.playhead > back);
    }

    #[test]
    fn playback_follows_the_audio_clock() {
        let (ctx, mut app) = app_with_project();
        let engine = Arc::new(AudioEngine::new(8000));
        app.audio = Some(engine.clone());
        press(&mut app, &ctx, egui::Key::Space);
        let mut buf = vec![0.0; 800 * 2];
        for _ in 0..200 {
            engine.pull(&mut buf, 2);
            if engine.position() >= Some(Time::from_seconds(0.5)) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        frame(&mut app, &ctx, vec![]);
        assert_eq!(Some(app.state.playhead), engine.position());
        // Edits while playing restart audio with the new project.
        app.touch();
        frame(&mut app, &ctx, vec![]);
        assert_eq!(app.audio_revision, app.project_revision);
        // Clicking the timeline while playing restarts audio at the new position.
        app.state.set_playhead(Time::from_seconds(2.0));
        frame(&mut app, &ctx, vec![]);
        assert_eq!(engine.position(), Some(Time::from_seconds(2.0)));
        // Reaching the end through the audio clock stops playback.
        let end = app.state.project.duration();
        app.state.set_playhead(end - Time::from_seconds(0.01));
        frame(&mut app, &ctx, vec![]);
        for _ in 0..200 {
            engine.pull(&mut buf, 2);
            if engine.position() >= Some(end) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        frame(&mut app, &ctx, vec![]);
        assert_eq!(app.state.playhead, end);
        assert!(!app.transport.is_playing());
        assert_eq!(engine.position(), None);
        // Audio errors are reported.
        app.state.project.media[2].path = "/missing.wav".into();
        app.state.set_playhead(Time::ZERO);
        press(&mut app, &ctx, egui::Key::Space);
        for _ in 0..200 {
            frame(&mut app, &ctx, vec![]);
            if app.state.status.starts_with("Audio error") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(
            app.state.status.starts_with("Audio error"),
            "{}",
            app.state.status
        );
    }

    #[test]
    fn stalled_audio_output_falls_back_to_the_wall_clock() {
        let (ctx, mut app) = app_with_project();
        app.audio = Some(Arc::new(AudioEngine::new(8000)));
        press(&mut app, &ctx, egui::Key::Space);
        frame(&mut app, &ctx, vec![]);
        assert_eq!(app.state.playhead, Time::ZERO);
        std::thread::sleep(AUDIO_STALL + std::time::Duration::from_millis(50));
        frame(&mut app, &ctx, vec![]);
        assert!(app.audio.is_none());
        assert!(app.state.status.contains("without sound"));
        std::thread::sleep(std::time::Duration::from_millis(20));
        frame(&mut app, &ctx, vec![]);
        assert!(app.state.playhead > Time::ZERO);
    }

    #[test]
    fn transport_bar_and_preview_quality() {
        let (ctx, mut app) = app_with_project();
        frame(&mut app, &ctx, vec![]);
        app.set_preview_quality(PreviewQuality {
            scale: PreviewScale::Quarter,
            max_fps: Some(10),
        });
        assert_eq!(app.preview_quality.scale, PreviewScale::Quarter);
        app.set_preview_quality(PreviewQuality {
            scale: PreviewScale::Quarter,
            max_fps: None,
        });
        // Throttled preview requests while playing at 10 fps.
        app.preview_quality.max_fps = Some(10);
        app.transport_action(Transport::toggle);
        for _ in 0..5 {
            frame(&mut app, &ctx, vec![]);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        app.transport.speed = 4.0;
        frame(&mut app, &ctx, vec![]);
        // Transport buttons: click each one in the bar under the preview.
        for x in (300..700).step_by(24) {
            frame(&mut app, &ctx, click(pos2(x as f32, 500.0)));
        }
        app.perform(UiAction::New, &ctx);
        assert!(!app.transport.is_playing());
    }
}
