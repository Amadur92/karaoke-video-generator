#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BOX_DIR="$ROOT_DIR/packaging/dist/KaraokeGenerator-macos"
WORKER_DIST="$($ROOT_DIR/packaging/macos/build_worker.sh | tail -n 1)"

FFMPEG_BIN="${FFMPEG_BIN:-$(command -v ffmpeg || true)}"
FFPROBE_BIN="${FFPROBE_BIN:-$(command -v ffprobe || true)}"

if [[ -z "$FFMPEG_BIN" || -z "$FFPROBE_BIN" ]]; then
  echo "ffmpeg and ffprobe must be available in PATH on the build machine." >&2
  exit 1
fi

cd "$ROOT_DIR/desktop_app"
TARGET_DIR="$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
APP_VERSION="$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')"
rm -f "$TARGET_DIR/release/desktop_app" "$TARGET_DIR/release/karaoke_render"
cargo build --release
BUILT_VERSION="$("$TARGET_DIR/release/desktop_app" --version)"
if [[ "$BUILT_VERSION" != "$APP_VERSION" ]]; then
  echo "Built desktop_app reports version '$BUILT_VERSION', expected '$APP_VERSION'." >&2
  exit 1
fi

rm -rf "$BOX_DIR"
mkdir -p "$BOX_DIR/worker" "$BOX_DIR/bin/lib" "$BOX_DIR/assets"

cp "$TARGET_DIR/release/desktop_app" "$BOX_DIR/Karaoke Generator"
cp "$TARGET_DIR/release/karaoke_render" "$BOX_DIR/worker/karaoke_render"
cp -R "$WORKER_DIST/"* "$BOX_DIR/worker/"
cp "$ROOT_DIR/desktop_app/assets/Montserrat-Regular.ttf" "$BOX_DIR/assets/Montserrat-Regular.ttf"
cp "$ROOT_DIR/desktop_app/assets/Montserrat-Bold.ttf" "$BOX_DIR/assets/Montserrat-Bold.ttf"
cp "$ROOT_DIR/desktop_app/assets/Montserrat-Black.ttf" "$BOX_DIR/assets/Montserrat-Black.ttf"
cp "$FFMPEG_BIN" "$BOX_DIR/bin/ffmpeg"
cp "$FFPROBE_BIN" "$BOX_DIR/bin/ffprobe"

# PyInstaller кладёт worker/_internal/Python как симлинк на
# Python.framework/Versions/3.11/Python. Симлинки хрупки: сторонние обёртки,
# cp без -R и часть архиваторов при копировании/распаковке ломают их, и тогда
# воркер падает с "[PYI] Failed to load Python shared library: no such file".
# Заменяем симлинк реальным файлом — надёжнее для любого downstream-потребителя.
PY_LINK="$BOX_DIR/worker/_internal/Python"
if [[ -L "$PY_LINK" ]]; then
  PY_REAL="$(readlink -f "$PY_LINK")"
  if [[ -f "$PY_REAL" ]]; then
    rm "$PY_LINK"
    cp "$PY_REAL" "$PY_LINK"
    chmod +x "$PY_LINK"
  fi
fi

chmod +x "$BOX_DIR/Karaoke Generator" "$BOX_DIR/worker/karaoke_worker" "$BOX_DIR/worker/karaoke_render" "$BOX_DIR/bin/ffmpeg" "$BOX_DIR/bin/ffprobe"

python3 "$ROOT_DIR/packaging/macos/bundle_dylibs.py" \
  --ffmpeg "$FFMPEG_BIN" \
  --ffprobe "$FFPROBE_BIN" \
  --bin-dir "$BOX_DIR/bin"

if command -v codesign >/dev/null 2>&1; then
  find "$BOX_DIR" \( -name "*.dylib" -o -name "*.so" -o -name "*.framework" \) -print0 |
    while IFS= read -r -d '' item; do
      codesign --force --sign - "$item" >/dev/null 2>&1 || true
    done

  codesign --force --deep --sign - "$BOX_DIR/worker/karaoke_worker" >/dev/null 2>&1 || true
  codesign --force --deep --sign - "$BOX_DIR/worker/karaoke_render" >/dev/null 2>&1 || true
  codesign --force --deep --sign - "$BOX_DIR/Karaoke Generator" >/dev/null 2>&1 || true
  codesign --force --sign - "$BOX_DIR/bin/ffmpeg" >/dev/null 2>&1 || true
  codesign --force --sign - "$BOX_DIR/bin/ffprobe" >/dev/null 2>&1 || true
fi

cat > "$BOX_DIR/README.txt" <<'EOF'
Karaoke Generator

Run:
  ./Karaoke Generator

If macOS says a bundled Python.framework is damaged, run:
  xattr -dr com.apple.quarantine .

The first generation can take longer because the selected Whisper model is
downloaded into your user cache.

Generated videos are saved to:
  ./exports
EOF

echo "Box created: $BOX_DIR"
