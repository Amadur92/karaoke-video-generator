"""ReplayGain normalization for the Python rendering fallback."""

import math
import os
import subprocess
import sys
import hashlib
import shutil
from cache_identity import file_sha256

DEFAULT_TARGET_DB = 89
MIN_TARGET_DB = 75
MAX_TARGET_DB = 100


def gain_from_report(report, target_db):
    if not MIN_TARGET_DB <= target_db <= MAX_TARGET_DB:
        raise ValueError(f"Громкость должна быть от {MIN_TARGET_DB} до {MAX_TARGET_DB} дБ")
    values = {}
    for line in report.splitlines():
        for key in ("track_gain", "track_peak"):
            marker = f"{key} ="
            if marker in line:
                values[key] = float(line.split(marker, 1)[1].split()[0])
    gain, peak = values.get("track_gain"), values.get("track_peak")
    if (gain is None or peak is None or not math.isfinite(gain)
            or not math.isfinite(peak) or peak < 0):
        raise ValueError("FFmpeg не сообщил корректные ReplayGain track_gain/track_peak")
    if peak == 0:
        return 0.0, 0.0
    requested = gain + target_db - DEFAULT_TARGET_DB
    return requested, min(requested, -1.0 - 20.0 * math.log10(peak))


def normalization_filter(audio_path, target_db=DEFAULT_TARGET_DB):
    if not MIN_TARGET_DB <= target_db <= MAX_TARGET_DB:
        raise ValueError(f"Громкость должна быть от {MIN_TARGET_DB} до {MAX_TARGET_DB} дБ")
    kwargs = {}
    if os.name == "nt":
        kwargs["creationflags"] = getattr(subprocess, "CREATE_NO_WINDOW", 0x08000000)
    cache_path = analysis_cache_path(audio_path)
    try:
        with open(cache_path, encoding="utf-8") as saved:
            report = saved.read()
        gain_from_report(report, target_db)
    except (OSError, ValueError):
        report = None
    if report is None:
        report = scan_loudness(audio_path, kwargs)
        try:
            os.makedirs(os.path.dirname(cache_path), exist_ok=True)
            temporary = cache_path + f".{os.getpid()}.tmp"
            with open(temporary, "w", encoding="utf-8") as saved:
                saved.write(report)
            os.replace(temporary, cache_path)
        except OSError:
            pass
    requested, applied = gain_from_report(report, target_db)
    print(f"[Audio] Цель {target_db} дБ ReplayGain; поправка {applied:+.2f} дБ", file=sys.stderr)
    if applied < requested - 0.01:
        print("[Audio] Усиление ограничено пиками; трек останется тише цели.", file=sys.stderr)
    return f"aformat=channel_layouts=stereo,volume={applied:.6f}dB:replaygain=drop"


def scan_loudness(audio_path, kwargs):
    result = subprocess.run(
        ["ffmpeg", "-hide_banner", "-nostdin", "-nostats", "-loglevel", "info",
         "-i", str(audio_path), "-map", "0:a:0", "-af",
         "aformat=channel_layouts=stereo,replaygain", "-f", "null", "-"],
        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, **kwargs,
    )
    if result.returncode:
        raise RuntimeError("Анализ громкости не удался:\n" + "\n".join(result.stderr.splitlines()[-8:]))
    gain_from_report(result.stderr, DEFAULT_TARGET_DB)
    return "\n".join(line for line in result.stderr.splitlines() if "track_gain =" in line or "track_peak =" in line)


def analysis_cache_path(audio_path):
    executable = shutil.which("ffmpeg") or "ffmpeg"
    try:
        stat = os.stat(executable)
        tool = f"{executable}:{stat.st_size}:{stat.st_mtime_ns}"
    except OSError:
        tool = executable
    key = hashlib.sha256(f"replaygain-stereo-v1:{file_sha256(audio_path)}:{tool}".encode()).hexdigest()
    if sys.platform == "darwin":
        root = os.path.expanduser("~/Library/Caches")
    elif os.name == "nt":
        root = os.environ.get("LOCALAPPDATA", os.path.expanduser("~"))
    else:
        root = os.environ.get("XDG_CACHE_HOME", os.path.expanduser("~/.cache"))
    return os.path.join(root, "karaoke-generator", "loudness-python", key + ".txt")
