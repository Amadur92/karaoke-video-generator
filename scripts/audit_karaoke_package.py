#!/usr/bin/env python3
"""Fast package audit for karaoke batches.

This is intentionally cheap: it does not run Whisper. It catches missing assets,
structurally suspicious timings, LRC/timing drift, weak lyric density, and stale
or missing quality reports. Use the output as the queue for heavier semantic
audio/text checks.
"""

from __future__ import annotations

import argparse
import csv
import json
import re
import subprocess
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
WORKER_DIR = REPO_ROOT / "worker"
if str(WORKER_DIR) not in sys.path:
    sys.path.insert(0, str(WORKER_DIR))

from karaoke_alignment import evaluate_alignment_quality, parse_lrc_timestamp, strip_lrc_timestamps  # noqa: E402


def folder_number(path: Path) -> int:
    prefix = path.name.split(".", 1)[0].strip()
    return int(prefix) if prefix.isdigit() else 9999


def word_count(text: str) -> int:
    return len([w for w in re.split(r"\s+", text or "") if re.sub(r"[^\w]+", "", w, flags=re.UNICODE)])


def ffprobe_duration(path: Path) -> float | None:
    try:
        proc = subprocess.run(
            [
                "ffprobe",
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "default=nw=1:nk=1",
                str(path),
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=20,
        )
        if proc.returncode != 0:
            return None
        return float(proc.stdout.strip())
    except Exception:
        return None


def parse_lrc_lines(path: Path | None) -> list[dict]:
    if not path or not path.exists():
        return []
    lines = []
    for raw in path.read_text(encoding="utf-8", errors="ignore").splitlines():
        line = raw.strip()
        if not line:
            continue
        timestamps = []
        rest = line
        while rest.startswith("[") and "]" in rest:
            tag, rest_tail = rest[1:].split("]", 1)
            ts = parse_lrc_timestamp(tag)
            if ts is None:
                break
            timestamps.append(ts)
            rest = rest_tail
        text = rest.strip()
        if text:
            lines.append({"time": timestamps[0] if timestamps else None, "text": text})
    return lines


def report_path_for_timings(timings_path: Path) -> Path:
    return timings_path.with_name(f"{timings_path.stem}_alignment_report.json")


def choose_one(folder: Path, patterns: list[str]) -> Path | None:
    found = []
    for pattern in patterns:
        found.extend(folder.glob(pattern))
    return sorted(found)[0] if found else None


def output_path(folder: Path, mode: str) -> Path | None:
    found = sorted(folder.glob(f"*({mode}).mp4"))
    return found[0] if found else None


def audit_folder(folder: Path, mode: str) -> dict:
    audio = choose_one(folder, ["*.mp3"])
    lyrics = choose_one(folder, ["*.lrc", "*.txt"])
    timings = choose_one(folder, ["*_timings.json"])
    video = output_path(folder, mode)
    issues: list[dict] = []
    notices: list[str] = []

    def issue(kind: str, severity: str, message: str, **extra) -> None:
        issues.append({"kind": kind, "severity": severity, "message": message, **extra})

    if audio is None:
        issue("missing_audio", "error", "No .mp3 file.")
    if lyrics is None:
        issue("missing_lyrics", "error", "No .lrc/.txt file.")
    if timings is None:
        issue("missing_timings", "error", "No *_timings.json file.")
    if video is None:
        issue("missing_video", "notice", f"No ({mode}).mp4 file.")

    duration = ffprobe_duration(audio) if audio else None
    lrc_lines = parse_lrc_lines(lyrics)
    stripped_lyrics = strip_lrc_timestamps(lyrics.read_text(encoding="utf-8", errors="ignore")) if lyrics else ""
    lyrics_words = word_count(stripped_lyrics)
    lyrics_line_count = len([line for line in stripped_lyrics.splitlines() if line.strip()])

    if lyrics and lyrics_words < 35:
        issue("short_lyrics", "warning", "Lyrics text is very short.", words=lyrics_words)
    if lyrics_line_count and duration and lyrics_line_count / max(duration / 60.0, 0.1) < 7:
        issue(
            "low_line_density",
            "warning",
            "Few lyric lines for audio duration.",
            lines=lyrics_line_count,
            duration=round(duration, 2),
        )

    timings_data = None
    quality = None
    if timings:
        try:
            timings_data = json.loads(timings.read_text(encoding="utf-8"))
            quality = evaluate_alignment_quality(timings_data, audio_duration=duration, source=str(timings))
            for item in quality.get("issues", []):
                severity = item.get("severity", "warning")
                if severity in {"error", "warning"}:
                    issue(
                        f"timing_{item.get('kind', 'issue')}",
                        severity,
                        item.get("message", "Timing quality issue."),
                        line_index=item.get("line_index"),
                        score=quality.get("score"),
                    )
        except Exception as exc:
            issue("invalid_timings", "error", f"Cannot read timings JSON: {exc}")

    if timings and not report_path_for_timings(timings).exists():
        issue("missing_alignment_report", "notice", "No alignment quality report next to timings.")

    if timings_data and lrc_lines:
        timed_lrc = [line for line in lrc_lines if line["time"] is not None]
        timing_lines = [line for line in timings_data if (line.get("text") or "").strip()]
        if timed_lrc and timing_lines:
            count = min(len(timed_lrc), len(timing_lines))
            diffs = []
            for idx in range(count):
                try:
                    diffs.append(abs(float(timing_lines[idx].get("start", 0.0)) - float(timed_lrc[idx]["time"])))
                except Exception:
                    pass
            if diffs:
                diffs_sorted = sorted(diffs)
                median = diffs_sorted[len(diffs_sorted) // 2]
                max_diff = max(diffs)
                if median > 2.0 or max_diff > 8.0:
                    issue(
                        "lrc_timing_drift",
                        "warning",
                        "Final timings drift from LRC timestamps.",
                        median=round(median, 3),
                        max=round(max_diff, 3),
                    )
        if abs(len(lrc_lines) - len(timing_lines)) > 2:
            issue(
                "line_count_mismatch",
                "warning",
                "Lyrics line count differs from timing line count.",
                lyrics_lines=len(lrc_lines),
                timing_lines=len(timing_lines),
            )

    if quality and quality.get("summary") == "ok" and not any(i["severity"] == "error" for i in issues):
        notices.append("semantic_check_recommended")

    serious_warning_kinds = {
        "invalid_timings",
        "short_lyrics",
        "low_line_density",
        "lrc_timing_drift",
        "line_count_mismatch",
        "timing_line_overlap",
        "timing_word_overlap",
        "timing_line_out_of_bounds",
    }
    timing_warning_kinds = {
        "timing_large_internal_gap",
        "timing_long_line",
        "timing_tiny_word",
        "timing_long_word",
    }
    has_error = any(item["severity"] == "error" for item in issues)
    has_serious_warning = any(item["kind"] in serious_warning_kinds for item in issues)
    timing_warning_count = sum(1 for item in issues if item["kind"] in timing_warning_kinds)
    if has_error:
        verdict = "needs_repair"
    elif has_serious_warning or timing_warning_count >= 4:
        verdict = "suspicious"
    elif timing_warning_count > 0:
        verdict = "minor_warnings"
    elif notices:
        verdict = "needs_semantic_check"
    else:
        verdict = "ok"

    return {
        "number": folder_number(folder),
        "folder": folder.name,
        "verdict": verdict,
        "duration": round(duration, 3) if duration else None,
        "lyrics_words": lyrics_words,
        "lyrics_lines": lyrics_line_count,
        "timing_lines": len(timings_data or []),
        "quality_summary": quality.get("summary") if quality else None,
        "quality_score": quality.get("score") if quality else None,
        "audio": str(audio) if audio else None,
        "lyrics": str(lyrics) if lyrics else None,
        "timings": str(timings) if timings else None,
        "video": str(video) if video else None,
        "issues": issues,
        "notices": notices,
    }


def write_markdown(path: Path, rows: list[dict]) -> None:
    buckets = ["needs_repair", "suspicious", "needs_semantic_check", "ok"]
    lines = ["# Karaoke Package Audit", ""]
    for bucket in buckets:
        subset = [row for row in rows if row["verdict"] == bucket]
        lines.append(f"## {bucket} ({len(subset)})")
        lines.append("")
        for row in subset:
            issue_text = "; ".join(
                f"{item['kind']}: {item['message']}" for item in row["issues"][:4]
            )
            if not issue_text and row["notices"]:
                issue_text = ", ".join(row["notices"])
            lines.append(f"- {row['number']:03d}. {row['folder']} — {issue_text or 'ok'}")
        lines.append("")
    path.write_text("\n".join(lines), encoding="utf-8")


def write_csv(path: Path, rows: list[dict]) -> None:
    fields = [
        "number",
        "folder",
        "verdict",
        "quality_summary",
        "quality_score",
        "duration",
        "lyrics_words",
        "lyrics_lines",
        "timing_lines",
        "issue_count",
        "issues",
    ]
    with path.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fields)
        writer.writeheader()
        for row in rows:
            writer.writerow(
                {
                    **{field: row.get(field) for field in fields if field not in {"issue_count", "issues"}},
                    "issue_count": len(row["issues"]),
                    "issues": "; ".join(f"{i['kind']}:{i['message']}" for i in row["issues"]),
                }
            )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("root", type=Path)
    parser.add_argument("--mode", default="karaoke-word", choices=["karaoke-word", "karaoke-lines"])
    parser.add_argument("--output-prefix", type=Path)
    args = parser.parse_args()

    root = args.root
    prefix = args.output_prefix or root / "package_audit"
    folders = sorted([path for path in root.iterdir() if path.is_dir()], key=folder_number)
    rows = [audit_folder(folder, args.mode) for folder in folders]

    json_path = prefix.with_suffix(".json")
    csv_path = prefix.with_suffix(".csv")
    md_path = prefix.with_suffix(".md")
    json_path.write_text(json.dumps(rows, ensure_ascii=False, indent=2), encoding="utf-8")
    write_csv(csv_path, rows)
    write_markdown(md_path, rows)

    counts = {}
    for row in rows:
        counts[row["verdict"]] = counts.get(row["verdict"], 0) + 1
    print(json.dumps({"root": str(root), "count": len(rows), "counts": counts, "json": str(json_path), "csv": str(csv_path), "md": str(md_path)}, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
