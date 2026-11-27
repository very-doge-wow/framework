#!/usr/bin/env bash
# Verifies a packaged build runs without the build-time library paths,
# i.e. that the bundled FFmpeg libraries are found and work end to end.
set -euo pipefail
dir="$1"
unset LD_LIBRARY_PATH DYLD_FALLBACK_LIBRARY_PATH FFMPEG_DIR
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    cli="$dir/framework-cli.exe"
    PATH="/usr/bin:/c/Windows/System32:/c/Windows" ;;
  *) cli="$dir/framework-cli" ;;
esac
work="$(mktemp -d)"
"$cli" --version
"$cli" profiles
"$cli" self-test "$work"
echo "smoke test passed"
