#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT_DIR="$ROOT_DIR/packaging/dist"
BUILD_DIR="$ROOT_DIR/packaging/build"
BOX_DIR="$OUT_DIR/KaraokeGenerator-macos"
APP_PATH="$OUT_DIR/Karaoke Generator.app"
DMG_ROOT="$BUILD_DIR/app-dmg-root"
DMG_NAME="${DMG_NAME:-KaraokeGenerator-macOS-$(uname -m)-app.dmg}"
DMG_PATH="$OUT_DIR/$DMG_NAME"
ENTITLEMENTS="$BUILD_DIR/app-entitlements.plist"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"
NOTARY_PROFILE="${NOTARY_PROFILE:-}"
APP_VERSION="$(cd "$ROOT_DIR/desktop_app" && cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')"

if [[ "${SKIP_BUILD_BOX:-0}" != "1" ]]; then
  "$ROOT_DIR/packaging/macos/build_box.sh"
elif [[ ! -x "$BOX_DIR/Karaoke Generator" ]]; then
  echo "Portable macOS box not found. Run packaging/macos/build_box.sh first." >&2
  exit 1
fi

rm -rf "$APP_PATH" "$DMG_ROOT" "$DMG_PATH"
mkdir -p \
  "$APP_PATH/Contents/MacOS" \
  "$APP_PATH/Contents/Resources/worker" \
  "$APP_PATH/Contents/Resources/bin" \
  "$APP_PATH/Contents/Resources/assets" \
  "$DMG_ROOT"

cp "$BOX_DIR/Karaoke Generator" "$APP_PATH/Contents/MacOS/Karaoke Generator"
cp -R "$BOX_DIR/worker/"* "$APP_PATH/Contents/Resources/worker/"
cp -R "$BOX_DIR/bin/"* "$APP_PATH/Contents/Resources/bin/"
cp -R "$BOX_DIR/assets/"* "$APP_PATH/Contents/Resources/assets/"
cp "$BOX_DIR/README.txt" "$APP_PATH/Contents/Resources/README.txt"

chmod +x "$APP_PATH/Contents/MacOS/Karaoke Generator"
chmod +x "$APP_PATH/Contents/Resources/worker/karaoke_worker"
chmod +x "$APP_PATH/Contents/Resources/worker/karaoke_render"
chmod +x "$APP_PATH/Contents/Resources/bin/ffmpeg"
chmod +x "$APP_PATH/Contents/Resources/bin/ffprobe"

cat > "$APP_PATH/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>Karaoke Generator</string>
  <key>CFBundleIdentifier</key><string>com.mikhailsokolenko.karaokegenerator</string>
  <key>CFBundleName</key><string>Karaoke Generator</string>
  <key>CFBundleDisplayName</key><string>Karaoke Generator</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${APP_VERSION}</string>
  <key>CFBundleVersion</key><string>${APP_VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF

cat > "$ENTITLEMENTS" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>com.apple.security.cs.disable-library-validation</key><true/>
  <key>com.apple.security.cs.allow-jit</key><true/>
  <key>com.apple.security.cs.allow-unsigned-executable-memory</key><true/>
</dict>
</plist>
EOF

sign_file() {
  local item="$1"
  [[ -f "$item" ]] || return 0
  chmod u+w "$item" 2>/dev/null || true
  codesign --remove-signature "$item" >/dev/null 2>&1 || true
  if [[ "$SIGN_IDENTITY" == "-" ]]; then
    codesign --force --options runtime --entitlements "$ENTITLEMENTS" --sign - "$item"
  else
    codesign --force --timestamp --options runtime --entitlements "$ENTITLEMENTS" --sign "$SIGN_IDENTITY" "$item"
  fi
}

while IFS= read -r -d '' item; do
  sign_file "$item"
done < <(find "$APP_PATH/Contents/Resources" -type f \( -name "*.dylib" -o -name "*.so" -o -name "*.framework" \) -print0)

while IFS= read -r -d '' item; do
  if file "$item" | grep -q "Mach-O"; then
    sign_file "$item"
  fi
done < <(find "$APP_PATH/Contents/Resources" -type f -perm -111 -print0)

sign_file "$APP_PATH/Contents/Resources/bin/ffmpeg"
sign_file "$APP_PATH/Contents/Resources/bin/ffprobe"
sign_file "$APP_PATH/Contents/Resources/worker/karaoke_worker"
sign_file "$APP_PATH/Contents/Resources/worker/karaoke_render"
if [[ "$SIGN_IDENTITY" == "-" ]]; then
  codesign --force --deep --options runtime --entitlements "$ENTITLEMENTS" --sign - "$APP_PATH"
else
  codesign --force --deep --timestamp --options runtime --entitlements "$ENTITLEMENTS" --sign "$SIGN_IDENTITY" "$APP_PATH"
fi

codesign --verify --deep --strict --verbose=4 "$APP_PATH"
spctl -a -t exec -vv "$APP_PATH" || true

cat > "$DMG_ROOT/README - запуск.txt" <<'EOF'
Karaoke Generator

Скопируйте Karaoke Generator.app в Программы или на Рабочий стол и запустите двойным кликом.
Первый запуск генерации может быть долгим: приложение скачивает выбранную модель Whisper.
EOF

ditto "$APP_PATH" "$DMG_ROOT/Karaoke Generator.app"
xattr -cr "$DMG_ROOT" || true

hdiutil create \
  -volname "Karaoke Generator" \
  -srcfolder "$DMG_ROOT" \
  -ov \
  -format UDZO \
  "$DMG_PATH"

if [[ "$SIGN_IDENTITY" != "-" ]]; then
  codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG_PATH"
  if [[ -n "$NOTARY_PROFILE" ]]; then
    xcrun notarytool submit "$DMG_PATH" --keychain-profile "$NOTARY_PROFILE" --wait
    xcrun stapler staple "$DMG_PATH"
    xcrun stapler validate "$DMG_PATH"
    spctl -a -t open --context context:primary-signature -vv "$DMG_PATH"
  fi
fi

echo "$DMG_PATH"
