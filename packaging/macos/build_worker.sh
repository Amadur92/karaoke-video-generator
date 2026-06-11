#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VENV_DIR="$ROOT_DIR/.packaging-venv"
WORKER_DIR="$ROOT_DIR/worker"
OUT_DIR="$ROOT_DIR/packaging/build/macos-worker"

python3 -m venv "$VENV_DIR"
"$VENV_DIR/bin/python" -m pip install --upgrade pip wheel "setuptools<82"
"$VENV_DIR/bin/python" -m pip install -r "$WORKER_DIR/requirements.txt"

rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"

"$VENV_DIR/bin/pyinstaller" \
  --noconfirm \
  --clean \
  --onedir \
  --name karaoke_worker \
  --distpath "$OUT_DIR/dist" \
  --workpath "$OUT_DIR/build" \
  --specpath "$OUT_DIR" \
  --collect-all numpy \
  --collect-data whisper \
  --add-data "$ROOT_DIR/desktop_app/assets/Montserrat-Regular.ttf:." \
  --add-data "$ROOT_DIR/desktop_app/assets/Montserrat-Bold.ttf:." \
  "$WORKER_DIR/karaoke_worker.py"

echo "$OUT_DIR/dist/karaoke_worker"
