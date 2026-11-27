use crate::MediaError;
use ffmpeg_next::{format, media};
use framework_core::{AudioStreamInfo, MediaInfo, MediaKind, Rational, Time, VideoStreamInfo};
use std::path::Path;

const STILL_IMAGE_CODECS: &[&str] = &["png", "mjpeg", "bmp", "tiff", "webp", "gif", "jpegls"];

/// Checks that every referenced (not yet embedded) asset is media of the kind
/// the project records, so collecting an untrusted project cannot embed
/// arbitrary local files.
pub fn check_collectable(project: &framework_core::Project) -> Result<(), MediaError> {
    for asset in project.media.iter().filter(|m| m.embedded.is_none()) {
        // Only regular local files: FFmpeg would open URLs such as `http://…`.
        let local = std::fs::metadata(&asset.path).is_ok_and(|m| m.is_file());
        if !local || probe(&asset.path)?.kind != asset.kind() {
            return Err(MediaError::UnexpectedKind(asset.path.clone()));
        }
    }
    Ok(())
}

/// Reads the technical metadata of a media file.
pub fn probe(path: &Path) -> Result<MediaInfo, MediaError> {
    crate::init();
    let ictx = format::input(path).map_err(|source| MediaError::Open {
        path: path.to_owned(),
        source,
    })?;

    let video = ictx.streams().best(media::Type::Video).map(|s| {
        let params = s.parameters();
        let (width, height) = unsafe {
            let p = params.as_ptr();
            ((*p).width.max(0) as u32, (*p).height.max(0) as u32)
        };
        let rate = s.rate();
        let rate = if rate.numerator() > 0 && rate.denominator() > 0 {
            rate
        } else {
            s.avg_frame_rate()
        };
        let frame_rate = if rate.numerator() > 0 && rate.denominator() > 0 {
            Rational::new(rate.numerator() as u32, rate.denominator() as u32)
        } else {
            Rational::new(30, 1)
        };
        (
            VideoStreamInfo {
                width,
                height,
                frame_rate,
                codec: params.id().name().to_owned(),
            },
            s.frames(),
        )
    });

    let audio = ictx.streams().best(media::Type::Audio).map(|s| {
        let params = s.parameters();
        let (sample_rate, channels) = unsafe {
            let p = params.as_ptr();
            (
                (*p).sample_rate.max(0) as u32,
                (*p).ch_layout.nb_channels.max(0) as u16,
            )
        };
        AudioStreamInfo {
            sample_rate,
            channels,
            codec: params.id().name().to_owned(),
        }
    });

    let format_name = ictx.format().name().to_owned();
    let is_image = video.as_ref().is_some_and(|(v, frames)| {
        audio.is_none()
            && (format_name == "image2"
                || format_name.ends_with("_pipe")
                || (STILL_IMAGE_CODECS.contains(&v.codec.as_str()) && *frames == 1))
    });

    let duration = (ictx.duration() > 0)
        .then(|| Time::from_timebase(ictx.duration(), 1, ffmpeg_next::ffi::AV_TIME_BASE));

    let kind = match (&video, &audio) {
        _ if is_image => MediaKind::Image,
        (Some(_), _) => MediaKind::Video,
        (None, Some(_)) => MediaKind::Audio,
        (None, None) => return Err(MediaError::NoStreams(path.to_owned())),
    };

    Ok(MediaInfo {
        kind,
        duration: if kind == MediaKind::Image {
            None
        } else {
            duration
        },
        video: video.map(|(v, _)| v),
        audio,
    })
}
