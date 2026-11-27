#!/usr/bin/env bash
# Runs the test suite with coverage and fails below the line-coverage threshold.
# Thin entry points (main.rs) are excluded; all logic lives in tested library code.
set -euo pipefail
cd "$(dirname "$0")/.."
threshold="${COVERAGE_THRESHOLD:-95}"
cargo llvm-cov --locked --workspace \
  --ignore-filename-regex '(^|/)(src/main\.rs)$' \
  --fail-under-lines "$threshold" \
  --summary-only
