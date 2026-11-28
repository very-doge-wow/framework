use crate::audio_out::{AudioEngine, AudioOutput};
use crate::playback::{self, PREVIEW_FPS, PreviewQuality, PreviewScale, Transport};
use crate::state::{EditorState, MAX_ZOOM, MIN_ZOOM};
use crate::theme;
use crate::worker::{ExportJob, ExportState, PreviewWorker};
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, Vec2};
use framework_core::{MediaKind, PROJECT_EXTENSION, SaveMode, Time, TrackKind};
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
                self.timeline_toolbar(ui);
                let (rect, response) =
                    ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
                self.timeline_input(ui, rect, &response);
                self.paint_timeline(ui, rect);
            });
    }

    fn timeline_toolbar(&mut self, ui: &mut egui::Ui) {
        let cmd = if cfg!(target_os = "macos") {
            "Cmd"
        } else {
            "Ctrl"
        };
        ui.horizontal(|ui| {
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

    fn paint_timeline(&self, ui: &egui::Ui, rect: Rect) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme::BG);
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

        // Lanes, top lane = highest video track, like most editors.
        let mut lanes: Vec<_> = self.state.project.tracks.iter().collect();
        lanes.sort_by_key(|t| t.kind == TrackKind::Audio);
        let video_count = lanes.iter().filter(|t| t.kind == TrackKind::Video).count();
        lanes[..video_count].reverse();
        for (row, track) in lanes.iter().enumerate() {
            let top = rect.top() + RULER_HEIGHT + row as f32 * TRACK_HEIGHT;
            let lane = Rect::from_min_size(
                Pos2::new(rect.left(), top),
                Vec2::new(rect.width(), TRACK_HEIGHT - 2.0),
            );
            painter.rect_filled(lane, 4.0, theme::LANE);
            painter.text(
                Pos2::new(rect.left() + 8.0, lane.center().y),
                Align2::LEFT_CENTER,
                &track.name,
                FontId::proportional(13.0),
                Color32::WHITE,
            );
            for clip in &track.clips {
                let Some(asset) = self.state.project.media(clip.media) else {
                    continue;
                };
                let x0 = origin + self.state.time_to_x(clip.start);
                let x1 = origin + self.state.time_to_x(clip.end());
                if x1 < origin || x0 > rect.right() {
                    continue;
                }
                let r = Rect::from_min_max(
                    Pos2::new(x0.max(origin), top + 3.0),
                    Pos2::new(x1, top + TRACK_HEIGHT - 5.0),
                );
                let fill = match asset.kind() {
                    MediaKind::Video => theme::VIDEO_CLIP,
                    MediaKind::Image => theme::IMAGE_CLIP,
                    MediaKind::Audio => theme::AUDIO_CLIP,
                };
                painter.rect_filled(r, 4.0, fill);
                if self.state.selected_clip == Some(clip.id) {
                    painter.rect_stroke(
                        r,
                        4.0,
                        Stroke::new(2.0, Color32::WHITE),
                        StrokeKind::Inside,
                    );
                }
                painter.text(
                    r.left_center() + Vec2::new(6.0, 0.0),
                    Align2::LEFT_CENTER,
                    asset.display_name(),
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
        app.state.selected_clip = Some(app.state.project.tracks[0].clips[0].id);
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
