use crate::time::Rational;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Container {
    Mp4,
}

impl Container {
    pub fn extension(self) -> &'static str {
        match self {
            Container::Mp4 => "mp4",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    H264,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCodec {
    Aac,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoSettings {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rational,
    /// Target average bitrate in kbit/s.
    pub bitrate_kbps: u32,
    /// Encoder speed/quality trade-off (x264 preset name, e.g. "medium").
    pub preset: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioSettings {
    pub codec: AudioCodec,
    pub sample_rate: u32,
    pub channels: u16,
    pub bitrate_kbps: u32,
}

/// Everything needed to render a project to a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportProfile {
    pub id: String,
    pub name: String,
    pub container: Container,
    pub video: VideoSettings,
    pub audio: AudioSettings,
}

impl ExportProfile {
    fn h264_aac(
        id: &str,
        name: &str,
        (width, height): (u32, u32),
        fps: u32,
        video_kbps: u32,
        audio_kbps: u32,
        preset: &str,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            container: Container::Mp4,
            video: VideoSettings {
                codec: VideoCodec::H264,
                width,
                height,
                frame_rate: Rational::new(fps, 1),
                bitrate_kbps: video_kbps,
                preset: preset.into(),
            },
            audio: AudioSettings {
                codec: AudioCodec::Aac,
                sample_rate: 48_000,
                channels: 2,
                bitrate_kbps: audio_kbps,
            },
        }
    }

    /// The built-in rendering presets, in the order they are shown to users.
    pub fn builtin() -> Vec<ExportProfile> {
        vec![
            Self::h264_aac(
                "1080p30",
                "1080p 30 fps",
                (1920, 1080),
                30,
                12_000,
                192,
                "medium",
            ),
            Self::h264_aac(
                "1080p60",
                "1080p 60 fps",
                (1920, 1080),
                60,
                18_000,
                192,
                "medium",
            ),
            Self::h264_aac(
                "2160p30",
                "4K 30 fps",
                (3840, 2160),
                30,
                45_000,
                256,
                "medium",
            ),
            Self::h264_aac(
                "2160p60",
                "4K 60 fps",
                (3840, 2160),
                60,
                68_000,
                256,
                "medium",
            ),
            Self::h264_aac(
                "720p30",
                "720p 30 fps (web)",
                (1280, 720),
                30,
                6_000,
                128,
                "medium",
            ),
            Self::h264_aac(
                "vertical-1080p30",
                "Vertical 1080×1920 30 fps (Shorts/Reels/TikTok)",
                (1080, 1920),
                30,
                12_000,
                192,
                "medium",
            ),
            Self::h264_aac(
                "square-1080p30",
                "Square 1080×1080 30 fps",
                (1080, 1080),
                30,
                10_000,
                192,
                "medium",
            ),
            Self::h264_aac(
                "draft-540p",
                "Draft 540p (fast)",
                (960, 540),
                30,
                2_500,
                128,
                "veryfast",
            ),
        ]
    }

    pub fn builtin_by_id(id: &str) -> Option<ExportProfile> {
        Self::builtin().into_iter().find(|p| p.id == id)
    }

    pub fn default_profile() -> ExportProfile {
        Self::builtin().remove(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_ids_are_unique_and_dimensions_even() {
        let profiles = ExportProfile::builtin();
        let mut ids: Vec<_> = profiles.iter().map(|p| p.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), profiles.len());
        for p in &profiles {
            // yuv420p requires even dimensions.
            assert_eq!(p.video.width % 2, 0, "{}", p.id);
            assert_eq!(p.video.height % 2, 0, "{}", p.id);
        }
    }

    #[test]
    fn lookup_by_id() {
        assert_eq!(
            ExportProfile::builtin_by_id("720p30").unwrap().video.width,
            1280
        );
        assert!(ExportProfile::builtin_by_id("nope").is_none());
    }
}
