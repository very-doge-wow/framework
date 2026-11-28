use framework_core::{
    ClipId, ExportProfile, MediaAsset, MediaId, Project, ProjectError, SaveMode, Time, TrackId,
    TrackKind,
};
use std::path::{Path, PathBuf};

pub const MIN_ZOOM: f32 = 0.01;
pub const MAX_ZOOM: f32 = 2000.0;
/// Zoom factor of one zoom-in/zoom-out step (buttons and shortcuts).
pub const ZOOM_STEP: f32 = 1.25;
/// Number of undo steps kept.
pub const HISTORY: usize = 200;

/// Compact ruler label: `m:ss`, with tenths when the step is below a second.
pub fn ruler_label(t: Time, step: f64) -> String {
    let secs = t.as_seconds().max(0.0);
    let whole = secs.floor() as u64;
    let (h, m, s) = (whole / 3600, (whole / 60) % 60, whole % 60);
    let base = if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    };
    if step < 1.0 {
        format!(
            "{base}.{}",
            ((secs - secs.floor()) * 10.0).round() as u64 % 10
        )
    } else {
        base
    }
}

/// All editor state that is independent of the GUI toolkit.
pub struct EditorState {
    pub project: Project,
    pub project_path: Option<PathBuf>,
    pub playhead: Time,
    /// Timeline zoom in pixels per second.
    pub zoom: f32,
    /// Timeline horizontal scroll offset in seconds.
    pub scroll: f64,
    pub selected_media: Option<MediaId>,
    /// Selected clips (clips grouped with them are edited along).
    pub selected_clips: Vec<ClipId>,
    pub status: String,
    pub dirty: bool,
    pub profiles: Vec<ExportProfile>,
    /// Index into `profiles` of the chosen preset; `None` once settings were edited.
    pub export_profile: Option<usize>,
    /// Settings used for the next export (a copy of the preset, possibly edited).
    pub export_settings: ExportProfile,
    pub export_path: String,
    undo_stack: Vec<Project>,
    redo_stack: Vec<Project>,
    /// Project before an ongoing drag, committed as one undo step when it ends.
    gesture: Option<Project>,
}

impl Default for EditorState {
    fn default() -> Self {
        Self::new(Project::default())
    }
}

impl EditorState {
    pub fn new(project: Project) -> Self {
        Self {
            project,
            project_path: None,
            playhead: Time::ZERO,
            zoom: 80.0,
            scroll: 0.0,
            selected_media: None,
            selected_clips: Vec::new(),
            status: "Ready".into(),
            dirty: false,
            export_settings: ExportProfile::builtin()[0].clone(),
            profiles: ExportProfile::builtin(),
            export_profile: Some(0),
            export_path: "export.mp4".into(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            gesture: None,
        }
    }

    pub fn title(&self) -> String {
        let name = self
            .project_path
            .as_deref()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.project.name.clone());
        format!("{}{} — Framework", name, if self.dirty { " •" } else { "" })
    }

    /// Probes and adds media files to the bin. Returns the number imported.
    pub fn import(&mut self, paths: &[PathBuf]) -> usize {
        let mut imported = 0;
        let mut errors = Vec::new();
        for path in paths {
            match framework_media::probe(path) {
                Ok(info) => {
                    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
                    let id = self.project.add_media(MediaAsset::new(path, info));
                    self.selected_media = Some(id);
                    imported += 1;
                }
                Err(e) => errors.push(e.to_string()),
            }
        }
        if imported > 0 {
            self.dirty = true;
        }
        self.status = match errors.first() {
            Some(e) => format!("Imported {imported} file(s); error: {e}"),
            None => format!("Imported {imported} file(s)"),
        };
        imported
    }

    /// Appends the selected media to the end of the timeline; video with
    /// audio goes onto a video and an audio lane, grouped.
    pub fn append_selected_media(&mut self) -> Option<ClipId> {
        let id = self.selected_media?;
        let clips = self.edit("Added clip to timeline", |p| p.place_media(id))?;
        self.selected_clips = clips.clone();
        clips.first().copied()
    }

    /// Applies `f` as one undoable step. On error the project is left
    /// unchanged and the error is shown in the status bar.
    pub fn edit<R>(
        &mut self,
        label: &str,
        f: impl FnOnce(&mut Project) -> Result<R, ProjectError>,
    ) -> Option<R> {
        let before = self.project.clone();
        match f(&mut self.project) {
            Ok(r) => {
                if self.project != before {
                    self.push_undo(before);
                    self.status = label.into();
                    self.prune_selection();
                }
                Some(r)
            }
            Err(e) => {
                self.project = before;
                self.status = e.to_string();
                None
            }
        }
    }

    fn push_undo(&mut self, before: Project) {
        self.undo_stack.push(before);
        if self.undo_stack.len() > HISTORY {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
        self.dirty = true;
    }

    /// Starts a drag; the changes until [`Self::end_gesture`] form one undo step.
    pub fn begin_gesture(&mut self) {
        if self.gesture.is_none() {
            self.gesture = Some(self.project.clone());
        }
    }

    /// The project as it was when the ongoing drag started.
    pub fn gesture_start(&self) -> Option<&Project> {
        self.gesture.as_ref()
    }

    pub fn end_gesture(&mut self, label: &str) {
        if let Some(before) = self.gesture.take()
            && before != self.project
        {
            self.push_undo(before);
            self.status = label.into();
        }
    }

    // Undo and redo restore the timeline (tracks) only: the media bin and
    // where media is stored (e.g. embedded by Collect and save) stay current.

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(mut previous) = self.undo_stack.pop() else {
            return false;
        };
        std::mem::swap(&mut self.project.tracks, &mut previous.tracks);
        self.redo_stack.push(previous);
        self.after_history_step("Undo");
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(mut next) = self.redo_stack.pop() else {
            return false;
        };
        std::mem::swap(&mut self.project.tracks, &mut next.tracks);
        self.undo_stack.push(next);
        self.after_history_step("Redo");
        true
    }

    /// Drops selected clips that no longer exist.
    fn prune_selection(&mut self) {
        let project = &self.project;
        self.selected_clips.retain(|c| project.clip(*c).is_some());
    }

    fn after_history_step(&mut self, label: &str) {
        self.prune_selection();
        self.dirty = true;
        self.status = label.into();
    }

    /// Selects a clip; `additive` toggles it in the current selection.
    pub fn select_clip(&mut self, id: ClipId, additive: bool) {
        if !additive {
            self.selected_clips = vec![id];
        } else if let Some(i) = self.selected_clips.iter().position(|c| *c == id) {
            self.selected_clips.remove(i);
        } else {
            self.selected_clips.push(id);
        }
    }

    pub fn select_all(&mut self) {
        self.selected_clips = self
            .project
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .map(|c| c.id)
            .collect();
    }

    /// Selected clips (with their groups) under the playhead; without a
    /// selection, all clips under the playhead.
    fn clips_at_playhead(&self) -> Vec<ClipId> {
        let t = self.playhead;
        let under = |id: &ClipId| self.project.clip(*id).is_some_and(|c| c.contains(t));
        let selected: Vec<ClipId> = self
            .project
            .with_groups(&self.selected_clips)
            .into_iter()
            .filter(under)
            .collect();
        if !self.selected_clips.is_empty() {
            return selected;
        }
        let all: Vec<ClipId> = self
            .project
            .tracks
            .iter()
            .flat_map(|tr| &tr.clips)
            .map(|c| c.id)
            .filter(under)
            .collect();
        self.project
            .with_groups(&all)
            .into_iter()
            .filter(under)
            .collect()
    }

    /// Cuts the clips under the playhead (see [`Self::clips_at_playhead`]).
    pub fn split_at_playhead(&mut self) -> bool {
        let ids = self.clips_at_playhead();
        let t = self.playhead;
        self.edit("Split", |p| {
            let created = p.split_clips(&ids, t);
            if created.is_empty() {
                Err(ProjectError::InvalidEdit("no clip under the playhead"))
            } else {
                Ok(())
            }
        })
        .is_some()
    }

    /// Trims the start (`start`) or end of the clips under the playhead to it.
    pub fn trim_to_playhead(&mut self, start: bool) -> bool {
        let ids = self.clips_at_playhead();
        let (t, min) = (self.playhead, self.min_clip_length());
        let label = if start {
            "Trimmed start"
        } else {
            "Trimmed end"
        };
        self.edit(label, |p| {
            if ids.is_empty() {
                return Err(ProjectError::InvalidEdit("no clip under the playhead"));
            }
            for id in &ids {
                if start {
                    p.trim_start(*id, t, min)?;
                } else {
                    p.trim_end(*id, t, min)?;
                }
            }
            Ok(())
        })
        .is_some()
    }

    /// Shortest clip a trim can leave: one frame.
    pub fn min_clip_length(&self) -> Time {
        self.project.settings.frame_rate.frame_duration()
    }

    /// Deletes the selected clips; `ripple` closes the gaps.
    pub fn delete_selection(&mut self, ripple: bool) -> bool {
        let ids = std::mem::take(&mut self.selected_clips);
        let label = if ripple { "Ripple delete" } else { "Deleted" };
        self.edit(label, |p| match p.remove_clips(&ids, ripple)? {
            0 => Err(ProjectError::InvalidEdit("nothing selected")),
            _ => Ok(()),
        })
        .is_some()
    }

    pub fn group_selection(&mut self) -> bool {
        let ids = self.selected_clips.clone();
        self.edit("Grouped", |p| p.group_clips(&ids)).is_some()
    }

    pub fn ungroup_selection(&mut self) -> bool {
        let ids = self.selected_clips.clone();
        self.edit("Ungrouped", |p| match p.ungroup_clips(&ids) {
            0 => Err(ProjectError::InvalidEdit("the selection is not grouped")),
            _ => Ok(()),
        })
        .is_some()
    }

    /// Adds an empty lane on top of the lanes of its kind.
    pub fn add_lane(&mut self, kind: TrackKind) -> Option<TrackId> {
        self.edit("Added lane", |p| {
            let index = match kind {
                TrackKind::Video => p.lanes_of(TrackKind::Video).last().map_or(0, |i| i + 1),
                TrackKind::Audio => p.tracks.len(),
            };
            Ok(p.insert_track(kind, index))
        })
    }

    pub fn remove_lane(&mut self, id: TrackId) -> bool {
        self.edit("Deleted lane", |p| p.remove_track(id).map(|_| ()))
            .is_some()
    }

    /// Moves a lane one step up (`up`) or down on screen.
    pub fn move_lane(&mut self, id: TrackId, up: bool) -> bool {
        self.edit("Moved lane", |p| {
            // Video lanes are drawn top-down from the highest track index.
            let video = p.track(id).is_some_and(|t| t.kind == TrackKind::Video);
            if p.move_track(id, up == video) {
                Ok(())
            } else {
                Err(ProjectError::InvalidEdit("the lane cannot move further"))
            }
        })
        .is_some()
    }

    pub fn toggle_mute(&mut self, id: TrackId) {
        self.edit("Toggled mute", |p| {
            let t = p
                .tracks
                .iter_mut()
                .find(|t| t.id == id)
                .ok_or(ProjectError::UnknownTrack(id))?;
            t.muted = !t.muted;
            Ok(())
        });
    }

    pub fn toggle_solo(&mut self, id: TrackId) {
        self.edit("Toggled solo", |p| {
            let t = p
                .tracks
                .iter_mut()
                .find(|t| t.id == id)
                .ok_or(ProjectError::UnknownTrack(id))?;
            t.solo = !t.solo;
            Ok(())
        });
    }

    pub fn set_playhead(&mut self, t: Time) {
        self.playhead = t.max(Time::ZERO);
    }

    pub fn zoom_by(&mut self, factor: f32) {
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
    }

    /// Zooms while keeping the time under timeline offset `x` in place.
    pub fn zoom_around(&mut self, factor: f32, x: f32) {
        let anchor = f64::from(x) / f64::from(self.zoom) + self.scroll;
        self.zoom_by(factor);
        self.scroll = (anchor - f64::from(x) / f64::from(self.zoom)).max(0.0);
    }

    /// Zooms around the playhead if it is visible in `width`, else around the left edge.
    pub fn zoom_step(&mut self, zoom_in: bool, width: f32) {
        let x = self.time_to_x(self.playhead);
        let x = if (0.0..=width).contains(&x) { x } else { 0.0 };
        self.zoom_around(if zoom_in { ZOOM_STEP } else { 1.0 / ZOOM_STEP }, x);
    }

    /// Fits the whole project into `width` pixels.
    pub fn zoom_to_fit(&mut self, width: f32) {
        let secs = self.project.duration().as_seconds();
        if secs > 0.0 && width > 0.0 {
            self.zoom = (width / secs as f32).clamp(MIN_ZOOM, MAX_ZOOM);
            self.scroll = 0.0;
        }
    }

    /// Loads preset `index` into the export settings.
    pub fn choose_profile(&mut self, index: usize) {
        if let Some(p) = self.profiles.get(index) {
            self.export_settings = p.clone();
            self.export_profile = Some(index);
        }
    }

    /// Switches to "Custom" once the export settings no longer match the chosen preset.
    pub fn sync_custom_profile(&mut self) {
        let preset = self.export_profile.and_then(|i| self.profiles.get(i));
        if preset.is_some_and(|p| p.video != self.export_settings.video) {
            self.export_profile = None;
            self.export_settings.id = "custom".into();
            self.export_settings.name = "Custom".into();
        }
    }

    /// X offset (relative to the timeline origin) of a timeline time.
    pub fn time_to_x(&self, t: Time) -> f32 {
        ((t.as_seconds() - self.scroll) * f64::from(self.zoom)) as f32
    }

    pub fn x_to_time(&self, x: f32) -> Time {
        Time::from_seconds(f64::from(x) / f64::from(self.zoom) + self.scroll).max(Time::ZERO)
    }

    /// Spacing in seconds between ruler labels that keeps them at least `min_px` apart.
    pub fn ruler_step(&self, min_px: f32) -> f64 {
        const STEPS: [f64; 16] = [
            0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 300.0, 600.0, 1800.0, 3600.0,
            7200.0, 14400.0,
        ];
        STEPS
            .into_iter()
            .find(|s| *s * f64::from(self.zoom) >= f64::from(min_px))
            .unwrap_or(28800.0)
    }

    /// Saves to `path`; [`SaveMode::Collect`] embeds all media into the file.
    pub fn save_to(&mut self, path: &Path, mode: SaveMode) -> bool {
        if mode == SaveMode::Collect
            && let Err(e) = framework_media::check_collectable(&self.project)
        {
            self.status = format!("Collect failed: {e}");
            return false;
        }
        match self.project.save_as(path, mode) {
            Ok(()) => {
                self.project_path = Some(path.to_owned());
                self.dirty = false;
                self.status = match mode {
                    SaveMode::Reference => format!("Saved {}", path.display()),
                    SaveMode::Collect => {
                        format!("Saved {} with embedded media", path.display())
                    }
                };
                true
            }
            Err(e) => {
                self.status = format!("Save failed: {e}");
                false
            }
        }
    }

    pub fn open(&mut self, path: &Path) -> bool {
        match Project::load(path) {
            Ok(project) => {
                *self = Self::new(project);
                let ext_ok = path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case(framework_core::PROJECT_EXTENSION));
                if ext_ok && Project::is_container(path) {
                    self.project_path = Some(path.to_owned());
                    self.status = format!("Opened {}", path.display());
                } else {
                    // Never overwrite a legacy JSON project with a `.fwk` container.
                    self.status = format!(
                        "Opened legacy project {}; saving creates a new .fwk file",
                        path.display()
                    );
                }
                true
            }
            Err(e) => {
                self.status = format!("Open failed: {e}");
                false
            }
        }
    }

    pub fn selected_profile(&self) -> &ExportProfile {
        &self.export_settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use framework_core::MediaKind;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("framework-app-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn import_append_save_open() {
        let video = tmp("v.mp4");
        framework_media::synthetic::write_test_video(&video, (64, 36), 10, 10, true).unwrap();
        let mut s = EditorState::default();
        assert_eq!(s.import(&[video.clone(), tmp("missing.mp4")]), 1);
        assert!(s.status.contains("error"));
        assert!(s.dirty);
        assert!(s.title().ends_with("• — Framework"));
        let clip = s.append_selected_media().unwrap();
        assert_eq!(s.selected_clips[0], clip);
        // Video with audio lands on a video and an audio lane, grouped.
        assert_eq!(s.selected_clips.len(), 2);
        assert!(s.project.clip(clip).unwrap().group.is_some());
        assert_eq!(s.project.media[0].kind(), MediaKind::Video);

        let path = tmp("p.fwk");
        assert!(s.save_to(&path, SaveMode::Reference));
        assert!(!s.dirty);
        let mut other = EditorState::default();
        assert!(other.open(&path));
        assert_eq!(other.project, s.project);
        assert!(other.title().starts_with("p.fwk"));
        let upper = tmp("UPPER.FWK");
        std::fs::copy(&path, &upper).unwrap();
        assert!(other.open(&upper));
        assert_eq!(other.project_path.as_deref(), Some(upper.as_path()));
        let disguised = tmp("disguised.fwk");
        std::fs::write(&disguised, other.project.to_json().unwrap()).unwrap();
        assert!(other.open(&disguised));
        assert_eq!(other.project_path, None);
        let legacy = tmp("legacy.json");
        std::fs::write(&legacy, s.project.to_json().unwrap()).unwrap();
        assert!(other.open(&legacy));
        assert_eq!(other.project_path, None);
        assert!(other.status.contains("legacy"));
        assert!(!other.open(&tmp("nope.json")));
        assert!(other.status.starts_with("Open failed"));
        assert!(!other.save_to(&tmp("no/such/dir/x.fwk"), SaveMode::Reference));

        // Collecting refuses files that are not the recorded kind of media.
        let mut bad = EditorState::new(s.project.clone());
        bad.project.media[0].info.kind = MediaKind::Audio;
        assert!(!bad.save_to(&tmp("bad.fwk"), SaveMode::Collect));
        assert!(bad.status.contains("not the kind"), "{}", bad.status);
        let secret = tmp("secret.txt");
        std::fs::write(&secret, "private key").unwrap();
        bad.project.media[0].path = secret;
        assert!(!bad.save_to(&tmp("bad.fwk"), SaveMode::Collect));
        assert!(bad.status.starts_with("Collect failed"));
        assert!(!tmp("bad.fwk").exists());
        bad.project.media[0].path = PathBuf::from("http://127.0.0.1:9/clip.mp4");
        assert!(!bad.save_to(&tmp("bad.fwk"), SaveMode::Collect));
        assert!(bad.status.contains("not the kind"), "{}", bad.status);
    }

    #[test]
    fn import_without_errors_and_append_failures() {
        let mut s = EditorState::default();
        assert_eq!(s.import(&[]), 0);
        assert_eq!(s.status, "Imported 0 file(s)");
        assert!(s.append_selected_media().is_none());
        // A selected id that is not in the project.
        s.selected_media = Some(MediaId::new());
        assert!(s.append_selected_media().is_none());
    }

    #[test]
    fn ruler_labels() {
        assert_eq!(ruler_label(Time::from_seconds(65.0), 5.0), "1:05");
        assert_eq!(ruler_label(Time::from_seconds(3725.0), 60.0), "1:02:05");
        assert_eq!(ruler_label(Time::from_seconds(1.5), 0.5), "0:01.5");
    }

    #[test]
    fn coordinates_zoom_and_ruler() {
        let mut s = EditorState {
            zoom: 100.0,
            scroll: 1.0,
            ..Default::default()
        };
        assert_eq!(s.time_to_x(Time::from_seconds(2.0)), 100.0);
        assert_eq!(s.x_to_time(100.0), Time::from_seconds(2.0));
        assert_eq!(s.x_to_time(-500.0), Time::ZERO);
        s.zoom_by(1000.0);
        assert_eq!(s.zoom, MAX_ZOOM);
        s.zoom_by(0.0);
        assert_eq!(s.zoom, MIN_ZOOM);
        s.zoom = 100.0;
        assert_eq!(s.ruler_step(80.0), 1.0);
        s.zoom = 0.001;
        assert_eq!(s.ruler_step(80.0), 28800.0);
        s.set_playhead(Time::from_seconds(-1.0));
        assert_eq!(s.playhead, Time::ZERO);

        // Zooming around x = 100 keeps the time under it in place.
        s.zoom = 100.0;
        s.scroll = 1.0;
        s.zoom_around(2.0, 100.0);
        assert_eq!(
            (s.zoom, s.x_to_time(100.0)),
            (200.0, Time::from_seconds(2.0))
        );
        s.zoom_around(0.01, 100.0);
        assert_eq!(s.scroll, 0.0);
        // Steps zoom around the visible playhead, else the left edge.
        s.zoom = 100.0;
        s.set_playhead(Time::from_seconds(3.0));
        s.zoom_step(true, 1000.0);
        assert_eq!(s.time_to_x(s.playhead), 300.0);
        // Playhead outside a 10 px wide view: the left edge (0.6 s) stays put.
        s.zoom_step(false, 10.0);
        assert_eq!(s.zoom, 100.0);
        assert!((s.scroll - 0.6).abs() < 1e-9);
        // Fitting needs a non-empty project.
        s.zoom_to_fit(1000.0);
        assert_eq!(s.zoom, 100.0);
    }

    #[test]
    fn export_presets_and_custom_settings() {
        let mut s = EditorState::default();
        s.choose_profile(1);
        assert_eq!(s.export_profile, Some(1));
        assert_eq!(s.selected_profile(), &s.profiles[1]);
        s.choose_profile(999);
        assert_eq!(s.export_profile, Some(1));
        s.sync_custom_profile();
        assert_eq!(s.export_profile, Some(1));
        s.export_settings.video.bitrate_kbps += 1;
        s.sync_custom_profile();
        assert_eq!(s.export_profile, None);
        assert_eq!(s.selected_profile().name, "Custom");
        s.sync_custom_profile();
        assert_eq!(s.selected_profile().id, "custom");
    }

    fn editable() -> (EditorState, ClipId, ClipId) {
        use framework_core::{Clip, MediaInfo};
        let mut p = Project::new("e");
        let m = p.add_media(MediaAsset::new(
            "still.png",
            MediaInfo {
                kind: MediaKind::Image,
                duration: None,
                video: None,
                audio: None,
            },
        ));
        let v1 = p.tracks[0].id;
        let s = Time::from_seconds;
        let a = p
            .add_clip(v1, Clip::new(m, s(0.0), s(0.0), s(2.0)))
            .unwrap();
        let b = p
            .add_clip(v1, Clip::new(m, s(3.0), s(0.0), s(2.0)))
            .unwrap();
        (EditorState::new(p), a, b)
    }

    #[test]
    fn edits_undo_and_redo() {
        let (mut st, a, b) = editable();
        let s = Time::from_seconds;
        assert!(!st.can_undo() && !st.can_redo());
        assert!(!st.undo() && !st.redo());
        // Split under the playhead without a selection.
        st.set_playhead(s(1.0));
        assert!(st.split_at_playhead());
        assert_eq!(st.project.tracks[0].clips.len(), 3);
        assert!(st.dirty && st.can_undo());
        assert!(st.undo());
        assert_eq!(st.project.tracks[0].clips.len(), 2);
        assert!(st.redo());
        assert_eq!(st.project.tracks[0].clips.len(), 3);
        st.set_playhead(s(2.5));
        assert!(!st.split_at_playhead());
        assert!(st.status.contains("no clip"));
        // Trims at the playhead apply to the selected clip.
        st.select_clip(b, false);
        st.set_playhead(s(4.0));
        assert!(st.trim_to_playhead(true));
        assert_eq!(st.project.clip(b).unwrap().start, s(4.0));
        st.set_playhead(s(4.5));
        assert!(st.trim_to_playhead(false));
        assert_eq!(st.project.clip(b).unwrap().end(), s(4.5));
        st.set_playhead(s(10.0));
        assert!(!st.trim_to_playhead(false));
        // Selection, grouping, deleting.
        st.select_clip(a, true);
        assert_eq!(st.selected_clips, vec![b, a]);
        st.select_clip(b, true);
        assert_eq!(st.selected_clips, vec![a]);
        assert!(!st.group_selection());
        assert!(!st.ungroup_selection());
        st.select_all();
        assert_eq!(st.selected_clips.len(), 3);
        assert!(st.group_selection());
        assert!(st.ungroup_selection());
        st.selected_clips = vec![a];
        assert!(st.delete_selection(true));
        assert!(st.selected_clips.is_empty());
        assert!(!st.delete_selection(false));
        assert!(st.undo());
        assert!(st.project.clip(a).is_some());
        // Undo drops selected clips that no longer exist.
        st.selected_clips = vec![a];
        st.undo();
        st.undo();
        st.redo();
        st.redo();
        st.redo();
        assert!(
            st.selected_clips
                .iter()
                .all(|c| st.project.clip(*c).is_some())
        );
    }

    #[test]
    fn playhead_edits_use_the_selection() {
        let (mut st, a, b) = editable();
        // At the cut after the selected clip, nothing else is trimmed.
        st.selected_clips = vec![a];
        st.set_playhead(Time::from_seconds(3.0));
        assert!(!st.trim_to_playhead(false));
        assert!(!st.split_at_playhead());
        assert_eq!(
            st.project.clip(b).unwrap().duration,
            Time::from_seconds(2.0)
        );
        // Deleting the lane of the selection deselects, so edits apply again.
        let v1 = st.project.tracks[0].id;
        let v2 = st.add_lane(TrackKind::Video).unwrap();
        st.project.move_clips(&[b], Time::ZERO, 1).unwrap();
        assert!(st.remove_lane(v1));
        assert!(st.selected_clips.is_empty());
        st.set_playhead(Time::from_seconds(4.0));
        assert!(st.split_at_playhead());
        assert_eq!(st.project.track(v2).unwrap().clips.len(), 2);
    }

    #[test]
    fn undo_keeps_the_media_bin() {
        let (mut st, a, _) = editable();
        st.selected_clips = vec![a];
        assert!(st.delete_selection(false));
        let mut asset = st.project.media[0].clone();
        asset.id = MediaId::new();
        st.project.add_media(asset);
        st.project.media[0].path = "collected.png".into();
        assert!(st.undo());
        assert!(st.project.clip(a).is_some());
        assert_eq!(st.project.media.len(), 2);
        assert!(st.redo());
        assert_eq!(st.project.media[0].path, PathBuf::from("collected.png"));
    }

    #[test]
    fn gestures_and_lanes() {
        let (mut st, a, _) = editable();
        let s = Time::from_seconds;
        st.begin_gesture();
        st.begin_gesture();
        assert!(st.gesture_start().is_some());
        st.project.move_clips(&[a], s(0.5), 0).unwrap();
        st.end_gesture("Moved");
        assert_eq!(st.status, "Moved");
        assert!(st.undo());
        assert_eq!(st.project.clip(a).unwrap().start, s(0.0));
        // A gesture without changes adds no undo step.
        st.begin_gesture();
        st.end_gesture("Nothing");
        assert!(!st.can_undo());

        let v2 = st.add_lane(TrackKind::Video).unwrap();
        let a2 = st.add_lane(TrackKind::Audio).unwrap();
        assert_eq!(st.project.tracks[1].id, v2);
        assert_eq!(st.project.tracks.last().unwrap().id, a2);
        // V2 is drawn above V1: moving it up is impossible, down works.
        assert!(!st.move_lane(v2, true));
        assert!(st.move_lane(v2, false));
        assert_eq!(st.project.tracks[0].id, v2);
        assert!(st.move_lane(a2, true));
        st.toggle_mute(a2);
        st.toggle_solo(a2);
        let track = st.project.track(a2).unwrap();
        assert!(track.muted && track.solo);
        st.toggle_mute(TrackId::new());
        st.toggle_solo(TrackId::new());
        assert!(st.remove_lane(a2));
        assert!(!st.remove_lane(a2));
        for _ in 0..HISTORY + 5 {
            st.toggle_mute(v2);
        }
        assert_eq!(st.undo_stack.len(), HISTORY);
    }
}
