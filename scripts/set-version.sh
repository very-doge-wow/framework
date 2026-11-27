#!/usr/bin/env bash
# Stamps a release version (from semantic-release) into the Cargo workspace.
# The version is only applied in CI; the committed version stays 0.0.0-dev.
set -euo pipefail
cd "$(dirname "$0")/.."
version="$1"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || { echo "invalid version '$version'" >&2; exit 1; }
sed -i.bak -E "/^\[workspace\.package\]/,/^\[/ s/^version = \".*\"/version = \"$version\"/" Cargo.toml
rm -f Cargo.toml.bak
cargo update --workspace --offline 2>/dev/null || cargo update --workspace
grep -A1 '^\[workspace.package\]' Cargo.toml
