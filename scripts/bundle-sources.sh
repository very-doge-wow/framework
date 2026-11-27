#!/usr/bin/env bash
# Creates the "corresponding source" archive for the GPL components we ship
# in binary form: the exact FFmpeg, x264 and zlib source archives that vcpkg
# downloads for the pinned baseline, plus the vcpkg port scripts and patches
# that control how they are configured and built.
# Usage: scripts/bundle-sources.sh <output.tar.gz>
set -euo pipefail
cd "$(dirname "$0")/.."
out="$1"
VCPKG_ROOT="${VCPKG_ROOT:-$(cd .. && pwd)/vcpkg}"
if [[ ! -d "$VCPKG_ROOT/.git" ]]; then
  git clone --filter=blob:none https://github.com/microsoft/vcpkg "$VCPKG_ROOT"
fi
baseline="$(sed -n 's/.*"builtin-baseline": *"\([0-9a-f]\{40\}\)".*/\1/p' vcpkg.json)"
git -C "$VCPKG_ROOT" -c advice.detachedHead=false checkout --quiet "$baseline"
[[ -x "$VCPKG_ROOT/vcpkg" ]] || "$VCPKG_ROOT/bootstrap-vcpkg.sh" -disableMetrics
"$VCPKG_ROOT/vcpkg" install --only-downloads \
  --triplet x64-linux-dynamic-release --overlay-triplets=vcpkg-triplets \
  --x-install-root="$PWD/vcpkg_installed" --disable-metrics

stage="$(mktemp -d)/framework-ffmpeg-sources"
mkdir -p "$stage/downloads" "$stage/ports"
for port in ffmpeg x264 zlib; do
  cp -R "$VCPKG_ROOT/ports/$port" "$stage/ports/"
done
find "$VCPKG_ROOT/downloads" -maxdepth 1 -type f \
  \( -iname '*ffmpeg*' -o -iname '*x264*' -o -iname '*zlib*' -o -iname '*madler*' -o -iname 'mirror-x264*' \) \
  -exec cp {} "$stage/downloads/" \;
cp vcpkg.json "$stage/"
cp -R vcpkg-triplets "$stage/"
cat > "$stage/README.txt" <<TXT
Corresponding source for the FFmpeg, x264 and zlib libraries shipped with Framework.

vcpkg baseline: https://github.com/microsoft/vcpkg/tree/$baseline
downloads/   original upstream source archives, exactly as used for the build
ports/       vcpkg build scripts and patches (configure flags are in ports/ffmpeg/portfile.cmake)
vcpkg.json   selected features (avcodec avformat avfilter swresample swscale gpl x264 zlib)

To rebuild: clone vcpkg at the baseline above and run scripts/vcpkg-install.sh
from the Framework source tree for the matching release tag.
TXT
mkdir -p "$(dirname "$out")"
tar -C "$(dirname "$stage")" -czf "$out" "$(basename "$stage")"
ls -l "$out"
