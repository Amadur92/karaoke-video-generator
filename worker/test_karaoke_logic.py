"""Юнит-тесты для чистой логики worker-а: парсинг LRC, оценка длительности строк,
нечёткое сравнение слов, клампинг таймингов. Не требуют Whisper, аудио или моделей.

Запуск: python3 -m pytest worker/test_karaoke_logic.py
или:    python3 worker/test_karaoke_logic.py
"""

import os
import sys
from types import SimpleNamespace

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from karaoke_worker import (  # noqa: E402
    clean_word,
    clamp_word_timing,
    distribute_words_between_anchors,
    estimate_line_duration,
    fuzzy_word_match,
    match_lyrics_to_whisper,
    parse_lrc_timestamp,
    parse_timestamped_lyrics,
    shift_karaoke_timings,
    strip_lrc_timestamps,
)


def _w(word, start, end, probability=None):
    """Имитация слова stable-ts для тестов матчинга."""
    return SimpleNamespace(word=word, start=start, end=end, probability=probability)



def test_clean_word_strips_punctuation_and_case():
    assert clean_word("Hello,") == "hello"
    assert clean_word("WORLD!") == "world"
    assert clean_word("  Spaces  ") == "spaces"


def test_parse_lrc_timestamp_formats():
    assert parse_lrc_timestamp("01:23.45") == 83.45
    assert parse_lrc_timestamp("0:00.00") == 0.0
    assert parse_lrc_timestamp("1:05") == 65.0
    # разделитель может быть как точкой, так и двоеточием
    assert parse_lrc_timestamp("00:10:5") == 10.5


def test_parse_lrc_timestamp_rejects_garbage():
    assert parse_lrc_timestamp("abc") is None
    assert parse_lrc_timestamp("") is None
    # regex принимает любые 2 цифры, так что "99:99" валиден (99 мин 99 сек)
    assert parse_lrc_timestamp("99:99") == 99 * 60 + 99


def test_parse_timestamped_lyrics_orders_by_time():
    lrc = "[00:20.00]second\n[00:10.00]first\n[00:30.00]third"
    entries = parse_timestamped_lyrics(lrc)
    assert entries is not None
    assert [e["text"] for e in entries] == ["first", "second", "third"]
    assert [e["time"] for e in entries] == [10.0, 20.0, 30.0]


def test_parse_timestamped_lyrics_returns_none_for_plain_text():
    assert parse_timestamped_lyrics("just some lyrics\nwithout timestamps") is None


def test_strip_lrc_timestamps_keeps_text():
    lrc = "[00:10.00]hello world\n[00:20.00]next line"
    assert strip_lrc_timestamps(lrc) == "hello world\nnext line"


def test_estimate_line_duration_bounds():
    words = ["one", "two", "three"]
    # без ограничения по доступному времени
    dur = estimate_line_duration(words, None)
    assert 0.75 <= dur <= 5.2
    # укладывается в доступный запас с запасом
    dur_limited = estimate_line_duration(words, 1.0)
    assert dur_limited <= 0.88


def test_estimate_line_duration_empty():
    assert estimate_line_duration([], None) == 0.0


def test_fuzzy_word_match_exact_and_substring():
    assert fuzzy_word_match("Hello", "hello") is True
    # substring-матч требует длину >= 4 у обоих слов
    assert fuzzy_word_match("running", "runn") is True
    assert fuzzy_word_match("cat", "dog") is False


def test_fuzzy_word_match_rejects_empty():
    assert fuzzy_word_match("", "word") is False
    assert fuzzy_word_match("word", "") is False


def test_clamp_word_timing_keeps_inside_bounds():
    assert clamp_word_timing(1.0, 2.0, 0.0, 5.0) == (1.0, 2.0)


def test_clamp_word_timing_clamps_to_line():
    # старт ниже границы линии прижимается к line_start
    s, e = clamp_word_timing(-1.0, 2.0, 0.0, 5.0)
    assert s == 0.0
    # конец выше границы прижимается к line_end
    s, e = clamp_word_timing(4.0, 9.0, 0.0, 5.0)
    assert e == 5.0


def test_distribute_words_between_anchors_covers_span():
    words = [{"word": "a"}, {"word": "bb"}, {"word": "ccc"}]
    out = distribute_words_between_anchors(words, 1.0, 4.0)
    assert len(out) == 3
    # первый начинает с start_time, последний заканчивает в end_time
    assert out[0]["start"] == 1.0
    assert out[-1]["end"] == 4.0
    # тайминги монотонны
    for prev, cur in zip(out, out[1:]):
        assert cur["start"] >= prev["end"] - 0.01


def test_shift_karaoke_timings_applies_offset():
    karaoke = [
        {"start": 5.0, "end": 7.0, "words": [
            {"word": "hi", "start": 5.0, "end": 6.0},
            {"word": "there", "start": 6.0, "end": 7.0},
        ]},
    ]
    shifted = shift_karaoke_timings(karaoke, 10.0)
    assert shifted[0]["start"] == 15.0
    assert shifted[0]["end"] == 17.0
    assert shifted[0]["words"][0]["start"] == 15.0
    assert shifted[0]["words"][1]["end"] == 17.0


def test_shift_karaoke_timings_clamps_negative_to_zero():
    karaoke = [{"start": 3.0, "end": 5.0, "words": [
        {"word": "x", "start": 3.0, "end": 5.0},
    ]}]
    shifted = shift_karaoke_timings(karaoke, -10.0)
    assert shifted[0]["start"] == 0.0


# ----------------- match_lyrics_to_whisper -----------------

def test_match_exact_words_take_whisper_timings():
    """Идеальный случай: текст и Whisper совпадают слово-в-слово."""
    lyrics = ["hello world"]
    whisper = [_w("hello", 1.0, 1.4, 0.9), _w("world", 1.4, 1.9, 0.9)]
    karaoke, stats = match_lyrics_to_whisper(lyrics, whisper)
    assert len(karaoke) == 1
    words = karaoke[0]["words"]
    assert [w["word"] for w in words] == ["hello", "world"]
    assert words[0]["start"] == 1.0 and words[0]["end"] == 1.4
    assert words[1]["start"] == 1.4 and words[1]["end"] == 1.9
    assert stats["matched_words"] == 2
    assert stats["interpolated_words"] == 0


def test_match_skips_low_confidence_hallucination():
    """Низкоуверенное Whisper-слово не используется как якорь — галлюцинация отброшена."""
    lyrics = ["one two"]
    # 'hallucination' — вставка с низким confidence, 'two' смещён во времени
    whisper = [
        _w("one", 1.0, 1.3, 0.9),
        _w("hallucination", 1.3, 1.8, 0.1),
        _w("two", 1.8, 2.2, 0.9),
    ]
    karaoke, stats = match_lyrics_to_whisper(lyrics, whisper, confidence_threshold=0.5)
    words = karaoke[0]["words"]
    # 'one' матчится точно, 'two' находится через lookahead несмотря на вставку
    assert words[0]["word"] == "one" and words[0]["start"] == 1.0
    assert words[1]["word"] == "two" and words[1]["start"] == 1.8


def test_match_interpolates_missing_word_not_foreign_timing():
    """Слово, пропущенное Whisper, интерполируется, а не берёт чужой тайминг.

    Это ключевое отличие от прежнего безусловного fallback: 'skipped' нет в
    Whisper, но есть 'first' и 'last'. Тайминг 'skipped' должен лечь между
    ними, а не равняться таймингу 'last'.
    """
    lyrics = ["first skipped last"]
    whisper = [_w("first", 1.0, 1.4, 0.9), _w("last", 2.4, 2.9, 0.9)]
    karaoke, stats = match_lyrics_to_whisper(lyrics, whisper)
    words = karaoke[0]["words"]
    assert words[0]["word"] == "first" and words[0]["start"] == 1.0
    assert words[2]["word"] == "last" and words[2]["start"] == 2.4
    skipped = words[1]
    assert skipped["word"] == "skipped"
    # интерполированное слово должно быть между first.end и last.start
    assert 1.4 <= skipped["start"] < 2.4
    assert skipped["end"] <= 2.4
    assert stats["interpolated_words"] >= 1


def test_match_handles_extra_whisper_insertion():
    """Лишнее слово Whisper перепрыгивается, совпадение ищется дальше."""
    lyrics = ["go home"]
    whisper = [_w("go", 1.0, 1.3, 0.9), _w("now", 1.3, 1.6, 0.8), _w("home", 1.6, 2.0, 0.9)]
    karaoke, stats = match_lyrics_to_whisper(lyrics, whisper)
    words = karaoke[0]["words"]
    assert [w["word"] for w in words] == ["go", "home"]
    assert words[1]["start"] == 1.6  # тайминг настоящего 'home', а не вставки 'now'


def test_match_fuzzy_substring_for_long_words():
    """Длинные слова матчатся по подстроке (running/runn) при опечатках Whisper."""
    lyrics = ["running fast"]
    whisper = [_w("runn", 1.0, 1.5, 0.8), _w("fast", 1.5, 1.9, 0.8)]
    karaoke, stats = match_lyrics_to_whisper(lyrics, whisper)
    words = karaoke[0]["words"]
    assert words[0]["word"] == "running"
    assert words[0]["start"] == 1.0


def test_match_multiple_lines_independent_cursors():
    """Каждая строка продолжает матчинг с того места, где остановилась предыдущая."""
    lyrics = ["line one", "line two"]
    whisper = [
        _w("line", 1.0, 1.3, 0.9), _w("one", 1.3, 1.6, 0.9),
        _w("line", 2.0, 2.3, 0.9), _w("two", 2.3, 2.6, 0.9),
    ]
    karaoke, stats = match_lyrics_to_whisper(lyrics, whisper)
    assert len(karaoke) == 2
    assert karaoke[0]["words"][1]["word"] == "one" and karaoke[0]["words"][1]["start"] == 1.3
    assert karaoke[1]["words"][1]["word"] == "two" and karaoke[1]["words"][1]["start"] == 2.3


def test_match_empty_whisper_does_not_crash():
    """При пустом выводе Whisper вся строка интерполируется от нуля."""
    lyrics = ["some words here"]
    karaoke, stats = match_lyrics_to_whisper(lyrics, [])
    assert len(karaoke) == 1
    assert len(karaoke[0]["words"]) == 3
    # все слова имеют валидные тайминги (не None)
    for w in karaoke[0]["words"]:
        assert w["start"] is not None and w["end"] is not None
        assert w["end"] > w["start"]
    assert stats["matched_words"] == 0


def test_match_stats_reflect_quality():
    """Статистика честно отражает долю уверенно сматченных слов."""
    lyrics = ["a b c d e"]
    # только 'a' и 'e' имеют высокую уверенность
    whisper = [
        _w("a", 1.0, 1.2, 0.9),
        _w("b", 1.2, 1.4, 0.2),
        _w("c", 1.4, 1.6, 0.2),
        _w("d", 1.6, 1.8, 0.2),
        _w("e", 1.8, 2.0, 0.9),
    ]
    _, stats = match_lyrics_to_whisper(lyrics, whisper, confidence_threshold=0.5)
    assert stats["total_words"] == 5
    assert stats["matched_words"] == 2  # только 'a' и 'e'


def _run():
    """Простой раннер без pytest: запускает все функции test_* и сообщает результат."""
    failures = []
    tests = sorted(
        (name, obj) for name, obj in sorted(globals().items()) if name.startswith("test_")
    )
    for name, obj in tests:
        try:
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
