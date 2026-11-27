#!/usr/bin/env bash
# Prints KEY=VALUE lines that let cargo find and load the vcpkg FFmpeg build.
# Usage:  eval "$(scripts/ffmpeg-env.sh | sed 's/^/export /')"
#    or:  scripts/ffmpeg-env.sh >> "$GITHUB_ENV"
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ -z "${FFMPEG_DIR:-}" ]]; then
  dirs=(vcpkg_installed/*-release)
  FFMPEG_DIR="$PWD/${dirs[0]}"
fi
echo "FFMPEG_DIR=$FFMPEG_DIR"
case "$(uname -s)" in
  Linux) echo "LD_LIBRARY_PATH=$FFMPEG_DIR/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" ;;
  Darwin) echo "DYLD_FALLBACK_LIBRARY_PATH=$FFMPEG_DIR/lib${DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}" ;;
  MINGW*|MSYS*|CYGWIN*) echo "PATH=$(cygpath -u "$FFMPEG_DIR")/bin:$PATH" ;;
esac
