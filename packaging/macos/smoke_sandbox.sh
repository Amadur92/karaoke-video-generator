#!/usr/bin/env bash
set -euo pipefail

APP_DIR="${1:-packaging/dist/KaraokeGenerator-macos}"
MODE="${2:-launch}"
VIDEO_PATH="${3:-}"

if [[ ! -d "$APP_DIR" ]]; then
  echo "Portable app folder not found: $APP_DIR" >&2
  echo "Usage: $0 path/to/KaraokeGenerator-macos" >&2
  exit 1
fi

APP_DIR="$(cd "$APP_DIR" && pwd)"
APP_BIN="$APP_DIR/Karaoke Generator"
WORKER_BIN="$APP_DIR/worker/karaoke_worker"
FFMPEG_BIN="$APP_DIR/bin/ffmpeg"
FFPROBE_BIN="$APP_DIR/bin/ffprobe"
SANDBOX_HOME="$(mktemp -d "${TMPDIR:-/tmp}/karaoke-empty-home.XXXXXX")"
SANDBOX_CACHE="$SANDBOX_HOME/.cache"

cleanup() {
  echo
  echo "Sandbox HOME was:"
  echo "  $SANDBOX_HOME"
  echo "Remove it when done:"
  echo "  rm -rf \"$SANDBOX_HOME\""
}
trap cleanup EXIT

echo "Portable app:"
echo "  $APP_DIR"
echo
echo "Sandbox HOME:"
echo "  $SANDBOX_HOME"
echo

echo "Clearing quarantine on portable folder..."
xattr -dr com.apple.quarantine "$APP_DIR" 2>/dev/null || true

echo "Checking bundled executables..."
test -x "$APP_BIN"
test -x "$WORKER_BIN"
test -x "$FFMPEG_BIN"
test -x "$FFPROBE_BIN"

echo "Checking ffprobe dynamic libraries..."
if otool -L "$FFPROBE_BIN" | grep -E '/opt/homebrew|/usr/local/Cellar|/opt/local' >/dev/null; then
  echo "ERROR: ffprobe still links to Homebrew/MacPorts libraries:" >&2
  otool -L "$FFPROBE_BIN" | grep -E '/opt/homebrew|/usr/local/Cellar|/opt/local' >&2
  exit 1
fi

echo "Checking ffmpeg dynamic libraries..."
if otool -L "$FFMPEG_BIN" | grep -E '/opt/homebrew|/usr/local/Cellar|/opt/local' >/dev/null; then
  echo "ERROR: ffmpeg still links to Homebrew/MacPorts libraries:" >&2
  otool -L "$FFMPEG_BIN" | grep -E '/opt/homebrew|/usr/local/Cellar|/opt/local' >&2
  exit 1
fi

echo "Running bundled ffprobe..."
env -i \
  HOME="$SANDBOX_HOME" \
  XDG_CACHE_HOME="$SANDBOX_CACHE" \
  PATH="/usr/bin:/bin:/usr/sbin:/sbin" \
  "$FFPROBE_BIN" -version >/dev/null

echo "Running bundled ffmpeg..."
env -i \
  HOME="$SANDBOX_HOME" \
  XDG_CACHE_HOME="$SANDBOX_CACHE" \
  PATH="/usr/bin:/bin:/usr/sbin:/sbin" \
  "$FFMPEG_BIN" -version >/dev/null

echo "Running bundled worker help..."
env -i \
  HOME="$SANDBOX_HOME" \
  XDG_CACHE_HOME="$SANDBOX_CACHE" \
  PATH="$APP_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  PYTHONUTF8=1 \
  "$WORKER_BIN" --help >/dev/null || true

if [[ "$MODE" == "--preview-check" ]]; then
  if [[ -z "$VIDEO_PATH" ]]; then
    VIDEO_PATH="$(find "$APP_DIR/exports" -maxdepth 1 -type f -name "*.mp4" 2>/dev/null | sort | tail -n 1 || true)"
  fi

  if [[ -z "$VIDEO_PATH" || ! -f "$VIDEO_PATH" ]]; then
    echo "ERROR: no MP4 found for preview check." >&2
    echo "Usage:" >&2
    echo "  $0 $APP_DIR --preview-check path/to/video.mp4" >&2
    echo "Or put an MP4 in:" >&2
    echo "  $APP_DIR/exports" >&2
    exit 1
  fi

  VIDEO_PATH="$(cd "$(dirname "$VIDEO_PATH")" && pwd)/$(basename "$VIDEO_PATH")"
  PREVIEW_DIR="$SANDBOX_HOME/preview-check"
  mkdir -p "$PREVIEW_DIR"

  echo
  echo "Checking generated video preview pipeline:"
  echo "  $VIDEO_PATH"

  echo "Reading video dimensions with bundled ffprobe..."
  env -i \
    HOME="$SANDBOX_HOME" \
    XDG_CACHE_HOME="$SANDBOX_CACHE" \
    PATH="$APP_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
    "$FFPROBE_BIN" \
      -v error \
      -select_streams v:0 \
      -show_entries stream=width,height \
      -of csv=s=x:p=0 \
      "$VIDEO_PATH"

  echo "Decoding one RGBA frame with bundled ffmpeg..."
  env -i \
    HOME="$SANDBOX_HOME" \
    XDG_CACHE_HOME="$SANDBOX_CACHE" \
    PATH="$APP_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
    "$FFMPEG_BIN" \
      -v error \
      -i "$VIDEO_PATH" \
      -frames:v 1 \
      -pix_fmt rgba \
      -f rawvideo \
      "$PREVIEW_DIR/frame.rgba"

  test -s "$PREVIEW_DIR/frame.rgba"

  echo "Extracting preview WAV audio with bundled ffmpeg..."
  env -i \
    HOME="$SANDBOX_HOME" \
    XDG_CACHE_HOME="$SANDBOX_CACHE" \
    PATH="$APP_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
    "$FFMPEG_BIN" \
      -v error \
      -y \
      -i "$VIDEO_PATH" \
      -vn \
      -acodec pcm_s16le \
      -ar 44100 \
      -ac 2 \
      "$PREVIEW_DIR/preview.wav"

  test -s "$PREVIEW_DIR/preview.wav"

  echo
  echo "Video preview pipeline check passed."
  exit 0
fi

echo
echo "Static smoke checks passed."
echo
echo "Whisper cache for this sandbox will be:"
echo "  $SANDBOX_CACHE/whisper"
echo

if [[ "$MODE" == "--check-only" ]]; then
  exit 0
fi

echo "Launching app with empty HOME and minimal PATH..."
echo "Close the app when finished. Generated videos should go to:"
echo "  $APP_DIR/exports"
echo

env -i \
  HOME="$SANDBOX_HOME" \
  XDG_CACHE_HOME="$SANDBOX_CACHE" \
  PATH="$APP_DIR/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  PYTHONUTF8=1 \
  "$APP_BIN" &

APP_PID="$!"
sleep 2

osascript <<'APPLESCRIPT' >/dev/null 2>&1 || true
tell application "System Events"
  repeat with p in (processes whose name is "Karaoke Generator")
    set frontmost of p to true
  end repeat
end tell
APPLESCRIPT

echo "App process started:"
echo "  pid $APP_PID"
echo
echo "If the window is not visible, check Mission Control or the Dock for"
echo "\"Karaoke Generator\". Press Ctrl+C here to stop the sandbox run."

wait "$APP_PID"
