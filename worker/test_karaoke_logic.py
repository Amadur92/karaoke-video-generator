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
    build_karaoke_from_align_result,
    clean_word,
    clamp_word_timing,
    distribute_words_between_anchors,
    evaluate_alignment_quality,
    estimate_line_duration,
    fuzzy_word_match,
    match_lyrics_to_whisper,
    parse_lrc_timestamp,
    parse_timestamped_lyrics,
    shift_karaoke_timings,
    strip_lrc_timestamps,
    _redistribute_words_in_line,
)
from karaoke_alignment import normalize_lyrics_text  # noqa: E402


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


def test_normalize_lyrics_text_preserves_user_line_breaks():
    text = "first phrase, second phrase\none...Two\nA denial\nA denial"
    normalized = normalize_lyrics_text(text)
    assert normalized.splitlines() == [
        "first phrase, second phrase",
        "one...Two",
        "A denial",
        "A denial",
    ]


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


def test_alignment_quality_report_ok_for_clean_timings():
    karaoke = [
        {"start": 1.0, "end": 2.0, "words": [
            {"word": "hello", "start": 1.0, "end": 1.4},
            {"word": "world", "start": 1.45, "end": 2.0},
        ]},
        {"start": 3.0, "end": 4.0, "words": [
            {"word": "next", "start": 3.0, "end": 3.4},
            {"word": "line", "start": 3.45, "end": 4.0},
        ]},
    ]
    report = evaluate_alignment_quality(karaoke, audio_duration=5.0, source="test")
    assert report["summary"] == "ok"
    assert report["score"] == 1.0
    assert report["line_count"] == 2
    assert report["word_count"] == 4
    assert report["issues"] == []


def test_alignment_quality_report_flags_suspicious_timings():
    karaoke = [
        {"start": 1.0, "end": 8.5, "words": [
            {"word": "very", "start": 1.0, "end": 4.3},
            {"word": "late", "start": 7.2, "end": 7.7},
        ]},
        {"start": 7.0, "end": 8.0, "words": [
            {"word": "overlap", "start": 7.0, "end": 7.01},
        ]},
    ]
    report = evaluate_alignment_quality(karaoke, audio_duration=7.5, source="test")
    kinds = {issue["kind"] for issue in report["issues"]}
    assert report["summary"] == "needs_repair"
    assert "line_overlap" in kinds
    assert "long_word" in kinds
    assert "large_internal_gap" in kinds
    assert "tiny_word" in kinds
    assert "line_out_of_bounds" in kinds
    assert report["score"] < 1.0


def test_alignment_quality_report_flags_text_mismatch():
    karaoke = [
        {"start": 1.0, "end": 2.0, "words": [
            {"word": "hello", "start": 1.0, "end": 1.4},
            {"word": "world", "start": 1.45, "end": 2.0},
        ]},
    ]
    report = evaluate_alignment_quality(
        karaoke,
        audio_duration=5.0,
        source="test",
        text_match={"score": 0.2, "recognized_preview": "totally different"},
    )
    assert report["summary"] == "needs_repair"
    assert report["metrics"]["text_match_score"] == 0.2
    assert any(issue["kind"] == "text_mismatch" for issue in report["issues"])


def test_alignment_quality_report_accepts_good_text_match():
    karaoke = [
        {"start": 1.0, "end": 2.0, "words": [
            {"word": "hello", "start": 1.0, "end": 1.4},
            {"word": "world", "start": 1.45, "end": 2.0},
        ]},
    ]
    report = evaluate_alignment_quality(
        karaoke,
        audio_duration=5.0,
        source="test",
        text_match={"score": 0.9, "recognized_preview": "hello world"},
    )
    assert report["summary"] == "ok"
    assert report["metrics"]["text_match_score"] == 0.9


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


def test_match_repeated_chorus_keeps_chronological_order():
    """Повторяющийся припев: каждая строка текста привязывается к своему по времени
    появлению в аудио, а не «съедается» с потерей синхронизации.

    Регрессия: при подаче на вход свободной транскрипции (с пропусками в проигрышах)
    нечёткий матчинг ломался на повторах — перескакивал к более позднему появлению,
    и середина песни схлопывалась. Forced alignment по точному тексту песни даёт
    слова в хронологическом порядке, поэтому каждое текстовое вождение должно
    получить тайминги своего, более раннего, появления.
    """
    chorus = "i love you"
    lyrics = ["verse one", chorus, chorus]
    whisper = [
        # verse one
        _w("verse", 10.0, 10.4, 0.9), _w("one", 10.4, 10.8, 0.9),
        # первое появление припева (~20с)
        _w("i", 20.0, 20.2, 0.9), _w("love", 20.2, 20.5, 0.9), _w("you", 20.5, 20.9, 0.9),
        # второе появление припева (~40с)
        _w("i", 40.0, 40.2, 0.9), _w("love", 40.2, 40.5, 0.9), _w("you", 40.5, 40.9, 0.9),
    ]
    karaoke, _ = match_lyrics_to_whisper(lyrics, whisper)
    assert len(karaoke) == 3
    # verse — до 11с
    assert karaoke[0]["end"] <= 11.0
    # ПЕРВЫЙ припев должен попасть на ~20с, а не «съехать» на второй (40с)
    first_chorus = karaoke[1]
    assert 19.0 <= first_chorus["start"] <= 21.0, (
        f"первый припев должен быть у 20с, а попал на {first_chorus['start']:.1f}"
    )
    # ВТОРОЙ припев — у 40с, то есть строки идут в хронологическом порядке
    second_chorus = karaoke[2]
    assert 39.0 <= second_chorus["start"] <= 41.0, (
        f"второй припев должен быть у 40с, а попал на {second_chorus['start']:.1f}"
    )
    assert first_chorus["end"] < second_chorus["start"]


def _seg(text, word_tuples):
    """Имитация сегмента WhisperResult для тестов прямого пути."""
    return SimpleNamespace(
        text=text,
        words=[SimpleNamespace(word=w[0], start=w[1], end=w[2]) for w in word_tuples],
    )


def test_build_karaoke_from_align_direct():
    """Прямой путь: сегменты align() 1-в-1 по строкам дают готовые тайминги."""
    raw_lines = ["hello world", "foo bar"]
    result = SimpleNamespace(segments=[
        _seg("hello world", [("hello", 1.0, 1.4), ("world", 1.4, 1.9)]),
        _seg("foo bar", [("foo", 3.0, 3.3), ("bar", 3.3, 3.6)]),
    ])
    kara, coverage = build_karaoke_from_align_result(raw_lines, result)
    assert coverage == 1.0
    assert len(kara) == 2
    assert kara[0]["start"] == 1.0 and kara[0]["end"] == 1.9
    assert kara[1]["start"] == 3.0
    assert [w["word"] for w in kara[0]["words"]] == ["hello", "world"]


def test_build_karaoke_from_align_keeps_repeated_chorus_order():
    """Глобальная монотонность: повтор припева берёт своё появление в аудио.

    Это ключевое преимущество прямого пути над match_lyrics_to_whisper: align()
    выравнивает именно второй экземпляр текста на второе появление вокала,
    а матчер схлопнул бы оба вхождения на одно.
    """
    chorus = "i love you"
    raw_lines = ["intro line", chorus, chorus]
    result = SimpleNamespace(segments=[
        _seg("intro line", [("intro", 5.0, 5.5), ("line", 5.5, 6.0)]),
        _seg("i love you", [("i", 20.0, 20.2), ("love", 20.2, 20.5), ("you", 20.5, 20.9)]),
        _seg("i love you", [("i", 40.0, 40.2), ("love", 40.2, 40.5), ("you", 40.5, 40.9)]),
    ])
    kara, coverage = build_karaoke_from_align_result(raw_lines, result)
    assert coverage == 1.0
    # ПЕРВЫЙ припев — у 20с, ВТОРОЙ — у 40с. порядок хронологический.
    assert 19.0 <= kara[1]["start"] <= 21.0
    assert 39.0 <= kara[2]["start"] <= 41.0
    assert kara[1]["end"] < kara[2]["start"]


def test_build_karaoke_from_align_fixes_zero_duration():
    """Слово с нулевой длительностью получает минимальный тайминг, не теряется."""
    result = SimpleNamespace(segments=[
        _seg("x", [("x", 5.0, 5.0)]),
    ])
    kara, coverage = build_karaoke_from_align_result(["x"], result)
    assert coverage == 1.0
    assert kara[0]["words"][0]["end"] > kara[0]["words"][0]["start"]


def test_build_karaoke_from_align_none_result():
    """None-результат → нельзя построить, возвращается (None, 0.0) для фолбэка."""
    kara, coverage = build_karaoke_from_align_result(["x"], None)
    assert kara is None and coverage == 0.0


def _redistribute_invariants(line):
    """Проверяет инварианты перераспределённой строки. Возвращает list ошибок."""
    errs = []
    ls, le = line["start"], line["end"]
    words = line["words"]
    if not words:
        return errs
    # 1. Якоря строки сохранены (начало первого, конец последнего).
    if abs(words[0]["start"] - ls) > 0.02:
        errs.append(f"first word start {words[0]['start']} != line start {ls}")
    if abs(words[-1]["end"] - le) > 0.02:
        errs.append(f"last word end {words[-1]['end']} != line end {le}")
    # 2. Монотонность и ненулевые длительности.
    for i, w in enumerate(words):
        if w["end"] <= w["start"]:
            errs.append(f"word {i} '{w['word']}' zero/negative duration")
        if i > 0 and w["start"] < words[i - 1]["end"] - 0.01:
            errs.append(f"word {i} '{w['word']}' overlaps previous")
    return errs


def test_redistribute_fixes_stretched_word():
    """Растянутое слово (>1.8с) после перераспределения становится <= max_word_dur."""
    # Whisper «приклеил» 'sustained' на 3 секунды к одной ноте.
    line = {
        "text": "one sustained three",
        "start": 10.0, "end": 14.0,
        "words": [
            {"word": "one", "start": 10.0, "end": 10.4},
            {"word": "sustained", "start": 10.4, "end": 13.6},  # 3.2с — растянуто
            {"word": "three", "start": 13.6, "end": 14.0},
        ],
    }
    _redistribute_words_in_line(line)
    errs = _redistribute_invariants(line)
    assert not errs, errs
    # Ни одно слово не должно быть растянуто (>0.9с по умолчанию).
    for w in line["words"]:
        assert w["end"] - w["start"] <= 0.9 + 0.01, (
            f"'{w['word']}' остался растянут: {w['end']-w['start']:.2f}s"
        )
    # 'sustained' (длиннее) должен получить больше времени, чем 'one'.
    d = {w["word"]: w["end"] - w["start"] for w in line["words"]}
    assert d["sustained"] > d["one"]


def test_redistribute_fixes_tiny_word():
    """Микро-слово (<0.12с) после перераспределения получает >= min_word_dur."""
    line = {
        "text": "alpha beta gamma",
        "start": 5.0, "end": 7.0,
        "words": [
            {"word": "alpha", "start": 5.0, "end": 5.8},
            {"word": "beta", "start": 5.8, "end": 5.85},  # 0.05с — микро
            {"word": "gamma", "start": 5.85, "end": 7.0},
        ],
    }
    _redistribute_words_in_line(line)
    errs = _redistribute_invariants(line)
    assert not errs, errs
    for w in line["words"]:
        assert w["end"] - w["start"] >= 0.15, (
            f"'{w['word']}' остался микро: {w['end']-w['start']:.3f}s"
        )


def test_redistribute_single_word_line():
    """Строка из одного слова: оно заполняет весь span, без разрывов."""
    line = {
        "text": "solo", "start": 20.0, "end": 24.0,
        "words": [{"word": "solo", "start": 20.0, "end": 24.0}],
    }
    _redistribute_words_in_line(line)
    assert line["words"][0]["start"] == 20.0
    assert line["words"][0]["end"] == 24.0


def test_redistribute_preserves_line_anchors_many_words():
    """На длинной строке: начало/конец строки не сдвигаются, всё монотонно."""
    line = {
        "text": "a b c d e f g h",
        "start": 0.0, "end": 3.0,
        "words": [{"word": c, "start": 0.0 if i == 0 else 3.0, "end": 3.0}
                  for i, c in enumerate("abcdefgh")],
    }
    _redistribute_words_in_line(line)
    errs = _redistribute_invariants(line)
    assert not errs, errs


def test_redistribute_zero_span_line():
    """Вырожденный случай: span строки <= 0 — слова разворачиваются от line.start."""
    line = {
        "text": "collapsed here", "start": 5.0, "end": 5.0,
        "words": [
            {"word": "collapsed", "start": 5.0, "end": 5.0},
            {"word": "here", "start": 5.0, "end": 5.0},
        ],
    }
    _redistribute_words_in_line(line)
    # Каждое слово получило минимальную длительность, конец строки сдвинут.
    for w in line["words"]:
        assert w["end"] > w["start"]
    assert line["end"] >= line["start"]


def test_build_karaoke_applies_redistribution_by_default():
    """Прямой путь применяет перераспределение: на выходе нет растянутых слов."""
    result = SimpleNamespace(segments=[
        _seg("big stretched word", [
            ("big", 1.0, 1.3),
            ("stretched", 1.3, 4.5),   # 3.2с растянуто
            ("word", 4.5, 5.0),
        ]),
    ])
    kara, _ = build_karaoke_from_align_result(["big stretched word"], result)
    assert kara[0]["start"] == 1.0 and kara[0]["end"] == 5.0
    for w in kara[0]["words"]:
        assert w["end"] - w["start"] <= 0.9 + 0.01


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
