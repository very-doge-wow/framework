# Installing a release

Every release on [GitHub Releases](https://github.com/very-doge-wow/framework/releases)
has one package per platform, named `framework-<version>-<platform>`:

| Platform | Asset |
|---|---|
| macOS, Apple silicon | `framework-<version>-macos-arm64.zip` (contains `Framework.app`) |
| Linux x86_64 / arm64 | `framework-<version>-linux-x86_64.tar.gz` / `-linux-arm64.tar.gz` |
| Windows x86_64 / arm64 | `framework-<version>-windows-x86_64.zip` / `-windows-arm64.zip` |

The packages include FFmpeg and x264. The matching source code is the
`framework-<version>-ffmpeg-x264-sources.tar.gz` asset.

The commands below download the **latest** release through the GitHub API.
To get a specific version, replace `latest` in the API URL with `tags/v1.2.3`.
With the [GitHub CLI](https://cli.github.com) you can instead run
`gh release download -R very-doge-wow/framework -p '*<platform>*'`.

## macOS (Apple silicon)

```sh
( set -e
  url=$(curl -fsSL https://api.github.com/repos/very-doge-wow/framework/releases/latest \
    | grep -o '"browser_download_url": *"[^"]*macos-arm64\.zip"' | cut -d'"' -f4)
  tmp=$(mktemp -d)
  curl -fL "$url" -o "$tmp/framework.zip"
  ditto -x -k "$tmp/framework.zip" "$tmp/x"
  rm -rf /Applications/Framework.app
  mv "$tmp"/x/framework-*/Framework.app /Applications/
  rm -rf "$tmp"
  open /Applications/Framework.app )
```

The commands run in a subshell with `set -e`, so a failed download stops
before anything is installed, and they use a private temporary directory.

The app is ad-hoc signed, not notarised. If you downloaded it with a browser
instead, macOS quarantines it; allow it once with
`xattr -dr com.apple.quarantine /Applications/Framework.app`.
The CLI is at `/Applications/Framework.app/Contents/MacOS/framework-cli`.

## Linux (x86_64 and arm64)

```sh
arch=$(uname -m | sed 's/aarch64/arm64/')
url=$(curl -fsSL https://api.github.com/repos/very-doge-wow/framework/releases/latest \
  | grep -o "\"browser_download_url\": *\"[^\"]*linux-$arch\.tar\.gz\"" | cut -d'"' -f4)
mkdir -p ~/.local/opt ~/.local/bin
rm -rf ~/.local/opt/framework && mkdir ~/.local/opt/framework
curl -fL "$url" | tar -xz -C ~/.local/opt/framework --strip-components=1
ln -sf ~/.local/opt/framework/framework ~/.local/opt/framework/framework-cli ~/.local/bin/
framework
```

Requires glibc 2.39 or newer (Ubuntu 24.04, Debian 13, Fedora 40 or later), because
the packages are built on Ubuntu 24.04. `~/.local/bin` must be on your `PATH`. The binaries find the bundled FFmpeg
libraries in `lib/` next to them. Audio playback needs ALSA (`libasound2`),
which every desktop distribution installs.

## Windows (x86_64 and arm64, PowerShell)

```powershell
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x86_64' }
$release = Invoke-RestMethod https://api.github.com/repos/very-doge-wow/framework/releases/latest
$asset = $release.assets | Where-Object name -like "*windows-$arch.zip"
Invoke-WebRequest $asset.browser_download_url -OutFile "$env:TEMP\framework.zip"
$dest = "$env:LOCALAPPDATA\Programs\Framework"
Remove-Item $dest -Recurse -Force -ErrorAction SilentlyContinue
Expand-Archive "$env:TEMP\framework.zip" "$env:TEMP\framework" -Force
Move-Item (Get-ChildItem "$env:TEMP\framework\framework-*")[0].FullName $dest
Remove-Item "$env:TEMP\framework" -Recurse
& "$dest\framework.exe"
```

The binaries are not code-signed yet, so SmartScreen may ask for confirmation
on first launch ("More info" → "Run anyway").

## Updating

Run the same commands again; they replace the previous installation. An
in-app updater is planned in [#27](https://github.com/very-doge-wow/framework/issues/27).
