use framework_core::{ClipId, ExportProfile, MediaAsset, MediaId, Project, SaveMode, Time};
use std::path::{Path, PathBuf};

pub const MIN_ZOOM: f32 = 2.0;
pub const MAX_ZOOM: f32 = 2000.0;
/// Zoom factor of one zoom-in/zoom-out step (buttons and shortcuts).
pub const ZOOM_STEP: f32 = 1.25;

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
    pub selected_clip: Option<ClipId>,
    pub status: String,
    pub dirty: bool,
    pub profiles: Vec<ExportProfile>,
    /// Index into `profiles` of the chosen preset; `None` once settings were edited.
    pub export_profile: Option<usize>,
    /// Settings used for the next export (a copy of the preset, possibly edited).
    pub export_settings: ExportProfile,
    pub export_path: String,
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
            selected_clip: None,
            status: "Ready".into(),
            dirty: false,
            export_settings: ExportProfile::builtin()[0].clone(),
            profiles: ExportProfile::builtin(),
            export_profile: Some(0),
            export_path: "export.mp4".into(),
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

    /// Appends the selected media to the end of its default track.
    pub fn append_selected_media(&mut self) -> Option<ClipId> {
        let id = self.selected_media?;
        let kind = self.project.media(id)?.kind();
        let track = self.project.default_track_for(kind);
        match self.project.append_media(track, id) {
            Ok(clip) => {
                self.selected_clip = Some(clip);
                self.dirty = true;
                self.status = "Added clip to timeline".into();
                Some(clip)
            }
            Err(e) => {
                self.status = e.to_string();
                None
            }
        }
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
        const STEPS: [f64; 12] = [
            0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 300.0, 600.0,
        ];
        STEPS
            .into_iter()
            .find(|s| *s * f64::from(self.zoom) >= f64::from(min_px))
            .unwrap_or(3600.0)
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
                if path
                    .extension()
                    .is_some_and(|e| e == framework_core::PROJECT_EXTENSION)
                {
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
        assert_eq!(s.selected_clip, Some(clip));
        assert_eq!(s.project.media[0].kind(), MediaKind::Video);

        let path = tmp("p.fwk");
        assert!(s.save_to(&path, SaveMode::Reference));
        assert!(!s.dirty);
        let mut other = EditorState::default();
        assert!(other.open(&path));
        assert_eq!(other.project, s.project);
        assert!(other.title().starts_with("p.fwk"));
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
        s.zoom = 0.01;
        assert_eq!(s.ruler_step(80.0), 3600.0);
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
}
