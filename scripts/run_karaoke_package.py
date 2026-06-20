#!/usr/bin/env python3
"""Run alignment and both karaoke renders for a downloaded package folder."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
WORKER = REPO_ROOT / "worker" / "karaoke_worker.py"
RENDERER_CANDIDATES = [
    REPO_ROOT / "desktop_app" / "target" / "release" / "karaoke_render",
    REPO_ROOT / "desktop_app" / "target" / "debug" / "karaoke_render",
]


def folder_number(path: Path) -> int:
    prefix = path.name.split(".", 1)[0].strip()
    return int(prefix) if prefix.isdigit() else 9999


def safe_output_filename(name: str) -> str:
    name = re.sub(r"[/:*?\"<>|]", "", name)
    name = re.sub(r"\s+", " ", name).strip()
    return name or "karaoke.mp4"


def choose_one(folder: Path, patterns: list[str]) -> Path | None:
    found: list[Path] = []
    for pattern in patterns:
        found.extend(folder.glob(pattern))
    return sorted(found)[0] if found else None


def split_artist_title(stem: str) -> tuple[str, str]:
    if " - " in stem:
        artist, title = stem.split(" - ", 1)
        return artist.strip(), title.strip()
    return "Исполнитель", stem.strip()


def timings_path(audio: Path) -> Path:
    return audio.with_name(f"{audio.stem}_timings.json")


def output_path(folder: Path, artist: str, title: str, mode: str) -> Path:
    return folder / safe_output_filename(f"{artist} - {title} ({mode}).mp4")


def package_items(root: Path) -> list[dict]:
    items = []
    for folder in sorted([path for path in root.iterdir() if path.is_dir()], key=folder_number):
        audio = choose_one(folder, ["*.mp3"])
        lyrics = choose_one(folder, ["*.lrc", "*.txt"])
        if not audio or not lyrics:
            items.append(
                {
                    "index": folder_number(folder),
                    "folder": folder,
                    "audio": audio,
                    "lyrics": lyrics,
                    "missing": True,
                }
            )
            continue
        artist, title = split_artist_title(audio.stem)
        items.append(
            {
                "index": folder_number(folder),
                "folder": folder,
                "audio": audio,
                "lyrics": lyrics,
                "artist": artist,
                "title": title,
                "timings": timings_path(audio),
                "word_output": output_path(folder, artist, title, "karaoke-word"),
                "lines_output": output_path(folder, artist, title, "karaoke-lines"),
                "missing": False,
            }
        )
    return items


def run(cmd: list[str], log_path: Path | None = None) -> None:
    print("$ " + " ".join(str(part) for part in cmd), flush=True)
    if log_path:
        with log_path.open("a", encoding="utf-8") as log:
            log.write("$ " + " ".join(str(part) for part in cmd) + "\n")
            proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
            assert proc.stdout is not None
            for line in proc.stdout:
                print(line, end="", flush=True)
                log.write(line)
            code = proc.wait()
    else:
        code = subprocess.call(cmd)
    if code != 0:
        raise SystemExit(code)


def renderer_path(explicit: Path | None) -> Path:
    if explicit:
        return explicit
    for candidate in RENDERER_CANDIDATES:
        if candidate.exists():
            return candidate
    raise SystemExit("karaoke_render binary not found. Run cargo build --bin karaoke_render first.")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("root", type=Path)
    parser.add_argument("--model", default="base")
    parser.add_argument("--quality", default="medium")
    parser.add_argument("--renderer", type=Path)
    parser.add_argument("--overwrite-timings", action="store_true")
    parser.add_argument("--overwrite-videos", action="store_true")
    parser.add_argument("--skip-align", action="store_true")
    parser.add_argument("--skip-render", action="store_true")
    parser.add_argument("--verify-lrc-with-whisper", action="store_true")
    args = parser.parse_args()

    root = args.root
    items = package_items(root)
    missing = [item for item in items if item.get("missing")]
    if missing:
        print(f"Missing audio/lyrics in {len(missing)} folders:")
        for item in missing[:20]:
            print(f"  {item['index']:03d}. {item['folder'].name}")

    ready = [item for item in items if not item.get("missing")]
    print(f"Package folders: {len(items)}; ready inputs: {len(ready)}")

    queue = []
    for item in ready:
        if item["timings"].exists() and not args.overwrite_timings:
            continue
        queue.append(
            {
                "index": item["index"],
                "audio": str(item["audio"]),
                "artist": item["artist"],
                "title": item["title"],
                "lyrics_file": str(item["lyrics"]),
                "timings_output": str(item["timings"]),
            }
        )

    if queue and not args.skip_align:
        queue_path = root / "batch_align_queue_word.json"
        queue_path.write_text(json.dumps(queue, ensure_ascii=False, indent=2), encoding="utf-8")
        cmd = [
            sys.executable,
            str(WORKER),
            "--cli",
            "--batch-align-queue",
            str(queue_path),
            "--model",
            args.model,
            "--quality",
            args.quality,
            "--font",
            "montserrat",
            "--color-active",
            "#000000",
            "--color-inactive",
            "#B4B9C3",
            "--color-bg",
            "#FFFFFF",
            "--inactive-opacity",
            "0.65",
            "--audio-delay",
            "0.0",
        ]
        if args.verify_lrc_with_whisper:
            cmd.append("--verify-lrc-with-whisper")
        run(cmd, root / "batch_align_word.log")
    elif queue:
        print(f"Align queue prepared but skipped: {len(queue)} items")
    else:
        print("All timings already exist.")

    if args.skip_render:
        return 0

    renderer = renderer_path(args.renderer)
    for item in ready:
        if not item["timings"].exists():
            print(f"skip render {item['index']:03d}: missing timings")
            continue
        for mode, out_path, extra in [
            ("karaoke-word", item["word_output"], []),
            ("karaoke-lines", item["lines_output"], ["--plain-lines"]),
        ]:
            if out_path.exists() and not args.overwrite_videos:
                print(f"skip {item['index']:03d} {mode}: exists")
                continue
            cmd = [
                str(renderer),
                "--timings",
                str(item["timings"]),
                "--audio",
                str(item["audio"]),
                "--output",
                str(out_path),
                "--quality",
                args.quality,
                "--color-active",
                "#000000",
                "--color-inactive",
                "#B4B9C3",
                "--color-bg",
                "#FFFFFF",
                "--inactive-opacity",
                "0.65",
                "--audio-delay",
                "0.0",
                *extra,
            ]
            run(cmd, root / f"render_{mode}.log")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
