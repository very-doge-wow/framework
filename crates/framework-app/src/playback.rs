//! Transport (play/pause/J-K-L shuttle) and preview-quality logic, independent of egui.
use framework_core::{Project, ProjectSettings, Rational, Time};

/// Fastest shuttle speed reached by repeatedly pressing J or L.
pub const MAX_SHUTTLE: f64 = 8.0;

/// Preview resolution relative to the project resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewScale {
    Full,
    Half,
    Quarter,
}

impl PreviewScale {
    pub const ALL: [PreviewScale; 3] = [Self::Full, Self::Half, Self::Quarter];

    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "Full",
            Self::Half => "1/2",
            Self::Quarter => "1/4",
        }
    }

    fn divisor(self) -> u32 {
        match self {
            Self::Full => 1,
            Self::Half => 2,
            Self::Quarter => 4,
        }
    }
}

/// Frame-rate limits offered for the preview; `None` follows the project rate.
pub const PREVIEW_FPS: [Option<u32>; 5] = [None, Some(30), Some(24), Some(15), Some(10)];

pub fn fps_label(fps: Option<u32>) -> String {
    fps.map_or_else(|| "Project fps".into(), |f| format!("{f} fps"))
}

/// User-selected preview quality; lower settings keep playback smooth on slow machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewQuality {
    pub scale: PreviewScale,
    pub max_fps: Option<u32>,
}

impl Default for PreviewQuality {
    fn default() -> Self {
        Self {
            scale: PreviewScale::Half,
            max_fps: None,
        }
    }
}

impl PreviewQuality {
    /// Preview render size in pixels (even, at least 2×2).
    pub fn size(&self, settings: &ProjectSettings) -> (u32, u32) {
        let d = self.scale.divisor();
        (
            (settings.width / d).max(2) & !1,
            (settings.height / d).max(2) & !1,
        )
    }

    /// Minimum timeline distance between two preview frames during playback.
    pub fn frame_interval(&self, project_rate: Rational) -> Time {
        let project = project_rate.frame_duration();
        match self.max_fps {
            Some(fps) => project.max(Time::from_seconds(1.0 / f64::from(fps.max(1)))),
            None => project,
        }
    }
}

/// Playback speed: 0 is paused, negative plays backwards.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Transport {
    pub speed: f64,
}

impl Transport {
    pub fn is_playing(&self) -> bool {
        self.speed != 0.0
    }

    /// Space: play at normal speed, or pause.
    pub fn toggle(&mut self) {
        self.speed = if self.is_playing() { 0.0 } else { 1.0 };
    }

    /// K.
    pub fn pause(&mut self) {
        self.speed = 0.0;
    }

    /// L: play forwards, doubling the speed on every further press.
    pub fn forward(&mut self) {
        self.speed = if self.speed <= 0.0 {
            1.0
        } else {
            (self.speed * 2.0).min(MAX_SHUTTLE)
        };
    }

    /// J: play backwards, doubling the speed on every further press.
    pub fn reverse(&mut self) {
        self.speed = if self.speed >= 0.0 {
            -1.0
        } else {
            (self.speed * 2.0).max(-MAX_SHUTTLE)
        };
    }

    /// Moves `playhead` by `dt` seconds of wall time; pauses at either end of the project.
    pub fn advance(&mut self, playhead: Time, dt: f64, end: Time) -> Time {
        let delta = (dt * self.speed * Time::FLICKS_PER_SECOND as f64) as i64;
        let t = Time::from_flicks(playhead.flicks() + delta);
        if self.speed > 0.0 && t >= end {
            self.pause();
            end
        } else if self.speed < 0.0 && t <= Time::ZERO {
            self.pause();
            Time::ZERO
        } else {
            t
        }
    }
}

/// Project start, end and every clip boundary, sorted and unique.
pub fn edit_points(project: &Project) -> Vec<Time> {
    let mut points = vec![Time::ZERO];
    for clip in project.tracks.iter().flat_map(|t| &t.clips) {
        points.extend([clip.start, clip.end()]);
    }
    points.sort();
    points.dedup();
    points
}

/// The nearest edit point after `t`.
pub fn next_edit(project: &Project, t: Time) -> Option<Time> {
    edit_points(project).into_iter().find(|&p| p > t)
}

/// The nearest edit point before `t`.
pub fn previous_edit(project: &Project, t: Time) -> Option<Time> {
    edit_points(project).into_iter().rev().find(|&p| p < t)
}

/// `HH:MM:SS:FF` timecode at `rate`.
pub fn timecode(t: Time, rate: Rational) -> String {
    let frames = t.max(Time::ZERO).to_frames_floor(rate);
    let fps = rate.as_f64().round().max(1.0) as i64;
    let secs = frames / fps;
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60,
        frames % fps
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use framework_core::{Clip, MediaAsset, MediaInfo, MediaKind};

    #[test]
    fn shuttle_speeds() {
        let mut t = Transport::default();
        assert!(!t.is_playing());
        t.toggle();
        assert_eq!(t.speed, 1.0);
        t.toggle();
        assert_eq!(t.speed, 0.0);
        for expected in [1.0, 2.0, 4.0, 8.0, 8.0] {
            t.forward();
            assert_eq!(t.speed, expected);
        }
        for expected in [-1.0, -2.0, -4.0, -8.0, -8.0] {
            t.reverse();
            assert_eq!(t.speed, expected);
        }
        t.pause();
        assert!(!t.is_playing());
    }

    #[test]
    fn advance_stops_at_the_ends() {
        let end = Time::from_seconds(2.0);
        let mut t = Transport { speed: 2.0 };
        assert_eq!(
            t.advance(Time::from_seconds(0.5), 0.25, end),
            Time::from_seconds(1.0)
        );
        assert_eq!(t.advance(Time::from_seconds(1.5), 1.0, end), end);
        assert!(!t.is_playing());
        let mut t = Transport { speed: -1.0 };
        assert_eq!(t.advance(Time::from_seconds(0.1), 0.5, end), Time::ZERO);
        assert!(!t.is_playing());
    }

    #[test]
    fn preview_quality_size_and_rate() {
        let settings = ProjectSettings::default();
        let mut q = PreviewQuality::default();
        assert_eq!(q.size(&settings), (960, 540));
        q.scale = PreviewScale::Quarter;
        assert_eq!(q.size(&settings), (480, 270 & !1));
        q.scale = PreviewScale::Full;
        assert_eq!(q.size(&settings), (1920, 1080));
        let rate = Rational::new(60, 1);
        assert_eq!(q.frame_interval(rate), rate.frame_duration());
        q.max_fps = Some(15);
        assert_eq!(q.frame_interval(rate), Time::from_seconds(1.0 / 15.0));
        assert_eq!(
            PreviewScale::ALL.map(PreviewScale::label),
            ["Full", "1/2", "1/4"]
        );
        assert_eq!(fps_label(None), "Project fps");
        assert_eq!(fps_label(Some(24)), "24 fps");
    }

    #[test]
    fn edit_points_and_timecode() {
        let mut p = Project::new("t");
        let m = p.add_media(MediaAsset::new(
            "a.png",
            MediaInfo {
                kind: MediaKind::Image,
                duration: None,
                video: None,
                audio: None,
            },
        ));
        let v = p.default_track_for(MediaKind::Image);
        let s = Time::from_seconds;
        p.add_clip(v, Clip::new(m, s(1.0), Time::ZERO, s(2.0)))
            .unwrap();
        p.add_clip(v, Clip::new(m, s(3.0), Time::ZERO, s(1.0)))
            .unwrap();
        assert_eq!(edit_points(&p), vec![s(0.0), s(1.0), s(3.0), s(4.0)]);
        assert_eq!(next_edit(&p, s(1.0)), Some(s(3.0)));
        assert_eq!(next_edit(&p, s(4.0)), None);
        assert_eq!(previous_edit(&p, s(1.0)), Some(s(0.0)));
        assert_eq!(previous_edit(&p, Time::ZERO), None);
        let rate = Rational::new(25, 1);
        assert_eq!(timecode(s(3725.5), rate), "01:02:05:12");
        assert_eq!(timecode(s(1.0), Rational::new(30000, 1001)), "00:00:00:29");
    }
}
