# Third-party notices

Framework is licensed under the GNU General Public License v3.0 or later (see
`LICENSE`).

## FFmpeg

This software uses libraries from the [FFmpeg](https://ffmpeg.org) project
(libavcodec, libavformat, libavfilter, libavutil, libswresample, libswscale),
built with `--enable-gpl` and therefore licensed under the GNU GPL version 2
or later. FFmpeg is a trademark of Fabrice Bellard.

## x264

H.264 encoding uses [x264](https://www.videolan.org/developers/x264.html),
licensed under the GNU GPL version 2 or later.

## zlib

PNG support uses [zlib](https://zlib.net), licensed under the zlib license.

## Source code

The complete corresponding source code of FFmpeg, x264 and zlib, including the
exact build scripts and configuration used for the shipped binaries, is
attached to every GitHub Release as
`framework-<version>-ffmpeg-x264-sources.tar.gz`. The Framework source code for
each release is available from the same release page and the git tag.

## Rust crates

The Rust dependencies and their licenses are listed in `Cargo.lock`; all of
them are verified to be GPL-3.0-compatible with `cargo deny check licenses`.
egui's built-in fonts are licensed under the SIL Open Font License 1.1 and
the Ubuntu Font Licence 1.0.
