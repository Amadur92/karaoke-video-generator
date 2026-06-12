from __future__ import annotations

import difflib
import re

from .models import Candidate, ScoredCandidate, TrackMetadata


BAD_TITLE_MARKERS = [
    "karaoke",
    "instrumental",
    "slowed",
    "sped up",
    "nightcore",
    "8d",
    "lyrics",
    "cover",
    "live",
    "remix",
    "kaver",
    "remiks",
    "layv",
    "minus",
    "minusovka",
]

CYR_TO_LAT = str.maketrans(
    {
        "а": "a",
        "б": "b",
        "в": "v",
        "г": "g",
        "д": "d",
        "е": "e",
        "ё": "e",
        "ж": "zh",
        "з": "z",
        "и": "i",
        "й": "y",
        "к": "k",
        "л": "l",
        "м": "m",
        "н": "n",
        "о": "o",
        "п": "p",
        "р": "r",
        "с": "s",
        "т": "t",
        "у": "u",
        "ф": "f",
        "х": "h",
        "ц": "ts",
        "ч": "ch",
        "ш": "sh",
        "щ": "sch",
        "ъ": "",
        "ы": "y",
        "ь": "",
        "э": "e",
        "ю": "yu",
        "я": "ya",
    }
)


def normalize(value: str) -> str:
    value = value.lower()
    value = value.translate(CYR_TO_LAT)
    value = re.sub(r"\([^)]*\)|\[[^]]*\]", " ", value)
    value = value.replace("&", " and ")
    value = re.sub(r"[^a-zа-яё0-9]+", " ", value, flags=re.IGNORECASE)
    return " ".join(value.split())


def ratio(a: str, b: str) -> float:
    a_norm = normalize(a)
    b_norm = normalize(b)
    if not a_norm or not b_norm:
        return 0.0
    return difflib.SequenceMatcher(None, a_norm, b_norm).ratio() * 100.0


def partial_word_match(needle: str, haystack: str) -> float:
    words = [w for w in normalize(needle).split() if len(w) > 1]
    hay = normalize(haystack)
    if not words or not hay:
        return 0.0
    hits = sum(1 for word in words if word in hay or difflib.SequenceMatcher(None, word, hay).ratio() > 0.8)
    return (hits / len(words)) * 100.0


def duration_score(expected: int | None, actual: int | None) -> float:
    if not expected or not actual:
        return 50.0
    diff = abs(expected - actual)
    if diff <= 2:
        return 100.0
    if diff <= 8:
        return 90.0 - diff
    if diff <= 20:
        return max(35.0, 80.0 - diff * 2)
    return 0.0


def score_candidate(track: TrackMetadata, candidate: Candidate) -> ScoredCandidate:
    title_score = max(ratio(track.title, candidate.title), partial_word_match(track.title, candidate.title))
    artist_target = " ".join(track.artists)
    artist_candidate = " ".join(candidate.artists)
    
    # Fix bug: only match artist against title for YouTube sources where format is Artist - Title
    if "youtube" in candidate.source:
        artist_score = max(ratio(artist_target, artist_candidate), partial_word_match(artist_target, candidate.title))
    else:
        artist_score = ratio(artist_target, artist_candidate)

    dur_score = duration_score(track.duration_sec, candidate.duration_sec)
    album_score = ratio(track.album or "", candidate.album or "") if track.album and candidate.album else 50.0

    score = title_score * 0.40 + artist_score * 0.30 + dur_score * 0.20 + album_score * 0.10
    flags: list[str] = []

    cand_title_markers = re.sub(r"[^a-zа-яё0-9]+", " ", candidate.title.lower().translate(CYR_TO_LAT), flags=re.IGNORECASE)
    track_title_markers = re.sub(r"[^a-zа-яё0-9]+", " ", track.title.lower().translate(CYR_TO_LAT), flags=re.IGNORECASE)
    
    # Critical bad markers (karaoke, minus, instrumental, live, cover) that should never be downloaded
    CRITICAL_BAD_MARKERS = [
        "karaoke",
        "instrumental",
        "minus",
        "minusovka",
        "backing track",
        "live",
        "layv",
        "concert",
        "концерт",
        "acoustic",
        "акустика",
        "cover",
        "кавер",
        "kaver",
    ]
    
    for marker in CRITICAL_BAD_MARKERS:
        if marker in cand_title_markers and marker not in track_title_markers:
            score -= 80
            flags.append(f"critical_marker:{marker}")

    for marker in BAD_TITLE_MARKERS:
        if marker in CRITICAL_BAD_MARKERS:
            continue
        if marker in cand_title_markers and marker not in track_title_markers:
            score -= 18
            flags.append(f"marker:{marker}")

    if track.duration_sec and candidate.duration_sec and abs(track.duration_sec - candidate.duration_sec) > 12:
        flags.append(f"duration_diff:{abs(track.duration_sec - candidate.duration_sec)}s")
    if track.duration_sec and candidate.duration_sec and abs(track.duration_sec - candidate.duration_sec) > 20:
        score -= 20

    if artist_score < 42:
        score -= 55  # Critical penalty for completely different artists!
        flags.append("critical_artist_mismatch")
    elif artist_score < 50:
        score -= 25
        flags.append("weak_artist_match")
    if title_score < 50:
        score -= 25
        flags.append("weak_title_match")

    return ScoredCandidate(
        candidate=candidate,
        score=round(max(score, 0.0), 2),
        title_score=round(title_score, 2),
        artist_score=round(artist_score, 2),
        duration_score=round(dur_score, 2),
        album_score=round(album_score, 2),
        flags=flags,
    )
