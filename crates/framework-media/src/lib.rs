//! Media I/O for Framework, built on the FFmpeg libraries.
//!
//! FFmpeg is linked dynamically and built with `--enable-gpl --enable-libx264`
//! (see `vcpkg.json`), which is why the whole project is GPL-3.0-or-later.

mod audio;
mod error;
mod export;
mod preview;
mod probe;
pub mod synthetic;
mod video;

pub use audio::AudioReader;
pub use error::MediaError;
pub use export::{ExportProgress, export};
pub use preview::{PreviewRenderer, RgbaImage};
pub use probe::probe;
pub use video::VideoReader;

use std::sync::Once;

/// Initialises FFmpeg. Safe to call many times; every public entry point calls it.
pub fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        ffmpeg_next::init().expect("failed to initialise FFmpeg");
        ffmpeg_next::log::set_level(ffmpeg_next::log::Level::Error);
    });
}

/// Version and configuration of the FFmpeg libraries in use, for "About" dialogs.
pub fn ffmpeg_version_info() -> String {
    init();
    let v = unsafe { ffmpeg_next::ffi::avcodec_version() };
    let license = unsafe {
        std::ffi::CStr::from_ptr(ffmpeg_next::ffi::avcodec_license())
            .to_string_lossy()
            .into_owned()
    };
    format!(
        "libavcodec {}.{}.{} ({license})",
        v >> 16,
        (v >> 8) & 0xff,
        v & 0xff
    )
}
