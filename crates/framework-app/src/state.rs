use framework_core::{ClipId, ExportProfile, MediaAsset, MediaId, Project, Time};
use std::path::{Path, PathBuf};

pub const MIN_ZOOM: f32 = 2.0;
pub const MAX_ZOOM: f32 = 2000.0;

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
    pub export_profile: usize,
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
            profiles: ExportProfile::builtin(),
            export_profile: 0,
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
                    let id = self.project.add_media(MediaAsset::new(path.clone(), info));
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

    pub fn save_to(&mut self, path: &Path) -> bool {
        match self.project.save(path) {
            Ok(()) => {
                self.project_path = Some(path.to_owned());
                self.dirty = false;
                self.status = format!("Saved {}", path.display());
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
                self.project_path = Some(path.to_owned());
                self.status = format!("Opened {}", path.display());
                true
            }
            Err(e) => {
                self.status = format!("Open failed: {e}");
                false
            }
        }
    }

    pub fn selected_profile(&self) -> &ExportProfile {
        &self.profiles[self.export_profile.min(self.profiles.len() - 1)]
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

        let path = tmp("p.framework.json");
        assert!(s.save_to(&path));
        assert!(!s.dirty);
        let mut other = EditorState::default();
        assert!(other.open(&path));
        assert_eq!(other.project, s.project);
        assert!(other.title().starts_with("p.framework.json"));
        assert!(!other.open(&tmp("nope.json")));
        assert!(other.status.starts_with("Open failed"));
        assert!(!other.save_to(&tmp("no/such/dir/x.json")));
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
        let mut s = EditorState::default();
        s.zoom = 100.0;
        s.scroll = 1.0;
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
        s.export_profile = 999;
        assert_eq!(s.selected_profile().id, s.profiles.last().unwrap().id);
    }
}
