#!/usr/bin/env bash
# Builds the pinned FFmpeg + x264 (see vcpkg.json) into ./vcpkg_installed/<triplet>.
#
# Environment:
#   TRIPLET     vcpkg triplet (default: detected from the host)
#   VCPKG_ROOT  where to clone vcpkg (default: ../vcpkg next to the repository)
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ -z "${TRIPLET:-}" ]]; then
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) TRIPLET=x64-linux-dynamic-release ;;
    Linux-aarch64) TRIPLET=arm64-linux-dynamic-release ;;
    Darwin-arm64) TRIPLET=arm64-osx-dynamic-release ;;
    MINGW*|MSYS*|CYGWIN*)
      if [[ "${PROCESSOR_ARCHITECTURE:-}" == "ARM64" ]]; then TRIPLET=arm64-windows-release; else TRIPLET=x64-windows-release; fi ;;
    *) echo "unsupported host $(uname -s)-$(uname -m); set TRIPLET" >&2; exit 1 ;;
  esac
fi
VCPKG_ROOT="${VCPKG_ROOT:-$(cd .. && pwd)/vcpkg}"
baseline="$(sed -n 's/.*"builtin-baseline": *"\([0-9a-f]\{40\}\)".*/\1/p' vcpkg.json)"

if [[ ! -d "$VCPKG_ROOT/.git" ]]; then
  git clone --filter=blob:none https://github.com/microsoft/vcpkg "$VCPKG_ROOT"
fi
git -C "$VCPKG_ROOT" fetch --quiet origin "$baseline" 2>/dev/null || git -C "$VCPKG_ROOT" fetch --quiet origin
git -C "$VCPKG_ROOT" -c advice.detachedHead=false checkout --quiet "$baseline"

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    [[ -x "$VCPKG_ROOT/vcpkg.exe" ]] || "$VCPKG_ROOT/bootstrap-vcpkg.bat" -disableMetrics
    vcpkg="$VCPKG_ROOT/vcpkg.exe" ;;
  *)
    [[ -x "$VCPKG_ROOT/vcpkg" ]] || "$VCPKG_ROOT/bootstrap-vcpkg.sh" -disableMetrics
    vcpkg="$VCPKG_ROOT/vcpkg" ;;
esac

if [[ -n "${VCPKG_DEFAULT_BINARY_CACHE:-}" ]]; then
  mkdir -p "$VCPKG_DEFAULT_BINARY_CACHE"
fi

"$vcpkg" install \
  --triplet "$TRIPLET" \
  --host-triplet "$TRIPLET" \
  --overlay-triplets=vcpkg-triplets \
  --x-install-root="$PWD/vcpkg_installed" \
  --disable-metrics

echo "FFmpeg installed to $PWD/vcpkg_installed/$TRIPLET"
