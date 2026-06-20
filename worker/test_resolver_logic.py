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

def _entry(title, duration, uploader="user", eid=None):
    return {
        "id": eid or title,
        "title": title,
        "duration": duration,
        "uploader": uploader,
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


def test_youtube_search_queries_adds_translit():
    """При латинском исполнителе добавляется кириллический вариант запроса."""
    from unified_resolver.downloader import Downloader
    dl = Downloader.__new__(Downloader)
    track = TrackMetadata(title="Hochesh", artists=["Zemfira"], duration_sec=234)
    queries = dl._youtube_search_queries("Zemfira - Hochesh", track)
    assert len(queries) == 2
    assert any(has_cyrillic(q) for q in queries)


# ----------------- file validation -----------------

def test_is_valid_audio_file_rejects_missing():
    assert is_valid_audio_file(None) is False
    assert is_valid_audio_file("/nonexistent/path.xyz") is False


def test_is_valid_audio_file_rejects_tiny(tmp_path):
    small = tmp_path / "tiny.mp3"
    small.write_bytes(b"not really audio")
    assert is_valid_audio_file(str(small)) is False


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
