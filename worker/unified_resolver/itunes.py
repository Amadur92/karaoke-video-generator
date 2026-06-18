"""
Enrichment метаданных трека через публичный iTunes Search API.

iTunes хорошо индексирует русскоязычные релизы и используется как fallback,
когда Deezer не нашёл метаданные. API публичный, без ключей, отдаёт длительность,
обложку, название альбома и каталожный trackId.

ISRC напрямую iTunes Search API не отдаёт — он доступен только через Catalog API
(требует токен Apple Music). Поэтому enrichment покрывает длительность/обложку/альбом,
а ISRC по-прежнему приходит из Deezer/Spotify.
"""
from __future__ import annotations

import json
import urllib.parse
import urllib.request
from typing import Optional

from .http import DEFAULT_UA, safe_urlopen
from .models import TrackMetadata
from .text_norm import normalize


# страны, по которым ищем — RU покрывает русские релизы, US — международные.
# Запросы идут последовательно, берётся первый результат с хорошим совпадением.
_DEFAULT_COUNTRIES = ("RU", "US")


def _itunes_search(term: str, country: str, limit: int = 5) -> list[dict]:
    url = "https://itunes.apple.com/search?" + urllib.parse.urlencode(
        {
            "term": term,
            "entity": "song",
            "limit": str(limit),
            "country": country,
        }
    )
    req = urllib.request.Request(url, headers={"User-Agent": DEFAULT_UA})
    try:
        with safe_urlopen(req, timeout=10) as resp:
            data = json.loads(resp.read().decode("utf-8"))
    except Exception:
        return []
    return data.get("results") or []


def _pick_best(track: TrackMetadata, results: list[dict]) -> Optional[dict]:
    """
    Выбирает лучший результат среди ответа iTunes:
    сравнивает нормализованное название/артиста и предпочитает точное совпадение
    по длительности. Отсеивает ремиксы, если искомый трек не ремикс.
    """
    if not results:
        return None

    want_remix = "remix" in normalize(track.title) or "remiks" in normalize(track.title)
    title_norm = normalize(track.title)
    artist_norm = normalize(track.primary_artist)

    best = None
    best_score = -1.0
    for item in results:
        item_title = item.get("trackName") or ""
        item_artist = item.get("artistName") or ""
        item_title_norm = normalize(item_title)
        is_remix = "remix" in item_title_norm or "remiks" in item_title_norm

        # Если ищем НЕ ремикс, пропускаем ремиксы-кандидаты.
        if not want_remix and is_remix:
            continue

        # Базовая оценка: совпадение по подстроке токенов названия и артиста.
        t_tokens = set(title_norm.split())
        it_tokens = set(item_title_norm.split())
        title_overlap = (
            len(t_tokens & it_tokens) / len(t_tokens) if t_tokens else 0.0
        )
        a_tokens = set(artist_norm.split())
        ia_tokens = set(normalize(item_artist).split())
        artist_overlap = (
            len(a_tokens & ia_tokens) / len(a_tokens) if a_tokens else 0.0
        )
        score = title_overlap * 0.6 + artist_overlap * 0.4

        # Бонус за близость длительности.
        dur = item.get("trackTimeMillis")
        if track.duration_sec and dur:
            diff = abs(track.duration_sec - round(dur / 1000))
            if diff <= 3:
                score += 0.25
            elif diff <= 8:
                score += 0.10
            elif diff > 20:
                score -= 0.20

        if score > best_score:
            best_score = score
            best = item

    return best


def enrich_from_itunes(
    track: TrackMetadata, countries: tuple[str, ...] = _DEFAULT_COUNTRIES
) -> None:
    """
    Дополняет track.duration_sec / album / cover_url из iTunes Search API,
    если они ещё не заполнены. Вызывается как fallback после Deezer.
    """
    if not track.title:
        return

    # Сначала ищем по «артист + название» (точнее), затем по одному названию.
    # Если артист латинизирован — добавляем кириллический вариант запроса:
    # iTunes RU-каталог лучше ищет «Земфira»/«Земфира», чем «Zemfira».
    from .text_norm import has_cyrillic, translit_lat_to_cyr

    queries = []
    if track.primary_artist:
        combo = f"{track.primary_artist} {track.title}"
        queries.append(combo)
        if not has_cyrillic(combo):
            cyr_combo = f"{translit_lat_to_cyr(track.primary_artist)} {track.title}"
            if cyr_combo.strip() and cyr_combo.lower() != combo.lower():
                queries.append(cyr_combo)
    queries.append(track.title)

    best_item: Optional[dict] = None
    for query in queries:
        for country in countries:
            results = _itunes_search(query, country)
            picked = _pick_best(track, results)
            if picked:
                best_item = picked
                break
        if best_item:
            break

    if not best_item:
        return

    if not track.duration_sec and best_item.get("trackTimeMillis"):
        track.duration_sec = round(best_item["trackTimeMillis"] / 1000)
    if not track.album and best_item.get("collectionName"):
        track.album = best_item["collectionName"]
    if not track.cover_url:
        # iTunes отдаёт 100x100; поднимаем до 600x600 заменой в URL.
        art = best_item.get("artworkUrl100") or best_item.get("artworkUrl60")
        if art:
            track.cover_url = art.replace("100x100", "600x600").replace("60x60", "600x600")
    # release_date формата «2010-06-07T07:00:00Z» — берём дату, если нет своей.
    if not track.release_date and best_item.get("releaseDate"):
        track.release_date = best_item["releaseDate"][:10]
