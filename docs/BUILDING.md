# Building Framework locally

Framework is a Rust workspace that links dynamically against FFmpeg (with
x264). FFmpeg is built from source by [vcpkg](https://vcpkg.io) at the exact
versions pinned in `vcpkg.json`, so local builds match CI and releases.

The first FFmpeg build takes 15–40 minutes depending on your machine; later
builds reuse vcpkg's binary cache.

## 1. Prerequisites

| | macOS (Apple silicon) | Linux (x86_64, arm64) | Windows (x86_64, arm64) |
|---|---|---|---|
| Rust | [rustup](https://rustup.rs) | [rustup](https://rustup.rs) | [rustup](https://rustup.rs) (MSVC toolchain) |
| C toolchain | Xcode Command Line Tools: `xcode-select --install` | `build-essential` | Visual Studio 2022 Build Tools with "Desktop development with C++" |
| libclang (bindgen) | included with Xcode CLT | `clang libclang-dev` | [LLVM](https://github.com/llvm/llvm-project/releases) – set `LIBCLANG_PATH=C:\Program Files\LLVM\bin` |
| Other | `git` | `git nasm pkg-config patchelf` and GUI headers (below) | [Git for Windows](https://gitforwindows.org) (provides Git Bash) |

Linux packages (Debian/Ubuntu names):

```sh
sudo apt-get install -y build-essential git nasm pkg-config clang libclang-dev patchelf \
  libxkbcommon-dev libwayland-dev libx11-dev libxrandr-dev libxi-dev libxcursor-dev libgl1-mesa-dev
```

The Rust version is pinned in `rust-toolchain.toml`; rustup installs it
automatically on the first `cargo` command.

## 2. Build FFmpeg + x264

Run from the repository root (on Windows, in **Git Bash**):

```sh
scripts/vcpkg-install.sh
```

This clones vcpkg next to the repository (`../vcpkg`, override with
`VCPKG_ROOT`), checks out the pinned baseline and installs FFmpeg into
`vcpkg_installed/<triplet>/`. The triplet is detected automatically
(`arm64-osx-dynamic-release`, `x64-linux-dynamic-release`,
`arm64-linux-dynamic-release`, `x64-windows-release` or
`arm64-windows-release`); override with `TRIPLET=…`.

## 3. Point Cargo at FFmpeg

```sh
eval "$(scripts/ffmpeg-env.sh | sed 's/^/export /')"
```

This sets `FFMPEG_DIR` (used at build time) and the dynamic library search
path (`LD_LIBRARY_PATH`, `DYLD_FALLBACK_LIBRARY_PATH` or `PATH`) so binaries
and tests find the shared libraries. Run it in every new shell.

## 4. Build, run and test

```sh
cargo run --bin framework                   # desktop editor
cargo run --bin framework-cli -- profiles   # CLI
cargo run --bin framework-cli -- self-test /tmp/framework-self-test

cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
scripts/coverage.sh                         # needs `cargo install cargo-llvm-cov`
cargo deny check licenses bans sources      # needs `cargo install cargo-deny`
```

## 5. Package (optional)

```sh
cargo build --locked --release --bin framework --bin framework-cli
scripts/package.sh <platform-name> <version>   # e.g. linux-x86_64 0.0.0-local
scripts/smoke-test.sh <dir printed by package.sh>
```

## Using the CLI

```sh
framework-cli probe clip.mp4
framework-cli new my.framework.json intro.mp4 logo.png music.wav
framework-cli export my.framework.json -o out.mp4 --profile 1080p30
framework-cli export my.framework.json -o out.mp4 --profile 1080p30 --width 1280 --height 720 --fps 25 --bitrate 5000
```

## Troubleshooting

- **`Unable to find libclang`**: install clang/LLVM and set `LIBCLANG_PATH`.
- **`error while loading shared libraries: libavcodec…`**: run step 3 in the
  current shell.
- **macOS: "Framework is damaged / can't be opened"** for downloaded release
  builds: they are not notarised yet; run
  `xattr -dr com.apple.quarantine Framework.app`.
