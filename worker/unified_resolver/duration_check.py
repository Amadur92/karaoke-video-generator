"""
Перекрёсная валидация эталонной длительности трека против реальной выдачи
YouTube/SoundCloud.

Проблема, которую решает этот модуль
------------------------------------
Enrichment из Deezer/iTunes иногда привязывается не к оригинальному студийному
релизу, а к сборнику, live-альбому или переизданию. В результате
``track.duration_sec`` становится равен длительности ЧУЖОЙ версии — и тогда
последующий duration-фильтр (в scoring/downloader) гарантированно убивает
настоящий оригинал, оставляя лишь ту же неправильную версию.

Реальный кейс (пакет Vau_Muzloto_184):
  • «Маша и Медведи - Любочка»: Deezer отдал альбом «DJ Groove и все, все…»,
    duration=318. Оригинал группы идёт ~257с и помечался как large_duration_mismatch.
  • «Рок-острова - Ничего не говори»: ISRC привязался к переизданию
    «Новое и Лучшее» (289с), оригинал 1996 года идёт ~240с.

Решение: после enrichment взять медиану длительностей топ-N результатов
поиска YouTube по запросу «{artist} {title}» и сравнить с эталоном. Если
расхождение слишком велико — эталон почти наверняка принадлежит чужой версии,
поэтому обнуляем его. Тогда duration-фильтр перестаёт работать против нас
(кандидаты не штрафуются за расхождение с неправильным эталоном), и выбор
идёт по title/artist/официальности канала.

Это «мягкая» защита: при совпадении длительностей (типичный случай) модуль
ничего не меняет — он срабатывает только при явном подозрении.
"""
from __future__ import annotations

import statistics
from typing import Optional, Callable

from .models import TrackMetadata


# Порог расхождения (в процентах от эталона), при котором эталон считаем
# подозрительным. Live/extended-версии обычно длиннее на 20–60%, сборники/
# ремастеры могут отличаться на 10–30%. Порог 15% ловит оба класса и при этом
# не срабатывает на нормальном разбросе длительности (±5–10с между сервисами).
_SUSPICIOUS_DEVIATION_PCT = 15

# Минимум валидных длительностей в выборке, чтобы медиане можно было доверять.
# При 1–2 значениях велик шанс, что это и есть та самая неправильная версия.
_MIN_SAMPLES = 3

# Не валидируем слишком короткие треки: для 30-секундного превью ±15% = ±4.5с,
# что сравнимо с точностью округления между сервисами.
_MIN_TRACK_DURATION = 60


class DurationValidator:
    """
    Сравнивает эталонную длительность трека с медианой реальной выдачи
    провайдера (обычно YouTube). При сильном расхождении обнуляет
    ``track.duration_sec`` и помечает причину в атрибуте ``duration_note``.
    """

    def __init__(self, search_fn: Optional[Callable[[str], list[int]]] = None):
        """
        :param search_fn: функция, принимающая поисковый запрос и возвращающая
            список длительностей (в секундах) найденных кандидатов. По умолчанию
            используется встроенный YouTube-поиск через yt-dlp. Инъекция нужна
            для тестов.
        """
        self._search_fn = search_fn or _default_youtube_durations

    def validate(self, track: TrackMetadata) -> Optional[str]:
        """
        Валидирует ``track.duration_sec``. Возвращает человекочитаемую заметку
        о действии (для логов/provenance) или None, если проверка не применялась
        (нет эталона / мало данных / расхождение в норме).
        """
        ref = track.duration_sec
        if not ref or ref < _MIN_TRACK_DURATION:
            return None

        query = (
            f"{track.primary_artist} {track.title}".strip()
            if track.primary_artist
            else track.title
        )
        try:
            durations = self._search_fn(query)
        except Exception as exc:
            # Сетевые ошибки не должны ломать общий pipeline — enrichment
            # остаётся как есть, просто без перекрёсной проверки.
            return f"validation_skipped:search_error:{type(exc).__name__}"

        valid = [d for d in durations if d and d >= _MIN_TRACK_DURATION]
        if len(valid) < _MIN_SAMPLES:
            return None

        median = int(statistics.median(valid))
        deviation = abs(ref - median) / ref * 100.0

        if deviation <= _SUSPICIOUS_DEVIATION_PCT:
            # Эталон согласуется с реальной выдачей — всё в порядке.
            return f"ok:median={median}s,deviation={deviation:.1f}%"

        # Расхождение слишком велико: enrichment почти наверняка привязался
        # к чужой версии. Обнуляем, чтобы duration-фильтр не убивал оригинал.
        note = (
            f"reference_duration_dropped:ref={ref}s,median={median}s,"
            f"deviation={deviation:.1f}%,samples={len(valid)}"
        )
        track.duration_sec = None
        if not getattr(track, "duration_note", None):
            try:
                track.duration_note = note  # type: ignore[attr-defined]
            except Exception:
                pass
        return note


def _default_youtube_durations(query: str) -> list[int]:
    """Извлекает длительности топ-результатов поиска YouTube через yt-dlp."""
    try:
        from yt_dlp import YoutubeDL
    except ImportError:
        from youtube_dl import YoutubeDL  # type: ignore

    options = {
        "quiet": True,
        "nooverwrites": True,
        "noplaylist": True,
        "nocheckcertificate": True,
        "extract_flat": "in_playlist",
    }
    durations: list[int] = []
    with YoutubeDL(options) as ydl:
        try:
            info = ydl.extract_info(f"ytsearch8:{query}", download=False)
        except Exception:
            return durations
        for entry in (info.get("entries") or []):
            if not entry:
                continue
            # В flat-режиме duration может отсутствовать — тогда пропускаем.
            dur = entry.get("duration")
            if isinstance(dur, (int, float)) and dur > 0:
                durations.append(int(dur))
    return durations
