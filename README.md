# Framework

A fast, cross-platform video editor written in Rust — developed 100 % agentically.

> Status: **Phase 2 (editor core)**. See the [roadmap](docs/ROADMAP.md).

- Classic multi-lane timeline for video, images and audio
- MP4 (H.264/AAC) export with built-in profiles (1080p, 4K, vertical, square, …) and custom resolution, frame rate and bitrate
- Planned: title generation, automatic white balance with weighting modes, and a
  "piece to camera" mode with automatic, editable emphasis zooms
- macOS (Apple silicon), Linux (x86_64, arm64) and Windows (x86_64, arm64)

## Download

Pre-built packages are attached to every [GitHub Release](https://github.com/very-doge-wow/framework/releases).
See [docs/INSTALLING.md](docs/INSTALLING.md) for one-line install commands for the latest release.

## Build from source

See [docs/BUILDING.md](docs/BUILDING.md).

## Documentation

- [Roadmap](docs/ROADMAP.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Installing](docs/INSTALLING.md)
- [Building](docs/BUILDING.md)
- [Agent instructions](AGENTS.md)

## License

GPL-3.0-or-later. Framework uses FFmpeg and x264, both licensed under the GPL;
see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
