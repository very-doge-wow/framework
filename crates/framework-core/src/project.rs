use crate::time::{Rational, Time};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Current on-disk project format version. Bump when the schema changes in a
/// way that older readers cannot handle and add a migration in [`Project::from_json`].
pub const PROJECT_FORMAT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid project file: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported project format version {found} (this build supports up to {supported})")]
    UnsupportedVersion { found: u32, supported: u32 },
    #[error("unknown media id {0}")]
    UnknownMedia(MediaId),
    #[error("unknown track id {0}")]
    UnknownTrack(TrackId),
    #[error("media kind {media:?} cannot be placed on a {track:?} track")]
    IncompatibleTrack { media: MediaKind, track: TrackKind },
    #[error("clip overlaps an existing clip on the same track")]
    Overlap,
}

macro_rules! id_type {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

id_type!(MediaId);
id_type!(TrackId);
id_type!(ClipId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Video,
    Audio,
    Image,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoStreamInfo {
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rational,
    pub codec: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioStreamInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub codec: String,
}

/// Technical information about a media file, filled in by probing it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub kind: MediaKind,
    /// `None` for still images, which can be stretched to any length.
    pub duration: Option<Time>,
    pub video: Option<VideoStreamInfo>,
    pub audio: Option<AudioStreamInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaAsset {
    pub id: MediaId,
    pub path: PathBuf,
    pub info: MediaInfo,
}

impl MediaAsset {
    pub fn new(path: impl Into<PathBuf>, info: MediaInfo) -> Self {
        Self {
            id: MediaId::new(),
            path: path.into(),
            info,
        }
    }

    pub fn kind(&self) -> MediaKind {
        self.info.kind
    }

    pub fn display_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
    }

    /// Default length of a clip created from this asset.
    pub fn default_clip_duration(&self) -> Time {
        self.info.duration.unwrap_or(Time::from_seconds(5.0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    /// Video tracks hold video and image clips. Their audio is mixed too.
    Video,
    Audio,
}

impl TrackKind {
    pub fn accepts(self, media: MediaKind) -> bool {
        match self {
            TrackKind::Video => matches!(media, MediaKind::Video | MediaKind::Image),
            TrackKind::Audio => matches!(media, MediaKind::Audio | MediaKind::Video),
        }
    }
}

/// A placement of (part of) a media asset on a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub media: MediaId,
    /// Position of the clip's first frame on the timeline.
    pub start: Time,
    /// Offset into the source media where the clip begins.
    pub source_in: Time,
    pub duration: Time,
}

impl Clip {
    pub fn new(media: MediaId, start: Time, source_in: Time, duration: Time) -> Self {
        Self {
            id: ClipId::new(),
            media,
            start,
            source_in,
            duration,
        }
    }

    pub fn end(&self) -> Time {
        self.start + self.duration
    }

    pub fn contains(&self, t: Time) -> bool {
        t >= self.start && t < self.end()
    }

    /// Maps a timeline time to the corresponding time in the source media.
    pub fn source_time(&self, timeline: Time) -> Time {
        self.source_in + (timeline - self.start)
    }

    pub fn overlaps(&self, start: Time, end: Time) -> bool {
        self.start < end && start < self.end()
    }
}

/// A timeline lane. For video tracks, a higher index in [`Project::tracks`]
/// is composited on top of lower ones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub kind: TrackKind,
    pub name: String,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub hidden: bool,
    /// Clips sorted by start time, never overlapping.
    pub clips: Vec<Clip>,
}

impl Track {
    pub fn new(kind: TrackKind, name: impl Into<String>) -> Self {
        Self {
            id: TrackId::new(),
            kind,
            name: name.into(),
            muted: false,
            hidden: false,
            clips: Vec::new(),
        }
    }

    pub fn end(&self) -> Time {
        self.clips.iter().map(Clip::end).max().unwrap_or(Time::ZERO)
    }

    pub fn clip_at(&self, t: Time) -> Option<&Clip> {
        self.clips.iter().find(|c| c.contains(t))
    }

    fn insert_sorted(&mut self, clip: Clip) -> Result<(), ProjectError> {
        if self
            .clips
            .iter()
            .any(|c| c.overlaps(clip.start, clip.end()))
        {
            return Err(ProjectError::Overlap);
        }
        let idx = self.clips.partition_point(|c| c.start < clip.start);
        self.clips.insert(idx, clip);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectSettings {
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rational,
    pub sample_rate: u32,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            frame_rate: Rational::new(30, 1),
            sample_rate: 48_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub format_version: u32,
    pub name: String,
    pub settings: ProjectSettings,
    pub media: Vec<MediaAsset>,
    pub tracks: Vec<Track>,
}

impl Default for Project {
    fn default() -> Self {
        Self::new("Untitled")
    }
}

impl Project {
    /// Creates an empty project with one video and one audio track.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            format_version: PROJECT_FORMAT_VERSION,
            name: name.into(),
            settings: ProjectSettings::default(),
            media: Vec::new(),
            tracks: vec![
                Track::new(TrackKind::Video, "V1"),
                Track::new(TrackKind::Audio, "A1"),
            ],
        }
    }

    pub fn duration(&self) -> Time {
        self.tracks
            .iter()
            .map(Track::end)
            .max()
            .unwrap_or(Time::ZERO)
    }

    pub fn media(&self, id: MediaId) -> Option<&MediaAsset> {
        self.media.iter().find(|m| m.id == id)
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn add_media(&mut self, asset: MediaAsset) -> MediaId {
        let id = asset.id;
        self.media.push(asset);
        id
    }

    pub fn add_track(&mut self, kind: TrackKind) -> TrackId {
        let n = self.tracks.iter().filter(|t| t.kind == kind).count() + 1;
        let prefix = match kind {
            TrackKind::Video => "V",
            TrackKind::Audio => "A",
        };
        let track = Track::new(kind, format!("{prefix}{n}"));
        let id = track.id;
        self.tracks.push(track);
        id
    }

    /// Places a clip on a track after validating it.
    pub fn add_clip(&mut self, track: TrackId, clip: Clip) -> Result<ClipId, ProjectError> {
        let media_kind = self
            .media(clip.media)
            .ok_or(ProjectError::UnknownMedia(clip.media))?
            .kind();
        let track = self
            .tracks
            .iter_mut()
            .find(|t| t.id == track)
            .ok_or(ProjectError::UnknownTrack(track))?;
        if !track.kind.accepts(media_kind) {
            return Err(ProjectError::IncompatibleTrack {
                media: media_kind,
                track: track.kind,
            });
        }
        let id = clip.id;
        track.insert_sorted(clip)?;
        Ok(id)
    }

    /// Appends the whole asset to the end of a track.
    pub fn append_media(&mut self, track: TrackId, media: MediaId) -> Result<ClipId, ProjectError> {
        let asset = self.media(media).ok_or(ProjectError::UnknownMedia(media))?;
        let duration = asset.default_clip_duration();
        let start = self
            .track(track)
            .ok_or(ProjectError::UnknownTrack(track))?
            .end();
        self.add_clip(track, Clip::new(media, start, Time::ZERO, duration))
    }

    /// The first video or audio track that accepts this media kind, creating one if needed.
    pub fn default_track_for(&mut self, kind: MediaKind) -> TrackId {
        let wanted = match kind {
            MediaKind::Audio => TrackKind::Audio,
            MediaKind::Video | MediaKind::Image => TrackKind::Video,
        };
        match self.tracks.iter().find(|t| t.kind == wanted) {
            Some(t) => t.id,
            None => self.add_track(wanted),
        }
    }

    pub fn to_json(&self) -> Result<String, ProjectError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(json: &str) -> Result<Self, ProjectError> {
        #[derive(Deserialize)]
        struct VersionProbe {
            format_version: u32,
        }
        let probe: VersionProbe = serde_json::from_str(json)?;
        if probe.format_version > PROJECT_FORMAT_VERSION {
            return Err(ProjectError::UnsupportedVersion {
                found: probe.format_version,
                supported: PROJECT_FORMAT_VERSION,
            });
        }
        Ok(serde_json::from_str(json)?)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, ProjectError> {
        Self::from_json(&fs::read_to_string(path)?)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), ProjectError> {
        let path = path.as_ref();
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp);
        fs::write(&tmp, self.to_json()?)?;
        fs::rename(tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video_asset(seconds: f64) -> MediaAsset {
        MediaAsset::new(
            "clip.mp4",
            MediaInfo {
                kind: MediaKind::Video,
                duration: Some(Time::from_seconds(seconds)),
                video: Some(VideoStreamInfo {
                    width: 1920,
                    height: 1080,
                    frame_rate: Rational::new(30, 1),
                    codec: "h264".into(),
                }),
                audio: None,
            },
        )
    }

    #[test]
    fn append_places_clips_back_to_back() {
        let mut p = Project::new("test");
        let m = p.add_media(video_asset(2.0));
        let v1 = p.default_track_for(MediaKind::Video);
        p.append_media(v1, m).unwrap();
        p.append_media(v1, m).unwrap();
        let track = p.track(v1).unwrap();
        assert_eq!(track.clips[1].start, Time::from_seconds(2.0));
        assert_eq!(p.duration(), Time::from_seconds(4.0));
    }

    #[test]
    fn overlapping_clips_are_rejected() {
        let mut p = Project::new("test");
        let m = p.add_media(video_asset(2.0));
        let v1 = p.default_track_for(MediaKind::Video);
        p.append_media(v1, m).unwrap();
        let clip = Clip::new(
            m,
            Time::from_seconds(1.0),
            Time::ZERO,
            Time::from_seconds(1.0),
        );
        assert!(matches!(p.add_clip(v1, clip), Err(ProjectError::Overlap)));
    }

    #[test]
    fn images_cannot_go_on_audio_tracks() {
        let mut p = Project::new("test");
        let mut asset = video_asset(0.0);
        asset.info.kind = MediaKind::Image;
        asset.info.duration = None;
        let m = p.add_media(asset);
        let a1 = p.default_track_for(MediaKind::Audio);
        assert!(matches!(
            p.append_media(a1, m),
            Err(ProjectError::IncompatibleTrack { .. })
        ));
    }

    #[test]
    fn json_round_trip() {
        let mut p = Project::new("test");
        let m = p.add_media(video_asset(2.0));
        let v1 = p.default_track_for(MediaKind::Video);
        p.append_media(v1, m).unwrap();
        let back = Project::from_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn newer_format_versions_are_rejected() {
        let mut p = Project::new("test");
        p.format_version = PROJECT_FORMAT_VERSION + 1;
        let json = serde_json::to_string(&p).unwrap();
        assert!(matches!(
            Project::from_json(&json),
            Err(ProjectError::UnsupportedVersion { .. })
        ));
    }
}
