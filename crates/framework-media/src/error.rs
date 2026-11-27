use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("FFmpeg error: {0}")]
    Ffmpeg(#[from] ffmpeg_next::Error),
    #[error("{path}: {source}")]
    Open {
        path: PathBuf,
        source: ffmpeg_next::Error,
    },
    #[error("{0}: no usable video or audio stream")]
    NoStreams(PathBuf),
    #[error("{0}: no video stream")]
    NoVideo(PathBuf),
    #[error("{0}: no audio stream")]
    NoAudio(PathBuf),
    #[error("{0}: not the kind of media the project expects")]
    UnexpectedKind(PathBuf),
    #[error("encoder '{0}' is not available in this FFmpeg build")]
    MissingEncoder(&'static str),
    #[error("the timeline is empty")]
    EmptyTimeline,
    #[error("export cancelled")]
    Cancelled,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
