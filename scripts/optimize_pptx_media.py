#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import time
import zipfile
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path


def probe_video(path: Path) -> dict:
    output = subprocess.check_output(
        [
            "ffprobe",
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,codec_name",
            "-show_entries",
            "format=duration,bit_rate",
            "-of",
            "json",
            str(path),
        ],
        text=True,
        stderr=subprocess.STDOUT,
    )
    data = json.loads(output)
    stream = data.get("streams", [{}])[0]
    fmt = data.get("format", {})
    return {
        "width": int(stream.get("width") or 0),
        "height": int(stream.get("height") or 0),
        "codec": stream.get("codec_name"),
        "duration": float(fmt.get("duration") or 0),
        "bit_rate": int(float(fmt.get("bit_rate") or 0)),
    }


def valid_optimized(path: Path, max_width: int) -> bool:
    if not path.exists() or path.stat().st_size < 1000:
        return False
    try:
        info = probe_video(path)
    except Exception:
        return False
    return info["width"] <= max_width + 8 and info["duration"] > 1


def optimize_one(
    source_pptx: Path,
    media_name: str,
    original_size: int,
    media_dir: Path,
    max_width: int,
    crf: int,
    preset: str,
    audio_bitrate: str,
) -> dict:
    started = time.time()
    base_name = Path(media_name).name
    raw = media_dir / f"{base_name}.{os.getpid()}.src.mp4"
    out = media_dir / base_name
    tmp = media_dir / f"{base_name}.tmp.mp4"

    with zipfile.ZipFile(source_pptx) as zip_file:
        raw.write_bytes(zip_file.read(media_name))

    input_info = probe_video(raw)
    if input_info["width"] <= max_width + 8 and original_size < 8 * 1024 * 1024:
        shutil.copy2(raw, out)
        action = "copy"
    else:
        command = [
            "ffmpeg",
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            str(raw),
            "-vf",
            f"scale='min({max_width},iw)':-2",
            "-c:v",
            "libx264",
            "-preset",
            preset,
            "-crf",
            str(crf),
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            audio_bitrate,
            "-movflags",
            "+faststart",
            "-f",
            "mp4",
            str(tmp),
        ]
        subprocess.run(command, check=True)
        tmp.replace(out)
        action = "transcode"

    raw.unlink(missing_ok=True)
    output_info = probe_video(out)
    return {
        "name": media_name,
        "action": action,
        "before": original_size,
        "after": out.stat().st_size,
        "input": input_info,
        "output": output_info,
        "seconds": round(time.time() - started, 1),
    }


def build_optimized_pptx(
    source_pptx: Path,
    output_pptx: Path,
    media_dir: Path,
    max_width: int,
    compresslevel: int,
) -> dict:
    with zipfile.ZipFile(source_pptx) as source_zip:
        media_infos = [
            info
            for info in source_zip.infolist()
            if info.filename.startswith("ppt/media/")
            and info.filename.lower().endswith(".mp4")
        ]
        replacements = {
            info.filename: media_dir / Path(info.filename).name
            for info in media_infos
        }
        missing = [
            name
            for name, path in replacements.items()
            if not valid_optimized(path, max_width)
        ]
        if missing:
            raise RuntimeError(f"Missing or invalid optimized media: {missing[:10]}")

        tmp_pptx = output_pptx.with_suffix(output_pptx.suffix + ".tmp")
        tmp_pptx.unlink(missing_ok=True)
        with zipfile.ZipFile(
            tmp_pptx,
            "w",
            compression=zipfile.ZIP_DEFLATED,
            compresslevel=compresslevel,
            allowZip64=True,
        ) as output_zip:
            for info in source_zip.infolist():
                if info.filename in replacements:
                    output_zip.write(
                        replacements[info.filename],
                        info.filename,
                        compress_type=zipfile.ZIP_STORED,
                    )
                else:
                    with source_zip.open(info) as source:
                        output_zip.writestr(info, source.read())
        tmp_pptx.replace(output_pptx)

    with zipfile.ZipFile(output_pptx) as zip_file:
        bad_member = zip_file.testzip()
        mp4_infos = [
            info
            for info in zip_file.infolist()
            if info.filename.startswith("ppt/media/")
            and info.filename.lower().endswith(".mp4")
        ]
    return {
        "bad": bad_member,
        "output_size": output_pptx.stat().st_size,
        "mp4_count": len(mp4_infos),
        "mp4_size": sum(info.file_size for info in mp4_infos),
    }


def optimize_pptx(args: argparse.Namespace) -> dict:
    source_pptx = args.input.expanduser().resolve()
    output_pptx = args.output.expanduser().resolve()
    work_dir = (args.work_dir or output_pptx.parent / f"_{output_pptx.stem}_media_opt").resolve()
    media_dir = work_dir / "media"
    media_dir.mkdir(parents=True, exist_ok=True)
    output_pptx.parent.mkdir(parents=True, exist_ok=True)

    started = time.time()
    log_path = work_dir / "optimize_log.jsonl"

    with zipfile.ZipFile(source_pptx) as zip_file:
        media_infos = [
            info
            for info in zip_file.infolist()
            if info.filename.startswith("ppt/media/")
            and info.filename.lower().endswith(".mp4")
        ]

    jobs = []
    skipped = 0
    for info in media_infos:
        out = media_dir / Path(info.filename).name
        if valid_optimized(out, args.width):
            skipped += 1
        else:
            out.unlink(missing_ok=True)
            for stale in media_dir.glob(Path(info.filename).name + ".*"):
                stale.unlink(missing_ok=True)
            jobs.append((info.filename, info.file_size))

    print(
        f"OPT_PLAN total={len(media_infos)} skipped={skipped} remaining={len(jobs)} "
        f"workers={args.workers} width={args.width} crf={args.crf} preset={args.preset}",
        flush=True,
    )

    failures = []
    with ThreadPoolExecutor(max_workers=args.workers) as executor:
        futures = [
            executor.submit(
                optimize_one,
                source_pptx,
                name,
                size,
                media_dir,
                args.width,
                args.crf,
                args.preset,
                args.audio_bitrate,
            )
            for name, size in jobs
        ]
        for index, future in enumerate(as_completed(futures), 1):
            try:
                row = future.result()
                with log_path.open("a", encoding="utf-8") as log_file:
                    log_file.write(json.dumps(row, ensure_ascii=False) + "\n")
                print(
                    f"OPT_DONE {index}/{len(jobs)} {Path(row['name']).name} "
                    f"{row['before'] / 1024 ** 2:.1f}MB -> "
                    f"{row['after'] / 1024 ** 2:.1f}MB {row['seconds']}s",
                    flush=True,
                )
            except Exception as exc:
                failures.append(str(exc))
                print(f"OPT_FAIL {index}/{len(jobs)} {exc}", flush=True)

    if failures:
        raise RuntimeError(failures[:5])

    build_summary = build_optimized_pptx(
        source_pptx,
        output_pptx,
        media_dir,
        args.width,
        args.compresslevel,
    )
    summary = {
        "source": str(source_pptx),
        "output": str(output_pptx),
        "source_size": source_pptx.stat().st_size,
        "elapsed_sec": round(time.time() - started, 1),
        "workers": args.workers,
        "width": args.width,
        "crf": args.crf,
        "preset": args.preset,
        **build_summary,
    }
    (work_dir / "summary.json").write_text(
        json.dumps(summary, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    print(
        "OPT_SUMMARY "
        + json.dumps(
            {
                "bad": summary["bad"],
                "source_MB": round(summary["source_size"] / 1024**2, 1),
                "output_MB": round(summary["output_size"] / 1024**2, 1),
                "mp4_MB": round(summary["mp4_size"] / 1024**2, 1),
                "mp4_count": summary["mp4_count"],
                "elapsed_sec": summary["elapsed_sec"],
            },
            ensure_ascii=False,
        ),
        flush=True,
    )
    return summary


def main() -> None:
    parser = argparse.ArgumentParser(description="Shrink embedded MP4 media inside a PPTX.")
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--width", type=int, default=1352)
    parser.add_argument("--crf", type=int, default=20)
    parser.add_argument("--preset", default="veryfast")
    parser.add_argument("--audio-bitrate", default="96k")
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--compresslevel", type=int, default=1)
    optimize_pptx(parser.parse_args())


if __name__ == "__main__":
    main()
