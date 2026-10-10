"""Verify exported loudness with actual FFmpeg decoding, without Whisper models.

python3 scripts/loudness_smoke.py --renderer desktop_app/target/debug/karaoke_render
"""

import argparse
import json
import math
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import wave

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "worker"))
from audio_loudness import gain_from_report  # noqa: E402


def run(command, **kwargs):
    result = subprocess.run(command, capture_output=True, text=True, **kwargs)
    if result.returncode:
        raise RuntimeError(f"{command}:\n{result.stdout[-1500:]}\n{result.stderr[-1500:]}")
    return result


def write_audio(path, amplitude, transient=False):
    with wave.open(str(path), "wb") as output:
        output.setnchannels(1)
        output.setsampwidth(2)
        output.setframerate(44100)
        samples = []
        for index in range(88200):
            sample = amplitude * math.sin(2 * math.pi * 440 * index / 44100)
            if transient and index == 44100:
                sample = 0.95
            samples.append(struct.pack("<h", round(sample * 32767)))
        output.writeframes(b"".join(samples))


def scan(ffmpeg, path):
    result = run([ffmpeg, "-hide_banner", "-nostats", "-i", str(path), "-map", "0:a:0",
                  "-af", "aformat=channel_layouts=stereo,replaygain", "-f", "null", "-"])
    gain, _ = gain_from_report(result.stderr, 89)
    return gain


def decoded_peak(ffmpeg, path):
    result = subprocess.run([ffmpeg, "-v", "error", "-i", str(path), "-map", "0:a:0",
                             "-f", "f32le", "-"], capture_output=True, check=True)
    return max(abs(sample[0]) for sample in struct.iter_unpack("<f", result.stdout))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--renderer", required=True)
    parser.add_argument("--ffmpeg", default="ffmpeg")
    parser.add_argument("--ffprobe", default="ffprobe")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="karaoke-loudness-smoke-") as temp:
        root = Path(temp)
        timings = root / "timings.json"
        timings.write_text(json.dumps([{"start": 0.1, "end": 1.8, "words": [
            {"word": "Audio", "start": 0.1, "end": 0.8},
            {"word": "test", "start": 0.8, "end": 1.8}]}]))
        inputs = {}
        for name, amplitude in [("quiet", 0.01), ("loud", 0.08), ("silence", 0.0), ("peaky", 0.01)]:
            inputs[name] = root / f"{name}.wav"
            write_audio(inputs[name], amplitude, transient=name == "peaky")
        original_bytes = {key: path.read_bytes() for key, path in inputs.items()}

        def render(name, engine="frames", flags=()):
            output = root / "output.mp4"
            result = run([args.renderer, "--timings", str(timings), "--audio", str(inputs[name]),
                          "--output", str(output), "--engine", engine, "--ffmpeg", args.ffmpeg,
                          "--ffprobe", args.ffprobe, *flags])
            return scan(args.ffmpeg, output), result.stderr

        filters = run([args.ffmpeg, "-hide_banner", "-filters"]).stdout
        engines = ["frames"]
        if any("ass" in line.split()[1:2] for line in filters.splitlines()):
            engines.append("ass")
        else:
            print("SKIP ASS: this FFmpeg build has no ass filter")
        for engine in engines:
            quiet, _ = render("quiet", engine)
            loud, _ = render("loud", engine)
            assert abs(quiet - loud) < 0.3, (engine, quiet, loud)
            assert abs(quiet) < 0.3, (engine, quiet)
            print(f"PASS {engine}: source levels 18 dB apart converge (residual {quiet:.2f}/{loud:.2f} dB)")

        shifted, _ = render("quiet", flags=("--loudness-target-db", "92"))
        assert abs(shifted + 3) < 0.3, shifted
        unchanged, _ = render("quiet", flags=("--no-normalize-audio",))
        assert abs(unchanged - scan(args.ffmpeg, inputs["quiet"])) < 0.3, unchanged
        _, silence_log = render("silence")
        assert "поправка" in silence_log.lower() and "+0.00" in silence_log
        assert decoded_peak(args.ffmpeg, root / "output.mp4") == 0.0
        _, peak_log = render("peaky", flags=("--loudness-target-db", "100"))
        assert "ограничено пиками" in peak_log
        assert decoded_peak(args.ffmpeg, root / "output.mp4") < 1.0
        print("PASS target offset, disabled normalization, silence, peak protection")

        lyrics = root / "lyrics.lrc"
        lyrics.write_text("[00:00.10]Audio test\n", encoding="utf-8")
        worker = Path(__file__).resolve().parents[1] / "worker" / "karaoke_worker.py"
        env = {**os.environ, "KARAOKE_EXPORT_DIR": str(root)}
        result = run([sys.executable, str(worker), "--cli", "--audio", str(inputs["quiet"]),
                      "--artist", "Smoke", "--title", "Audio", "--lyrics-file", str(lyrics),
                      "--plain-lines", "--loudness-target-db", "92"], env=env)
        output = root / "Smoke - Audio (karaoke-lines).mp4"
        assert output.exists(), result.stdout[-1500:]
        fallback_gain = scan(args.ffmpeg, output)
        assert abs(fallback_gain + 3) < 0.3, fallback_gain
        run([sys.executable, str(worker), "--cli", "--audio", str(inputs["quiet"]),
             "--artist", "Smoke", "--title", "Audio", "--lyrics-file", str(lyrics),
             "--plain-lines", "--no-normalize-audio"], env=env)
        assert abs(scan(args.ffmpeg, output) - scan(args.ffmpeg, inputs["quiet"])) < 0.3
        assert all(path.read_bytes() == original_bytes[key] for key, path in inputs.items())
        print(f"PASS Python renderer: target 92 dB (residual {fallback_gain:.2f} dB); sources unchanged")


if __name__ == "__main__":
    main()
