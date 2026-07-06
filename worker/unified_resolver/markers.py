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
#
# Matching идёт по словам (word-boundary), а не по подстроке:
# «live» матчит «Song Live Version», но НЕ «delivery».
# Это позволяет безопасно добавлять короткие маркеры (bbc, tour, session).
CRITICAL_BAD_MARKERS: list[str] = [
    # караоке / минусовки
    "karaoke", "караоке",
    "instrumental", "инструментал", "инструментальная",
    "minus", "минус", "минусовка", "minusovka", "backing track", "минус один",
    # каверы / переделки
    "cover", "кавер", "kaver",
    "remake", "переделка",
    # лайв / концерт (включая формы без слова «live»)
    "live", "лайв", "concert", "концерт",
    "unplugged", "анплаггед",
    "session", "sessions", "сессия",
    "tour", "tour edition",
    "soundcheck", "rehearsal",
    # русские концертные площадки / телеэфиры / фестивали
    "maxidrom",
    "авторадио",
    "супердискотека", "супердискотэка",
    "дискотека 80", "дискотека 90",
    "привет андрей",
    "музыкальный ринг",
    "новогодняя ночь",
    "песня года",
    "золотой граммофон",
    # концертные формулировки без слова «live» в названии
    "хит 90", "хиты 90", "хиты 80", "хит 80",
    "собрал публику", "выступление",
    "концертный", "концертная",
    # конкурсы / фестивали (часто фигурируют в названиях концертных видео)
    "новая волна",
    "пятёрка",
    "гора хитов",
    "звезда по имени",
    # ремиксы / мэшапы
    "remix", "ремикс", "remiks",
    "mashup", "mash up", "мешап", "мэшап",
    "bootleg", "бутлег",
    # radio edit / club mix и подобные варианты без слова «remix»
    "radio mix", "radio edit", "dfm mix", "extended mix", "club mix",
    "edit version", "extended version",
    # акустика / трибьюты
    "acoustic", "акустика", "акустик",
    "tribute", "трибьют",
    # искажённые по скорости версии
    "8d", "nightcore", "slowed", "sped up", "speed up", "ускорен",
    # фрагменты / превью / реакции
    "snippet", "превью", "teaser", "preview",
    "reaction", "реакция", "react",
    # AI-генерация / нейросети
    "ai cover", "ai кавер",
    "udio ai", "suno ai",
    "нейросеть", "нейросетевая",
    # демо / рабочие версии
    "demo version", "demo версия",
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


# Маркеры «живого выступления» для анализа описания видео (description).
# В отличие от CRITICAL_BAD_MARKERS (которые применяются к заголовку и означают
# однозначную непригодность), эти маркеры проверяются в описании как
# подозрительный сигнал: концерты часто не подписаны явно в названии, но в
# описании содержат «запись концерта», «выступление», «tour», «live at» и т.д.
# Используется только в YouTube/SoundCloud fallback-ранжировании (не в candidate
# scoring), и даёт штраф, а не жёсткий отсев — описание бывает неточным.
CONCERT_DESCRIPTION_MARKERS: list[str] = [
    "live at", "live in", "live from", "live on",
    "concert", "tour 20", "tour 19", "world tour",
    "выступление", "концерт", "запись концерта", "концертный",
    "фестиваль", "festival",
    "выступал", "выступление на",
    "запись передачи", "эфир", "телевизионный",
    "сцена", "на сцене", "со сцены",
    "аплодисменты", "audience", "crowd",
    "привет андрей", "вечерний urgant", "вечерний ургант",  # русские телепередачи
    "maxidrom", "авторадио", "супердискотека", "супердискотэка",
    "soundcheck", "rehearsal", "саундчек",
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


def _marker_in_text(marker_norm: str, text_norm: str) -> bool:
    """
    Проверяет присутствие маркера в тексте по границам слов (word-boundary).

    Раньше использовался substring-match: «live» in title_norm. Это работало
    для длинных маркеров, но давало ложные срабатывания на коротких (планировали
    добавить «bbc»/«tour»/«session» — а они матчатся внутри обычных слов вроде
    «freelance», «contour», «sessions» в смысле «сессии записи»).

    Логика:
      • Многословные маркеры («backing track», «дискотека 80», «ai cover»)
        ищутся как подстрока с пробелами по краям — word-boundary для фразы.
      • Однословные маркеры ищутся как отдельное слово в строке (окружено
        пробелами/началом/концом). Нормализованный текст уже содержит только
        буквы/цифры и пробелы, поэтому этого достаточно.
      • Маркеры из одних цифр/букв (например «8d») требуют точного совпадения
        слова, чтобы не матчить «80x» или «hd».

    text_norm гарантированно обёрнут в пробелы спереди/сзади, чтобы упростить
    проверку границ для маркеров в начале/конце строки.
    """
    if not marker_norm:
        return False
    padded = f" {text_norm} "
    # Если маркер содержит пробел — это фраза, ищем « с фразой » целиком.
    if " " in marker_norm:
        return f" {marker_norm} " in padded
    # Однословный маркер — отдельное слово.
    return f" {marker_norm} " in padded


def find_critical_markers(title: str, reference_title: str | None = None) -> list[str]:
    """
    Возвращает критические bad-маркеры, присутствующие в ``title``,
    но отсутствующие в ``reference_title`` (если задан).

    Маркер считается «своим» для трека, если он есть в reference_title —
    тогда поиск оригинала с таким же маркером легитимен
    (например, ищем «Acoustic version» и кандидат тоже acoustic).

    Matching идёт по словам (word-boundary), а не по подстроке:
    «live» матчит «Song Live Version», но НЕ «delivery».
    """
    title_norm = _norm_for_markers(title)
    ref_norm = _norm_for_markers(reference_title) if reference_title else ""
    hits: list[str] = []
    for marker in CRITICAL_BAD_MARKERS:
        marker_norm = _norm_for_markers(marker)
        if not marker_norm:
            continue
        if _marker_in_text(marker_norm, title_norm) and not _marker_in_text(
            marker_norm, ref_norm
        ):
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
        if _marker_in_text(marker_norm, title_norm) and not _marker_in_text(
            marker_norm, ref_norm
        ):
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


def has_concert_marker_in_text(text: str, reference_title: str | None = None) -> bool:
    """
    Проверяет, содержит ли произвольный текст (обычно description видео)
    признаки концертной записи. В отличие от find_critical_markers, здесь
    используется substring-match по нижнему регистру: для описаний точность
    слово-границы не важна, важна скорость и покрытие («tour 20» должно
    матчить «World Tour 2023», «выступление» — «запись выступления»).

    Если reference_title сам содержит концертный признак (трек называется
    «Song Live» / «Live at Wembley» — концертная запись легитимна) —
    текст не помечается. Проверяем как полные фразы, так и базовые маркеры
    (live/концерт/выступление), чтобы поймать любой вариант.
    """
    if not text:
        return False
    blob = text.lower()
    ref = (reference_title or "").lower()
    for marker in CONCERT_DESCRIPTION_MARKERS:
        if marker in blob:
            # Легитимно, если reference_title тоже содержит этот маркер
            # ИЛИ базовый concert-маркер (live/концерт), на который опирается маркер.
            if marker in ref:
                continue
            base = marker.split()[0]  # «live at» → «live», «tour 20» → «tour»
            if base in {"live", "концерт", "выступление", "tour", "фестиваль"} and base in ref:
                continue
            return True
    return False
