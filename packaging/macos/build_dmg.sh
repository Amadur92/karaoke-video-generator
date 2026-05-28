#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BOX_DIR="$ROOT_DIR/packaging/dist/KaraokeGenerator-macos"
OUT_DIR="$ROOT_DIR/packaging/dist"
DMG_ROOT="$ROOT_DIR/packaging/build/dmg-root"
DMG_PATH="$OUT_DIR/KaraokeGenerator-macOS-AppleSilicon-arm64-portable.dmg"

if [[ ! -x "$BOX_DIR/Karaoke Generator" ]]; then
  echo "Portable macOS box not found. Run packaging/macos/build_box.sh first." >&2
  exit 1
fi

rm -rf "$DMG_ROOT" "$DMG_PATH"
mkdir -p "$DMG_ROOT"

ditto "$BOX_DIR" "$DMG_ROOT/KaraokeGenerator-macos"
rm -rf "$DMG_ROOT/KaraokeGenerator-macos/exports"
rm -rf "$DMG_ROOT/KaraokeGenerator-macos/uploads"

cat > "$DMG_ROOT/Fix Karaoke.command" <<'EOF'
#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
APP_DIR="$SCRIPT_DIR/KaraokeGenerator-macos"

if [ ! -d "$APP_DIR" ]; then
  APP_DIR="$(find "$HOME/Downloads" "$HOME/Desktop" -maxdepth 3 -type d -name "KaraokeGenerator-macos" 2>/dev/null | head -n 1)"
fi

if [ -z "$APP_DIR" ] || [ ! -d "$APP_DIR" ]; then
  osascript -e 'display dialog "Не нашла папку KaraokeGenerator-macos. Скопируйте ее из DMG в Загрузки или на Рабочий стол и запустите Fix Karaoke.command снова." buttons {"OK"}'
  exit 1
fi

xattr -dr com.apple.quarantine "$APP_DIR" 2>/dev/null || true
chmod +x "$APP_DIR/Karaoke Generator" 2>/dev/null || true
chmod +x "$APP_DIR/bin/ffmpeg" "$APP_DIR/bin/ffprobe" "$APP_DIR/worker/karaoke_worker" 2>/dev/null || true

open "$APP_DIR/Karaoke Generator"
EOF
chmod +x "$DMG_ROOT/Fix Karaoke.command"

cat > "$DMG_ROOT/README - запуск.txt" <<'EOF'
Karaoke Generator

1. Скопируйте папку KaraokeGenerator-macos из этого окна в Загрузки или на Рабочий стол.
2. Откройте папку KaraokeGenerator-macos.
3. Запустите Karaoke Generator.

Если macOS пишет, что приложение повреждено:

1. Запустите Fix Karaoke.command.
2. Если macOS спросит разрешение для Терминала, разрешите.
3. Попробуйте открыть Karaoke Generator снова.

Первый запуск генерации может быть долгим: приложение скачивает выбранную модель Whisper.
Готовые видео сохраняются в папку exports рядом с приложением.
EOF

xattr -cr "$DMG_ROOT" || true

hdiutil create \
  -volname "Karaoke Generator" \
  -srcfolder "$DMG_ROOT" \
  -ov \
  -format UDZO \
  "$DMG_PATH"

echo "$DMG_PATH"
