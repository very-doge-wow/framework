# Agent instructions

Framework is developed agentically. Read this file, then follow it.

## Before starting any task

1. Read [docs/ROADMAP.md](docs/ROADMAP.md) – it is the persistent plan.
2. If the task relates to a GitHub issue, read the issue **and all of its
   comments** (`gh issue view <n> --comments`). Maintainer comments override
   the roadmap and the original issue text.
3. Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and
   [docs/BUILDING.md](docs/BUILDING.md).

## Rules

- **Conventional Commits, Angular preset**, for every commit and PR title
  (`feat(media): …`, `fix(app): …`, `ci: …`, `docs: …`, `build: …`,
  `test: …`, `refactor: …`, `perf: …`). Allowed types: build, ci, docs,
  feat, fix, perf, refactor, revert, style, test. semantic-release derives
  versions from them; commitlint enforces them in CI.
- **Pin everything exactly.** Crates use `=x.y.z` in the root `Cargo.toml`
  `[workspace.dependencies]` (crates inherit with `workspace = true`),
  `Cargo.lock` is committed and builds use `--locked`. GitHub Actions are
  pinned by commit SHA with a version comment, runner images by version (never
  `-latest`), npm packages exactly with `package-lock.json`, FFmpeg/x264 via
  the `builtin-baseline` in `vcpkg.json`, Rust via `rust-toolchain.toml`.
  Renovate keeps them current.
- **Unit test coverage must stay ≥ 95 % (lines)**, enforced by
  `scripts/coverage.sh` in CI. Keep logic out of `main.rs`; put it in
  library modules with tests. Generate test media with
  `framework_media::synthetic` instead of committing binary fixtures.
- **License: GPL-3.0-or-later.** Every new dependency must be compatible
  (`cargo deny check licenses` runs in CI). Never enable FFmpeg `nonfree`.
- Must build and run on **macOS (arm64), Linux (x86_64, arm64) and Windows (x86_64, arm64)**.
- `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo test --workspace` must pass before pushing.
- Update `docs/ROADMAP.md` (tick items, add new ones) in the same PR that
  changes scope, and reference the issue (`Closes #n`) in the PR body.
