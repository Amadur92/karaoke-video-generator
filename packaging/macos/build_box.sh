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
cargo build --release
TARGET_DIR="$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"

rm -rf "$BOX_DIR"
mkdir -p "$BOX_DIR/worker" "$BOX_DIR/bin/lib" "$BOX_DIR/assets"

cp "$TARGET_DIR/release/desktop_app" "$BOX_DIR/Karaoke Generator"
cp "$TARGET_DIR/release/karaoke_render" "$BOX_DIR/worker/karaoke_render"
cp -R "$WORKER_DIST/"* "$BOX_DIR/worker/"
cp "$ROOT_DIR/desktop_app/assets/Montserrat-Regular.ttf" "$BOX_DIR/assets/Montserrat-Regular.ttf"
cp "$ROOT_DIR/desktop_app/assets/Montserrat-Bold.ttf" "$BOX_DIR/assets/Montserrat-Bold.ttf"
cp "$FFMPEG_BIN" "$BOX_DIR/bin/ffmpeg"
cp "$FFPROBE_BIN" "$BOX_DIR/bin/ffprobe"

chmod +x "$BOX_DIR/Karaoke Generator" "$BOX_DIR/worker/karaoke_worker" "$BOX_DIR/worker/karaoke_render" "$BOX_DIR/bin/ffmpeg" "$BOX_DIR/bin/ffprobe"

is_system_dylib() {
  case "$1" in
    /usr/lib/*|/System/Library/*) return 0 ;;
    *) return 1 ;;
  esac
}

copy_dylib_deps() {
  local binary="$1"
  local dep

  otool -L "$binary" | tail -n +2 | awk '{print $1}' | while read -r dep; do
    if [[ -z "$dep" || "$dep" == "$binary" || "$dep" == @* ]] || is_system_dylib "$dep"; then
      continue
    fi

    if [[ -f "$dep" ]]; then
      local dest="$BOX_DIR/bin/lib/$(basename "$dep")"
      if [[ ! -f "$dest" ]]; then
        cp "$dep" "$dest"
        chmod +w "$dest"
        copy_dylib_deps "$dest"
      fi
    fi
  done
}

rewrite_dylib_deps() {
  local binary="$1"
  local loader_prefix="$2"
  local dep

  otool -L "$binary" | tail -n +2 | awk '{print $1}' | while read -r dep; do
    if [[ -z "$dep" || "$dep" == "$binary" || "$dep" == @* ]] || is_system_dylib "$dep"; then
      continue
    fi

    if [[ -f "$BOX_DIR/bin/lib/$(basename "$dep")" ]]; then
      install_name_tool -change "$dep" "$loader_prefix/$(basename "$dep")" "$binary" || true
    fi
  done
}

copy_dylib_deps "$BOX_DIR/bin/ffmpeg"
copy_dylib_deps "$BOX_DIR/bin/ffprobe"

for dylib in "$BOX_DIR"/bin/lib/*.dylib; do
  [[ -e "$dylib" ]] || continue
  install_name_tool -id "@rpath/$(basename "$dylib")" "$dylib" || true
  rewrite_dylib_deps "$dylib" "@loader_path"
done

rewrite_dylib_deps "$BOX_DIR/bin/ffmpeg" "@executable_path/lib"
rewrite_dylib_deps "$BOX_DIR/bin/ffprobe" "@executable_path/lib"

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
