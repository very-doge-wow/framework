# Architecture

## Goals that drive the design

- **Performance**: Rust everywhere, FFmpeg for codecs, GPU rendering via
  wgpu, background threads for decoding/rendering so the UI never blocks.
- **Cross-platform**: one code base for macOS, Linux and Windows; the same
  FFmpeg build (vcpkg) on all three.
- **Testability**: GUI-independent core, synthetic test media, ≥ 95 % line
  coverage.
- **Reproducibility**: every input pinned (see `AGENTS.md`).

## Crates

```
framework-core   pure data model, no FFmpeg/GUI   (time, project, profiles)
      ▲
framework-media  FFmpeg: probe, decode, render, export, preview, synthetic media
      ▲
framework-cli    command-line front end (scriptable, used for CI smoke tests)
framework-app    egui/eframe desktop editor (wgpu renderer)
```

### framework-core

- `Time`: `i64` in **flicks** (1/705 600 000 s). Every common frame rate and
  sample rate divides it exactly, so frame/sample positions never drift.
- `Project` → `MediaAsset`s (probed metadata) and `Track`s (lanes) holding
  non-overlapping `Clip`s (`start`, `source_in`, `duration`). For video
  tracks, a higher index is composited on top.
- Project files are versioned JSON (`format_version`); newer versions are
  rejected with a clear error, migrations go in `Project::from_json`.
- `ExportProfile`: container/codec/resolution/frame rate/bitrate plus the
  built-in presets.

### framework-media

- `probe` – reads stream metadata and classifies video/audio/image.
- `VideoReader` – sequential-optimised decoder with seeking; still images
  return their single frame for any time.
- `AudioReader` – decodes and resamples to interleaved stereo `f32` at the
  project rate; `mix_into` adds samples at an arbitrary source position.
- `export` – renders a project: per output frame, the topmost visible clip is
  decoded, letterboxed into the output size and encoded with libx264; audio
  from all unmuted tracks is mixed and encoded with FFmpeg's AAC encoder into
  MP4 (`+faststart`). Cancellable, reports progress.
- `PreviewRenderer` – renders single RGBA frames for the editor preview,
  keeping decoders open for cheap scrubbing.
- `synthetic` – generates test videos/images/WAVs (used by tests and
  `framework-cli self-test`).

Phase 2 replaces the CPU letterboxing compositor with a wgpu compositor that
is shared by preview and export (transforms, opacity, keyframes).

### framework-app

- `EditorState` – toolkit-independent state (project, playhead, zoom,
  selection, export settings) with unit tests.
- `ui::FrameworkApp` – egui UI: media bin, preview, timeline, export dialog.
  UI code is exercised headlessly in tests through `egui::Context::run`.
- `worker` – `PreviewWorker` (latest-request-wins background rendering) and
  `ExportJob` (background export with progress and cancel).

## FFmpeg and licensing

FFmpeg 9.1 is built by vcpkg with `gpl`, `x264` and `zlib` features and linked
**dynamically**. Because x264 is GPL, Framework is GPL-3.0-or-later. Releases
ship the shared libraries next to the binaries and attach the corresponding
FFmpeg/x264/zlib source archive (`scripts/bundle-sources.sh`).

## Build, CI and releases

- `scripts/vcpkg-install.sh` builds FFmpeg for the host triplet;
  `scripts/ffmpeg-env.sh` exports `FFMPEG_DIR` and library paths.
- `.github/workflows/build.yml` (reusable): fmt, clippy, tests, coverage
  gate, cargo-deny, release build, packaging and smoke test on Linux, macOS
  and Windows.
- `ci.yml` runs it for PRs and `main`; `commitlint.yml` checks Conventional
  Commits.
- `release.yml` runs semantic-release on `main`. If a release is due it
  creates the tag and GitHub Release, then runs the build with the version
  stamped in and uploads the packages and the FFmpeg source bundle.
