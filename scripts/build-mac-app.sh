#!/bin/sh
# Build a distributable Kurnace.app: core sidecar, modules, Node and the
# headless Chromium cache, then ad-hoc sign and create a DMG.
#
# Prerequisites for full feature use: ffmpeg/ffprobe and Python 3 on PATH.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
APP=app/src-tauri
TRIPLE=$(rustc -vV | sed -n 's/^host: //p')

echo "== build core"
cargo build --release -p kurn

echo "== stage core sidecar"
mkdir -p "$APP/binaries"
cp -f target/release/kurn "$APP/binaries/kurn-$TRIPLE"

echo "== stage modules"
rm -rf "$APP/resources/modules"
mkdir -p "$APP/resources"
# -L dereferences pnpm's workspace symlinks so the bundle is self-contained.
rsync -aL --exclude '.vite' --exclude 'dist' --exclude '*.log' --exclude '.DS_Store' \
  modules/ "$APP/resources/modules/"

echo "== stage node"
mkdir -p "$APP/resources/bin"
if command -v node >/dev/null 2>&1; then
  cp -L "$(command -v node)" "$APP/resources/bin/node"
fi

echo "== stage Chromium"
rm -rf "$APP/resources/puppeteer"
mkdir -p "$APP/resources/puppeteer"
if [ -d "$HOME/.cache/puppeteer" ]; then
  rsync -a "$HOME/.cache/puppeteer/" "$APP/resources/puppeteer/"
fi

echo "== build app"
pnpm -C app tauri build --config src-tauri/tauri.prod.conf.json --bundles app

APP_BUNDLE="$ROOT/target/release/bundle/macos/Kurnace.app"
if [ -d "$APP_BUNDLE" ]; then
  echo "== ad-hoc sign"
  codesign --force --deep --sign - "$APP_BUNDLE" >/dev/null 2>&1 || true

  echo "== dmg"
  DMG_DIR="$ROOT/target/release/bundle/dmg"
  mkdir -p "$DMG_DIR"
  STAGE=$(mktemp -d -t kurn-dmg)
  cp -R "$APP_BUNDLE" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  DMG="$DMG_DIR/Kurnace_0.0.1_aarch64.dmg"
  rm -f "$DMG"
  hdiutil create -quiet -volname Kurnace -srcfolder "$STAGE" -ov -format UDZO "$DMG"
  rm -rf "$STAGE"
  echo "built $APP_BUNDLE"
  echo "built $DMG"
fi
