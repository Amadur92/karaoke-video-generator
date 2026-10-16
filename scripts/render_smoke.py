#!/usr/bin/env python3
"""Cross-platform end-to-end smoke test for the packaged Rust renderer."""

from __future__ import annotations

import argparse
import json
import math
import struct
import subprocess
import sys
import tempfile
import wave
from pathlib import Path


def run(command: list[str]) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(command, text=True, encoding="utf-8", errors="replace", capture_output=True)
    if result.returncode:
        raise RuntimeError(f"Command failed ({result.returncode}): {command}\n{result.stdout[-2000:]}\n{result.stderr[-4000:]}")
    return result


def write_audio(path: Path, duration: float = 2.0) -> None:
    sample_rate = 44_100
    with wave.open(str(path), "wb") as output:
        output.setnchannels(1)
        output.setsampwidth(2)
        output.setframerate(sample_rate)
        frames = bytearray()
        for index in range(round(duration * sample_rate)):
            sample = int(0.08 * 32767 * math.sin(2 * math.pi * 440 * index / sample_rate))
            frames.extend(struct.pack("<h", sample))
        output.writeframes(frames)


def write_timings(path: Path) -> None:
    payload = [
        {
            "start": 0.151,
            "end": 0.152,
            "words": [{"word": "Миг", "start": 0.151, "end": 0.152}],
        },
        {
            "start": 0.8,
            "end": 1.85,
            "words": [
                {"word": "Render", "start": 0.8, "end": 1.2},
                {"word": "smoke", "start": 1.2, "end": 1.5},
                {"word": "test", "start": 1.5, "end": 1.85},
            ],
        }
    ]
    path.write_text(json.dumps(payload, ensure_ascii=False), encoding="utf-8")


def verify_output(ffprobe: str, output: Path, expected_fps: int) -> None:
    result = run(
        [
            ffprobe,
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,width,height,avg_frame_rate",
            "-show_entries",
            "format=duration",
            "-of",
            "json",
            str(output),
        ]
    )
    payload = json.loads(result.stdout)
    streams = payload.get("streams") or []
    video = next((item for item in streams if item.get("codec_type") == "video"), None)
    audio = next((item for item in streams if item.get("codec_type") == "audio"), None)
    if not video or not audio:
        raise RuntimeError(f"missing audio/video stream: {payload}")
    if (video.get("width"), video.get("height")) != (1352, 224):
        raise RuntimeError(f"unexpected frame size: {video}")
    numerator, denominator = map(int, video["avg_frame_rate"].split("/"))
    fps = numerator / denominator
    if abs(fps - expected_fps) > 0.01:
        raise RuntimeError(f"unexpected fps {fps}, expected {expected_fps}")
    duration = float(payload["format"]["duration"])
    if abs(duration - 2.0) > 0.25:
        raise RuntimeError(f"unexpected duration: {duration}")


def verify_short_line_highlight(ffmpeg: str, output: Path, expected_fps: int) -> None:
    def dark_pixels(frame_index: int) -> int:
        raw = subprocess.check_output(
            [
                ffmpeg, "-v", "error", "-i", str(output), "-vf",
                f"select=eq(n\\,{frame_index})", "-frames:v", "1",
                "-pix_fmt", "rgb24", "-f", "rawvideo", "pipe:1",
            ]
        )
        if len(raw) != 1352 * 224 * 3:
            raise RuntimeError(f"unexpected decoded frame size: {len(raw)}")
        return sum(
            1 for pixel in range(0, len(raw), 3)
            if max(raw[pixel:pixel + 3]) < 60
        )

    before = dark_pixels(0)
    during = dark_pixels(int(0.151 * expected_fps))
    if during < before + 50:
        raise RuntimeError(
            f"sub-frame line was not highlighted: before={before}, during={during}"
        )


def main() -> int:
    # The renderer emits UTF-8, including Russian status text. Windows CI pipes
    # otherwise default to cp1252 and cannot print these successful diagnostics.
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")
    parser = argparse.ArgumentParser()
    parser.add_argument("--renderer", required=True)
    parser.add_argument("--ffmpeg", required=True)
    parser.add_argument("--ffprobe", required=True)
    parser.add_argument("--engine", choices=("ass", "frames"), default="ass")
    args = parser.parse_args()
    args.renderer = str(Path(args.renderer).resolve())
    args.ffmpeg = str(Path(args.ffmpeg).resolve())
    args.ffprobe = str(Path(args.ffprobe).resolve())

    expected_fps = 60 if args.engine == "ass" else 30
    with tempfile.TemporaryDirectory(prefix="karaoke-render-smoke-") as temp:
        root = Path(temp)
        audio = root / "input.wav"
        timings = root / "timings.json"
        output = root / "output.mp4"
        write_audio(audio)
        write_timings(timings)
        result = run(
            [
                args.renderer,
                "--timings",
                str(timings),
                "--audio",
                str(audio),
                "--output",
                str(output),
                "--quality",
                "high",
                "--engine",
                args.engine,
                "--ffmpeg",
                args.ffmpeg,
                "--ffprobe",
                args.ffprobe,
            ]
        )
        verify_output(args.ffprobe, output, expected_fps)
        verify_short_line_highlight(args.ffmpeg, output, expected_fps)
        print(result.stderr.strip())
        print(f"render smoke passed: engine={args.engine}, fps={expected_fps}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
