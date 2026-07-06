from __future__ import annotations

import difflib

from .markers import find_critical_markers, find_soft_markers
from .models import Candidate, ScoredCandidate, TrackMetadata
from .text_norm import CYR_TO_LAT, normalize  # re-export для обратной совместимости


def ratio(a: str, b: str) -> float:
    a_norm = normalize(a)
    b_norm = normalize(b)
    if not a_norm or not b_norm:
        return 0.0
    return difflib.SequenceMatcher(None, a_norm, b_norm).ratio() * 100.0


def partial_word_match(needle: str, haystack: str) -> float:
    """
    Доля слов из needle, присутствующих в haystack (по точному или
    нечёткому совпадению слово-к-слову).

    Раньше здесь был баг: нечёткое сравнение шло word-vs-весь-haystack,
    что давало ложные срабатывания для коротких слов. Теперь каждое слово
    needle сравнивается с каждым словом haystack по отдельности.
    """
    needle_words = [w for w in normalize(needle).split() if len(w) > 1]
    hay_words = set(normalize(haystack).split())
    if not needle_words or not hay_words:
        return 0.0
    hits = 0
    for word in needle_words:
        if word in hay_words:
            hits += 1
            continue
        # нечёткое слово-к-слову: берём лучшее совпадение по словам haystack
        best = 0.0
        for hay_word in hay_words:
            if abs(len(hay_word) - len(word)) > max(len(word), 3):
                continue
            sim = difflib.SequenceMatcher(None, word, hay_word).ratio()
            if sim > best:
                best = sim
        if best > 0.8:
            hits += 1
    return (hits / len(needle_words)) * 100.0


def duration_score(expected: int | None, actual: int | None) -> float:
    # Если эталонная длительность неизвестна — нет смысла штрафовать за неё.
    if not expected:
        return 50.0
    # Если эталон известен, а у кандидата длительности нет — это подозрительно:
    # кандидат не может быть проверен на «правильную версию», поэтому даём
    # умеренный штраф (35), уступающий любому кандидату с близкой длительностью.
    # Раньше тут было 50 (нейтрально), и источники без длительности (JioSaavn,
    # Deezer metadata-only) обходили YouTube с реальной, но чуть отличающейся
    # длительностью — выбиралась не та версия.
    if not actual:
        return 35.0
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

    # Длительность — критичный сигнал выбора правильной версии: у популярного
    # трека на YouTube/SoundCloud есть десятки видео с одинаковым title/artist,
    # но разной длиной (radio edit, extended, live, mashup). Поэтому вес
    # длительности поднят с 0.20 до 0.30, а при сильном расхождении (>25с)
    # дополнительно штрафуем, чтобы версия «правильной длины» всегда побеждала.
    score = title_score * 0.35 + artist_score * 0.25 + dur_score * 0.30 + album_score * 0.10
    flags: list[str] = []

    # Критические bad-маркеры (караоке, минус, лайв, ремикс, кавер ...) —
    # жёсткое исключение: score прижимается к 0, чтобы версия с маркером
    # НИКОГДА не могла обойти оригинал в ранжировании (даже при идеальном
    # совпадении title/artist/duration). Раньше был штраф −80, и при слабой
    # конкуренции (нет студийного кандидата) live мог формально «выиграть».
    # Флаг critical_marker:* по-прежнему выставляется, чтобы download-loop и
    # YouTube-fallback могли применить свою логику пропуска.
    critical_hits = find_critical_markers(candidate.title, track.title)
    for marker in critical_hits:
        flags.append(f"critical_marker:{marker}")
    if critical_hits:
        score = 0.0

    soft_hits = find_soft_markers(candidate.title, track.title)
    for marker in soft_hits:
        score -= 12
        flags.append(f"marker:{marker}")

    if track.duration_sec and candidate.duration_sec and abs(track.duration_sec - candidate.duration_sec) > 12:
        flags.append(f"duration_diff:{abs(track.duration_sec - candidate.duration_sec)}s")
    if track.duration_sec and candidate.duration_sec and abs(track.duration_sec - candidate.duration_sec) > 20:
        score -= 20
    # Сильное расхождение (>25с) почти всегда означает другую версию
    # (extended mix, live, mashup). Дополнительный штраф гарантирует, что
    # при наличии кандидата правильной длины он победит.
    if track.duration_sec and candidate.duration_sec and abs(track.duration_sec - candidate.duration_sec) > 25:
        score -= 30
        flags.append("large_duration_mismatch")

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
