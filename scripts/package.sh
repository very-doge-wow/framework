#!/usr/bin/env bash
# Packages the release binaries together with the FFmpeg shared libraries.
# Usage: scripts/package.sh <platform-name> <version>
# Writes `dir=` and `archive=` to $GITHUB_OUTPUT when running in Actions.
set -euo pipefail
cd "$(dirname "$0")/.."
platform="$1"
version="$2"
: "${FFMPEG_DIR:?set FFMPEG_DIR (see scripts/ffmpeg-env.sh)}"
name="framework-${version}-${platform}"
rm -rf dist && mkdir -p dist
docs=(LICENSE README.md THIRD_PARTY_NOTICES.md)

case "$(uname -s)" in
  Linux)
    dir="dist/$name"
    mkdir -p "$dir/lib"
    cp target/release/framework target/release/framework-cli "$dir/"
    cp -P "$FFMPEG_DIR"/lib/*.so* "$dir/lib/"
    for bin in "$dir/framework" "$dir/framework-cli"; do
      patchelf --set-rpath '$ORIGIN/lib' "$bin"
    done
    cp "${docs[@]}" "$dir/"
    archive="dist/$name.tar.gz"
    tar -C dist -czf "$archive" "$name"
    ;;
  Darwin)
    app="dist/$name/Framework.app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks" "$app/Contents/Resources"
    cp target/release/framework target/release/framework-cli "$app/Contents/MacOS/"
    cp -P "$FFMPEG_DIR"/lib/*.dylib "$app/Contents/Frameworks/"
    for bin in "$app/Contents/MacOS/framework" "$app/Contents/MacOS/framework-cli"; do
      install_name_tool -add_rpath "@executable_path/../Frameworks" "$bin"
    done
    sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
    cp "${docs[@]}" "dist/$name/"
    codesign --force --deep --sign - "$app"
    dir="$app/Contents/MacOS"
    archive="dist/$name.zip"
    (cd dist && ditto -c -k --keepParent "$name" "$name.zip")
    ;;
  MINGW*|MSYS*|CYGWIN*)
    dir="dist/$name"
    mkdir -p "$dir"
    cp target/release/framework.exe target/release/framework-cli.exe "$dir/"
    cp "$(cygpath -u "$FFMPEG_DIR")"/bin/*.dll "$dir/"
    cp "${docs[@]}" "$dir/"
    archive="dist/$name.zip"
    (cd dist && 7z a -tzip -bso0 "$name.zip" "$name")
    ;;
esac

echo "dir=$dir" | tee -a "${GITHUB_OUTPUT:-/dev/null}"
echo "archive=$archive" | tee -a "${GITHUB_OUTPUT:-/dev/null}"
