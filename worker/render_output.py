"""Validate encoded video before replacing a previously exported file."""

import json
import os
import subprocess


def subprocess_kwargs():
    return {"creationflags": getattr(subprocess, "CREATE_NO_WINDOW", 0x08000000)} if os.name == "nt" else {}


def validate_video(path, expected_duration):
    if not os.path.isfile(path) or os.path.getsize(path) == 0:
        raise RuntimeError("FFmpeg не создал непустой MP4")
    result = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration",
         "-show_entries", "stream=codec_type", "-of", "json", path],
        capture_output=True, text=True, **subprocess_kwargs(),
    )
    if result.returncode:
        raise RuntimeError("Не удалось проверить MP4: " + result.stderr[-1200:])
    payload = json.loads(result.stdout)
    kinds = {stream.get("codec_type") for stream in payload.get("streams", [])}
    if not {"audio", "video"}.issubset(kinds):
        raise RuntimeError("В MP4 отсутствует аудио или видео")
    duration = float(payload.get("format", {}).get("duration", 0))
    if abs(duration - expected_duration) > max(0.2, min(0.5, expected_duration * 0.002)):
        raise RuntimeError(f"Длительность MP4 не совпадает с аудио: {duration:.3f} / {expected_duration:.3f} с")
    decoded = subprocess.run(
        ["ffmpeg", "-v", "error", "-xerror", "-i", path, "-map", "0:v:0",
         "-map", "0:a:0", "-f", "null", "-"],
        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, **subprocess_kwargs(),
    )
    if decoded.returncode:
        raise RuntimeError("MP4 повреждён: " + decoded.stderr[-1600:])


def finish_encoder(process, diagnostic_file, temporary, destination, expected_duration):
    process.stdin.close()
    returncode = process.wait()
    if returncode:
        diagnostic_file.seek(0)
        diagnostic = diagnostic_file.read().decode("utf-8", errors="replace")[-1600:]
        raise RuntimeError(f"FFmpeg завершился с кодом {returncode}: {diagnostic}")
    validate_video(temporary, expected_duration)
    os.replace(temporary, destination)
