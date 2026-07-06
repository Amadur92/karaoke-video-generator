"""Юнит-тесты для unified_resolver: нормализация, маркеры, транслитерация,
скоринг, выбор YouTube/SoundCloud-кандидата, валидация аудиофайла.

Запуск: python3 -m pytest worker/test_resolver_logic.py
или:    python3 worker/test_resolver_logic.py
"""
import os
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from unified_resolver.markers import (  # noqa: E402
    find_critical_markers,
    has_critical_marker,
    is_official_channel,
)
from unified_resolver.models import Candidate, TrackMetadata  # noqa: E402
from unified_resolver.scoring import partial_word_match, score_candidate  # noqa: E402
from unified_resolver.text_norm import (  # noqa: E402
    has_cyrillic,
    normalize,
    translit_lat_to_cyr,
)
from unified_resolver.downloader import is_valid_audio_file  # noqa: E402


# ----------------- normalize / transliteration -----------------

def test_normalize_transliterates_cyrillic():
    assert normalize("Земфира") == "zemfira"
    assert normalize("Ёжик") == "ezhik"


def test_normalize_strips_brackets_and_punctuation():
    # normalize намеренно вырезает содержимое скобок целиком (для скоринга названий);
    # это отличает её от markers._norm_for_markers, которая скобки «раскрывает».
    assert normalize("Song (Remix) [Live]") == "song"


def test_translit_lat_to_cyr_basic():
    assert translit_lat_to_cyr("Zemfira") == "земфира"
    assert translit_lat_to_cyr("Miyagi") == "мияги"


def test_translit_digraphs_first():
    # Диграфы матчатся раньше одиночных букв: «sh» → «ш» (а не «s»+»h»), «zh» → «ж».
    assert translit_lat_to_cyr("shuk") == "шук"
    assert translit_lat_to_cyr("zhuk") == "жук"


def test_has_cyrillic_detection():
    assert has_cyrillic("Земфира") is True
    assert has_cyrillic("Zemfira") is False
    assert has_cyrillic("Зеmфира") is True  # смешанная строка


# ----------------- markers -----------------

def test_critical_marker_simple_word():
    assert has_critical_marker("Song karaoke", "Song") is True


def test_critical_marker_in_brackets():
    """Регрессия: раньше normalize вырезал содержимое скобок целиком,
    и «(Remix)»/«[Live]»/«(Acoustic)» НЕ детектировались как bad-маркеры."""
    assert find_critical_markers("Хочешь (Remix)", "Хочешь") == ["remix"]
    assert find_critical_markers("Song [Live Version]", "Song") == ["live"]
    assert find_critical_markers("Song (Acoustic)", "Song") == ["acoustic"]


def test_critical_marker_cyrillic():
    assert find_critical_markers("Песня караоке", "Песня") == ["karaoke", "караоке"]
    assert find_critical_markers("Песня минус", "Песня") == ["minus", "минус"]


def test_critical_marker_legitimate_when_in_reference():
    """Если искомый трек сам содержит маркер — кандидат с тем же маркером легитимен."""
    assert find_critical_markers("Хочешь (Remix)", "Хочешь remix") == []
    assert find_critical_markers("Song (Acoustic)", "Song acoustic") == []


def test_is_official_channel_keywords():
    assert is_official_channel("ZemfiraVEVO") is True
    assert is_official_channel("Zemfira - Topic") is True
    assert is_official_channel("somelabel Records") is True
    assert is_official_channel("ivan228") is False


# ----------------- partial_word_match (багфикс) -----------------

def test_partial_word_match_old_bug_gone():
    """Регрессия: раньше короткое слово ложно матчило всю строку целиком
    (word vs весь haystack через SequenceMatcher), давая ~100 для несвязанных слов."""
    # 'ai' не должно матчиться со строкой из повторов 'zai'
    assert partial_word_match("ai", "zai zai zai zai zai") == 0.0


def test_partial_word_match_exact():
    assert partial_word_match("привет мир", "привет") == 50.0


def test_partial_word_match_full_hit():
    assert partial_word_match("hello world", "hello world foo") == 100.0


def test_partial_word_match_fuzzy():
    # нечёткое слово-к-слову: близкие варианты матчатся (порог ratio > 0.8)
    assert partial_word_match("running", "runing") >= 80.0


def test_partial_word_match_below_threshold():
    # слишком разные слова (>0.8 ratio) не матчатся — защищает от ложных срабатываний
    assert partial_word_match("running", "runn") == 0.0


# ----------------- score_candidate integration -----------------

def _track(title="Хочешь", artist="Земфира", duration=234):
    return TrackMetadata(title=title, artists=[artist], duration_sec=duration)


def test_score_prefers_original_over_remix_in_brackets():
    track = _track()
    original = Candidate(
        source="youtube", title="Земфира - Хочешь",
        artists=["ZemfiraVEVO"], duration_sec=233,
    )
    remix = Candidate(
        source="youtube", title="Хочешь (Remix)",
        artists=["Zemfira"], duration_sec=240,
    )
    s_orig = score_candidate(track, original)
    s_remix = score_candidate(track, remix)
    assert s_orig.score > s_remix.score
    assert any(f.startswith("critical_marker:") for f in s_remix.flags)
    assert not any(f.startswith("critical_marker:") for f in s_orig.flags)


def test_score_duration_mismatch_penalized():
    track = _track(duration=234)
    good = Candidate(source="youtube", title="Хочешь", artists=["Земфира"], duration_sec=233)
    bad = Candidate(source="youtube", title="Хочешь", artists=["Земфира"], duration_sec=300)
    assert score_candidate(track, good).score > score_candidate(track, bad).score


def test_download_quality_summary_ok_for_clean_candidate():
    from unified_resolver.__main__ import download_quality_summary

    track = _track(duration=234)
    candidate = Candidate(source="youtube", title="Земфира - Хочешь", artists=["Земфира"], duration_sec=234)
    scored = score_candidate(track, candidate)
    quality, reasons = download_quality_summary(scored)
    assert quality == "ok"
    assert reasons == []


def test_download_quality_summary_flags_live_candidate():
    from unified_resolver.__main__ import download_quality_summary

    track = _track(duration=234)
    candidate = Candidate(source="youtube", title="Земфира - Хочешь Live Version", artists=["Земфира"], duration_sec=260)
    scored = score_candidate(track, candidate)
    quality, reasons = download_quality_summary(scored)
    assert quality == "suspicious"
    assert any("live" in reason for reason in reasons)
    assert any(reason.startswith("candidate:") for reason in reasons)


# ----------------- YouTube/SoundCloud candidate ranking -----------------

def _entry(title, duration, uploader="user", eid=None, view_count=None, description=None):
    return {
        "id": eid or title,
        "title": title,
        "duration": duration,
        "uploader": uploader,
        "view_count": view_count,
        "description": description,
        "webpage_url": f"https://www.youtube.com/watch?v={eid or title}",
    }


def test_rank_youtube_entries_prefers_close_duration():
    """Главный сигнал — близость длительности к эталону."""
    from unified_resolver.downloader import Downloader
    dl = Downloader.__new__(Downloader)  # без __init__ (не создаём директории)
    dl.duration_tolerance = None
    track = _track(duration=234)
    entries = [
        _entry("Хочешь", 300, "ZemfiraVEVO"),   # далеко по длительности
        _entry("Хочешь", 233, "user"),           # точно по длительности
        _entry("Хочешь (Remix)", 235, "user"),   # bad-маркер — исключается
    ]
    ranked = dl._rank_youtube_entries(entries, track)
    # remix-кандидат должен быть полностью исключён
    titles = [e.get("title") for _, e in ranked]
    assert "Хочешь (Remix)" not in titles
    # лучший — тот, что по длительности
    assert ranked[0][1]["title"] == "Хочешь"


def test_rank_youtube_entries_excludes_critical_markers():
    from unified_resolver.downloader import Downloader
    dl = Downloader.__new__(Downloader)
    dl.duration_tolerance = None
    track = _track()
    entries = [
        _entry("Хочешь караоке", 234, "user"),
        _entry("Хочешь instrumental", 234, "user"),
        _entry("Хочешь", 234, "user"),
    ]
    ranked = dl._rank_youtube_entries(entries, track)
    assert len(ranked) == 1
    assert ranked[0][1]["title"] == "Хочешь"


def test_youtube_search_queries_adds_translit_and_topic():
    """При латинском исполнителе добавляется кириллический вариант запроса
    и topic-вариант (для смещения выдачи к студийным YouTube Music-каналам)."""
    from unified_resolver.downloader import Downloader
    dl = Downloader.__new__(Downloader)
    track = TrackMetadata(title="Hochesh", artists=["Zemfira"], duration_sec=234)
    queries = dl._youtube_search_queries("Zemfira - Hochesh", track)
    assert any(has_cyrillic(q) for q in queries)
    # topic-запрос должен присутствовать
    assert any(q.lower().endswith(" topic") for q in queries)


# ----------------- view_count signal (G) -----------------

def test_rank_youtube_entries_view_count_bonus():
    """Более популярный клип (больше просмотров) должен получать бонус.
    Оригинальный клип у популярного трека обычно имеет на порядки больше
    просмотров, чем live/phonk/AI-версии."""
    from unified_resolver.downloader import Downloader
    dl = Downloader.__new__(Downloader)
    dl.duration_tolerance = None
    track = _track(duration=234)
    entries = [
        _entry("Хочешь", 234, "ZemfiraVEVO", view_count=5_000_000),
        _entry("Хочешь", 234, "user", view_count=5_000),
    ]
    ranked = dl._rank_youtube_entries(entries, track)
    # Оба прошли фильтры, но более популярный — первый.
    assert ranked[0][1]["uploader"] == "ZemfiraVEVO"


def test_rank_youtube_entries_view_count_not_for_wrong_title():
    """Регрессия: просмотры популярного клипа ДРУГОГО трека того же артиста
    не должны давать бонус. Кейс «Рок-Острова - Ничего не говори»: клип
    «Костры» того же артиста имеет 12M просмотров, но это другая песня.
    Бонус за view_count применяется только при title_sim ≥ 60."""
    from unified_resolver.downloader import Downloader
    dl = Downloader.__new__(Downloader)
    dl.duration_tolerance = None
    track = TrackMetadata(title="Ничего не говори", artists=["Рок-Острова"], duration_sec=289)
    entries = [
        # Другой трек того же артиста, огромная популярность, близкая длина.
        _entry("Рок-Острова – Костры (2010)", 296, "Рок-Острова", view_count=12_500_000),
        # Нужный трек, меньше просмотров, но точное совпадение названия.
        _entry("Рок-Острова - Ничего не говори", 289, "Рок-Острова", view_count=500_000),
    ]
    ranked = dl._rank_youtube_entries(entries, track)
    # Нужный трек обязан победить, несмотря на меньшую популярность.
    assert "Ничего не говори" in ranked[0][1]["title"]


def test_rank_youtube_entries_description_concert_penalty():
    """Описание с признаками концерта штрафует кандидата.
    Регрессия Рок-острова #36: «Хит 90-х собрал публику» с описанием
    «Выступление на улице» — это концертная запись, не студийный оригинал."""
    from unified_resolver.downloader import Downloader
    dl = Downloader.__new__(Downloader)
    dl.duration_tolerance = None
    track = TrackMetadata(title="Ничего не говори", artists=["Рок-Острова"], duration_sec=289)
    entries = [
        _entry(
            "Рок Острова Ничего не говори | Хит 90-х собрал публику",
            289, "Ivan Calen",
            description="Выступление на улице. Рок Острова - Ничего не говори. Русские хиты 90-х",
        ),
        _entry(
            "Рок-Острова – Ничего не говори (1996)",
            240, "Рок-Острова",
            description="Рок-Острова – Ничего не говори (1996). Эксклюзивы на Бусти",
        ),
    ]
    ranked = dl._rank_youtube_entries(entries, track)
    # Студийная версия должна победить концертную, несмотря на меньшую длительность.
    assert ranked[0][1]["title"].startswith("Рок-Острова")


def test_has_concert_marker_in_text_detects_live_description():
    """Прямая проверка детектора концертных описаний."""
    from unified_resolver.markers import has_concert_marker_in_text
    assert has_concert_marker_in_text("Live at Wembley Stadium 1998") is True
    assert has_concert_marker_in_text("Запись концерта в Кремле") is True
    assert has_concert_marker_in_text("Выступление на фестивале Максидром") is True
    assert has_concert_marker_in_text("Studio recording, 2024") is False
    assert has_concert_marker_in_text("") is False
    # Если reference-title содержит маркер — не срабатывает (легитимный live-трек).
    assert has_concert_marker_in_text("Live at Wembley", "Song Live") is False


# ----------------- file validation -----------------

def test_is_valid_audio_file_rejects_missing():
    assert is_valid_audio_file(None) is False
    assert is_valid_audio_file("/nonexistent/path.xyz") is False


def test_is_valid_audio_file_rejects_tiny(tmp_path):
    small = tmp_path / "tiny.mp3"
    small.write_bytes(b"not really audio")
    assert is_valid_audio_file(str(small)) is False


# ----------------- word-boundary marker matching (D) -----------------

def test_marker_word_boundary_no_false_positive_on_substring():
    """Регрессия: «live» НЕ должно матчиться внутри «delivery»/«oliver».
    Раньше использовался substring-match; с переходом на word-boundary
    короткие маркеры безопасны."""
    from unified_resolver.markers import has_critical_marker
    assert has_critical_marker("Song delivery", "Song") is False
    assert has_critical_marker("Oliver Twist Song", "Oliver Twist") is False
    # А реальное слово «live» — матчится.
    assert has_critical_marker("Song (Live)", "Song") is True


def test_marker_new_live_markers_detected():
    """Новые маркеры для live-выступлений без слова «live» в названии."""
    from unified_resolver.markers import find_critical_markers
    # Реальные кейсы из пакета Vau_Muzloto_184:
    assert "супердискотэка" in find_critical_markers(
        "Кар-Мэн - Лондон, Гудбай (СупердискотЭка 90-х)", "Лондон гуд бай"
    )
    assert "привет андрей" in find_critical_markers(
        "Рок-Острова - Ничего не говори (Привет, Андрей!)", "Ничего не говори"
    )
    assert "музыкальный ринг" in find_critical_markers(
        'Маша и Медведи - Любочка (Live @ "Музыкальный ринг")', "Любочка"
    )
    # Универсальные live-формы:
    assert "unplugged" in find_critical_markers("Song (MTV Unplugged)", "Song")
    assert "bbc" not in find_critical_markers("Song (BBC Session)", "Song") or \
           "session" in find_critical_markers("Song (BBC Session)", "Song")


def test_marker_ai_cover_detected():
    """AI-каверы и нейрогенерация — отдельный класс «не той версии»."""
    from unified_resolver.markers import find_critical_markers
    assert "udio ai" in find_critical_markers(
        "Рок-Острова - Ничего не говори [Udio Ai]", "Ничего не говори"
    )
    assert "ai cover" in find_critical_markers("Song (AI cover)", "Song")


def test_marker_word_boundary_safe_for_short_markers():
    """Короткие маркеры (tour, bbc, 8d) не должны давать ложных срабатываний."""
    from unified_resolver.markers import has_critical_marker
    # «tour» внутри «contour»/«tournament» — НЕ матчится.
    assert has_critical_marker("Song contour demo", "Song") is False
    # Но «Tour Edition» — матчится.
    assert has_critical_marker("Song (Tour Edition)", "Song") is True


# ----------------- critical marker = hard exclude (C) -----------------

def test_score_critical_marker_is_hard_zero():
    """Регрессия: критический маркер прижимает score к 0 (раньше был −80),
    чтобы live/remix никогда не выигрывали ранжирование при слабой конкуренции."""
    track = _track()
    # Идеальное совпадение title/artist/duration, но это live-версия.
    live = Candidate(
        source="youtube", title="Земфира - Хочешь (Live)",
        artists=["Земfira"], duration_sec=234,
    )
    s = score_candidate(track, live)
    assert s.score == 0.0
    assert any(f.startswith("critical_marker:") for f in s.flags)


def test_score_original_beats_live_even_with_duration_bonus():
    """Даже если у live-версии идеальная длительность, а у оригинала есть
    небольшое расхождение — оригинал обязан победить (live принудительно = 0)."""
    track = _track(duration=234)
    original = Candidate(
        source="youtube", title="Земфира - Хочешь",
        artists=["Земфira"], duration_sec=242,  # 8с расхождение
    )
    live = Candidate(
        source="youtube", title="Земфира - Хочешь (Live)",
        artists=["Земфira"], duration_sec=234,  # идеально
    )
    assert score_candidate(track, original).score > score_candidate(track, live).score


# ----------------- weak album detection (B) -----------------

def test_is_weak_album_detects_compilations_and_live():
    from unified_resolver.itunes import is_weak_album
    assert is_weak_album("Новое и Лучшее") is True
    assert is_weak_album("Greatest Hits") is True
    assert is_weak_album("Дискотека 80-х (Авторадио)") is True
    assert is_weak_album("MTV Unplugged") is True
    assert is_weak_album("Live at Wembley") is True
    assert is_weak_album("Deluxe Edition") is True
    # Студийный альбом — НЕ слабый.
    assert is_weak_album("Вокруг света") is False
    assert is_weak_album(None) is False


def test_deezer_enrich_skips_wrong_artist_compilation():
    """Регрессия Vau_Muzloto_184 #29: «Маша и Медведи - Любочка» обогащался
    из сборника «DJ Groove и все, все…» с чужой длительностью (318с вместо
    ~257с). Теперь enrichment должен отсечь чужого артиста и не брать его
    длительность из сборника."""
    import unified_resolver.resolver as resolver_mod
    from unified_resolver.resolver import enrich_from_deezer

    # Мокаем Deezer API: первый результат — сборник DJ Groove (чужой артист,
    # слабый альбом), второй — настоящий трек группы.
    fake_results = {
        "data": [
            {
                "title": "Любочка",
                "artist": {"name": "DJ Groove"},
                "album": {"title": "DJ Groove и все, все, все...", "cover_big": "x"},
                "duration": 318, "isrc": "FAKE1",
            },
            {
                "title": "Любочка",
                "artist": {"name": "Маша и Медведи"},
                "album": {"title": "Сlobber", "cover_big": "y"},
                "duration": 257, "isrc": "FR2X41802514",
            },
        ]
    }
    real_get_json = resolver_mod.get_json
    try:
        resolver_mod.get_json = lambda url: fake_results
        from unified_resolver.models import TrackMetadata
        t = TrackMetadata(title="Любочка", artists=["Маша и Медведи"])
        enrich_from_deezer(t)
        # Должен выбрать запись нужного артиста из студийного альбома.
        assert t.duration_sec == 257, f"expected 257, got {t.duration_sec}"
        assert t.isrc == "FR2X41802514"
    finally:
        resolver_mod.get_json = real_get_json


def test_deezer_enrich_falls_back_to_weak_when_no_strong():
    """Если студийного альбома нужного артиста нет, enrichment всё равно
    берёт доступную запись (не ухудшаем поведение для редких релизов)."""
    import unified_resolver.resolver as resolver_mod
    from unified_resolver.resolver import enrich_from_deezer

    fake_results = {
        "data": [
            {
                "title": "Редкая песня",
                "artist": {"name": "Исполнитель"},
                "album": {"title": "Greatest Hits", "cover_big": "x"},
                "duration": 200, "isrc": "FB1",
            },
        ]
    }
    real_get_json = resolver_mod.get_json
    try:
        resolver_mod.get_json = lambda url: fake_results
        from unified_resolver.models import TrackMetadata
        t = TrackMetadata(title="Редкая песня", artists=["Исполнитель"])
        enrich_from_deezer(t)
        assert t.duration_sec == 200
    finally:
        resolver_mod.get_json = real_get_json


# ----------------- duration validation (A) -----------------

def test_duration_validator_drops_suspicious_reference():
    """Регрессия Vau_Muzloto_184 #29/#36: enrichment привязался к чужой
    длительности (318с), а реальный оригинал идёт ~257с. Медиана YouTube-
    выдачи ~257 — расхождение >15%, эталон должен быть обнулён, чтобы
    duration-фильтр не убивал настоящий оригинал."""
    from unified_resolver.duration_check import DurationValidator
    from unified_resolver.models import TrackMetadata

    # Имитируем YouTube-выдачу: преобладают версии ~255–262с.
    fake_durations = [257, 260, 255, 262, 273, 261, 258]
    validator = DurationValidator(search_fn=lambda q: fake_durations)

    t = TrackMetadata(title="Любочка", artists=["Маша и Медведи"], duration_sec=318)
    note = validator.validate(t)
    assert t.duration_sec is None, "reference duration should have been dropped"
    assert note and note.startswith("reference_duration_dropped")
    assert "deviation=" in note


def test_duration_validator_keeps_consistent_reference():
    """При согласованности эталона с медианой выдачи — ничего не меняется."""
    from unified_resolver.duration_check import DurationValidator
    from unified_resolver.models import TrackMetadata

    fake_durations = [256, 260, 258, 262, 257, 259, 261]
    validator = DurationValidator(search_fn=lambda q: fake_durations)

    t = TrackMetadata(title="Песня", artists=["А"], duration_sec=258)
    note = validator.validate(t)
    assert t.duration_sec == 258
    assert note and note.startswith("ok:")


def test_duration_validator_skips_when_too_few_samples():
    """Мало данных — не принимаем поспешных решений, оставляем эталон."""
    from unified_resolver.duration_check import DurationValidator
    from unified_resolver.models import TrackMetadata

    validator = DurationValidator(search_fn=lambda q: [257, 999])
    t = TrackMetadata(title="Песня", artists=["А"], duration_sec=318)
    note = validator.validate(t)
    assert t.duration_sec == 318
    assert note is None


def test_duration_validator_tolerates_search_error():
    """Сетевая ошибка YouTube не должна заваливать весь pipeline."""
    from unified_resolver.duration_check import DurationValidator
    from unified_resolver.models import TrackMetadata

    def boom(_q):
        raise RuntimeError("network down")

    t = TrackMetadata(title="Песня", artists=["А"], duration_sec=200)
    note = DurationValidator(search_fn=boom).validate(t)
    # Эталон сохранён, проверка просто пропущена.
    assert t.duration_sec == 200
    assert note and note.startswith("validation_skipped")


# ----------------- MusicBrainz enrichment (E) -----------------

_MB_LJOBOCHKA_RESPONSE = {
    "recordings": [
        {
            "title": "Любочка", "length": 261000, "video": None,
            "disambiguation": None,
            "releases": [{"title": "Звездная серия", "date": "2000",
                          "release-group": {"primary-type": "Album"}}],
        },
        {
            "title": "Любочка", "length": 256000, "video": None,
            "disambiguation": None,
            "releases": [{"title": "ВсеСОЮЗный 2", "date": "1998",
                          "release-group": {"primary-type": "Album"}}],
        },
        {
            # Сборник — должен быть отсеян.
            "title": "Любочка", "length": 318000, "video": None,
            "disambiguation": None,
            "releases": [{"title": "DJ Groove и все, все...", "date": "1999",
                          "release-group": {"primary-type": "Album",
                                            "secondary-types": ["Compilation"]}}],
        },
    ]
}


def test_musicbrainz_overwrites_wrong_reference_duration():
    """Регрессия: MusicBrainz должен перезаписать enrichment-длительность,
    если она расходится с медианой студийных релизов > 10%. Кейс «Маша и
    Медведи - Любочка»: Deezer дал 318с из сборника, MB знает ~258с.
    Медиана студийных {261, 256} = 258 (сборник 318 отсеян)."""
    import unified_resolver.musicbrainz as mb_mod
    from unified_resolver.musicbrainz import enrich_from_musicbrainz
    from unified_resolver.models import TrackMetadata

    real_get_json = mb_mod.get_json
    try:
        mb_mod.get_json = lambda url, **kw: _MB_LJOBOCHKA_RESPONSE
        t = TrackMetadata(title="Любочка", artists=["Маша и Медведи"], duration_sec=318)
        note = enrich_from_musicbrainz(t)
        assert t.duration_sec == 258, f"expected 258, got {t.duration_sec}"
        assert note and note.startswith("mb_overwrote_duration")
    finally:
        mb_mod.get_json = real_get_json


def test_musicbrainz_keeps_consistent_reference():
    """При согласии MusicBrainz с текущим ref-duration — ничего не меняется."""
    import unified_resolver.musicbrainz as mb_mod
    from unified_resolver.musicbrainz import enrich_from_musicbrainz
    from unified_resolver.models import TrackMetadata

    real_get_json = mb_mod.get_json
    try:
        mb_mod.get_json = lambda url, **kw: _MB_LJOBOCHKA_RESPONSE
        t = TrackMetadata(title="Любочка", artists=["Маша и Медведи"], duration_sec=259)
        note = enrich_from_musicbrainz(t)
        assert t.duration_sec == 259
        assert note and note.startswith("mb_ok:")
    finally:
        mb_mod.get_json = real_get_json


def test_musicbrainz_sets_duration_when_missing():
    """Если у трека не было duration_sec — берём медиану MusicBrainz."""
    import unified_resolver.musicbrainz as mb_mod
    from unified_resolver.musicbrainz import enrich_from_musicbrainz
    from unified_resolver.models import TrackMetadata

    real_get_json = mb_mod.get_json
    try:
        mb_mod.get_json = lambda url, **kw: _MB_LJOBOCHKA_RESPONSE
        t = TrackMetadata(title="Любочка", artists=["Маша и Медведи"])
        note = enrich_from_musicbrainz(t)
        assert t.duration_sec == 258
        assert note and note.startswith("mb_set_duration")
    finally:
        mb_mod.get_json = real_get_json


def test_musicbrainz_works_with_single_studio_sample():
    """MusicBrainz дедуплицирует recordings по ID, поэтому для нишевых
    треков возвращается 1 студийная запись — её достаточно для перезаписи."""
    import unified_resolver.musicbrainz as mb_mod
    from unified_resolver.musicbrainz import enrich_from_musicbrainz
    from unified_resolver.models import TrackMetadata

    single = {"recordings": [{
        "title": "Song", "length": 240000, "video": None, "disambiguation": None,
        "releases": [{"title": "Studio Album", "date": "2000",
                      "release-group": {"primary-type": "Album"}}],
    }]}
    real_get_json = mb_mod.get_json
    try:
        mb_mod.get_json = lambda url, **kw: single
        t = TrackMetadata(title="Song", artists=["X"], duration_sec=320)
        note = enrich_from_musicbrainz(t)
        assert t.duration_sec == 240
        assert note and note.startswith("mb_overwrote_duration")
    finally:
        mb_mod.get_json = real_get_json


def test_musicbrainz_skips_live_and_remix_disambiguations():
    """Recording с disambiguation 'live'/'remix' не должен учитываться."""
    from unified_resolver.musicbrainz import _collect_studio_durations
    from unified_resolver.models import TrackMetadata

    data = {
        "recordings": [
            {"title": "Song", "length": 240000, "video": None, "disambiguation": None,
             "releases": [{"release-group": {"primary-type": "Album"}}]},
            {"title": "Song", "length": 300000, "video": None, "disambiguation": "live recording",
             "releases": [{"release-group": {"primary-type": "Album"}}]},
            {"title": "Song", "length": 250000, "video": None, "disambiguation": "radio edit",
             "releases": [{"release-group": {"primary-type": "Album"}}]},
        ]
    }
    t = TrackMetadata(title="Song", artists=["X"])
    durations = _collect_studio_durations(data, t)
    # Только студийная 240с осталась.
    assert durations == [240]


def test_musicbrainz_tolerates_api_failure():
    """Сетевая ошибка / пустой ответ не ломают pipeline."""
    import unified_resolver.musicbrainz as mb_mod
    from unified_resolver.musicbrainz import enrich_from_musicbrainz
    from unified_resolver.models import TrackMetadata

    real_get_json = mb_mod.get_json
    try:
        mb_mod.get_json = lambda url, **kw: None
        t = TrackMetadata(title="Песня", artists=["А"], duration_sec=200)
        note = enrich_from_musicbrainz(t)
        assert t.duration_sec == 200  # эталон сохранён
        assert note is None
    finally:
        mb_mod.get_json = real_get_json


# ----------------- provenance (I) -----------------

def test_save_provenance_writes_json(tmp_path):
    from unified_resolver.downloader import Downloader
    from unified_resolver.models import TrackMetadata
    import json

    dl = Downloader.__new__(Downloader)
    dl.format = "mp3"
    track = TrackMetadata(title="Хочешь", artists=["Земфира"], duration_sec=234)
    provenance = {
        "track": {"title": "Хочешь", "artists": ["Земфира"], "duration_sec_reference": 234},
        "selected": {"source": "youtube", "title": "Хочешь", "actual_duration": 234},
        "tried": [],
    }
    dl._save_provenance(track, provenance, str(tmp_path))
    out = tmp_path / "Земфира - Хочешь.source.json"
    assert out.exists()
    saved = json.loads(out.read_text(encoding="utf-8"))
    assert saved["selected"]["source"] == "youtube"
    assert saved["track"]["duration_sec_reference"] == 234


# ----------------- простой раннер -----------------

def _run():
    failures = []
    tests = sorted(
        (name, obj) for name, obj in sorted(globals().items()) if name.startswith("test_")
    )
    for name, obj in tests:
        try:
            if "tmp_path" in getattr(obj, "__code__").co_varnames[: obj.__code__.co_argcount]:
                with tempfile.TemporaryDirectory() as tmp:
                    obj(Path(tmp))
            else:
                obj()
            print(f"PASS {name}")
        except AssertionError as exc:
            failures.append((name, str(exc)))
            print(f"FAIL {name}: {exc}")
        except Exception as exc:  # noqa: BLE001
            failures.append((name, repr(exc)))
            print(f"ERROR {name}: {exc!r}")
    print(f"\n{len(tests) - len(failures)}/{len(tests)} passed")
    return not failures


if __name__ == "__main__":
    sys.exit(0 if _run() else 1)
