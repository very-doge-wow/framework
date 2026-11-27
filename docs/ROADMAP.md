# Roadmap

This is the persistent implementation plan for Framework. Every agent session
should read it before starting work, keep it up to date when scope changes, and
tick items off as they land. Each major item has a GitHub issue; the issue holds
the detailed discussion, this file holds the big picture.

Effort is given in agent sessions (roughly one week of human engineering each).

## Product goals

- A classic non-linear video editor: multi-lane timeline, cutting, trimming,
  layering, video + image + audio clips.
- Sleek, modern UI and ultra-high performance (Rust everywhere, GPU compositing,
  hardware decode/encode where available).
- Signature features: title/text generation, a "piece to camera" mode with
  automatic, editable emphasis zooms, and automatic white balance with weighting
  modes.
- Configurable output (resolution, frame rate, bitrate) with sensible built-in
  profiles. MP4 (H.264/AAC) first, more formats later.
- Runs on macOS, Linux and Windows. Built, tested and published by GitHub
  Actions. Fully reproducible builds (every dependency pinned).
- Licensed GPL-3.0-or-later (required by FFmpeg with x264).

## Phase 1 – Foundation (2–4 sessions) — [#1](https://github.com/very-doge-wow/framework/issues/1)

- [x] License: GPL-3.0-or-later.
- [x] Architecture document ([ARCHITECTURE.md](ARCHITECTURE.md)) and local build guide ([BUILDING.md](BUILDING.md)).
- [x] Cargo workspace: `framework-core`, `framework-media`, `framework-cli`, `framework-app`.
- [x] FFmpeg 9.1 + x264 via vcpkg manifest, linked dynamically, same build on all three OSes.
- [x] Project model and versioned project format (JSON, superseded by `.fwk` in #22).
- [x] Media probing, sequential decoding, H.264/AAC MP4 export of a layered timeline with mixed audio.
- [x] Built-in export profiles.
- [x] Minimal desktop app shell: browser panel with Media pool / Effects / Generators tabs (the latter two list planned items), preview, timeline, export dialog.
- [x] CI on Linux/macOS/Windows: fmt, clippy, tests, license check, packaged artifacts.
- [x] semantic-release on `main` creates the `v*` tag and GitHub Release; per-OS packages and the matching FFmpeg/x264 source are attached.
- [x] Everything pinned (toolchain, crates, vcpkg, actions by SHA, runner images); Renovate keeps it current.

Acceptance: `framework-cli export` renders a multi-clip project to a playable MP4
on all three platforms in CI; merging a releasable Conventional Commit to `main`
publishes a tagged release.

## Phase 2 – Editor core (8–12 sessions)

- [ ] Real-time playback engine: GPU (wgpu) compositing, frame cache, audio
      output, A/V sync, J/K/L shuttle, selectable preview quality (resolution, frame rate). — [#2](https://github.com/very-doge-wow/framework/issues/2)
- [ ] Timeline editing: move, trim, split/cut, ripple, snapping, multi-select,
      add/remove/reorder lanes (including empty lanes), clip grouping/ungrouping
      (video with its audio is imported onto separate lanes and grouped), time
      zoom (buttons, Cmd/Ctrl +/−, Cmd/Ctrl + wheel), resizable panels, undo/redo
      for every edit. — [#3](https://github.com/very-doge-wow/framework/issues/3)
- [ ] Compositing: per-clip transform (position, scale, rotation, crop,
      opacity), keyframes, transparency for images (alpha), basic transitions.
      — [#4](https://github.com/very-doge-wow/framework/issues/4)
- [ ] Media management: import of video/image/audio, thumbnails, waveforms,
      relinking missing media, optional proxy generation. — [#5](https://github.com/very-doge-wow/framework/issues/5)
- [ ] Audio editing: clip gain, fades, track volume/mute/solo, level meters.
      — [#6](https://github.com/very-doge-wow/framework/issues/6)
- [ ] Effects system: per-clip and per-lane effect stacks (keyframable
      parameters, GPU rendered), one effect inspector next to the preview for
      every effect, and the **Effects** browser tab. — [#19](https://github.com/very-doge-wow/framework/issues/19)
- [ ] Generators: solid colour and gradient clips (text via phase 3) and the
      **Generators** browser tab. — [#20](https://github.com/very-doge-wow/framework/issues/20)
- [ ] Export UX: profile picker, custom profile editor (resolution, fps,
      bitrate), background rendering with progress and cancel. — [#7](https://github.com/very-doge-wow/framework/issues/7)
- [ ] `.fwk` project file: Save (references) and Collect and save (embedded
      media). — [#22](https://github.com/very-doge-wow/framework/issues/22)
- [ ] Keyboard shortcuts: full default set, remappable in Settings. — [#23](https://github.com/very-doge-wow/framework/issues/23)
- [ ] Record audio and video from connected microphones and cameras. — [#24](https://github.com/very-doge-wow/framework/issues/24)

Acceptance: a user can assemble, cut and layer a multi-track edit with images
and music, play it back smoothly at 1080p, and export it.

## Phase 3 – Titles and text (2–4 sessions) — [#8](https://github.com/very-doge-wow/framework/issues/8)

- [ ] Text clips on the timeline: font, size, colour, outline, shadow, background box.
- [ ] Title templates (lower thirds, centred title, end card) and in/out animations.
- [ ] GPU text rendering with proper shaping (bidi, emoji) and bundled open-licence fonts.

## Phase 4 – Colour and automatic white balance (3–5 sessions) — [#9](https://github.com/very-doge-wow/framework/issues/9)

- [ ] Colour-managed pipeline (BT.709/BT.2020 aware, linear-light compositing).
- [ ] Manual white balance / tint / exposure controls per clip.
- [ ] Automatic white balance with weighting modes: neutral (grey world),
      natural skin tones (skin-tone detection weighting), cinematic (warm/teal
      bias), and user-adjustable strength.
- [ ] Analysis runs in the background and results are cached per clip.

## Phase 5 – Piece-to-camera mode (8–12 sessions)

- [ ] Speech analysis: on-device transcription with word timestamps, plus
      prosody features (loudness, pitch, speaking rate, pauses). — [#10](https://github.com/very-doge-wow/framework/issues/10)
- [ ] Face analysis: on-device face detection and tracking to frame zooms on
      the subject. — [#11](https://github.com/very-doge-wow/framework/issues/11)
- [ ] Emphasis detection that classifies moments by type (e.g. loudness peak,
      pitch accent, keyword, pause/beat) and generates zoom-in/zoom-out
      keyframes centred on the face. — [#12](https://github.com/very-doge-wow/framework/issues/12)
- [ ] An "emphasis" lane in the timeline where every detected emphasis is
      visible by type and can be moved, retimed, retyped, disabled or added
      manually; zooms update live. — [#13](https://github.com/very-doge-wow/framework/issues/13)

Models must run locally (e.g. via ONNX Runtime or candle) and have licences
compatible with GPL-3.0 redistribution.

## Phase 6 – Performance, packaging and polish (5–10+ sessions)

- [ ] Hardware decode/encode: VideoToolbox (macOS), NVENC/QSV/AMF (Windows),
      VAAPI/NVENC (Linux), with automatic fallback to software. — [#14](https://github.com/very-doge-wow/framework/issues/14)
- [ ] Profiling and performance budget (4K timeline scrubbing, export speed). — [#15](https://github.com/very-doge-wow/framework/issues/15)
- [ ] Installers and signing: macOS notarised .dmg (universal), Windows signed
      installer, Linux AppImage/Flatpak; bit-for-bit reproducible release
      verification. — [#16](https://github.com/very-doge-wow/framework/issues/16)
- [ ] UI/UX polish: themes, keyboard shortcuts, accessibility, onboarding. — [#17](https://github.com/very-doge-wow/framework/issues/17)

## Later

- [ ] More output formats (HEVC, AV1, ProRes/MOV, WebM, audio-only). — [#18](https://github.com/very-doge-wow/framework/issues/18)
