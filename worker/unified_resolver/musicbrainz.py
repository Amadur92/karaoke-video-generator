"""
Enrichment метаданных трека через публичный MusicBrainz API.

MusicBrainz — открытое музыкальное сообщество, для русскоязычных релизов
часто содержит более точные данные, чем Deezer/iTunes, и принципиально
отличается тем, что для одного произведения хранит НЕСКОЛЬКО recording-записей
(по одной на каждый релиз/альбом) с разными длительностями. Это позволяет:

  1. Получить медианную «студийную» длительность, отличив оригинал от
     переиздания/сборника/live-альбома (у них primary-type=Album vs
     Compilation, и есть date/status у каждого релиза).
  2. Перекрёстно проверить Deezer-овский duration_sec: если MusicBrainz
     уверенно даёт другое значение — это сильный сигнал, что enrichment
     привязался к чужой версии (сборник/переиздание).

API публичное, без ключей, лимит — 1 запрос/сек (для батчей хватает при
2 параллельных воркерах). Требует информативный User-Agent.
"""
from __future__ import annotations

import statistics
import urllib.parse
from typing import Optional

from .http import DEFAULT_UA, get_json, safe_urlopen
from .models import TrackMetadata


# MusicBrainz требует User-Agent с контактом; иначе может вернуть 403/429.
_MB_USER_AGENT = "KaraokeVideoGenerator/0.5 (https://github.com/mihailsokolenko/karaoke-video-generator)"

# Минимум валидных durations, чтобы медиане можно было доверять.
# MusicBrainz дедуплицирует recordings по MB-recording-ID, поэтому для нишевых
# треков часто возвращается ровно 1 студийная запись — это всё равно сильнее,
# чем отсутствие MB-данных. Порог 1 позволяет работать с таким каталогом.
_MIN_SAMPLES = 1

# Расхождение (в процентах) между MusicBrainz-медианой и текущим ref-duration,
# при котором MusicBrainz переписывает enrichment-длительность.
_OVERWRITE_DEVIATION_PCT = 10


def _mb_get(path: str, params: dict[str, str]) -> Optional[dict]:
    """GET-запрос к MusicBrainz API (возвращает parsed JSON или None)."""
    url = "https://musicbrainz.org/ws/2/" + path
    query = dict(params)
    query["fmt"] = "json"
    url += "?" + urllib.parse.urlencode(query)
    try:
        return get_json(url, headers={"User-Agent": _MB_USER_AGENT}, timeout=15)
    except Exception:
        return None


def _is_studio_release(release: dict) -> bool:
    """
    True, если релиз похож на студийный альбом нужной версии.
    MusicBrainz различает primary-type: Album / Single / EP / Broadcast / Other,
    и secondary-types: Compilation / Live / Remix / Soundtrack / Interview ...
    """
    rg = release.get("release-group") or {}
    primary = (rg.get("primary-type") or "").lower()
    secondary = rg.get("secondary-types") or []
    secondary_lower = [s.lower() for s in secondary]

    # Явные live/compilation/soundtrack — не студийный оригинал.
    if "live" in secondary_lower:
        return False
    if "compilation" in secondary_lower:
        return False
    if "soundtrack" in secondary_lower:
        return False
    if "remix" in secondary_lower:
        return False
    # Album / Single / EP — ок; Broadcast / Other — подозрительно.
    if primary in {"album", "single", "ep"}:
        return True
    return False


def _collect_studio_durations(data: dict, track: TrackMetadata) -> list[int]:
    """
    Из ответа MusicBrainz /ws/2/recording собирает длительности (сек) из
    студийных релизов. Для каждого recording берём длину из самого recording
    (length, ms) и проверяем его releases на студийность.
    """
    durations: list[int] = []
    for rec in (data.get("recordings") or []):
        # video-записи — это видеоклипы, не аудио; пропускаем.
        if rec.get("video"):
            continue
        # disambiguation может помечать remix/live явно.
        disambig = (rec.get("disambiguation") or "").lower()
        if any(bad in disambig for bad in ("live", "remix", "radio edit", "demo")):
            continue

        length_ms = rec.get("length")
        if not isinstance(length_ms, (int, float)) or length_ms <= 0:
            continue
        length_sec = int(round(length_ms / 1000))

        # У recording может быть несколько releases; если хотя бы один студийный
        # — считаем эту длительность валидной студийной версией.
        releases = rec.get("releases") or []
        if not releases:
            # Без релиза — нельзя проверить студийность, но длительность берём
            # как слабый сигнал (MusicBrainz-редакторы обычно привязывают
            # верный length к recording).
            durations.append(length_sec)
            continue
        if any(_is_studio_release(r) for r in releases):
            durations.append(length_sec)
    return durations


def enrich_from_musicbrainz(track: TrackMetadata) -> Optional[str]:
    """
    Перекрёсная проверка/обогащение duration_sec через MusicBrainz.

    Стратегия:
      • Ищем recordings по «recording:"{title}" AND artist:"{artist}"».
      • Из студийных релизов собираем durations, берём медиану.
      • Если текущий ref-duration расходится с медианой > 10% — перезаписываем
        (сильный сигнал, что Deezer/iTunes привязались к сборнику/переизданию).
      • Если ref-duration не задан — берём медиану как опорную.

    Возвращает человекочитаемую заметку для логов/provenance или None.
    """
    if not track.title:
        return None

    artist = track.primary_artist
    if artist:
        query = f'recording:"{track.title}" AND artist:"{artist}"'
    else:
        query = f'recording:"{track.title}"'

    data = _mb_get("recording", {"query": query, "limit": "10"})
    if not data:
        return None

    durations = _collect_studio_durations(data, track)
    if len(durations) < _MIN_SAMPLES:
        return (
            f"mb_insufficient_samples:samples={len(durations)}"
            if durations
            else None
        )

    median = int(statistics.median(durations))

    ref = track.duration_sec
    if ref:
        deviation = abs(ref - median) / ref * 100.0
        if deviation <= _OVERWRITE_DEVIATION_PCT:
            return f"mb_ok:median={median}s,ref={ref}s,deviation={deviation:.1f}%"
        # Расхождение слишком велико — доверяем MusicBrainz (несколько
        # независимых студийных релизов — сильнее одного Deezer-совпадения).
        note = (
            f"mb_overwrote_duration:ref={ref}s,median={median}s,"
            f"deviation={deviation:.1f}%,samples={len(durations)}"
        )
        track.duration_sec = median
        if not getattr(track, "duration_note", None):
            try:
                track.duration_note = note  # type: ignore[attr-defined]
            except Exception:
                pass
        return note

    # ref-duration не задан — берём медиану как опорную.
    track.duration_sec = median
    note = f"mb_set_duration:median={median}s,samples={len(durations)}"
    if not getattr(track, "duration_note", None):
        try:
            track.duration_note = note  # type: ignore[attr-defined]
        except Exception:
            pass
    return note
