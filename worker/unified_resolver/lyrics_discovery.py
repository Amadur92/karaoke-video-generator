from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import json
import re
import subprocess
import urllib.parse
import urllib.request

from .http import DEFAULT_UA, check_public_url
from .lyrics import fetch_lrclib_candidates
from .models import TrackMetadata
from .scoring import normalize, ratio


@dataclass
class LyricsSource:
    provider: str
    status: str
    public_url: str | None = None
    provider_id: str | None = None
    title: str | None = None
    artist: str | None = None
    confidence: float | None = None
    notes: list[str] | None = None
    has_synced: bool | None = None
    has_plain: bool | None = None
    local_path: str | None = None

    def payload(self) -> dict:
        return {
            "provider": self.provider,
            "status": self.status,
            "public_url": self.public_url,
            "provider_id": self.provider_id,
            "title": self.title,
            "artist": self.artist,
            "confidence": self.confidence,
            "notes": self.notes or [],
            "has_synced": self.has_synced,
            "has_plain": self.has_plain,
            "local_path": self.local_path,
        }


CYR_TO_LAT_WORDS = {
    "артур": "artur",
    "пирожков": "pirozhkov",
    "она": "ona",
    "решила": "reshila",
    "сдаться": "sdatsya",
}


def safe_slug(value: str) -> str:
    words = []
    for token in normalize(value).split():
        words.append(CYR_TO_LAT_WORDS.get(token, token))
    return "-".join(words)


def latin_slug(value: str) -> str:
    slug = safe_slug(value)
    slug = re.sub(r"[^a-z0-9]+", "-", slug.lower())
    return slug.strip("-")


def metadata_from_audio_file(path: str) -> TrackMetadata:
    cmd = [
        "ffprobe",
        "-v",
        "error",
        "-show_entries",
        "format=duration:format_tags",
        "-of",
        "json",
        path,
    ]
    proc = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or "ffprobe failed")
    data = json.loads(proc.stdout)
    fmt = data.get("format") or {}
    tags = fmt.get("tags") or {}
    duration = fmt.get("duration")
    return TrackMetadata(
        title=tags.get("title") or Path(path).stem,
        artists=[tags.get("artist") or ""],
        duration_sec=round(float(duration)) if duration else None,
        album=tags.get("album"),
    )


def validate_local_lrc(path: Path) -> LyricsSource:
    notes: list[str] = []
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except Exception as exc:
        return LyricsSource("local-lrc", "error", local_path=str(path), notes=[str(exc)])

    stripped = text.strip()
    nonempty_lines = [line for line in stripped.splitlines() if line.strip()]
    has_timestamps = bool(re.search(r"\[\d{1,2}:\d{2}(?:\.\d{1,3})?\]", text))

    if len(stripped) < 300:
        notes.append("too_short")
    if len(nonempty_lines) <= 1:
        notes.append("single_line")
    if not has_timestamps:
        notes.append("no_timestamps")
    if re.search(r"\bcontributors\b.*\blyrics\b", stripped, re.IGNORECASE):
        notes.append("bad_scrape_stub")

    status = "ok"
    if "bad_scrape_stub" in notes or ("too_short" in notes and "single_line" in notes):
        status = "invalid_stub"
    elif notes:
        status = "suspicious"

    return LyricsSource(
        provider="local-lrc",
        status=status,
        local_path=str(path),
        notes=notes,
        has_synced=has_timestamps,
        has_plain=bool(stripped),
    )


def discover_local_lrc(audio_path: str | None) -> list[LyricsSource]:
    if not audio_path:
        return []
    path = Path(audio_path)
    candidates = [
        path.with_suffix(".lrc"),
        path.parent / f"{path.stem}.txt",
    ]
    return [validate_local_lrc(candidate) for candidate in candidates if candidate.exists()]


def discover_lrclib(track: TrackMetadata, limit: int = 5) -> list[LyricsSource]:
    candidates = fetch_lrclib_candidates(track)
    if not candidates:
        return [LyricsSource("lrclib", "not_found", notes=["no_candidates"])]
    sources: list[LyricsSource] = []
    for candidate in candidates[:limit]:
        sources.append(
            LyricsSource(
                provider="lrclib",
                status="candidate",
                provider_id=candidate.provider_id,
                title=candidate.title,
                artist=candidate.artist,
                confidence=candidate.score,
                notes=candidate.flags or [],
                has_synced=candidate.has_synced,
                has_plain=candidate.has_plain,
            )
        )
    return sources


def discover_lyricfind(track: TrackMetadata) -> LyricsSource:
    artist_slug = safe_slug(track.primary_artist)
    title_slug = safe_slug(track.title)
    if not artist_slug or not title_slug:
        return LyricsSource("lyricfind", "skipped", notes=["missing_artist_or_title"])
    url = f"https://lyrics.lyricfind.com/lyrics/{artist_slug}-{title_slug}"
    ok = check_public_url(url, timeout=8)
    return LyricsSource(
        provider="lyricfind",
        status="public_url_found" if ok else "not_found",
        public_url=url,
        title=track.title,
        artist=track.primary_artist,
        notes=["licensed_provider", "url_only_no_scrape"],
        has_synced=None,
        has_plain=None,
    )


def discover_apple_catalog(track: TrackMetadata) -> list[LyricsSource]:
    term = f"{track.primary_artist} {track.title}".strip()
    if not term:
        return [LyricsSource("apple-itunes", "skipped", notes=["missing_query"])]
    url = "https://itunes.apple.com/search?" + urllib.parse.urlencode(
        {"term": term, "entity": "song", "limit": "5", "country": "US"}
    )
    try:
        req = urllib.request.Request(url, headers={"User-Agent": DEFAULT_UA})
        with urllib.request.urlopen(req, timeout=10) as resp:
            data = json.loads(resp.read().decode("utf-8"))
    except Exception as exc:
        return [LyricsSource("apple-itunes", "error", notes=[str(exc)])]

    results = data.get("results") or []
    sources: list[LyricsSource] = []
    for item in results:
        item_title = item.get("trackName") or ""
        item_artist = item.get("artistName") or ""
        confidence = round((ratio(track.title, item_title) * 0.55) + (ratio(track.primary_artist, item_artist) * 0.45), 2)
        if confidence < 55:
            continue
        sources.append(
            LyricsSource(
                provider="apple-itunes",
                status="catalog_match",
                public_url=item.get("trackViewUrl"),
                provider_id=str(item.get("trackId")) if item.get("trackId") else None,
                title=item_title,
                artist=item_artist,
                confidence=confidence,
                notes=["catalog_match", "lyrics_may_be_available_in_apple_or_shazam"],
            )
        )
    if not sources:
        return [LyricsSource("apple-itunes", "not_found", notes=["no_strong_catalog_match"])]
    return sorted(sources, key=lambda item: item.confidence or 0, reverse=True)


def discover_shazam_from_apple(apple_sources: list[LyricsSource]) -> list[LyricsSource]:
    sources: list[LyricsSource] = []
    for source in apple_sources:
        if source.status != "catalog_match" or not source.provider_id:
            continue
        slug = safe_slug(source.title or "")
        url = f"https://www.shazam.com/song/{source.provider_id}/{slug}" if slug else f"https://www.shazam.com/song/{source.provider_id}"
        ok = check_public_url(url, timeout=8)
        sources.append(
            LyricsSource(
                provider="shazam",
                status="public_url_found" if ok else "candidate_url_unverified",
                public_url=url,
                provider_id=source.provider_id,
                title=source.title,
                artist=source.artist,
                confidence=source.confidence,
                notes=["derived_from_apple_track_id", "url_only_no_scrape"],
            )
        )
    return sources


def discover_genius_url(track: TrackMetadata) -> LyricsSource:
    artist_slug = "-".join(part.capitalize() for part in latin_slug(track.primary_artist).split("-"))
    title_slug = "-".join(part.capitalize() for part in latin_slug(track.title).split("-"))
    if not artist_slug or not title_slug:
        return LyricsSource("genius", "skipped", notes=["missing_artist_or_title"])
    url = f"https://genius.com/{artist_slug}-{title_slug}-lyrics"
    ok = check_public_url(url, timeout=8)
    return LyricsSource(
        provider="genius",
        status="public_url_found" if ok else "candidate_url_unverified",
        public_url=url,
        title=track.title,
        artist=track.primary_artist,
        notes=["url_only_no_api_scrape"],
    )


def discover_musixmatch_url(track: TrackMetadata) -> LyricsSource:
    query = f"{track.primary_artist} {track.title}".strip()
    if not query:
        return LyricsSource("musixmatch", "skipped", notes=["missing_query"])
    url = "https://www.musixmatch.com/search/" + urllib.parse.quote(query)
    ok = check_public_url(url, timeout=8)
    return LyricsSource(
        provider="musixmatch",
        status="search_url_found" if ok else "candidate_url_unverified",
        public_url=url,
        title=track.title,
        artist=track.primary_artist,
        notes=["licensed_provider_if_using_api", "search_url_only_no_scrape"],
    )


def discover_youtube_lyrics_videos(track: TrackMetadata, limit: int = 3) -> list[LyricsSource]:
    try:
        from youtube_dl import YoutubeDL
    except Exception:
        return [LyricsSource("youtube-lyrics-video", "skipped", notes=["youtube_dl_not_available"])]

    query = f"ytsearch{limit}:{track.primary_artist} {track.title} lyrics"
    options = {
        "quiet": True,
        "skip_download": True,
        "extract_flat": True,
        "noplaylist": True,
        "ignoreerrors": True,
        "nocheckcertificate": True,
    }
    try:
        with YoutubeDL(options) as ydl:
            info = ydl.extract_info(query, download=False)
    except Exception as exc:
        return [LyricsSource("youtube-lyrics-video", "error", notes=[str(exc)])]

    sources: list[LyricsSource] = []
    for item in (info or {}).get("entries") or []:
        if not item:
            continue
        video_title = item.get("title") or ""
        video_id = item.get("id")
        confidence = round((ratio(track.title, video_title) * 0.65) + (ratio(track.primary_artist, video_title) * 0.35), 2)
        sources.append(
            LyricsSource(
                provider="youtube-lyrics-video",
                status="candidate",
                public_url=f"https://www.youtube.com/watch?v={video_id}" if video_id else item.get("webpage_url"),
                provider_id=video_id,
                title=video_title,
                artist=item.get("uploader"),
                confidence=confidence,
                notes=["public_video_candidate", "lyrics_text_not_extracted"],
                has_synced=None,
                has_plain=None,
            )
        )
    if not sources:
        return [LyricsSource("youtube-lyrics-video", "not_found", notes=["no_candidates"])]
    return sorted(sources, key=lambda item: item.confidence or 0, reverse=True)


def discover_lyrics_sources(track: TrackMetadata, audio_path: str | None = None) -> list[LyricsSource]:
    sources: list[LyricsSource] = []
    sources.extend(discover_local_lrc(audio_path))
    sources.extend(discover_lrclib(track))
    sources.append(discover_lyricfind(track))
    apple_sources = discover_apple_catalog(track)
    sources.extend(apple_sources)
    sources.extend(discover_shazam_from_apple(apple_sources))
    sources.append(discover_genius_url(track))
    sources.append(discover_musixmatch_url(track))
    sources.extend(discover_youtube_lyrics_videos(track))
    return sources
