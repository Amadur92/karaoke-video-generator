from __future__ import annotations

from dataclasses import dataclass
import json
import re
import urllib.parse
import urllib.request

from .http import DEFAULT_UA, safe_urlopen
from .models import TrackMetadata
from .scoring import duration_score, normalize, partial_word_match, ratio


@dataclass
class LyricsCandidate:
    provider: str
    provider_id: str | None
    title: str
    artist: str
    album: str | None
    duration_sec: int | None
    synced: str
    plain: str
    query_variant: str
    score: float = 0.0
    flags: list[str] | None = None
    variant_artist: str | None = None
    variant_title: str | None = None

    @property
    def has_synced(self) -> bool:
        return bool(self.synced.strip())

    @property
    def has_plain(self) -> bool:
        return bool(self.plain.strip())

    def safe_summary(self) -> dict:
        return {
            "provider": self.provider,
            "provider_id": self.provider_id,
            "score": self.score,
            "title": self.title,
            "artist": self.artist,
            "album": self.album,
            "duration_sec": self.duration_sec,
            "has_synced": self.has_synced,
            "has_plain": self.has_plain,
            "query_variant": self.query_variant,
            "flags": self.flags or [],
        }


def _get_json(url: str) -> object:
    req = urllib.request.Request(url, headers={"User-Agent": DEFAULT_UA})
    with safe_urlopen(req, timeout=8) as resp:
        return json.loads(resp.read().decode("utf-8"))


def _clean_title(value: str) -> str:
    value = re.sub(r"\([^)]*\)|\[[^]]*\]", " ", value)
    value = re.sub(r"\s+", " ", value)
    return value.strip()


def _search_variants(track: TrackMetadata) -> list[tuple[str, str]]:
    seen: set[tuple[str, str]] = set()
    variants: list[tuple[str, str]] = []

    def add(title: str, artist: str) -> None:
        title = title.strip()
        artist = artist.strip()
        if not title or not artist:
            return
        key = (title.casefold(), artist.casefold())
        if key in seen:
            return
        seen.add(key)
        variants.append((title, artist))

    title = track.title
    artist = track.primary_artist
    add(title, artist)

    clean_title = _clean_title(title)
    add(clean_title, artist)

    norm_title = normalize(title)
    norm_artist = normalize(artist)
    if norm_title == "privet":
        add("Привет", artist)
    if norm_artist in {"yulia savicheva", "yuliya savicheva", "julia savicheva"}:
        add(title, "Юлия Савичева")
        if norm_title == "privet":
            add("Привет", "Юлия Савичева")

    # Rules for complex/misspelled tracks in package 177
    if "ай-ай-ай" in title.lower() or "ай яй яй" in title.lower():
        alt_title = title.replace("ай-ай-ай", "ай-яй-яй").replace("ай яй яй", "ай-яй-яй")
        add(alt_title, artist)
    if "дубцов" in artist.lower() and "вспоминать" in title.lower():
        add("Москва-Нева", "Ирина Дубцова")
    if "агутин" in artist.lower() and "ай" in title.lower():
        add("Ай-яй-яй", "Леонид Агутин")
    if "утренняя гимнастика" in title.lower():
        add("Утренняя гимнастика", "Владимир Высоцкий")
    if "глюкоza" in artist.lower() or "glukoza" in artist.lower():
        add(title, "Глюкоза")
        add(title, "Glukoza")
    if "артур пирожков" in artist.lower() or "arthur pirozhkov" in artist.lower():
        add(title, "Arthur Pirozhkov")
        add(title, "Артур Пирожков")
    if "чумаков" in artist.lower() or "chumakov" in artist.lower():
        add(title, "Чумаков Алексей")
        add(title, "Алексей Чумаков")
    if "кадышева" in artist.lower():
        add(title, "Золотое Кольцо")
        add(title, "Золотое кольцо")

    return variants


def _candidate_from_lrclib(
    item: dict,
    query_variant: str,
    variant_artist: str | None = None,
    variant_title: str | None = None
) -> LyricsCandidate:
    duration = item.get("duration")
    if duration is not None:
        duration = round(float(duration))
    return LyricsCandidate(
        provider="lrclib",
        provider_id=str(item.get("id")) if item.get("id") is not None else None,
        title=item.get("trackName") or item.get("name") or "",
        artist=item.get("artistName") or "",
        album=item.get("albumName") or None,
        duration_sec=duration,
        synced=item.get("syncedLyrics") or "",
        plain=item.get("plainLyrics") or "",
        query_variant=query_variant,
        variant_artist=variant_artist,
        variant_title=variant_title,
    )


def _score_candidate(track: TrackMetadata, candidate: LyricsCandidate) -> LyricsCandidate:
    variant_artist = candidate.variant_artist
    variant_title = candidate.variant_title

    title_score_orig = max(ratio(track.title, candidate.title), partial_word_match(track.title, candidate.title))
    if variant_title:
        title_score_variant = max(ratio(variant_title, candidate.title), partial_word_match(variant_title, candidate.title))
        title_score = max(title_score_orig, title_score_variant)
    else:
        title_score = title_score_orig

    artist_score_orig = max(ratio(track.primary_artist, candidate.artist), partial_word_match(track.primary_artist, candidate.artist))
    if variant_artist:
        artist_score_variant = max(ratio(variant_artist, candidate.artist), partial_word_match(variant_artist, candidate.artist))
        artist_score = max(artist_score_orig, artist_score_variant)
    else:
        artist_score = artist_score_orig

    dur_score = duration_score(track.duration_sec, candidate.duration_sec)
    album_score = ratio(track.album or "", candidate.album or "") if track.album and candidate.album else 50.0
    score = title_score * 0.42 + artist_score * 0.32 + dur_score * 0.18 + album_score * 0.08
    flags: list[str] = []

    # Safeguard against synced lyrics mismatch (timed lyrics drift)
    if track.duration_sec and candidate.duration_sec:
        diff = abs(track.duration_sec - candidate.duration_sec)
        if diff > 4:
            if candidate.synced:
                candidate.synced = ""  # Drop timed lyrics
                flags.append("synced_disabled_by_duration_mismatch")
        if diff > 20:
            score -= 30  # Heavy penalty for different versions
            flags.append(f"extreme_duration_diff:{diff}s")

    if candidate.has_synced:
        score += 5
    expected_title_tokens = set(normalize(track.title).split())
    actual_title_tokens = set(normalize(candidate.title).split())
    extra_title_tokens = actual_title_tokens - expected_title_tokens
    if expected_title_tokens and expected_title_tokens.issubset(actual_title_tokens) and extra_title_tokens:
        penalty = min(22, 8 * len(extra_title_tokens))
        score -= penalty
        flags.append(f"extra_title_tokens:{len(extra_title_tokens)}")
    if title_score < 55:
        score -= 35
        flags.append("weak_title_match")
    if artist_score < 45:
        score -= 30
        flags.append("weak_artist_match")
    if track.duration_sec and candidate.duration_sec and abs(track.duration_sec - candidate.duration_sec) > 20 and not any(f.startswith("extreme_duration_diff:") for f in flags):
        score -= 20
        flags.append(f"duration_diff:{abs(track.duration_sec - candidate.duration_sec)}s")

    candidate.score = round(max(score, 0.0), 2)
    candidate.flags = flags
    return candidate


def fetch_lrclib_candidates(track: TrackMetadata) -> list[LyricsCandidate]:
    candidates: list[LyricsCandidate] = []
    seen_ids: set[str] = set()

    variants = _search_variants(track)
    for idx, (title, artist) in enumerate(variants):
        query_label = f"{artist} - {title}"
        params = {
            "artist_name": artist,
            "track_name": title,
        }
        if track.album:
            params["album_name"] = track.album
        if track.duration_sec:
            params["duration"] = str(track.duration_sec)

        if idx == 0:
            try:
                exact_url = "https://lrclib.net/api/get?" + urllib.parse.urlencode(params)
                exact = _get_json(exact_url)
                if isinstance(exact, dict):
                    candidate = _candidate_from_lrclib(exact, query_label, variant_artist=artist, variant_title=title)
                    if candidate.provider_id and candidate.provider_id not in seen_ids:
                        seen_ids.add(candidate.provider_id)
                        candidates.append(candidate)
            except Exception:
                pass

        # Сначала пробуем структурированный поиск по полям
        results = None
        try:
            search_url = "https://lrclib.net/api/search?" + urllib.parse.urlencode(
                {"artist_name": artist, "track_name": title}
            )
            results = _get_json(search_url)
        except Exception:
            pass

        # Если структурированный поиск ничего не дал, делаем fallback на поиск по q
        if not results:
            try:
                q_url = "https://lrclib.net/api/search?" + urllib.parse.urlencode(
                    {"q": f"{artist} {title}"}
                )
                results = _get_json(q_url)
            except Exception:
                pass

        try:
            if isinstance(results, list):
                for item in results:
                    if not isinstance(item, dict):
                        continue
                    candidate = _candidate_from_lrclib(item, query_label, variant_artist=artist, variant_title=title)
                    if candidate.provider_id and candidate.provider_id in seen_ids:
                        continue
                    if candidate.provider_id:
                        seen_ids.add(candidate.provider_id)
                    candidates.append(candidate)
        except Exception:
            pass

    return sorted(
        (_score_candidate(track, candidate) for candidate in candidates if candidate.has_synced or candidate.has_plain),
        key=lambda candidate: candidate.score,
        reverse=True,
    )


def resolve_lyrics(track: TrackMetadata, min_score: float = 62.0) -> LyricsCandidate | None:
    for candidate in fetch_lrclib_candidates(track):
        if candidate.score >= min_score:
            return candidate
    return None
