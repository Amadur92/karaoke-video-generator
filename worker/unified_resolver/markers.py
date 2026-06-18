"""
Единый источник «плохих» маркеров для отсева неподходящих версий трека
(караоке, минус, ремикс, лайв и т.д.).

Маркеры хранятся в исходной форме (русские — кириллицей, английские — латиницей).
Сравнение всегда идёт по нормализованному тексту (транслитерация + lower case),
см. scoring.normalize, поэтому кириллический и латинский варианты матчатся единообразно.
"""
from __future__ import annotations

import re

from .text_norm import normalize


# Маркеры, указывающие на ОДНОЗНАЧНО неподходящую версию.
# Если такой маркер есть в названии кандидата, но отсутствует в названии
# искомого трека — кандидат отбрасывается / получает критический штраф.
CRITICAL_BAD_MARKERS: list[str] = [
    # караоке / минусовки
    "karaoke", "караоке",
    "instrumental", "инструментал", "инструментальная",
    "minus", "минус", "минусовка", "minusovka", "backing track", "минус один",
    # каверы / переделки
    "cover", "кавер", "kaver",
    "remake", "переделка",
    # лайв / концерт
    "live", "лайв", "concert", "концерт",
    # ремиксы / мэшапы
    "remix", "ремикс", "remiks",
    "mashup", "mash up", "мешап", "мэшап",
    "bootleg", "бутлег",
    # акустика / трибьюты
    "acoustic", "акустика", "акустик",
    "tribute", "трибьют",
    # искажённые по скорости версии
    "8d", "nightcore", "slowed", "sped up", "speed up", "ускорен",
    # фрагменты / превью / реакции
    "snippet", "превью", "teaser", "preview",
    "reaction", "реакция", "react",
    # прочие помехи
    "phonk", "фанк",  # осторожно: «фанк» может быть жанром, но для поп/рок-поиска это remix-стиль
]

# Менее критичные маркеры — дают умеренный штраф, но не отбрасывают кандидата.
SOFT_BAD_MARKERS: list[str] = [
    "lyrics", "lyric video", "текст песни",
    "audio",  # часто = низкокачественная загрузка, но иногда официальный «Audio»
]

# Ключевые слова, повышающие уверенность в «официальности» канала/видео.
# Используется как один из сигналов при ранжировании YouTube-кандидатов.
OFFICIAL_CHANNEL_KEYWORDS: list[str] = [
    "vevo",
    "topic",  # YouTube Music auto-generated «... - Topic»
    "official",
    "records", "recordings", "label", "music",
    "мьюзик", "музыка",
    "tv", "channel", "канал",
]


def _norm_for_markers(value: str) -> str:
    """
    Нормализация для поиска bad-маркеров.

    В отличие от scoring.normalize, НЕ вырезает содержимое скобок целиком —
    вместо этого заменяет сами символы скобок на пробелы. Иначе версии вида
    «Хочешь (Remix)» или «Song [Live]» теряли бы маркер вместе со скобками,
    и критические версии (remix/live/acoustic в скобках) не отсевались бы.
    """
    # Используем базовую normalize, но предварительно снимаем скобки,
    # превращая их в пробелы (а не удаляя содержимое).
    flattened = re.sub(r"[\(\)\[\]\{\}<>]", " ", value or "")
    return normalize(flattened)


def find_critical_markers(title: str, reference_title: str | None = None) -> list[str]:
    """
    Возвращает критические bad-маркеры, присутствующие в ``title``,
    но отсутствующие в ``reference_title`` (если задан).

    Маркер считается «своим» для трека, если он есть в reference_title —
    тогда поиск оригинала с таким же маркером легитимен
    (например, ищем «Acoustic version» и кандидат тоже acoustic).
    """
    title_norm = _norm_for_markers(title)
    ref_norm = _norm_for_markers(reference_title) if reference_title else ""
    hits: list[str] = []
    for marker in CRITICAL_BAD_MARKERS:
        marker_norm = _norm_for_markers(marker)
        if not marker_norm:
            continue
        if marker_norm in title_norm and marker_norm not in ref_norm:
            hits.append(marker)
    return hits


def find_soft_markers(title: str, reference_title: str | None = None) -> list[str]:
    """Аналогично find_critical_markers, но для мягких маркеров."""
    title_norm = _norm_for_markers(title)
    ref_norm = _norm_for_markers(reference_title) if reference_title else ""
    hits: list[str] = []
    for marker in SOFT_BAD_MARKERS:
        marker_norm = _norm_for_markers(marker)
        if not marker_norm:
            continue
        if marker_norm in title_norm and marker_norm not in ref_norm:
            hits.append(marker)
    return hits


def has_critical_marker(title: str, reference_title: str | None = None) -> bool:
    return bool(find_critical_markers(title, reference_title))


def is_official_channel(uploader: str, channel: str | None = None) -> bool:
    """
    Эвристика «официальности» источника по имени канала/загрузчика.
    Один из сигналов ранжирования YouTube-кандидатов.
    """
    blob = " ".join(filter(None, [uploader or "", channel or ""])).lower()
    if not blob.strip():
        return False
    return any(kw and kw in blob for kw in OFFICIAL_CHANNEL_KEYWORDS)
