#!/usr/bin/env bash
set -euo pipefail

APP_DIR="${1:-packaging/dist/KaraokeGenerator-macos}"
MODE="${2:-launch}"

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
  "$APP_BIN"
