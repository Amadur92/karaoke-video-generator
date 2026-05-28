#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BOX_DIR="$ROOT_DIR/packaging/dist/KaraokeGenerator-macos"
WORKER_DIST="$($ROOT_DIR/packaging/macos/build_worker.sh | tail -n 1)"

FFMPEG_BIN="$(command -v ffmpeg || true)"
FFPROBE_BIN="$(command -v ffprobe || true)"

if [[ -z "$FFMPEG_BIN" || -z "$FFPROBE_BIN" ]]; then
  echo "ffmpeg and ffprobe must be available in PATH on the build machine." >&2
  exit 1
fi

cd "$ROOT_DIR/desktop_app"
cargo build --release
TARGET_DIR="$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"

rm -rf "$BOX_DIR"
mkdir -p "$BOX_DIR/worker" "$BOX_DIR/bin"

cp "$TARGET_DIR/release/desktop_app" "$BOX_DIR/Karaoke Generator"
cp -R "$WORKER_DIST/"* "$BOX_DIR/worker/"
cp "$FFMPEG_BIN" "$BOX_DIR/bin/ffmpeg"
cp "$FFPROBE_BIN" "$BOX_DIR/bin/ffprobe"

chmod +x "$BOX_DIR/Karaoke Generator" "$BOX_DIR/worker/karaoke_worker" "$BOX_DIR/bin/ffmpeg" "$BOX_DIR/bin/ffprobe"

if command -v codesign >/dev/null 2>&1; then
  find "$BOX_DIR" \( -name "*.dylib" -o -name "*.so" -o -name "*.framework" \) -print0 |
    while IFS= read -r -d '' item; do
      codesign --force --sign - "$item" >/dev/null 2>&1 || true
    done

  codesign --force --deep --sign - "$BOX_DIR/worker/karaoke_worker" >/dev/null 2>&1 || true
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
EOF

echo "Box created: $BOX_DIR"
