from __future__ import annotations

import argparse
import json
import os
import sys

from .models import TrackMetadata
from .resolver import Resolver
from .spotify import SpotifyResolver, extract_spotify_track_id


def parse_artist_list(value: str) -> list[str]:
    return [part.strip() for part in value.split(",") if part.strip()]


def metadata_from_args(args: argparse.Namespace) -> TrackMetadata:
    spotify_id = extract_spotify_track_id(args.query)
    if spotify_id:
        resolver = SpotifyResolver()
        meta = resolver.enrich_from_spotify_id(
            spotify_id,
            fallback_title=args.title or "",
            fallback_artists=parse_artist_list(args.artist or ""),
        )
        if args.title:
            meta.title = args.title
        if args.artist:
            meta.artists = parse_artist_list(args.artist)
        if args.duration:
            meta.duration_sec = args.duration
        return meta

    title = args.title or args.query
    artists = parse_artist_list(args.artist or "")
    if " - " in args.query and not args.title and not artists:
        left, right = args.query.split(" - ", 1)
        artists = [left.strip()]
        title = right.strip()

    return TrackMetadata(
        title=title,
        artists=artists,
        duration_sec=args.duration,
    )


def candidate_payload(rank: int, item) -> dict:
    c = item.candidate
    return {
        "rank": rank,
        "score": item.score,
        "source": c.source,
        "title": c.title,
        "artists": c.artists,
        "duration_sec": c.duration_sec,
        "album": c.album,
        "public_url": c.public_url,
        "public_url_ok": c.public_url_ok,
        "external_id": c.external_id,
        "quality_hint": c.quality_hint,
        "route_kind": c.route_kind,
        "next_step": c.next_step,
        "blocked_at": c.blocked_at,
        "score_parts": {
            "title": item.title_score,
            "artist": item.artist_score,
            "duration": item.duration_score,
            "album": item.album_score,
        },
        "flags": item.flags,
    }


def best_route(scored) -> tuple[int, object] | None:
    for rank, item in enumerate(scored, 1):
        c = item.candidate
        if c.route_kind == "resolvable_blocked" and item.score >= 55:
            return rank, item
    return (1, scored[0]) if scored else None


def download_quality_summary(scored_candidate) -> tuple[str, list[str]]:
    reasons: list[str] = []
    candidate = scored_candidate.candidate
    if scored_candidate.score < 65:
        reasons.append(f"low_score:{scored_candidate.score}")
    if scored_candidate.title_score < 72:
        reasons.append(f"title_match:{scored_candidate.title_score}")
    if scored_candidate.artist_score < 72:
        reasons.append(f"artist_match:{scored_candidate.artist_score}")
    if scored_candidate.duration_score < 70:
        reasons.append(f"duration_score:{scored_candidate.duration_score}")
    if candidate.duration_sec is None:
        reasons.append("duration_unknown")
    for flag in scored_candidate.flags:
        if (
            flag.startswith("marker:")
            or flag.startswith("critical_marker:")
            or flag.startswith("duration_diff:")
            or flag in {"large_duration_mismatch", "weak_artist_match", "weak_title_match"}
        ):
            reasons.append(flag)
    title_lower = (candidate.title or "").lower()
    source_markers = [
        "live",
        "concert",
        "remix",
        "cover",
        "karaoke",
        "instrumental",
        "slowed",
        "sped up",
        "speed up",
        "nightcore",
        "версия",
        "концерт",
        "ремикс",
        "кавер",
    ]
    for marker in source_markers:
        if marker in title_lower and not any(marker in reason for reason in reasons):
            reasons.append(f"title_marker:{marker}")
    if reasons and candidate.source:
        reasons.append(f"source:{candidate.source}")
    if reasons and candidate.title:
        reasons.append(f"candidate:{candidate.title[:80]}")
    return ("suspicious" if reasons else "ok", reasons)


def print_report(meta: TrackMetadata, scored) -> None:
    print("TRACK")
    print(json.dumps(meta.__dict__, ensure_ascii=False, indent=2))
    print("\nCANDIDATES")
    for rank, item in enumerate(scored, 1):
        print(json.dumps(candidate_payload(rank, item), ensure_ascii=False))


def print_resolution_report(report) -> None:
    print("TRACK")
    print(json.dumps(report.track.__dict__, ensure_ascii=False, indent=2))

    print("\nPROVIDER_STATUS")
    for run in report.provider_runs:
        print(json.dumps(run.__dict__, ensure_ascii=False))

    selected = best_route(report.candidates)
    print("\nSELECTED_SAFE_ROUTE")
    if selected:
        rank, item = selected
        print(json.dumps(candidate_payload(rank, item), ensure_ascii=False, indent=2))
    else:
        print(json.dumps({"status": "no_candidate"}, ensure_ascii=False))

    print("\nCANDIDATES")
    for rank, item in enumerate(report.candidates, 1):
        print(json.dumps(candidate_payload(rank, item), ensure_ascii=False))

    print("\nSAFE_MODE_BOUNDARY")
    print(
        json.dumps(
            {
                "download_performed": False,
                "direct_media_url_requested": False,
                "boundary": "Stopped before any provider call that returns a direct audio/media/download URL.",
            },
            ensure_ascii=False,
        )
    )


def find_column_indices(headers: list[str]) -> tuple[int, int, int]:
    """
    Returns indices for (artist_idx, track_idx, position_idx) based on headers.
    Raises ValueError if any required column is not found.
    """
    artist_keywords = ['исполнитель', 'исполнители', 'artist', 'artists', 'певец', 'группа']
    track_keywords = ['трек', 'track', 'песня', 'название', 'title']
    position_keywords = ['порядковый номер', 'номер', '№', 'index', 'position', 'number', 'порядковый', 'id']

    artist_idx = -1
    track_idx = -1
    position_idx = -1

    # 1. Position column
    for idx, header in enumerate(headers):
        if not header:
            continue
        h_lower = str(header).strip().lower()
        if any(h_lower == kw or h_lower.startswith(kw) for kw in position_keywords):
            position_idx = idx
            break
    if position_idx == -1:
        for idx, header in enumerate(headers):
            if not header:
                continue
            h_lower = str(header).strip().lower()
            if any(kw in h_lower for kw in position_keywords):
                position_idx = idx
                break

    # 2. Artist column
    for idx, header in enumerate(headers):
        if idx == position_idx or not header:
            continue
        h_lower = str(header).strip().lower()
        if any(h_lower == kw for kw in artist_keywords):
            artist_idx = idx
            break
    if artist_idx == -1:
        for idx, header in enumerate(headers):
            if idx == position_idx or not header:
                continue
            h_lower = str(header).strip().lower()
            if any(kw in h_lower for kw in artist_keywords):
                artist_idx = idx
                break

    # 3. Track column
    for idx, header in enumerate(headers):
        if idx in (position_idx, artist_idx) or not header:
            continue
        h_lower = str(header).strip().lower()
        if any(h_lower == kw for kw in track_keywords):
            track_idx = idx
            break
    if track_idx == -1:
        for idx, header in enumerate(headers):
            if idx in (position_idx, artist_idx) or not header:
                continue
            h_lower = str(header).strip().lower()
            if any(kw in h_lower for kw in track_keywords):
                track_idx = idx
                break

    missing = []
    if artist_idx == -1:
        missing.append("исполнитель (artist)")
    if track_idx == -1:
        missing.append("трек (track)")

    if missing:
        raise ValueError(f"Не удалось найти колонки: {', '.join(missing)}. Заголовки таблицы: {headers}")

    return artist_idx, track_idx, position_idx


def parse_spreadsheet(file_path: str) -> list[tuple[int, str, str]]:
    """
    Parses a CSV or Excel file and returns a list of (position, artist, track).
    """
    rows = []
    ext = os.path.splitext(file_path)[1].lower()
    
    if ext == ".csv":
        import csv
        with open(file_path, "r", encoding="utf-8-sig") as f:
            reader = csv.reader(f)
            raw_rows = [row for row in reader]
    elif ext in (".xlsx", ".xlsm", ".xltx", ".xltm"):
        import openpyxl
        wb = openpyxl.load_workbook(file_path, read_only=True, data_only=True)
        sheet = wb.active
        raw_rows = []
        for row in sheet.iter_rows(values_only=True):
            raw_rows.append([str(cell) if cell is not None else "" for cell in row])
    else:
        raise ValueError(f"Неподдерживаемый формат файла: {ext}. Поддерживаются только .csv и .xlsx")

    # Find the header row in the first 10 rows
    header_idx = -1
    artist_col = -1
    track_col = -1
    pos_col = -1
    
    for i in range(min(10, len(raw_rows))):
        try:
            artist_col, track_col, pos_col = find_column_indices(raw_rows[i])
            header_idx = i
            break
        except ValueError:
            continue
            
    if header_idx == -1:
        first_row = raw_rows[0] if raw_rows else []
        raise ValueError(
            f"Не удалось найти заголовки для колонок (Исполнитель, Трек, Порядковый номер) в первых 10 строках файла. "
            f"Первая строка: {first_row}"
        )
        
    parsed_tracks = []
    for r_idx in range(header_idx + 1, len(raw_rows)):
        row = raw_rows[r_idx]
        required_len = max(artist_col, track_col)
        if pos_col != -1:
            required_len = max(required_len, pos_col)
        if not row or len(row) <= required_len:
            continue
            
        artist_val = str(row[artist_col]).strip()
        track_val = str(row[track_col]).strip()
        
        if not artist_val or not track_val:
            continue
            
        if pos_col != -1:
            pos_val_raw = str(row[pos_col]).strip()
            try:
                pos_val = int(float(pos_val_raw))
            except ValueError:
                if not pos_val_raw:
                    pos_val = r_idx - header_idx
                else:
                    digits = "".join(c for c in pos_val_raw if c.isdigit())
                    pos_val = int(digits) if digits else (r_idx - header_idx)
        else:
            pos_val = r_idx - header_idx
                
        parsed_tracks.append((pos_val, artist_val, track_val))
        
    return parsed_tracks


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="unified-resolver")
    sub = parser.add_subparsers(dest="command", required=True)

    candidates = sub.add_parser("candidates", help="dry-run candidate search")
    candidates.add_argument("query", help="Spotify URL/ID or text query")
    candidates.add_argument("--title", help="override title")
    candidates.add_argument("--artist", help="comma-separated artists")
    candidates.add_argument("--duration", type=int, help="expected duration in seconds")
    candidates.add_argument("--limit", type=int, default=5, help="limit per provider")

    resolve = sub.add_parser("resolve", help="full safe dry-run up to the last pre-download step")
    resolve.add_argument("query", help="Spotify URL/ID or text query")
    resolve.add_argument("--title", help="override title")
    resolve.add_argument("--artist", help="comma-separated artists")
    resolve.add_argument("--duration", type=int, help="expected duration in seconds")
    resolve.add_argument("--limit", type=int, default=5, help="limit per provider")
    resolve.add_argument("--no-url-check", action="store_true", help="skip public URL preflight checks")

    lyrics = sub.add_parser("lyrics", help="safe lyrics search report without printing lyrics text")
    lyrics.add_argument("query", help="Spotify URL/ID or text query")
    lyrics.add_argument("--title", help="override title")
    lyrics.add_argument("--artist", help="comma-separated artists")
    lyrics.add_argument("--duration", type=int, help="expected duration in seconds")
    lyrics.add_argument("--limit", type=int, default=10, help="number of lyric candidates to print")

    lyrics_discover = sub.add_parser("lyrics-discover", help="discover safe lyrics sources without printing lyrics text")
    lyrics_discover.add_argument("query", nargs="?", help="Spotify URL/ID, text query, or omitted with --audio-file")
    lyrics_discover.add_argument("--title", help="override title")
    lyrics_discover.add_argument("--artist", help="comma-separated artists")
    lyrics_discover.add_argument("--duration", type=int, help="expected duration in seconds")
    lyrics_discover.add_argument("--audio-file", help="audio file path used for metadata and local .lrc validation")
    lyrics_discover.add_argument("--limit", type=int, default=5, help="number of LRCLIB candidates to include")

    download = sub.add_parser("download", help="download resolved track")
    download.add_argument("query", help="Spotify URL/ID or text query")
    download.add_argument("--title", help="override title")
    download.add_argument("--artist", help="comma-separated artists")
    download.add_argument("--duration", type=int, help="expected duration in seconds")
    download.add_argument("--limit", type=int, default=5, help="limit per provider")
    download.add_argument("--output", default=".", help="output directory")
    download.add_argument("--format", default="mp3", choices=["mp3", "m4a", "flac"], help="output format")
    download.add_argument("--no-url-check", action="store_true", help="skip public URL preflight checks")
    download.add_argument("--cookies-from-browser", help="browser name for yt-dlp cookies, e.g. safari/chrome/firefox")
    download.add_argument("--duration-tolerance", type=int, help="allowed downloaded/reference duration diff in seconds")

    batch = sub.add_parser("batch", help="batch download tracks from a CSV or Excel file")
    batch.add_argument("file_path", help="path to CSV or Excel file")
    batch.add_argument("project_id", help="project identifier/folder name")
    batch.add_argument("--output", default=".", help="output directory")
    batch.add_argument("--format", default="mp3", choices=["mp3", "m4a", "flac"], help="output format")
    batch.add_argument("--limit", type=int, default=5, help="limit per provider")
    batch.add_argument("--no-url-check", action="store_true", help="skip public URL preflight checks")
    batch.add_argument("--tracks-file", help="path to JSON file containing subset of tracks to download")
    batch.add_argument("--overwrite", action="store_true", help="overwrite already downloaded tracks")
    batch.add_argument("--workers", type=int, default=2, help="maximum parallel download workers")
    batch.add_argument("--cookies-from-browser", help="browser name for yt-dlp cookies, e.g. safari/chrome/firefox")
    batch.add_argument("--duration-tolerance", type=int, help="allowed downloaded/reference duration diff in seconds")

    parse_sheet = sub.add_parser("parse-sheet", help="parse CSV or Excel file and print tracks as JSON")
    parse_sheet.add_argument("file_path", help="path to CSV or Excel file")

    args = parser.parse_args(argv)
    if args.command == "candidates":
        meta = metadata_from_args(args)
        if not meta.title:
            print("Could not resolve title. Pass --title/--artist for this input.", file=sys.stderr)
            return 2
        scored = Resolver().candidates(meta, limit_per_provider=args.limit)
        print_report(meta, scored)
        return 0
    if args.command == "resolve":
        meta = metadata_from_args(args)
        if not meta.title:
            print("Could not resolve title. Pass --title/--artist for this input.", file=sys.stderr)
            return 2
        report = Resolver().resolve(meta, limit_per_provider=args.limit, check_urls=not args.no_url_check)
        print_resolution_report(report)
        return 0
    if args.command == "lyrics":
        meta = metadata_from_args(args)
        if not meta.title:
            print("Could not resolve title. Pass --title/--artist for this input.", file=sys.stderr)
            return 2
        from .lyrics import fetch_lrclib_candidates

        candidates = fetch_lrclib_candidates(meta)
        print("TRACK")
        print(json.dumps(meta.__dict__, ensure_ascii=False, indent=2))
        print("\nLYRICS_CANDIDATES")
        for rank, candidate in enumerate(candidates[: args.limit], 1):
            payload = candidate.safe_summary()
            payload["rank"] = rank
            print(json.dumps(payload, ensure_ascii=False))
        print("\nSAFE_MODE_BOUNDARY")
        print(json.dumps({"lyrics_text_printed": False, "provider": "lrclib"}, ensure_ascii=False))
        return 0
    if args.command == "lyrics-discover":
        from .lyrics_discovery import discover_lyrics_sources, metadata_from_audio_file

        if args.audio_file:
            try:
                meta = metadata_from_audio_file(args.audio_file)
            except Exception as exc:
                print(f"Could not read audio metadata: {exc}", file=sys.stderr)
                return 2
            if args.title:
                meta.title = args.title
            if args.artist:
                meta.artists = parse_artist_list(args.artist)
            if args.duration:
                meta.duration_sec = args.duration
        else:
            if not args.query:
                print("Pass a query or --audio-file.", file=sys.stderr)
                return 2
            meta = metadata_from_args(args)
        if not meta.title:
            print("Could not resolve title. Pass --title/--artist for this input.", file=sys.stderr)
            return 2

        print("TRACK")
        print(json.dumps(meta.__dict__, ensure_ascii=False, indent=2))
        print("\nLYRICS_SOURCES")
        for source in discover_lyrics_sources(meta, audio_path=args.audio_file)[: args.limit + 8]:
            print(json.dumps(source.payload(), ensure_ascii=False))
        print("\nSAFE_MODE_BOUNDARY")
        print(
            json.dumps(
                {
                    "lyrics_text_printed": False,
                    "lyrics_text_scraped": False,
                    "mode": "source_discovery_only",
                },
                ensure_ascii=False,
            )
        )
        return 0
    if args.command == "download":
        meta = metadata_from_args(args)
        if not meta.title:
            print("Could not resolve title. Pass --title/--artist for this input.", file=sys.stderr)
            return 2
        print("[*] Resolving track candidates...")
        report = Resolver().resolve(meta, limit_per_provider=args.limit, check_urls=not args.no_url_check)
        selected = best_route(report.candidates)
        if not selected:
            print("[-] Error: No suitable candidates found to download.", file=sys.stderr)
            return 3
        rank, scored_candidate = selected
        print(f"[+] Selected candidate: Rank {rank}, Score {scored_candidate.score} ({scored_candidate.candidate.source})")
        from .downloader import Downloader
        dl = Downloader(
            output_dir=args.output,
            format=args.format,
            cookies_from_browser=args.cookies_from_browser,
            duration_tolerance=args.duration_tolerance,
        )
        success = dl.download(report.track, report.candidates)
        if success:
            quality, reasons = download_quality_summary(scored_candidate)
            detail = ", ".join(reasons) if reasons else "clean_match"
            print(f"[?] Download quality: {quality}; {detail}")
        return 0 if success else 4
    if args.command == "parse-sheet":
        try:
            tracks = parse_spreadsheet(args.file_path)
            json_data = [{"pos": t[0], "artist": t[1], "title": t[2]} for t in tracks]
            print(json.dumps(json_data, ensure_ascii=False))
            return 0
        except Exception as e:
            print(json.dumps({"error": str(e)}, ensure_ascii=False), file=sys.stderr)
            return 5

    if args.command == "batch":
        try:
            if args.tracks_file:
                with open(args.tracks_file, "r", encoding="utf-8") as f:
                    data = json.load(f)
                tracks = [(item["pos"], item["artist"], item["title"]) for item in data]
            else:
                tracks = parse_spreadsheet(args.file_path)
        except Exception as e:
            print(f"[-] Error parsing tracks: {e}", file=sys.stderr)
            return 5
            
        if not tracks:
            print("[-] No tracks found in the spreadsheet.", file=sys.stderr)
            return 6
            
        print(f"[+] Found {len(tracks)} tracks to download in project '{args.project_id}'.")
        
        from .downloader import Downloader
        from concurrent.futures import ThreadPoolExecutor, as_completed
        
        dl = Downloader(
            output_dir=args.output,
            format=args.format,
            cookies_from_browser=args.cookies_from_browser,
            duration_tolerance=args.duration_tolerance,
        )
        
        def process_track(track_info) -> bool:
            pos, artist, title = track_info
            
            safe_title = "".join(c for c in title if c.isalnum() or c in " -_().")
            safe_artist = "".join(c for c in artist if c.isalnum() or c in " -_().")
            
            track_folder = f"{pos:02d}. {safe_artist} - {safe_title}"
            override_dir = os.path.join(args.output, args.project_id, track_folder)
            
            # Check if file already exists to allow safe resuming
            final_file = os.path.join(override_dir, f"{safe_artist} - {safe_title}.{args.format}")
            if not args.overwrite and os.path.exists(final_file):
                print(f"[#] Track {pos} '{artist} - {title}' already downloaded, skipping.")
                return True

            meta = TrackMetadata(
                title=title,
                artists=[artist],
            )
            
            print(f"[*] [{pos}] Resolving track candidates for '{artist} - {title}'...")
            try:
                report = Resolver().resolve(meta, limit_per_provider=args.limit, check_urls=not args.no_url_check)
                selected = best_route(report.candidates)
                if not selected:
                    print(f"[-] [{pos}] Error: No suitable candidates found to download for '{artist} - {title}'.", file=sys.stderr)
                    return False
                    
                rank, scored_candidate = selected
                print(f"[+] [{pos}] Selected candidate: Rank {rank}, Score {scored_candidate.score} ({scored_candidate.candidate.source})")
                
                success = dl.download(report.track, report.candidates, override_dir=override_dir)
                if success:
                    print(f"[+] [{pos}] Successfully downloaded and processed '{artist} - {title}'.")
                    quality, reasons = download_quality_summary(scored_candidate)
                    detail = ", ".join(reasons) if reasons else "clean_match"
                    print(f"[?] [{pos}] Download quality: {quality}; {detail}")
                    return True
                else:
                    print(f"[-] [{pos}] Download failed for '{artist} - {title}'.", file=sys.stderr)
                    return False
            except Exception as e:
                print(f"[-] [{pos}] Unexpected error processing '{artist} - {title}': {e}", file=sys.stderr)
                return False

        success_count = 0
        max_workers = getattr(args, 'workers', 2)
        print(f"[*] Starting parallel download with {max_workers} threads...")
        with ThreadPoolExecutor(max_workers=max_workers) as executor:
            futures = {executor.submit(process_track, track): track for track in tracks}
            for future in as_completed(futures):
                track = futures[future]
                pos, artist, title = track
                try:
                    success = future.result()
                    if success:
                        success_count += 1
                except Exception as e:
                    print(f"[-] Thread error processing track {pos} '{artist} - {title}': {e}", file=sys.stderr)
                
        print(f"\n[+] Batch process finished. Successfully downloaded {success_count}/{len(tracks)} tracks.")
        return 0 if success_count == len(tracks) else 7


if __name__ == "__main__":
    raise SystemExit(main())
