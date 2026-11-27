use crate::MediaError;
use crate::export::top_video_clip;
use crate::video::{Fitter, VideoReader};
use ffmpeg_next::format;
use framework_core::{ClipId, MediaKind, Project, Time};
use std::collections::HashMap;

/// A tightly packed 8-bit RGBA image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Renders single timeline frames for the editor preview, keeping decoders
/// open between calls so that scrubbing and playback stay cheap.
pub struct PreviewRenderer {
    readers: HashMap<ClipId, VideoReader>,
    fitter: Fitter,
    width: u32,
    height: u32,
}

impl PreviewRenderer {
    pub fn new(width: u32, height: u32) -> Self {
        let width = width.max(2) & !1;
        let height = height.max(2) & !1;
        Self {
            readers: HashMap::new(),
            fitter: Fitter::new(format::Pixel::RGBA, width, height),
            width,
            height,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Renders the frame visible at timeline time `t`.
    pub fn render(&mut self, project: &Project, t: Time) -> Result<RgbaImage, MediaError> {
        let frame = match top_video_clip(project, t) {
            Some((clip, asset)) => {
                let reader = match self.readers.entry(clip.id) {
                    std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                    std::collections::hash_map::Entry::Vacant(e) => e.insert(VideoReader::open(
                        &asset.path,
                        asset.kind() == MediaKind::Image,
                    )?),
                };
                let orientation = reader.orientation();
                match reader.frame_at(clip.source_time(t))? {
                    Some(src) => self.fitter.fit(src, orientation)?,
                    None => self.fitter.black(),
                }
            }
            None => self.fitter.black(),
        };
        self.readers.retain(|id, _| {
            project
                .tracks
                .iter()
                .flat_map(|tr| &tr.clips)
                .any(|c| c.id == *id)
        });

        let stride = frame.stride(0);
        let row = self.width as usize * 4;
        let mut pixels = Vec::with_capacity(row * self.height as usize);
        for y in 0..self.height as usize {
            pixels.extend_from_slice(&frame.data(0)[y * stride..y * stride + row]);
        }
        Ok(RgbaImage {
            width: self.width,
            height: self.height,
            pixels,
        })
    }
}
