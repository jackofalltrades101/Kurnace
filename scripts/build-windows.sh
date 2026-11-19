#!/bin/sh
# Cross-build a portable Windows x64 app on macOS with cargo-xwin.
# Produces dist/windows/Kurnace/ and Kurnace-<version>-windows-x64.zip.
#
# Requires: rustup target x86_64-pc-windows-msvc, cargo-xwin, LLVM + LLD
# (`brew install llvm lld`).
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
TARGET=x86_64-pc-windows-msvc
VERSION=0.0.1
DEPS="$ROOT/target/windows-deps"
STAGE="$ROOT/dist/windows/Kurnace"
OUT="$ROOT/dist/windows/Kurnace-$VERSION-windows-x64.zip"
LLVM_BIN=${LLVM_BIN:-$(brew --prefix llvm 2>/dev/null)/bin}
LLD_BIN=${LLD_BIN:-$(brew --prefix lld 2>/dev/null)/bin}
export PATH="$LLVM_BIN:$LLD_BIN:$PATH"

rustup target list --installed | grep -qx "$TARGET" || rustup target add "$TARGET"
command -v cargo-xwin >/dev/null || { echo "install cargo-xwin: cargo install cargo-xwin"; exit 1; }
command -v llvm-lib >/dev/null || { echo "llvm-lib missing: brew install llvm"; exit 1; }
command -v lld-link >/dev/null || { echo "lld-link missing: brew install lld"; exit 1; }

echo "== build frontend"
pnpm -C app build

echo "== build Windows core + app"
cargo xwin build --release -p kurn --target "$TARGET"
cargo xwin build --release -p kurnace-app --target "$TARGET"

echo "== stage"
rm -rf "$STAGE"
mkdir -p "$STAGE" "$DEPS"
cp "target/$TARGET/release/kurn.exe" "$STAGE/kurn.exe"
cp "target/$TARGET/release/kurnace-app.exe" "$STAGE/Kurnace.exe"
rsync -aL --exclude '.vite' --exclude 'dist' --exclude '*.log' --exclude '.DS_Store' \
  modules/ "$STAGE/modules/"

fetch_unzip() {
  name=$1 url=$2
  if [ ! -f "$DEPS/$name.zip" ]; then
    echo "   download $name"
    curl -L --fail --retry 3 -o "$DEPS/$name.zip" "$url"
  fi
  rm -rf "$DEPS/$name" && mkdir -p "$DEPS/$name"
  unzip -q "$DEPS/$name.zip" -d "$DEPS/$name"
}

echo "== node"
fetch_unzip node https://nodejs.org/dist/v24.14.0/node-v24.14.0-win-x64.zip
cp "$(find "$DEPS/node" -name node.exe | head -1)" "$STAGE/node.exe"

echo "== python"
fetch_unzip python https://www.python.org/ftp/python/3.12.8/python-3.12.8-embed-amd64.zip
mkdir -p "$STAGE/python"
cp -R "$DEPS/python"/* "$STAGE/python/"

echo "== ffmpeg"
fetch_unzip ffmpeg https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip
cp "$(find "$DEPS/ffmpeg" -name ffmpeg.exe | head -1)" "$STAGE/ffmpeg.exe"
cp "$(find "$DEPS/ffmpeg" -name ffprobe.exe | head -1)" "$STAGE/ffprobe.exe"

echo "== Chromium"
mkdir -p "$STAGE/puppeteer"
PUPPETEER_CACHE_DIR="$STAGE/puppeteer" modules/three.render/node_modules/.bin/puppeteer \
  browsers install chrome --platform win64 >/dev/null
PUPPETEER_CACHE_DIR="$STAGE/puppeteer" modules/three.render/node_modules/.bin/puppeteer \
  browsers install chrome-headless-shell --platform win64 >/dev/null

cat > "$STAGE/README-WINDOWS.txt" <<'EOF'
Kurnace for Windows (portable)

1. Install Microsoft Edge WebView2 Runtime if Windows 10 does not already have it:
   https://developer.microsoft.com/microsoft-edge/webview2/
2. Run Kurnace.exe.
3. ffmpeg, Node, Python and headless Chromium are bundled next to the app.
4. Godot projects additionally need Godot 4.7.2 .NET and the .NET SDK installed.

The app stores projects in %USERPROFILE%\Kurnace\projects and config in
%USERPROFILE%\.kurnace. The kurn.exe CLI is in this folder; set KURN_ACTOR when
using it as an agent.
EOF

echo "== zip"
rm -f "$OUT"
(cd "$ROOT/dist/windows" && zip -q -r -y "$OUT" Kurnace)

if command -v makensis >/dev/null 2>&1; then
  echo "== installer"
  SETUP="$ROOT/dist/windows/Kurnace-Setup-$VERSION.exe"
  rm -f "$SETUP"
  makensis -V2 -DSTAGE="$STAGE" -DOUTFILE="$SETUP" -DKURN_VERSION="$VERSION" \
    "$ROOT/installer/windows.nsi"
  ls -lh "$SETUP"
else
  echo "makensis not found; skipped installer (brew install nsis)"
fi
ls -lh "$OUT"
