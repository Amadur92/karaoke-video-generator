#!/usr/bin/env python3
"""Run expensive audio/text semantic checks for selected package rows."""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
WORKER_DIR = REPO_ROOT / "worker"
if str(WORKER_DIR) not in sys.path:
    sys.path.insert(0, str(WORKER_DIR))

from karaoke_worker import get_whisper_model, infer_lyrics_language, measure_lyrics_text_match, strip_lrc_timestamps  # noqa: E402


def load_lyrics(path: str | None) -> str:
    if not path:
        return ""
    return Path(path).read_text(encoding="utf-8", errors="ignore")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("audit_json", type=Path)
    parser.add_argument("--model", default="base")
    parser.add_argument("--verdict", action="append", default=["needs_semantic_check"])
    parser.add_argument("--numbers", help="Comma-separated track numbers to force-check.")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    rows = json.loads(args.audit_json.read_text(encoding="utf-8"))
    forced = set()
    if args.numbers:
        forced = {int(part.strip()) for part in args.numbers.split(",") if part.strip()}

    selected = []
    for row in rows:
        if forced and row["number"] in forced:
            selected.append(row)
        elif not forced and row.get("verdict") in set(args.verdict or []):
            selected.append(row)
    selected.sort(key=lambda row: row["number"])
    if args.limit:
        selected = selected[: args.limit]

    output = args.output or args.audit_json.with_name(args.audit_json.stem + "_semantic.json")
    results = []
    started = time.time()

    def status(message: str) -> None:
        print(message, flush=True)

    model = get_whisper_model(args.model, status_callback=status)
    for idx, row in enumerate(selected, 1):
        audio = row.get("audio")
        lyrics_path = row.get("lyrics")
        print(f"[{idx}/{len(selected)}] {row['number']:03d}. {row['folder']}", flush=True)
        result = {
            "number": row["number"],
            "folder": row["folder"],
            "audio": audio,
            "lyrics": lyrics_path,
            "source_verdict": row.get("verdict"),
        }
        try:
            lyrics = load_lyrics(lyrics_path)
            language = infer_lyrics_language(strip_lrc_timestamps(lyrics) or lyrics)
            match = measure_lyrics_text_match(
                model,
                audio,
                lyrics,
                language=language,
                status_callback=status,
            )
            score = None if match is None else match.get("score")
            if score is None:
                verdict = "unknown"
            elif score < 0.35:
                verdict = "needs_repair"
            elif score < 0.55:
                verdict = "suspicious"
            elif score < 0.72:
                verdict = "minor_warnings"
            else:
                verdict = "ok"
            result.update({"semantic_verdict": verdict, "text_match": match})
        except Exception as exc:
            result.update({"semantic_verdict": "error", "error": str(exc)})
        results.append(result)
        output.write_text(
            json.dumps(
                {
                    "audit": str(args.audit_json),
                    "model": args.model,
                    "elapsed_sec": round(time.time() - started, 2),
                    "count": len(results),
                    "total": len(selected),
                    "results": results,
                },
                ensure_ascii=False,
                indent=2,
            ),
            encoding="utf-8",
        )

    counts = {}
    for row in results:
        counts[row["semantic_verdict"]] = counts.get(row["semantic_verdict"], 0) + 1
    print(json.dumps({"output": str(output), "counts": counts}, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
