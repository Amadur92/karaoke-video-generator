#!/usr/bin/env python3
import os
import sys
import json
import re
import uuid
import math
import bisect
import threading
import subprocess
import traceback
import ssl
import shutil

# Отключаем проверку SSL-сертификатов: нужна для скачивания моделей/инструментов на macOS.
ssl._create_default_https_context = ssl._create_unverified_context


def configure_utf8_stdio() -> None:
    """Avoid Windows console code page crashes when logs contain mixed alphabets."""
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except Exception:
            pass


configure_utf8_stdio()

if getattr(sys, "frozen", False):
    BASE_DIR = os.path.dirname(os.path.abspath(sys.executable))
    RESOURCE_DIR = getattr(sys, "_MEIPASS", BASE_DIR)
else:
    BASE_DIR = os.path.dirname(os.path.abspath(__file__))
    RESOURCE_DIR = BASE_DIR

EXPORT_FOLDER = os.environ.get("KARAOKE_EXPORT_DIR", os.path.join(BASE_DIR, "web_exports"))
os.makedirs(EXPORT_FOLDER, exist_ok=True)


# Глобальный словарь для фоновых задач
# Формат: { job_id: { "progress": float, "status": str, "done": bool, "error": str, "file": str } }
jobs = {}

from karaoke_alignment import (
    align_timestamped_lrc_words,
    build_karaoke_from_timestamped_lyrics,
    clean_word,
    clamp_word_timing,
    distribute_words_between_anchors,
    evaluate_alignment_quality,
    estimate_line_duration,
    fuzzy_word_match,
    lyric_text_score,
    normalize_lyrics_text,
    normalize_mixed_cyrillic_text,
    normalize_mixed_cyrillic_word,
    parse_lrc_timestamp,
    parse_timestamped_lyrics,
    refine_timestamped_words_with_whisper,
    replace_special_spaces,
    shift_karaoke_timings,
    strip_lrc_timestamps,
    timestamped_matches_whisper_probe,
    timestamped_whisper_probe_decision,
)

def subprocess_no_window_kwargs():
    if os.name == "nt":
        return {"creationflags": getattr(subprocess, "CREATE_NO_WINDOW", 0x08000000)}
    return {}


def alignment_report_path_for_timings(timings_path):
    root, _ = os.path.splitext(timings_path)
    return f"{root}_alignment_report.json"


def write_timings_and_report(job_id, lyrics_karaoke, timings_output=None, audio_duration=None, source="unknown", text_match=None):
    final_dump_path = timings_output or os.path.join(EXPORT_FOLDER, f"{job_id}_timings_final.json")
    with open(final_dump_path, 'w', encoding='utf-8') as f:
        json.dump(lyrics_karaoke, f, ensure_ascii=False, indent=2)

    report = evaluate_alignment_quality(
        lyrics_karaoke,
        audio_duration=audio_duration,
        source=source,
        text_match=text_match,
    )
    report_path = alignment_report_path_for_timings(final_dump_path)
    with open(report_path, 'w', encoding='utf-8') as f:
        json.dump(report, f, ensure_ascii=False, indent=2)

    jobs[job_id]["timings_file"] = final_dump_path
    jobs[job_id]["alignment_report_file"] = report_path
    jobs[job_id]["alignment_score"] = report.get("score")
    jobs[job_id]["alignment_summary"] = report.get("summary")
    return final_dump_path, report_path, report

# ----------------- ВСПОМОГАТЕЛЬНЫЕ ФУНКЦИИ -----------------
def split_plain_lyrics_phrases(text):
    """Дробит plain text на более короткие вокальные фразы.

    Stable Whisper хуже выравнивает строки, где через запятую склеены две
    самостоятельные фразы с паузой в аудио. Для LRC это не применяется:
    там разбиение строк уже является пользовательской разметкой.
    """
    if not text or parse_timestamped_lyrics(text):
        return text

    result = []
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if not line:
            result.append(raw_line)
            continue

        parts = [part.strip() for part in re.split(r"\s*[,;]\s*", line) if part.strip()]
        if len(parts) <= 1:
            result.append(raw_line.rstrip())
            continue

        split_parts = []
        for part in parts:
            words = [word for word in part.split() if clean_word(word)]
            if len(words) < 2:
                split_parts = []
                break
            if split_parts and clean_word(words[0]) in {"что", "чем", "как", "где", "кто"}:
                split_parts = []
                break
            split_parts.append(part)

        if split_parts:
            result.extend(split_parts)
        else:
            result.append(raw_line.rstrip())

    return "\n".join(result).strip()

def line_internal_max_gap(line_data):
    words = line_data.get("words") or []
    max_gap = 0.0
    for prev, curr in zip(words, words[1:]):
        try:
            max_gap = max(max_gap, float(curr["start"]) - float(prev["end"]))
        except Exception:
            pass
    return max_gap

def line_zero_word_ratio(line_data):
    words = line_data.get("words") or []
    if not words:
        return 0.0
    tiny = 0
    for word in words:
        try:
            if float(word["end"]) - float(word["start"]) <= 0.04:
                tiny += 1
        except Exception:
            tiny += 1
    return tiny / len(words)

def is_short_interjection_line(line_data):
    words = line_data.get("words") or []
    clean_words = [clean_word(w.get("word", "")) for w in words]
    clean_words = [w for w in clean_words if w]
    if not clean_words or len(clean_words) > 3:
        return False
    interjections = {
        "yay", "yeah", "hey", "oh", "ah", "oooh", "ooh", "la", "na",
        "denial"
    }
    return all(word in interjections for word in clean_words)

def compact_interjection_run_before_tail(lyrics_karaoke, suspect_idx):
    start_idx = suspect_idx
    while start_idx > 0 and is_short_interjection_line(lyrics_karaoke[start_idx - 1]):
        start_idx -= 1
    if start_idx == suspect_idx:
        return lyrics_karaoke, suspect_idx

    run = lyrics_karaoke[start_idx:suspect_idx]
    kept = []
    for line in run:
        if not kept:
            kept.append(line)
            continue
        try:
            gap = float(line["start"]) - float(kept[-1]["end"])
        except Exception:
            gap = 0.0
        if gap <= 3.0:
            kept.append(line)

    compact_words = []
    compact_text_parts = []
    for line in kept:
        compact_words.extend(line.get("words") or [])
        compact_text_parts.append(line.get("text", "").strip())
    if not compact_words:
        return lyrics_karaoke, suspect_idx

    compact_line = {
        "text": " ".join(part for part in compact_text_parts if part),
        "start": compact_words[0]["start"],
        "end": compact_words[-1]["end"],
        "words": compact_words,
    }
    repaired = lyrics_karaoke[:start_idx] + [compact_line] + lyrics_karaoke[suspect_idx:]
    return repaired, start_idx + 1

def repeated_line_key(text):
    words = [clean_word(word) for word in re.split(r'\s+', text or '') if clean_word(word)]
    if len(words) < 2:
        return None
    half = len(words) // 2
    if half >= 2 and words[:half] == words[half:half * 2]:
        return " ".join(words[:half])
    return " ".join(words[:2])

def redistribute_repeated_tail_lines(lyrics_karaoke):
    result = list(lyrics_karaoke)
    idx = 0
    while idx < len(result):
        key = repeated_line_key(result[idx].get("text", ""))
        if not key:
            idx += 1
            continue
        end = idx + 1
        while end < len(result) and repeated_line_key(result[end].get("text", "")) == key:
            end += 1
        run_len = end - idx
        if run_len < 3:
            idx = end
            continue

        run = result[idx:end]
        broken = False
        for line in run:
            try:
                if float(line["end"]) - float(line["start"]) > 6.0:
                    broken = True
                if line_internal_max_gap(line) > 3.0:
                    broken = True
            except Exception:
                broken = True
        if not broken:
            idx = end
            continue

        start = float(run[0]["start"])
        interval = 4.0
        for line_idx, line in enumerate(run):
            words = line.get("text", "").split()
            if not words:
                continue
            line_start = start + line_idx * interval
            line_duration = 2.45 if len(words) > 2 else 1.25
            total_chars = sum(max(len(clean_word(word)), 1) for word in words) or len(words)
            cursor = line_start
            new_words = []
            for word in words:
                slot = max(0.16, line_duration * max(len(clean_word(word)), 1) / total_chars)
                new_words.append({
                    "word": word,
                    "start": round(cursor, 3),
                    "end": round(min(line_start + line_duration, cursor + slot * 0.88), 3),
                })
                cursor += slot
            line["words"] = new_words
            line["start"] = new_words[0]["start"]
            line["end"] = new_words[-1]["end"]

        idx = end
    return result

def repair_short_lines_with_large_internal_gaps(lyrics_karaoke):
    repaired = []
    for line in lyrics_karaoke:
        words = line.get("words") or []
        if len(words) < 2 or len(words) > 5:
            repaired.append(line)
            continue

        try:
            line_start = float(line["start"])
            line_end = float(line["end"])
        except Exception:
            repaired.append(line)
            continue

        duration = line_end - line_start
        expected = estimate_line_duration([w.get("word", "") for w in words], None)
        max_gap = line_internal_max_gap(line)
        if max_gap < 2.5 or duration < max(4.0, expected * 2.1):
            repaired.append(line)
            continue

        text_words = [w.get("word", "") for w in words]
        total_chars = sum(max(len(clean_word(word)), 1) for word in text_words) or len(text_words)
        line_duration = min(max(expected, 1.1), max(1.0, duration - 0.25))
        cursor = line_start
        new_words = []
        for word in text_words:
            slot = max(0.16, line_duration * max(len(clean_word(word)), 1) / total_chars)
            new_words.append({
                "word": word,
                "start": round(cursor, 3),
                "end": round(min(line_start + line_duration, cursor + slot * 0.9), 3),
            })
            cursor += slot

        repaired_line = dict(line)
        repaired_line["words"] = new_words
        repaired_line["start"] = new_words[0]["start"]
        repaired_line["end"] = new_words[-1]["end"]
        repaired.append(repaired_line)

    return repaired

def repair_stretched_short_lines(lyrics_karaoke):
    repaired = []
    for line in lyrics_karaoke:
        words = line.get("words") or []
        if len(words) < 2 or len(words) > 6:
            repaired.append(line)
            continue

        try:
            line_start = float(line["start"])
            line_end = float(line["end"])
        except Exception:
            repaired.append(line)
            continue

        duration = line_end - line_start
        expected = estimate_line_duration([w.get("word", "") for w in words], None)
        max_gap = line_internal_max_gap(line)
        if max_gap < 8.0 or duration < max(10.0, expected * 3.0):
            repaired.append(line)
            continue

        total_chars = sum(max(len(clean_word(w.get("word", ""))), 1) for w in words) or len(words)
        compact_duration = min(max(expected, 1.4), 5.5)
        cursor = line_start
        new_words = []
        for word_data in words:
            word = word_data.get("word", "")
            slot = max(0.18, compact_duration * max(len(clean_word(word)), 1) / total_chars)
            new_words.append({
                "word": word,
                "start": round(cursor, 3),
                "end": round(min(line_start + compact_duration, cursor + slot * 0.9), 3),
            })
            cursor += slot

        repaired_line = dict(line)
        repaired_line["words"] = new_words
        repaired_line["start"] = new_words[0]["start"]
        repaired_line["end"] = new_words[-1]["end"]
        repaired.append(repaired_line)

    return repaired

VOCALIZATION_SYLLABLES = (
    "на", "ня", "ну", "ла", "ля", "лу", "да", "ра", "уа", "оу", "уо",
    "еа", "иа", "ай", "ой", "эй", "ей", "ах", "ох", "ух", "хэй", "мм",
    "na", "nah", "la", "lya", "da", "ra", "ah", "oh", "ooh", "oo", "uh",
    "woo", "whoa", "woah", "yeah", "yea", "yah", "ya", "yo", "yu", "hey",
    "ha", "ay", "ey", "eh", "mm", "hm", "hmm",
)
OPEN_VOCALIZATION_SYLLABLES = ("а", "о", "у", "э", "е", "и", "a", "o", "u", "e", "i")

def split_vocalization_syllables(value):
    text = (value or "").strip().lower()
    if not text:
        return []

    text_key = re.sub(r'[^a-zа-яё]+', '', text)
    if not text_key:
        return []

    syllables = sorted(
        VOCALIZATION_SYLLABLES + OPEN_VOCALIZATION_SYLLABLES,
        key=len,
        reverse=True,
    )
    parts = []
    pos = 0
    while pos < len(text_key):
        match = next((item for item in syllables if text_key.startswith(item, pos)), None)
        if match is None:
            return []
        parts.append(match)
        pos += len(match)
    return parts

def is_vocalization_text(value):
    parts = split_vocalization_syllables(value)
    if len(parts) < 3:
        return False
    if all(part in OPEN_VOCALIZATION_SYLLABLES for part in parts):
        return True
    return sum(part in VOCALIZATION_SYLLABLES for part in parts) >= 3

def is_vocalization_word(value):
    text_key = re.sub(r'[^a-zа-яё]+', '', (value or "").strip().lower())
    if not text_key:
        return False
    if re.fullmatch(r'(?:[aeiouyаеёиоуыэюяh]+|m+|м+)', text_key):
        return True
    if text_key in VOCALIZATION_SYLLABLES or text_key in OPEN_VOCALIZATION_SYLLABLES:
        return True
    if split_vocalization_syllables(text_key):
        return True
    normalized = re.sub(r'(.)\1{2,}', r'\1\1', text_key)
    normalized = re.sub(r'h+$', '', normalized)
    if not normalized:
        return True
    if normalized in VOCALIZATION_SYLLABLES or normalized in OPEN_VOCALIZATION_SYLLABLES:
        return True
    return bool(split_vocalization_syllables(normalized))

def is_vocalization_line(line):
    words = line.get("words") or []
    if is_vocalization_text(line.get("text", "")):
        return True
    word_values = [w.get("word", "") for w in words]
    meaningful = [w for w in word_values if clean_word(w)]
    return bool(meaningful) and all(is_vocalization_word(w) for w in meaningful)

def repair_vocalization_lines(lyrics_karaoke):
    repaired = []
    for line in lyrics_karaoke:
        words = line.get("words") or []
        if len(words) < 2 or not is_vocalization_line(line):
            repaired.append(line)
            continue

        try:
            line_start = float(line.get("start", words[0].get("start", 0.0)))
            line_end = float(line.get("end", words[-1].get("end", line_start + 0.5)))
        except Exception:
            repaired.append(line)
            continue

        durations = []
        gaps = []
        invalid = False
        for idx, word in enumerate(words):
            try:
                start = float(word.get("start", line_start))
                end = float(word.get("end", start))
            except Exception:
                invalid = True
                start, end = line_start, line_start
            if end <= start + 0.03:
                invalid = True
            durations.append(max(0.0, end - start))
            if idx > 0:
                try:
                    prev_end = float(words[idx - 1].get("end", line_start))
                    gaps.append(max(0.0, start - prev_end))
                except Exception:
                    invalid = True

        span = max(0.12, line_end - line_start)
        longest = max(durations or [0.0])
        max_gap = max(gaps or [0.0])
        stretched = longest > max(1.25, span * 0.58)
        broken = invalid or max_gap > 1.25
        if not (stretched or broken):
            repaired.append(line)
            continue

        fixed_words = distribute_words_between_anchors(
            [dict(word) for word in words],
            line_start,
            line_end,
        )
        fixed = dict(line)
        fixed["words"] = fixed_words
        fixed["start"] = fixed_words[0]["start"]
        fixed["end"] = fixed_words[-1]["end"]
        repaired.append(fixed)
    return repaired

def sanitize_word_timings(lyrics_karaoke):
    sanitized = []
    for line in lyrics_karaoke:
        words = [dict(word) for word in (line.get("words") or [])]
        if not words:
            sanitized.append(line)
            continue
        try:
            line_start = float(line.get("start", words[0].get("start", 0.0)))
            line_end = float(line.get("end", words[-1].get("end", line_start + 0.1)))
        except Exception:
            sanitized.append(line)
            continue
        line_end = max(line_start + 0.05, line_end)
        for idx, word in enumerate(words):
            try:
                start = float(word.get("start", line_start))
                end = float(word.get("end", start + 0.08))
            except Exception:
                start, end = line_start, line_start + 0.08
            if idx > 0:
                prev_end = float(words[idx - 1]["end"])
                start = max(start, prev_end + 0.01)
            start = max(line_start, min(start, line_end - 0.01))
            end = max(start + 0.05, end)
            end = min(end, line_end)
            if end <= start:
                start = max(line_start, min(start, line_end - 0.05))
                end = min(line_end, start + 0.05)
            word["start"] = round(start, 3)
            word["end"] = round(end, 3)
        fixed = dict(line)
        fixed["words"] = words
        fixed["start"] = round(min(line_start, float(words[0]["start"])), 3)
        fixed["end"] = round(max(line_start + 0.05, float(words[-1]["end"])), 3)
        sanitized.append(fixed)
    return sanitized

def lead_vocalization_lines(lyrics_karaoke, lead_seconds=1.35):
    repaired = []
    for line in lyrics_karaoke:
        words = line.get("words") or []
        is_vocalization = is_vocalization_line(line)

        if not is_vocalization:
            repaired.append(line)
            continue

        shifted = dict(line)
        shifted_words = []
        previous_end = None
        if repaired:
            try:
                previous_end = float(repaired[-1].get("end", 0.0))
            except Exception:
                previous_end = None
        first_start = None
        if words:
            try:
                first_start = float(words[0]["start"])
            except Exception:
                first_start = None
        min_start = 0.0
        if previous_end is not None:
            min_start = previous_end + 0.05
        effective_lead = lead_seconds
        if first_start is not None:
            effective_lead = min(effective_lead, max(0.0, first_start - min_start))
        for word in words:
            new_word = dict(word)
            new_word["start"] = round(max(0.0, float(new_word["start"]) - effective_lead), 3)
            new_word["end"] = round(max(new_word["start"] + 0.08, float(new_word["end"]) - effective_lead), 3)
            shifted_words.append(new_word)
        if shifted_words:
            shifted["words"] = shifted_words
            shifted["start"] = shifted_words[0]["start"]
            shifted["end"] = shifted_words[-1]["end"]
        repaired.append(shifted)
    return repaired

def find_suspicious_tail_start(lyrics_karaoke):
    for idx, line in enumerate(lyrics_karaoke):
        words = line.get("words") or []
        if len(words) < 2:
            continue
        try:
            duration = float(line["end"]) - float(line["start"])
        except Exception:
            continue
        expected = estimate_line_duration([w.get("word", "") for w in words], None)
        max_gap = line_internal_max_gap(line)
        zero_ratio = line_zero_word_ratio(line)
        long_split_line = len(words) >= 4 and max_gap >= 4.0 and duration >= max(6.0, expected * 2.3)
        compressed_line = len(words) >= 4 and zero_ratio >= 0.55 and duration <= max(1.2, expected * 0.45)
        if long_split_line or compressed_line:
            return idx
    return None

def find_phrase_start_in_audio(model, audio_path, target_text, search_start, search_end, language='en', status_callback=None):
    import tempfile
    best = None
    offset = max(0.0, float(search_start))
    search_end = max(offset, float(search_end))
    chunk_seconds = 12.0
    hop_seconds = 8.0
    while offset < search_end:
        current_duration = min(chunk_seconds, search_end - offset)
        if current_duration < 2.0:
            break
        if status_callback:
            status_callback(f"Проверка проигрыша: ищем реальный вход вокала {offset:.1f}-{offset + current_duration:.1f} сек...")
        with tempfile.NamedTemporaryFile(prefix='karaoke_tail_scan_', suffix='.wav', delete=False) as tmp:
            scan_path = tmp.name
        try:
            extract_audio_window(audio_path, scan_path, offset, current_duration)
            result = model.transcribe(scan_path, language=language, vad=True, vad_threshold=0.05)
            for segment in getattr(result, 'segments', []) or []:
                text = (getattr(segment, 'text', '') or '').strip()
                score = lyric_text_score(target_text, text)
                if score <= 0:
                    continue
                start = offset + float(getattr(segment, 'start', 0.0) or 0.0)
                no_speech_prob = float(getattr(segment, 'no_speech_prob', 0.0) or 0.0)
                avg_logprob = float(getattr(segment, 'avg_logprob', 0.0) or 0.0)
                confidence = score - max(0.0, no_speech_prob - 0.45) - max(0.0, -0.85 - avg_logprob)
                if best is None or confidence > best["confidence"]:
                    best = {
                        "start": start,
                        "confidence": confidence,
                        "score": score,
                        "text": text,
                    }
                if score >= 0.55 and confidence >= 0.6:
                    return max(0.0, start)
        finally:
            try:
                os.remove(scan_path)
            except OSError:
                pass
        offset += hop_seconds

    if best and best["score"] >= 0.28 and best["confidence"] >= 0.15:
        return max(0.0, best["start"])
    return None


def measure_lyrics_text_match(model, audio_path, lyrics_text, language='en', status_callback=None):
    expected_text = strip_lrc_timestamps(lyrics_text or "")
    expected_words = [w for w in re.split(r'\s+', expected_text) if clean_word(w)]
    if not expected_words:
        return None

    if status_callback:
        status_callback("Проверка текста: распознаём вокал и сравниваем с выданным текстом...")

    result = model.transcribe(audio_path, language=language, vad=True, vad_threshold=0.05)
    heard_parts = []
    for segment in getattr(result, 'segments', []) or []:
        text = (getattr(segment, 'text', '') or '').strip()
        if text:
            heard_parts.append(text)
    heard_text = " ".join(heard_parts).strip()
    heard_words = [w for w in re.split(r'\s+', heard_text) if clean_word(w)]
    score = lyric_text_score(expected_text, heard_text)
    return {
        "score": round(float(score), 3),
        "expected_words": len(expected_words),
        "recognized_words": len(heard_words),
        "recognized_preview": heard_text[:500],
    }

def repair_large_internal_gaps(lyrics_karaoke, max_allowed_gap=3.5):
    for line in lyrics_karaoke:
        words = line.get("words") or []
        if len(words) < 2:
            continue
        for idx in range(1, len(words)):
            prev = words[idx - 1]
            curr = words[idx]
            try:
                prev_end = float(prev["end"])
                curr_start = float(curr["start"])
            except Exception:
                continue
            gap = curr_start - prev_end
            if gap > max_allowed_gap:
                shift = gap - 1.2
                for j in range(idx, len(words)):
                    words[j]["start"] = round(float(words[j]["start"]) - shift, 3)
                    words[j]["end"] = round(float(words[j]["end"]) - shift, 3)
        if words:
            line["start"] = words[0]["start"]
            line["end"] = words[-1]["end"]
    return lyrics_karaoke

def repair_compressed_tails(lyrics_karaoke, audio_duration):
    if not lyrics_karaoke or not audio_duration:
        return lyrics_karaoke
    num_lines = len(lyrics_karaoke)
    fail_start_idx = None
    for idx in range(num_lines - 1, -1, -1):
        line = lyrics_karaoke[idx]
        words = line.get("words", [])
        if not words:
            continue
        duration = float(line["end"]) - float(line["start"])
        num_words = len(words)
        is_broken = False
        unique_starts = set()
        has_zero_duration_word = False
        for w in words:
            w_start = float(w.get("start", 0.0))
            w_end = float(w.get("end", 0.0))
            if abs(w_end - w_start) < 0.01:
                has_zero_duration_word = True
            unique_starts.add(round(w_start, 3))
        if num_words >= 2 and len(unique_starts) <= num_words * 0.6:
            is_broken = True
        elif has_zero_duration_word:
            is_broken = True
        elif num_words > 0 and (duration / num_words) < 0.18:
            is_broken = True
        if is_broken:
            fail_start_idx = idx
        else:
            break
    if fail_start_idx is None:
        return lyrics_karaoke
    if fail_start_idx > 0:
        start_time = float(lyrics_karaoke[fail_start_idx - 1]["end"])
    else:
        start_time = 0.0
    available_time = float(audio_duration) - start_time
    if available_time <= 0.5:
        available_time = max(3.0, float(audio_duration) - start_time)
    tail_lines = lyrics_karaoke[fail_start_idx:]
    total_chars = sum(max(len(line.get("text", "")), 1) for line in tail_lines)
    current_time = start_time
    for line in tail_lines:
        line_text = line.get("text", "")
        line_chars = max(len(line_text), 1)
        line_share = line_chars / total_chars
        line_duration = max(0.8, available_time * line_share)
        line_start = current_time
        line_end = line_start + line_duration
        words = line.get("words", [])
        if words:
            word_total_chars = sum(max(len(clean_word(w.get("word", ""))), 1) for w in words)
            w_cursor = line_start
            for w in words:
                w_text = w.get("word", "")
                w_chars = max(len(clean_word(w_text)), 1)
                w_share = w_chars / word_total_chars
                w_dur = max(0.12, line_duration * w_share)
                w["start"] = round(w_cursor, 3)
                w["end"] = round(min(line_end, w_cursor + w_dur * 0.95), 3)
                w_cursor += w_dur
            line["start"] = round(words[0]["start"], 3)
            line["end"] = round(words[-1]["end"], 3)
        else:
            line["start"] = round(line_start, 3)
            line["end"] = round(line_end, 3)
        current_time = line["end"] + 0.05
    return lyrics_karaoke

def build_karaoke_from_aligned_segments(segments, fallback_lines, offset=0.0):
    lyrics_karaoke = []
    fallback_idx = 0
    for segment in segments:
        while fallback_idx < len(fallback_lines) and not fallback_lines[fallback_idx].strip():
            fallback_idx += 1
        text = fallback_lines[fallback_idx].strip() if fallback_idx < len(fallback_lines) else (getattr(segment, 'text', '') or '').strip()
        fallback_idx += 1
        if not text:
            continue

        segment_words = list(getattr(segment, 'words', []) or [])
        line_words = []
        for idx, orig_word in enumerate(text.split()):
            if idx < len(segment_words):
                w = segment_words[idx]
                start = float(getattr(w, 'start', getattr(segment, 'start', 0.0)) or 0.0) + offset
                end = float(getattr(w, 'end', getattr(segment, 'end', start)) or start) + offset
            else:
                start = float(getattr(segment, 'start', 0.0) or 0.0) + offset
                end = float(getattr(segment, 'end', start) or start) + offset
            if end <= start:
                end = start + 0.1
            line_words.append({
                "word": orig_word,
                "start": round(start, 3),
                "end": round(end, 3),
            })

        for idx in range(1, len(line_words)):
            prev = line_words[idx - 1]
            curr = line_words[idx]
            if curr["start"] < prev["end"] + 0.03:
                if idx == 1 and prev["end"] - prev["start"] <= 0.22:
                    prev["end"] = round(min(curr["start"] - 0.04, prev["start"] + 0.18), 3)
                    if prev["end"] <= prev["start"]:
                        prev["start"] = round(max(0.0, curr["start"] - 0.22), 3)
                        prev["end"] = round(max(prev["start"] + 0.08, curr["start"] - 0.04), 3)
                else:
                    curr["start"] = round(prev["end"] + 0.03, 3)
                    if curr["end"] <= curr["start"]:
                        curr["end"] = round(curr["start"] + 0.1, 3)

        if line_words:
            if len(line_words) >= 2:
                first = line_words[0]
                second = line_words[1]
                first_duration = first["end"] - first["start"]
                first_gap = second["start"] - first["end"]
                if first_duration <= 0.18 and first_gap >= 0.65:
                    fixed_start = max(first["start"], second["start"] - 0.28)
                    first["start"] = round(fixed_start, 3)
                    first["end"] = round(min(second["start"] - 0.04, fixed_start + 0.18), 3)
            lyrics_karaoke.append({
                "text": text,
                "start": line_words[0]["start"],
                "end": line_words[-1]["end"],
                "words": line_words,
            })
    return lyrics_karaoke










def infer_lyrics_language(text):
    return 'ru' if re.search(r'[А-Яа-яЁё]', text or '') else 'en'

def get_system_font(font_name='montserrat', bold=False, black=False):
    font_name = font_name.lower().strip()
    black = False
    base_dir = os.path.dirname(os.path.abspath(__file__))
    repo_dir = os.path.dirname(base_dir)
    font_search_dirs = (
        RESOURCE_DIR,
        base_dir,
        os.path.join(base_dir, "assets"),
        os.path.join(repo_dir, "desktop_app", "assets"),
        os.path.join(repo_dir, "assets"),
    )
    
    # 1. Montserrat
    if font_name == 'montserrat':
        font_file = "Montserrat-Bold.ttf" if bold or black else "Montserrat-Regular.ttf"
        for search_dir in font_search_dirs:
            montserrat_path = os.path.join(search_dir, font_file)
            if os.path.exists(montserrat_path):
                return montserrat_path
        if black:
            # Fallback to bold if black not found
            return get_system_font(font_name='montserrat', bold=True, black=False)
            
    # 2. Arial
    if font_name == 'arial':
        if sys.platform == "win32":
            path = os.path.join(os.environ.get("SystemRoot", "C:\\Windows"), "Fonts", "arialbd.ttf" if bold or black else "arial.ttf")
            if os.path.exists(path): return path
        elif sys.platform == "darwin":
            paths = [
                "/Library/Fonts/Arial Bold.ttf" if bold or black else "/Library/Fonts/Arial.ttf",
                "/System/Library/Fonts/Supplemental/Arial Bold.ttf" if bold or black else "/System/Library/Fonts/Supplemental/Arial.ttf"
            ]
            for p in paths:
                if os.path.exists(p): return p
                
    # 3. Helvetica
    if font_name == 'helvetica':
        if sys.platform == "darwin":
            path = "/System/Library/Fonts/Helvetica.ttc"
            if os.path.exists(path): return path
        return get_system_font(font_name='arial', bold=bold or black)
        
    # 4. Georgia
    if font_name == 'georgia':
        if sys.platform == "win32":
            path = os.path.join(os.environ.get("SystemRoot", "C:\\Windows"), "Fonts", "georgiab.ttf" if bold or black else "georgia.ttf")
            if os.path.exists(path): return path
        elif sys.platform == "darwin":
            paths = [
                "/Library/Fonts/Georgia Bold.ttf" if bold or black else "/Library/Fonts/Georgia.ttf",
                "/System/Library/Fonts/Supplemental/Georgia Bold.ttf" if bold or black else "/System/Library/Fonts/Supplemental/Georgia.ttf"
            ]
            for p in paths:
                if os.path.exists(p): return p
                
    # Default fallback to Montserrat
    font_file = "Montserrat-Bold.ttf" if bold or black else "Montserrat-Regular.ttf"
    for search_dir in font_search_dirs:
        montserrat_path = os.path.join(search_dir, font_file)
        if os.path.exists(montserrat_path):
            return montserrat_path
    if black:
        return get_system_font(font_name='montserrat', bold=True, black=False)
        
    # Final generic fallback
    if sys.platform == "win32":
        return os.path.join(os.environ.get("SystemRoot", "C:\\Windows"), "Fonts", "arialbd.ttf" if bold or black else "arial.ttf")
    elif sys.platform == "darwin":
        return "/Library/Fonts/Arial Bold.ttf" if bold or black else "/Library/Fonts/Arial.ttf"
    return None


def draw_gradient_background(draw, width, height):
    color_start = (10, 15, 30)  # Глубокий космос
    color_end = (20, 25, 45)    # Насыщенный сине-грифельный
    for y in range(height):
        ratio = y / height
        r = int(color_start[0] * (1 - ratio) + color_end[0] * ratio)
        g = int(color_start[1] * (1 - ratio) + color_end[1] * ratio)
        b = int(color_start[2] * (1 - ratio) + color_end[2] * ratio)
        draw.line([(0, y), (width, y)], fill=(r, g, b))

def draw_glow_circle(image_draw, cx, cy, r, color, max_alpha=35):
    for radius in range(r, 0, -6):
        ratio = radius / r
        alpha = int(max_alpha * (1 - ratio) ** 1.8)
        if alpha > 0:
            image_draw.ellipse(
                [(cx - radius, cy - radius), (cx + radius, cy + radius)], 
                fill=(color[0], color[1], color[2], alpha)
            )

def get_word_widths(words, font):
    widths = []
    space_width = font.getbbox(" ")[2] - font.getbbox(" ")[0]
    for w_data in words:
        word = w_data["word"]
        bbox = font.getbbox(word)
        word_w = bbox[2] - bbox[0]
        widths.append(word_w)
    return widths, space_width

# Глобальный кэш загруженных ИИ-моделей Whisper в оперативной памяти
loaded_models = {}

def get_whisper_model(model_name, status_callback=None):
    import stable_whisper
    import torch
    if model_name not in loaded_models:
        requested_device = os.environ.get("KARAOKE_WHISPER_DEVICE", "").strip().lower()
        device = "cpu"
        if requested_device in {"cpu", "cuda", "mps"}:
            device = requested_device
        elif torch.cuda.is_available():
            device = "cuda"
        
        if status_callback:
            status_callback(f"Загрузка ИИ-модели Whisper '{model_name}' в память ({device})...")
        try:
            loaded_models[model_name] = stable_whisper.load_model(model_name, device=device)
        except Exception as e:
            if device != "cpu":
                if status_callback:
                    status_callback(f"Откат автоопределения с {device} на cpu: {e}")
                loaded_models[model_name] = stable_whisper.load_model(model_name, device="cpu")
            else:
                raise e
    elif status_callback:
        status_callback(f"Использование готовой модели Whisper '{model_name}'...")
    return loaded_models[model_name]

def audio_duration_seconds(audio_path):
    try:
        res = subprocess.run(
            ['ffprobe', '-v', 'error', '-show_entries', 'format=duration',
             '-of', 'default=noprint_wrappers=1:nokey=1', audio_path],
            capture_output=True,
            text=True,
            check=True,
            **subprocess_no_window_kwargs()
        )
        return max(0.0, float(res.stdout.strip()))
    except Exception:
        return 0.0

def extract_audio_window(input_path, output_path, start=0.0, duration=None):
    cmd = ['ffmpeg', '-y', '-ss', f'{max(0.0, start):.3f}']
    if duration is not None:
        cmd.extend(['-t', f'{max(0.1, duration):.3f}'])
    cmd.extend([
        '-i', input_path,
        '-vn',
        '-acodec', 'pcm_s16le',
        '-ar', '16000',
        '-ac', '1',
        output_path
    ])
    subprocess.run(
        cmd,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=True,
        **subprocess_no_window_kwargs()
    )

def align_in_chunks(model, audio_path, lyrics_lines, language,
                    audio_duration=None, vocal_start=0.0,
                    target_chunk_secs=45.0, overlap_secs=5.0,
                    status_callback=None, progress_callback=None):
    """
    Выравнивает длинный трек по независимым кускам (~45 сек каждый).
    Предотвращает drift Whisper: на треках >2-3 мин модель теряет ориентацию
    и начинает галлюцинировать. Разбивая на окна, мы гарантируем что каждый
    вызов model.align() видит не более 30-40 Whisper-чанков.

    Возвращает плоский список SimpleNamespace(word, start, end)
    с абсолютными таймингами, либо None если деление не нужно.
    """
    import tempfile
    from types import SimpleNamespace

    non_empty_lines = [l.strip() for l in lyrics_lines if l.strip()]
    total_lines = len(non_empty_lines)

    if audio_duration is None or audio_duration <= 0:
        audio_duration = 300.0

    effective_duration = max(10.0, audio_duration - max(0.0, vocal_start))

    # Динамически вычисляем размер чанка в строках на основе темпа песни
    secs_per_line = effective_duration / max(1, total_lines)
    chunk_lines = max(5, min(22, int(target_chunk_secs / max(1.5, secs_per_line))))

    # Если весь текст умещается в один чанк — нет смысла делить
    if total_lines <= chunk_lines + 3:
        return None

    # Разбиваем на чанки
    chunks = []
    i = 0
    while i < total_lines:
        end = min(i + chunk_lines, total_lines)
        chunks.append(non_empty_lines[i:end])
        i = end

    n_chunks = len(chunks)
    total_words = sum(len(l.split()) for l in non_empty_lines)

    # Рассчитываем оценочные временны́е зоны пропорционально кол-ву слов
    cumulative_words = 0
    chunk_audio_starts_est = []
    chunk_audio_ends_est = []
    for chunk in chunks:
        cw = sum(len(l.split()) for l in chunk)
        start_frac = cumulative_words / max(1, total_words)
        end_frac = (cumulative_words + cw) / max(1, total_words)
        chunk_audio_starts_est.append(vocal_start + start_frac * effective_duration)
        chunk_audio_ends_est.append(vocal_start + end_frac * effective_duration)
        cumulative_words += cw

    all_words = []
    prev_actual_end = max(0.0, vocal_start)

    for idx, chunk_lines_data in enumerate(chunks):
        is_last = (idx == n_chunks - 1)

        # Начало окна: реальный конец предыдущего чанка минус overlap (для стыковки)
        if idx == 0:
            window_start = max(0.0, vocal_start - 0.5)
        else:
            window_start = max(0.0, prev_actual_end - overlap_secs)

        # Конец окна: оценочный конец чанка + запас, либо конец аудио
        if is_last:
            window_end = audio_duration
        else:
            window_end = min(audio_duration, chunk_audio_ends_est[idx] + overlap_secs * 1.5)

        window_duration = max(8.0, window_end - window_start)
        chunk_text = "\n".join(chunk_lines_data)

        if status_callback:
            status_callback(
                f"Выравнивание фрагмента {idx + 1}/{n_chunks} "
                f"({window_start:.0f}–{window_end:.0f} сек, {len(chunk_lines_data)} строк)..."
            )

        with tempfile.NamedTemporaryFile(
            prefix=f'karaoke_chunk{idx}_', suffix='.wav', delete=False
        ) as tmp:
            chunk_audio_path = tmp.name

        try:
            extract_audio_window(audio_path, chunk_audio_path, window_start, window_duration)
            chunk_result = model.align(
                chunk_audio_path,
                chunk_text,
                language=language,
                original_split=True,
                max_word_dur=2.0,
                vad=True,
                vad_threshold=0.35
            )

            chunk_words = []
            last_valid_abs_end = window_start

            for segment in getattr(chunk_result, 'segments', []) or []:
                for w in getattr(segment, 'words', []) or []:
                    word_str = getattr(w, 'word', '') or ''
                    try:
                        rel_start = float(w.start)
                        rel_end = float(w.end)
                    except Exception:
                        continue
                    if rel_end <= rel_start:
                        continue
                    abs_start = round(rel_start + window_start, 3)
                    abs_end = round(rel_end + window_start, 3)
                    word_obj = SimpleNamespace(word=word_str, start=abs_start, end=abs_end)
                    chunk_words.append(word_obj)
                    last_valid_abs_end = max(last_valid_abs_end, abs_end)

            if chunk_words:
                # Монотонность: чанк не должен сильно перекрываться с предыдущим
                if all_words and chunk_words[0].start < all_words[-1].end - 0.5:
                    shift = (all_words[-1].end + 0.05) - chunk_words[0].start
                    for wobj in chunk_words:
                        wobj.start = round(wobj.start + shift, 3)
                        wobj.end = round(wobj.end + shift, 3)
                    last_valid_abs_end = round(last_valid_abs_end + shift, 3)

                all_words.extend(chunk_words)
                prev_actual_end = last_valid_abs_end
            else:
                # Пустой чанк — двигаем курсор по оценке
                prev_actual_end = chunk_audio_ends_est[idx]

            # Обновляем прогресс после каждого чанка (0.11 -> 0.38 пропорционально)
            if progress_callback:
                chunk_progress = 0.11 + (0.27 * (idx + 1) / n_chunks)
                progress_callback(chunk_progress)

        except Exception as exc:
            if status_callback:
                status_callback(f"Ошибка в фрагменте {idx + 1}/{n_chunks}: {exc}. Продолжаем...")
            prev_actual_end = chunk_audio_ends_est[idx]

        finally:
            try:
                os.remove(chunk_audio_path)
            except OSError:
                pass

    return all_words if all_words else None

def detect_vocal_start(audio_path, model_name='base', window_seconds=45.0, chunk_seconds=12.0, hop_seconds=8.0, status_callback=None, language='ru', lyrics_text=''):
    import tempfile

    duration = audio_duration_seconds(audio_path)
    scan_duration = min(max(window_seconds, chunk_seconds), duration or window_seconds)
    chunk_seconds = max(4.0, min(chunk_seconds, scan_duration))
    hop_seconds = max(2.0, min(hop_seconds, chunk_seconds))
    model = get_whisper_model(model_name)
    expected_words = {
        clean_word(word)
        for word in re.split(r'\s+', lyrics_text or '')
        if len(clean_word(word)) >= 3
    }

    def format_time(seconds):
        seconds = max(0, int(round(seconds)))
        return f"{seconds // 60:02d}:{seconds % 60:02d}"

    offset = 0.0
    all_candidates = []
    while offset < scan_duration:
        current_duration = min(chunk_seconds, scan_duration - offset)
        if current_duration < 1.0:
            break
        if status_callback:
            status_callback(
                f"Предобработка: ищем вокал {format_time(offset)}-{format_time(offset + current_duration)}..."
            )

        with tempfile.NamedTemporaryFile(prefix='karaoke_vocal_scan_', suffix='.wav', delete=False) as tmp:
            scan_path = tmp.name

        try:
            extract_audio_window(audio_path, scan_path, offset, current_duration)
            result = model.transcribe(scan_path, language=language, vad=True, vad_threshold=0.05)

            candidates = []
            for segment in getattr(result, 'segments', []) or []:
                text = (getattr(segment, 'text', '') or '').strip()
                clean = re.sub(r'[^A-Za-zА-Яа-яЁё0-9]+', '', text)
                if len(clean) < 2:
                    continue
                recognized_words = {
                    clean_word(word)
                    for word in re.split(r'\s+', text)
                    if len(clean_word(word)) >= 3
                }
                no_speech_prob = getattr(segment, 'no_speech_prob', 0.0) or 0.0
                avg_logprob = getattr(segment, 'avg_logprob', 0.0) or 0.0
                local_start = float(getattr(segment, 'start', 0.0) or 0.0)
                start = offset + local_start
                if no_speech_prob > 0.75:
                    continue
                if avg_logprob < -1.4 and len(clean) < 8:
                    continue
                if expected_words and not (expected_words & recognized_words):
                    if no_speech_prob > 0.35 or avg_logprob < -0.65:
                        continue
                candidates.append({
                    'start': round(max(0.0, start), 3),
                    'end': round(max(start, offset + float(getattr(segment, 'end', local_start) or local_start)), 3),
                    'text': text[:80],
                    'no_speech_prob': round(float(no_speech_prob), 3),
                    'avg_logprob': round(float(avg_logprob), 3),
                })

            all_candidates.extend(candidates)
            if candidates:
                first = candidates[0]
                start = max(0.0, first['start'] - 0.35)
                confidence = 'high' if first['start'] > 3.0 else 'medium'
                return {
                    'vocal_start': round(start, 3),
                    'confidence': confidence,
                    'segments': candidates[:5],
                    'scanned_until': round(offset + current_duration, 3),
                }
        finally:
            try:
                os.remove(scan_path)
            except OSError:
                pass

        offset += hop_seconds

    return {
        'vocal_start': 0.0,
        'confidence': 'low',
        'segments': all_candidates[:5],
        'scanned_until': round(scan_duration, 3),
    }

def vocal_cache_path_for_audio(audio_path):
    audio_dir = os.path.dirname(os.path.abspath(audio_path))
    stem = os.path.splitext(os.path.basename(audio_path))[0]
    safe_stem = re.sub(r'[^\w\- .А-Яа-яЁё]+', '_', stem).strip() or 'audio'
    return os.path.join(audio_dir, f"{safe_stem}_vocals.wav")

def volume_root_for_path(path):
    abs_path = os.path.abspath(path)
    parts = abs_path.split(os.sep)
    if len(parts) >= 4 and parts[1] == "Volumes":
        return os.path.join(os.sep, parts[1], parts[2])
    return None

def demucs_model_cache_dir(audio_path, out_dir):
    volume_root = volume_root_for_path(audio_path)
    if volume_root and os.access(volume_root, os.W_OK):
        return os.path.join(volume_root, ".karaoke_demucs_model_cache")
    return os.path.join(out_dir, "model_cache")

def demucs_command_candidates():
    candidates = []
    env_python = os.environ.get("KARAOKE_DEMUCS_PYTHON")
    env_bin = os.environ.get("KARAOKE_DEMUCS_BIN")
    if env_bin:
        candidates.append([env_bin])
    if env_python:
        candidates.append([env_python, "-m", "demucs"])

    demucs_bin = shutil.which("demucs")
    if demucs_bin:
        candidates.append([demucs_bin])

    candidates.append([sys.executable, "-m", "demucs"])

    volume_root = "/Volumes"
    if os.path.isdir(volume_root):
        try:
            for volume_name in os.listdir(volume_root):
                python_path = os.path.join(
                    volume_root,
                    volume_name,
                    "karaoke-demucs-venv",
                    "bin",
                    "python",
                )
                if os.path.exists(python_path):
                    candidates.append([python_path, "-m", "demucs"])
        except Exception:
            pass

    seen = set()
    unique = []
    for candidate in candidates:
        key = tuple(candidate)
        if key not in seen:
            seen.add(key)
            unique.append(candidate)
    return unique

def find_demucs_command():
    errors = []
    for candidate in demucs_command_candidates():
        try:
            proc = subprocess.run(
                candidate + ["--help"],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=20,
                **subprocess_no_window_kwargs(),
            )
            if proc.returncode == 0:
                return candidate
            errors.append(f"{' '.join(candidate)}: {(proc.stderr or proc.stdout or '').strip()[:180]}")
        except Exception as exc:
            errors.append(f"{' '.join(candidate)}: {exc}")
    raise RuntimeError("Demucs не найден. " + " | ".join(errors[-3:]))

def separate_vocals_with_demucs(audio_path, status_callback=None):
    cache_path = vocal_cache_path_for_audio(audio_path)
    if os.path.exists(cache_path) and os.path.getsize(cache_path) > 1024:
        if status_callback:
            status_callback(f"Используем кэш вокала: {os.path.basename(cache_path)}")
        return cache_path

    cmd = find_demucs_command()

    out_dir = os.path.join(os.path.dirname(cache_path), ".karaoke_demucs")
    model_cache_dir = demucs_model_cache_dir(audio_path, out_dir)
    os.makedirs(out_dir, exist_ok=True)
    os.makedirs(model_cache_dir, exist_ok=True)
    cmd.extend([
        "--two-stems=vocals",
        "-n", "mdx_q",
        "--out", out_dir,
        audio_path,
    ])

    if status_callback:
        status_callback("Выделяем вокал для word-level синхронизации через Demucs...")

    try:
        env = os.environ.copy()
        env["TORCH_HOME"] = model_cache_dir
        env["XDG_CACHE_HOME"] = model_cache_dir
        env.setdefault("PYTHONHTTPSVERIFY", "0")
        proc = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=env,
            **subprocess_no_window_kwargs(),
        )
    except Exception as exc:
        raise RuntimeError(f"Demucs не запустился: {exc}") from exc

    if proc.returncode != 0:
        details = (proc.stderr or proc.stdout or "").strip().splitlines()
        tail = " ".join(details[-3:])[:700] if details else "unknown error"
        raise RuntimeError(f"Demucs завершился с ошибкой: {tail}")

    found_vocals = None
    for root, _, files in os.walk(out_dir):
        for name in files:
            if name.lower() == "vocals.wav":
                candidate = os.path.join(root, name)
                if os.path.getsize(candidate) > 1024:
                    found_vocals = candidate
                    break
        if found_vocals:
            break

    if not found_vocals:
        raise RuntimeError("Demucs не создал vocals.wav")

    shutil.copy2(found_vocals, cache_path)
    if status_callback:
        status_callback(f"Вокал сохранен в кэш: {os.path.basename(cache_path)}")
    return cache_path


def _word_probability(word):
    """Достаёт confidence слова из stable-ts; если поле недоступно — None."""
    for attr in ('probability', 'confidence'):
        value = getattr(word, attr, None)
        if value is not None:
            try:
                return float(value)
            except (TypeError, ValueError):
                return None
    return None


def _distribute_words_linear(words, start_time, end_time):
    """Пропорционально раскладывает слова по интервалу [start, end] по длине.

    Используется, когда Whisper не дал надёжных таймингов для группы слов
    (галлюцинация/пропуск): честнее угадать плавный темп, чем привязывать
    тайминги чужих слов из текста.
    """
    if not words:
        return []
    start_time = float(start_time)
    end_time = max(start_time + 0.08, float(end_time))
    total_chars = sum(max(len(clean_word(w.get("word", ""))), 1) for w in words) or len(words)
    span = end_time - start_time
    cursor = start_time
    distributed = []
    for idx, word in enumerate(words):
        share = max(len(clean_word(word.get("word", ""))), 1) / total_chars
        slot = span * share
        word_end = end_time if idx == len(words) - 1 else min(end_time, cursor + max(0.08, slot * 0.88))
        updated = dict(word)
        updated["start"] = round(cursor, 3)
        updated["end"] = round(max(cursor + 0.05, word_end), 3)
        distributed.append(updated)
        cursor = min(end_time, cursor + max(0.08, slot))
    return distributed


def _interp_gaps(line_words, anchors):
    """Линейно интерполирует тайминги незаматченных слов между якорями.

    anchors: список индексов (в line_words), для которых уже есть надёжный
    тайминг (start/end). Группы незаматченных слов до первого, между и после
    последнего якоря раскладываются пропорционально длине.
    """
    if not anchors:
        return
    n = len(line_words)

    # Кусок до первого якоря: от примерного начала строки до якоря.
    first = anchors[0]
    anchor_start = line_words[first]["start"]
    if first > 0:
        seg = _distribute_words_linear(line_words[:first], max(0.0, anchor_start - 0.5), anchor_start)
        for i, w in enumerate(seg):
            line_words[i]["start"] = w["start"]
            line_words[i]["end"] = w["end"]

    # Куски между соседними якорями.
    for ai in range(len(anchors) - 1):
        a = anchors[ai]
        b = anchors[ai + 1]
        if b - a <= 1:
            continue
        seg_start = line_words[a]["end"]
        seg_end = line_words[b]["start"]
        seg = _distribute_words_linear(line_words[a + 1:b], seg_start, seg_end)
        for i, w in enumerate(seg):
            line_words[a + 1 + i]["start"] = w["start"]
            line_words[a + 1 + i]["end"] = w["end"]

    # Кусок после последнего якоря: до примерного конца строки.
    last = anchors[-1]
    if last < n - 1:
        anchor_end = line_words[last]["end"]
        seg_end = anchor_end + 0.5 * (n - 1 - last)
        seg = _distribute_words_linear(line_words[last + 1:], anchor_end, seg_end)
        for i, w in enumerate(seg):
            line_words[last + 1 + i]["start"] = w["start"]
            line_words[last + 1 + i]["end"] = w["end"]


def match_lyrics_to_whisper(raw_lines, whisper_words, confidence_threshold=0.5, lookahead=5):
    """Сопоставляет текст песни со словами Whisper и строит караоке-тайминги.

    Ключевые отличия от прежнего инлайн-матчинга:
    - Низкоуверенные слова Whisper (ниже confidence_threshold) не используются
      как точные якоря — это убирает галлюцинации в инструментальных паузах.
    - Слова текста, которым не нашлось совпадения в Whisper, НЕ привязываются
      к таймингу чужого Whisper-слова (прежний безусловный fallback). Вместо
      этого их тайминги честно интерполируются между соседними найденными
      словами внутри той же строки.
    - Лишние вставки Whisper аккуратно перепрыгиваются.

    Аргументы:
      raw_lines: строки текста песни (без LRC-меток).
      whisper_words: объекты слов stable-ts с .word/.start/.end/.probability.
      confidence_threshold: минимальный confidence для слова-якоря.
      lookahead: сколько слов вперёд искать при неточном совпадении.

    Возвращает: (lyrics_karaoke, stats) где stats описывает качество матчинга.
    """
    num_whisper_words = len(whisper_words or [])

    def w_word_clean(idx):
        return clean_word(getattr(whisper_words[idx], 'word', '') or '')

    def w_time(idx):
        w = whisper_words[idx]
        return round(float(w.start), 3), round(float(w.end), 3)

    def w_confident(idx):
        prob = _word_probability(whisper_words[idx])
        # Если confidence недоступен (старая модель/формат) — принимаем слово.
        return prob is None or prob >= confidence_threshold

    def find_probable_line_start(orig_words, start_idx):
        targets = [clean_word(word) for word in orig_words if clean_word(word)]
        if not targets or start_idx >= num_whisper_words:
            return start_idx

        min_hits = 1 if len(targets) <= 2 else min(3, len(targets))
        min_ratio = 0.55 if len(targets) <= 4 else 0.45
        search_limit = min(num_whisper_words, start_idx + 120)
        best_idx = start_idx
        best_score = -1.0

        for candidate_idx in range(start_idx, search_limit):
            local_idx = candidate_idx
            hits = 0
            for target in targets:
                found_idx = None
                local_limit = min(num_whisper_words, local_idx + max(lookahead, 6))
                for probe_idx in range(local_idx, local_limit):
                    if not w_confident(probe_idx):
                        continue
                    if fuzzy_word_match(target, w_word_clean(probe_idx)):
                        found_idx = probe_idx
                        break
                if found_idx is None:
                    continue
                hits += 1
                local_idx = found_idx + 1

            ratio = hits / max(1, len(targets))
            if hits < min_hits or ratio < min_ratio:
                continue

            # Дальний прыжок допустим, но при равном качестве предпочитаем ближайший.
            score = hits + ratio - 0.01 * (candidate_idx - start_idx)
            if score > best_score:
                best_score = score
                best_idx = candidate_idx

        return best_idx

    lyrics_karaoke = []
    whisper_idx = 0
    matched_total = 0
    interpolated_total = 0
    total_words = 0

    for raw_line in raw_lines or []:
        line_cleaned = (raw_line or '').strip()
        if not line_cleaned:
            continue
        orig_words = [w for w in line_cleaned.split() if clean_word(w)]
        if not orig_words:
            continue
        total_words += len(orig_words)

        line_words = []
        anchors = []
        whisper_idx = find_probable_line_start(orig_words, whisper_idx)

        for orig_pos, orig_w in enumerate(orig_words):
            orig_w_clean = clean_word(orig_w)

            matched_whisper_idx = None

            # 1. Точное совпадение с текущим словом Whisper (с учётом confidence).
            if whisper_idx < num_whisper_words and fuzzy_word_match(orig_w_clean, w_word_clean(whisper_idx)) and w_confident(whisper_idx):
                matched_whisper_idx = whisper_idx

            # 2. Lookahead: ищем совпадение, перепрыгивая низкоуверенные/лишние слова Whisper.
            if matched_whisper_idx is None:
                search_to = min(whisper_idx + lookahead, num_whisper_words)
                for k in range(whisper_idx, search_to):
                    cand_clean = w_word_clean(k)
                    if not cand_clean:
                        continue
                    if fuzzy_word_match(orig_w_clean, cand_clean):
                        if w_confident(k):
                            matched_whisper_idx = k
                            break

            if matched_whisper_idx is not None:
                start, end = w_time(matched_whisper_idx)
                line_words.append({"word": orig_w, "start": start, "end": end})
                anchors.append(orig_pos)
                whisper_idx = matched_whisper_idx + 1
                matched_total += 1
            else:
                line_words.append({"word": orig_w, "start": None, "end": None})

        # Интерполяция незаматченных слов.
        if anchors:
            _interp_gaps(line_words, anchors)
            interpolated_total += len(line_words) - len(anchors)
        else:
            # Ни одного якоря: раскладываем по позиции курсора Whisper.
            est_start = 0.0
            if whisper_idx < num_whisper_words:
                est_start = w_time(whisper_idx)[0]
            est_end = est_start + 0.6 * len(line_words)
            seg = _distribute_words_linear(line_words, est_start, est_end)
            for i, w in enumerate(seg):
                line_words[i]["start"] = w["start"]
                line_words[i]["end"] = w["end"]
            interpolated_total += len(line_words)

        # Страховка от None/инвертированных таймингов.
        for w in line_words:
            if w["start"] is None or w["end"] is None or w["end"] <= w["start"]:
                base = w["start"] if w["start"] is not None else 0.0
                w["start"] = round(base, 3)
                w["end"] = round(base + 0.08, 3)

        lyrics_karaoke.append({
            "text": line_cleaned,
            "start": line_words[0]["start"],
            "end": line_words[-1]["end"],
            "words": line_words,
        })

    stats = {
        "matched_words": matched_total,
        "interpolated_words": interpolated_total,
        "total_words": total_words,
    }
    return lyrics_karaoke, stats


def _redistribute_words_in_line(line, max_word_dur=0.9, min_word_dur=0.18):
    """Перераспределяет слова внутри строки, исправляя растянутые/микро-слова.

    Проблема: Whisper (cross-attention + DTW, обученный на речи) плохо сегментирует
    отдельные ноты в пении. На затянутых гласных он «приклеивает» соседнее слово
    к той же области вокала, и слово растягивается на 2-3 секунды. Тюнинг
    параметров align() (max_word_dur, fast_mode) либо не помогает, либо ломает
    позиции строк (MAE vs ground truth). Demucs-разделение вокала только worsens
    сегментацию (артефакты).

    Решение: границы строк надёжны (MAE ~0.9с от эталона), а слова внутри строки
    идёт в правильном порядке 1-в-1. Поэтому перераспределяем слова по длине
    (в символах) внутри span строки, клампя длительность каждого в
    [min_word_dur, max_word_dur], а остаток времени раскладываем равномерно как
    межсловные паузы. Начало/конец строки не меняются (якоря), меняются только
    внутренние границы слов.

    Аргументы:
      line: dict караоке-строки с ключами start/end/words (мутируется in-place).
      max_word_dur: верхний предел длительности одного слова (сек).
      min_word_dur: нижний предел длительности одного слова (сек).

    Возвращает тот же line (для удобства чейнинга).
    """
    words = line.get("words") or []
    n = len(words)
    if n == 0:
        return line

    ls = float(line["start"])
    le = float(line["end"])
    span = le - ls
    if span <= 0:
        # Вырожденный span — всем словам даём минимальную длительность от ls.
        cur = ls
        for w in words:
            w["start"] = round(cur, 3)
            w["end"] = round(cur + min_word_dur, 3)
            cur += min_word_dur
        line["end"] = round(cur, 3)
        return line

    # Одно слово — оно заполняет весь span строки целиком (клампить не к чему).
    if n == 1:
        words[0]["start"] = round(ls, 3)
        words[0]["end"] = round(le, 3)
        return line

    # Вес слова — по длине в символах (без пробелов/пунктуации не чистим,
    # т.к. для пропорции важен видимый размер слова в караоке).
    weights = [max(len((w.get("word") or "").strip()), 1) for w in words]
    total_w = sum(weights)

    # Пропорциональная длительность, клампнутая в [min, max].
    durations = []
    for i in range(n):
        prop = span * weights[i] / total_w
        durations.append(max(min_word_dur, min(max_word_dur, prop)))

    # Абсолютный приоритет: НИ ОДНО слово не должно стать короче min_word_dur
    # (микро-слово в караоке выглядит как рывок подсветки). Поэтому если сумма
    # min_word_dur-длительностей превышает span строки (переполненная строка —
    # Whisper сжал её слишком сильно), мы НЕ ужимаем слова, а продлеваем конец
    # строки. Это смещает только границу строки (что заметно меньше, чем рваная
    # подсветка), и сохраняет monotonicность относительно следующей строки: если
    # новое line.end заедет на следующую строку, защитный монотонный постпроцесс
    # сдвинет её.
    total_d = sum(durations)
    if total_d > span:
        # Пробуем сжать пропорционально, но держим min_word_dur как пол.
        scale = span / total_d
        scaled = [max(min_word_dur, d * scale) for d in durations]
        if sum(scaled) <= span + 1e-6:
            durations = scaled
            total_d = sum(durations)
        else:
            # Сжаться до span не выходит без микро-слов — оставляем слова как
            # есть (>= min_word_dur каждое) и продлеваем line.end.
            durations = [max(min_word_dur, d) for d in durations]
            total_d = sum(durations)

    # Slack (свободное время) — это ЕСТЕСТВЕННЫЕ паузы между словами в пении.
    # Кладём его только в n-1 межсловных промежутков: первое слово начинается
    # ровно в ls, последнее кончается ровно в le. Растягивать слова сверх
    # max_word_dur этим slack'ом нельзя (вернёт растянутость, ради которой всё
    # и затевалось).
    actual_span = max(span, total_d)
    slack = actual_span - total_d
    gap = slack / (n - 1) if n > 1 else 0.0

    cur = ls
    for i in range(n):
        words[i]["start"] = round(cur, 3)
        words[i]["end"] = round(cur + durations[i], 3)
        cur += durations[i] + gap

    # Точная стыковка якорей: первое слово в ls. Конец строки = конец последнего
    # слова. Если слова переполнили исходный span (строка была переполнена), line.end
    # продлевается вслед за словами — это лучше, чем микро-слова. Если под line.end
    # остался slack — последнее слово аккуратно продлеваем до line.end, но не сверх
    # max_word_dur.
    words[0]["start"] = round(ls, 3)
    natural_last_end = words[-1]["end"]
    if natural_last_end < le - 0.02:
        # Можно продлить последнее слово до le, не нарушая max_word_dur.
        if le - words[-1]["start"] <= max_word_dur + 0.02:
            words[-1]["end"] = round(le, 3)
        else:
            line["end"] = round(natural_last_end, 3)
    elif natural_last_end > le + 0.02:
        # Слова переполнили span — продлеваем границу строки.
        line["end"] = round(natural_last_end, 3)
    # Гарантия монотонности и ненулевых длительностей.
    for i in range(n):
        if words[i]["end"] <= words[i]["start"]:
            words[i]["end"] = round(words[i]["start"] + min_word_dur, 3)
        if i > 0 and words[i]["start"] < words[i - 1]["end"] - 0.005:
            words[i]["start"] = round(words[i - 1]["end"] + 0.005, 3)
            words[i]["end"] = round(max(words[i]["start"] + min_word_dur, words[i]["end"]), 3)
    return line


def build_karaoke_from_align_result(raw_lines, align_result, redistribute=True):
    """Строит караоке-тайминги напрямую из результата model.align().

    Когда модель делает forced alignment по точному тексту песни (с
    original_split=True), каждый сегмент результата уже соответствует одной строке
    текста, а слова внутри — в порядке 1-в-1. Это идеальный, глобально-монотонный
    результат: повторы припева разведены по своим появлениям в аудио, куплеты
    не схлопываются.

    Использовать этот путь вместо match_lyrics_to_whisper предпочтительно: матчер
    делает повторное нечёткое сопоставление и на повторяющихся припевах теряет
    синхронизацию (прыгает к более позднему появлению и схлопывает куплет между
    ними). match_lyrics_to_whisper остаётся фолбэком на случай, когда align()
    частично провалился и слов меньше, чем в тексте.

    После сборки применяется перераспределение слов внутри строки
    (_redistribute_words_in_line): это убирает растянутые/микро-слова (Whisper
    плохо сегментирует ноты в пении), не сдвигая позиции строк.

    Аргументы:
      raw_lines: строки исходного текста (нужны для подсчёта ожидаемого числа слов).
      align_result: stable_whisper.WhisperResult (или None) от model.align(...).
      redistribute: применять ли перераспределение слов (по умолчанию True).

    Возвращает: (lyrics_karaoke, coverage_ratio) или (None, 0.0), если построить
    нельзя (нет слов / сегментов). coverage_ratio = доля строк текста, для которых
    найден сегмент с словами.
    """
    if align_result is None:
        return None, 0.0

    segments = list(getattr(align_result, 'segments', []) or [])
    non_empty_lines = [l for l in raw_lines if l.strip()]

    # Берём только сегменты, в которых есть слова с валидными (или исправимыми)
    # временны́ми метками. align() с original_split даёт сегмент на строку, но
    # отдельные слова могут прийти с нулевой длительностью — это нормально и
    # чинится локально.
    karaoke = []
    seg_idx = 0
    for line in non_empty_lines:
        # Пропускаем пустые сегменты, чтобы выровнять текст по реальным данным.
        while seg_idx < len(segments) and not (getattr(segments[seg_idx], 'words', None)):
            seg_idx += 1
        if seg_idx >= len(segments):
            break
        seg = segments[seg_idx]
        seg_idx += 1

        words = []
        for w in getattr(seg, 'words', []) or []:
            try:
                start = float(getattr(w, 'start', 0.0) or 0.0)
                end = float(getattr(w, 'end', start) or start)
            except (TypeError, ValueError):
                continue
            if end <= start + 0.01:
                # Нулевую/сингулярную длительность даём минимальную — это слово,
                # которое align не смог локализовать точнее, но оно на своём месте.
                end = round(start + 0.3, 3)
            words.append({
                "word": getattr(w, 'word', '') or '',
                "start": round(start, 3),
                "end": round(end, 3),
            })
        if not words:
            seg_idx -= 1
            continue

        line_obj = {
            "text": line.strip(),
            "start": words[0]["start"],
            "end": words[-1]["end"],
            "words": words,
        }
        if redistribute:
            _redistribute_words_in_line(line_obj)
        karaoke.append(line_obj)

    if not karaoke:
        return None, 0.0

    coverage = len(karaoke) / max(1, len(non_empty_lines))
    return karaoke, coverage


# ----------------- РЕНДЕРИНГ (ФОНОВЫЙ ПОТОК) -----------------
def generate_karaoke_thread(job_id, audio_path, artist, title, lyrics, model_name, quality='medium', font_family='montserrat', color_active='#000000', color_inactive='#B4B9C3', color_bg='#FFFFFF', audio_delay=0.0, vocal_start=0.0, auto_vocal_start=False, timings_only=False, timings_output=None, plain_lines=False, inactive_opacity=0.65, verify_lrc_with_whisper=False, separate_vocals_for_alignment=False):
    cleanup_align_audio_path = None
    try:
        from PIL import Image, ImageDraw, ImageFont
        
        def hex_to_rgba(hex_str, alpha=255):
            hex_str = hex_str.lstrip('#')
            if len(hex_str) == 6:
                r, g, b = tuple(int(hex_str[i:i+2], 16) for i in (0, 2, 4))
            elif len(hex_str) == 3:
                r, g, b = tuple(int(hex_str[i]*2, 16) for i in (0, 1, 2))
            else:
                r, g, b = 255, 255, 255
            return (r, g, b, alpha)
            
        # Очищаем невидимые и специальные пробельные символы во всём тексте
        lyrics = normalize_lyrics_text(lyrics)
        artist = replace_special_spaces(artist)
        title = replace_special_spaces(title)
        lyrics_language = infer_lyrics_language(strip_lrc_timestamps(lyrics) or lyrics)

        timestamped_karaoke = None
        audio_duration_for_lrc = None
        try:
            audio_duration_for_lrc = audio_duration_seconds(audio_path)
            timestamped_karaoke = build_karaoke_from_timestamped_lyrics(
                lyrics,
                audio_duration_for_lrc,
            )
        except Exception:
            timestamped_karaoke = None

        probe_timestamped_with_whisper = bool(
            timestamped_karaoke and (verify_lrc_with_whisper or not plain_lines)
        )
        alignment_lyrics = strip_lrc_timestamps(lyrics) if timestamped_karaoke else lyrics
        text_match_report = None

        if timestamped_karaoke and not probe_timestamped_with_whisper:
            lyrics_karaoke = timestamped_karaoke
            jobs[job_id]["status"] = "✅ Использована точная разметка из текста. Тайминги подготовлены."
            try:
                write_timings_and_report(
                    job_id,
                    lyrics_karaoke,
                    timings_output=timings_output,
                    audio_duration=audio_duration_for_lrc,
                    source="lrc",
                )
            except Exception:
                pass

            if timings_only:
                jobs[job_id]["progress"] = 1.0
                jobs[job_id]["status"] = "✅ Использована точная разметка из текста. Тайминги подготовлены для Rust-рендера."
                jobs[job_id]["done"] = True
                return
        else:
            # 1. ЗАПУСК ИИ-ВЫРАВНИВАНИЯ
            def update_model_status(message):
                jobs[job_id]["status"] = message

            jobs[job_id]["progress"] = 0.1
            model = get_whisper_model(model_name, update_model_status)
            vocal_start = max(0.0, float(vocal_start or 0.0))
            if auto_vocal_start and vocal_start < 0.5:
                jobs[job_id]["progress"] = 0.16
                try:
                    detected = detect_vocal_start(
                        audio_path,
                        model_name,
                        status_callback=lambda message: jobs[job_id].update({"status": message}),
                        language=lyrics_language,
                        lyrics_text=alignment_lyrics,
                    )
                    vocal_start = max(0.0, float(detected.get('vocal_start') or 0.0))
                    if vocal_start >= 0.5:
                        jobs[job_id]["status"] = f"Первый вокал найден: {vocal_start:.1f} сек. Используем как ориентир для таймингов."
                    else:
                        jobs[job_id]["status"] = "Длинное интро не найдено, распознавание начнется с 00:00."
                except Exception as e:
                    vocal_start = 0.0
                    jobs[job_id]["status"] = f"Предобработка не удалась, продолжаем с 00:00: {str(e)}"
            align_audio_path = audio_path
            if separate_vocals_for_alignment and not plain_lines:
                try:
                    jobs[job_id]["progress"] = 0.18
                    align_audio_path = separate_vocals_with_demucs(
                        audio_path,
                        status_callback=lambda message: jobs[job_id].update({"status": message}),
                    )
                    print(f"[LOG] Demucs vocals ready: {align_audio_path}", flush=True)
                except Exception as exc:
                    message = (
                        "Demucs включен, но вокал не был выделен. "
                        f"Останавливаем задачу, чтобы не делать word-level по исходному миксу. {exc}"
                    )
                    jobs[job_id]["status"] = message
                    print(f"[LOG] {message}", flush=True)
                    raise RuntimeError(message) from exc

            model = loaded_models[model_name]
            jobs[job_id]["progress"] = 0.2

            # Пре-считаем строки и длительность — нужны для решения о чанках
            _align_raw_lines = alignment_lyrics.split('\n')
            _align_non_empty = [l.strip() for l in _align_raw_lines if l.strip()]
            # Получаем длительность аудио заранее (нужна и для чанков, и для интерполяции)
            audio_duration = 120.0
            try:
                _dur_res = subprocess.run(
                    ['ffprobe', '-v', 'error', '-show_entries', 'format=duration',
                     '-of', 'default=noprint_wrappers=1:nokey=1', audio_path],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                    **subprocess_no_window_kwargs()
                )
                audio_duration = float(_dur_res.stdout.strip())
            except Exception:
                pass
            _use_chunked = False  # Отключаем экспериментальное деление по чанкам

            whisper_words = None
            result = None  # WhisperResult от model.align(); нужен для прямого пути

            if _use_chunked:
                jobs[job_id]["status"] = "Длинный трек: выравниваем по фрагментам (защита от drift Whisper)..."
                try:
                    whisper_words = align_in_chunks(
                        model,
                        align_audio_path,
                        _align_raw_lines,
                        language=lyrics_language,
                        audio_duration=audio_duration,
                        vocal_start=vocal_start,
                        status_callback=lambda msg: jobs[job_id].update({"status": msg}),
                        progress_callback=lambda p: jobs[job_id].update({"progress": p}),
                    )
                except Exception as _chunk_err:
                    jobs[job_id]["status"] = f"Чанковое выравнивание не удалось, переходим к однопроходному: {_chunk_err}"
                    whisper_words = None

            if whisper_words is None:
                # Принудительное пословное выравнивание (forced alignment) по точному
                # тексту песни. original_split=True даёт сегмент на строку текста —
                # результат глобально-монотонный и 1-в-1 по словам, повторы припева
                # разведены по своим появлениям в аудио.
                # fast_mode НЕ включаем: он хоть и уменьшает количество нулевых слов,
                # но растягивает отдельные слова по инструментальным паузам (слово
                # «поёт» пол-минуты под проигрыш), что визуально хуже для караоке.
                jobs[job_id]["status"] = "Запуск пословного выравнивания ИИ по аудио..."
                result = model.align(
                    align_audio_path,
                    alignment_lyrics,
                    language=lyrics_language,
                    original_split=True,
                    max_word_dur=2.0,
                    nonspeech_skip=3.0,
                )
                whisper_words = []
                for segment in getattr(result, 'segments', []) or []:
                    for w in getattr(segment, 'words', []) or []:
                        whisper_words.append(w)

            if not plain_lines or verify_lrc_with_whisper:
                try:
                    text_match_report = measure_lyrics_text_match(
                        model,
                        align_audio_path,
                        alignment_lyrics,
                        language=lyrics_language,
                        status_callback=lambda message: jobs[job_id].update({"status": message}),
                    )
                except Exception as exc:
                    text_match_report = {
                        "score": None,
                        "error": str(exc),
                    }

            # ДАМП СЫРЫХ ДАННЫХ WHISPER для отладки
            try:
                raw_dump = [
                    {
                        "word": getattr(w, 'word', '?'),
                        "start": round(float(w.start), 3) if hasattr(w, 'start') else -1,
                        "end": round(float(w.end), 3) if hasattr(w, 'end') else -1,
                    }
                    for w in whisper_words
                ]
                dump_path = os.path.join(EXPORT_FOLDER, f"{job_id}_whisper_raw.json")
                with open(dump_path, 'w', encoding='utf-8') as f:
                    json.dump(raw_dump, f, ensure_ascii=False, indent=2)
            except Exception:
                pass

            jobs[job_id]["progress"] = 0.4
            jobs[job_id]["status"] = "Интерполяция таймингов для сбойных сегментов ИИ..."

            n_total = len(whisper_words)

            def word_duration_limit(word):
                clean = clean_word(getattr(word, 'word', '') or '')
                return min(2.8, max(0.85, 0.34 * max(len(clean), 1)))

            def valid_word_time(word):
                if not (hasattr(word, 'start') and hasattr(word, 'end')):
                    return False
                try:
                    start = float(word.start)
                    end = float(word.end)
                except Exception:
                    return False
                if start < 0 or end <= start + 0.02:
                    return False
                if end - start > word_duration_limit(word):
                    return False
                return True

            is_valid = [valid_word_time(w) for w in whisper_words]
            interpolated_count = 0
            i = 0
            while i < n_total:
                if is_valid[i]:
                    i += 1
                    continue

                group_start = i
                while i < n_total and not is_valid[i]:
                    i += 1
                group_end = i
                num_broken = group_end - group_start

                left_time = 0.0
                for j in range(group_start - 1, -1, -1):
                    if is_valid[j]:
                        left_time = float(whisper_words[j].end)
                        break

                right_time = None
                for j in range(group_end, n_total):
                    if is_valid[j]:
                        right_time = float(whisper_words[j].start)
                        break
                if group_start == 0 and right_time is not None:
                    total_chars_before = sum(max(len(clean_word(whisper_words[group_start + k].word)), 1) for k in range(num_broken))
                    estimated_span = min(8.0, max(0.45 * num_broken, total_chars_before * 0.16))
                    left_time = max(0.0, right_time - estimated_span)
                if right_time is None:
                    total_chars = sum(max(len(clean_word(whisper_words[group_start + k].word)), 1) for k in range(num_broken))
                    right_time = min(audio_duration, left_time + min(8.0, max(0.45 * num_broken, total_chars * 0.16)))

                max_span = max(0.45 * num_broken, min(8.0, num_broken * 0.9))
                span = min(max(right_time - left_time, 0.2), max_span)
                total_chars = sum(max(len(clean_word(whisper_words[group_start + k].word)), 1) for k in range(num_broken)) or num_broken

                current_time = left_time
                for k in range(num_broken):
                    idx = group_start + k
                    word_clean = clean_word(whisper_words[idx].word)
                    char_len = max(len(word_clean), 1) if word_clean else 1
                    word_share = char_len / total_chars
                    w_dur = min(max(span * word_share, 0.12), 2.0)
                    whisper_words[idx].start = round(current_time, 3)
                    whisper_words[idx].end = round(current_time + w_dur * 0.85, 3)
                    current_time += w_dur
                interpolated_count += num_broken

            for i in range(1, n_total):
                if whisper_words[i].start < whisper_words[i - 1].end:
                    whisper_words[i].start = round(whisper_words[i - 1].end + 0.01, 3)
                max_end = whisper_words[i].start + word_duration_limit(whisper_words[i])
                if whisper_words[i].end <= whisper_words[i].start:
                    whisper_words[i].end = round(whisper_words[i].start + 0.15, 3)
                elif whisper_words[i].end > max_end:
                    whisper_words[i].end = round(max_end, 3)

            num_whisper_words = len(whisper_words)
            jobs[job_id]["status"] = f"Обработано {num_whisper_words} слов (интерполировано: {interpolated_count})"

            used_timestamped_hybrid = False
            if timestamped_karaoke:
                lrc_decision = timestamped_whisper_probe_decision(
                    timestamped_karaoke,
                    whisper_words,
                )
                if lrc_decision["action"] in ("lrc", "lrc_confined"):
                    try:
                        lyrics_karaoke, refine_stats = refine_timestamped_words_with_whisper(
                            timestamped_karaoke,
                            whisper_words,
                            line_left_pad=1.5 if lrc_decision["action"] == "lrc_confined" else 0.45,
                        )
                        if refine_stats["matched_words"] < max(1, int(refine_stats["total_words"] * 0.45)):
                            lyrics_karaoke, refine_stats = align_timestamped_lrc_words(
                                model,
                                align_audio_path,
                                timestamped_karaoke,
                                lyrics_language,
                            )
                    except Exception:
                        lyrics_karaoke, refine_stats = refine_timestamped_words_with_whisper(
                            timestamped_karaoke,
                            whisper_words,
                            line_left_pad=1.5 if lrc_decision["action"] == "lrc_confined" else 0.45,
                        )
                    jobs[job_id]["status"] = (
                        f"✅ LRC совпал с Whisper: {lrc_decision['reason']}. "
                        f"Строки из LRC, слова размечены внутри границ строк: "
                        f"{refine_stats['matched_words']}/{refine_stats['total_words']}."
                    )
                    try:
                        write_timings_and_report(
                            job_id,
                            lyrics_karaoke,
                            timings_output=timings_output,
                            audio_duration=audio_duration,
                            source="lrc_whisper_hybrid",
                            text_match=text_match_report,
                        )
                    except Exception:
                        pass
                    used_timestamped_hybrid = True
                    if timings_only:
                        jobs[job_id]["progress"] = 1.0
                        jobs[job_id]["done"] = True
                        return
                elif lrc_decision["action"] == "shift_lrc":
                    shift = float(lrc_decision.get("shift") or 0.0)
                    try:
                        lyrics_karaoke, refine_stats = refine_timestamped_words_with_whisper(
                            timestamped_karaoke,
                            whisper_words,
                            whisper_time_offset=-shift,
                            line_left_pad=0.45,
                        )
                        if refine_stats["matched_words"] < max(1, int(refine_stats["total_words"] * 0.45)):
                            lyrics_karaoke, refine_stats = align_timestamped_lrc_words(
                                model,
                                align_audio_path,
                                timestamped_karaoke,
                                lyrics_language,
                            )
                    except Exception:
                        lyrics_karaoke, refine_stats = refine_timestamped_words_with_whisper(
                            timestamped_karaoke,
                            whisper_words,
                            whisper_time_offset=-shift,
                            line_left_pad=0.45,
                        )
                    jobs[job_id]["status"] = (
                        f"✅ LRC сверено с Whisper: {lrc_decision['reason']}. "
                        f"Строки оставлены по LRC, слова размечены внутри границ строк: "
                        f"{refine_stats['matched_words']}/{refine_stats['total_words']}."
                    )
                    try:
                        write_timings_and_report(
                            job_id,
                            lyrics_karaoke,
                            timings_output=timings_output,
                            audio_duration=audio_duration,
                            source="shifted_lrc_whisper_hybrid",
                            text_match=text_match_report,
                        )
                    except Exception:
                        pass
                    used_timestamped_hybrid = True
                    if timings_only:
                        jobs[job_id]["progress"] = 1.0
                        jobs[job_id]["done"] = True
                        return
                else:
                    jobs[job_id]["status"] = f"⚠️ LRC не совпал с Whisper: {lrc_decision['reason']}. Используем тайминги Whisper."

            if not used_timestamped_hybrid:
                raw_lines = alignment_lyrics.split('\n')
                # ПРЯМОЙ ПУТЬ: результат model.align() уже глобально-монотонный —
                # сегменты соответствуют строкам текста 1-в-1, повторы припева
                # разведены по своим появлениям в аудио. Строим караоке напрямую,
                # минуя матчер, который на повторяющихся припевах теряет
                # синхронизацию (схлопывает куплеты). Матчер — фолбэк, когда align
                # провалился (слов существенно меньше, чем в тексте).
                lyrics_karaoke = None
                _align_coverage = 0.0
                if result is not None:
                    lyrics_karaoke, _align_coverage = build_karaoke_from_align_result(
                        raw_lines, result
                    )
                if lyrics_karaoke is None or _align_coverage < 0.85:
                    # Фолбэк: align не покрыл текст (мало слов/сегментов). Сначала
                    # пробуем прямой путь с тем, что есть, только если он в принципе
                    # построился; иначе — нечёткий матчер.
                    _matched = 0
                    _total = 0
                    _interp = 0
                    if lyrics_karaoke is None:
                        lyrics_karaoke, _fb_stats = match_lyrics_to_whisper(
                            raw_lines, whisper_words, confidence_threshold=0.5, lookahead=5,
                        )
                        _matched = _fb_stats.get('matched_words', 0)
                        _total = _fb_stats.get('total_words', 0)
                        _interp = _fb_stats.get('interpolated_words', 0)
                    else:
                        _total = sum(len(w['words']) for w in lyrics_karaoke)
                    jobs[job_id]["status"] = (
                        f"Выравнивание через фолбэк-матчер: {_matched}/{_total} слов "
                        f"({_interp} интерполировано). Покрытие строк align: {_align_coverage:.0%}."
                    )
                else:
                    jobs[job_id]["status"] = (
                        f"Точное пословное выравнивание: {len(lyrics_karaoke)} строк, "
                        f"покрытие {_align_coverage:.0%}."
                    )

            # ЗАЩИТА МОНОТОННОСТИ: каждая строка должна начинаться после предыдущей
            for i in range(1, len(lyrics_karaoke)):
                prev_end = lyrics_karaoke[i - 1]["end"]
                curr_start = lyrics_karaoke[i]["start"]
                if curr_start < prev_end:
                    shift = prev_end - curr_start + 0.05
                    lyrics_karaoke[i]["start"] += shift
                    lyrics_karaoke[i]["end"] += shift
                    for w in lyrics_karaoke[i]["words"]:
                        w["start"] += shift
                        w["end"] += shift

            # Локальное пере-выравнивание хвоста отключено, так как новые алгоритмы постобработки
            # Drifted Words Pullback и Unresolved Tail Redistribution полностью решают проблемы сдвигов
            # и сжатий таймингов за миллисекунды, устраняя необходимость в тяжелых повторных вызовах Whisper (VAD).
            if False and not parse_timestamped_lyrics(lyrics):
                suspect_idx = find_suspicious_tail_start(lyrics_karaoke)
                if suspect_idx is not None and 0 < suspect_idx < len(lyrics_karaoke) - 2:
                    try:
                        lyrics_karaoke, suspect_idx = compact_interjection_run_before_tail(
                            lyrics_karaoke,
                            suspect_idx,
                        )
                        target_text = lyrics_karaoke[suspect_idx]["text"]
                        prev_end = float(lyrics_karaoke[suspect_idx - 1]["end"])
                        search_start = max(0.0, prev_end + 3.0)
                        search_end = min(audio_duration, search_start + 75.0)
                        jobs[job_id]["status"] = "Найден возможный длинный проигрыш. Проверяем реальный вход следующего куплета..."
                        real_start = find_phrase_start_in_audio(
                            model,
                            audio_path,
                            target_text,
                            search_start,
                            search_end,
                            language=lyrics_language,
                            status_callback=lambda message: jobs[job_id].update({"status": message}),
                        )
                        if real_start is not None and real_start > lyrics_karaoke[suspect_idx]["start"] + 4.0:
                            import tempfile
                            nonempty_raw_lines = [line.strip() for line in raw_lines if line.strip()]
                            raw_tail_idx = suspect_idx
                            target_clean = clean_word(target_text)
                            for raw_idx in range(max(0, suspect_idx - 4), len(nonempty_raw_lines)):
                                if clean_word(nonempty_raw_lines[raw_idx]) == target_clean:
                                    raw_tail_idx = raw_idx
                                    break
                            tail_lines = nonempty_raw_lines[raw_tail_idx:]
                            tail_text = "\n".join(tail_lines)
                            window_start = max(0.0, real_start - 2.0)
                            window_duration = max(8.0, min(audio_duration - window_start, audio_duration))
                            jobs[job_id]["status"] = f"Пере-выравнивание хвоста после проигрыша с {real_start:.1f} сек..."
                            with tempfile.NamedTemporaryFile(prefix='karaoke_tail_align_', suffix='.wav', delete=False) as tmp:
                                tail_audio_path = tmp.name
                            try:
                                extract_audio_window(audio_path, tail_audio_path, window_start, window_duration)
                                tail_result = model.align(
                                    tail_audio_path,
                                    tail_text,
                                    language=lyrics_language,
                                    original_split=True,
                                    max_word_dur=2.0,
                                    vad=True,
                                    vad_threshold=0.05
                                )
                                repaired_tail = build_karaoke_from_aligned_segments(
                                    getattr(tail_result, 'segments', []) or [],
                                    tail_lines,
                                    offset=window_start,
                                )
                                if len(repaired_tail) >= 2:
                                    lyrics_karaoke = lyrics_karaoke[:suspect_idx] + repaired_tail
                                    jobs[job_id]["status"] = f"Хвост после проигрыша пере-выравнен с {real_start:.1f} сек."
                            finally:
                                try:
                                    os.remove(tail_audio_path)
                                except OSError:
                                    pass
                    except Exception as e:
                        jobs[job_id]["status"] = f"Автопроверка проигрыша не удалась, используем базовые тайминги: {str(e)}"

            # === ИНТЕЛЛЕКТУАЛЬНЫЙ АЛГОРИТМ СГЛАЖИВАНИЯ ВОКАЛЬНЫХ ХВОСТОВ (VOCAL TAIL SMOOTHING) ===
            # Этот алгоритм находит паузы после слов и плавно продлевает время их звучания,
            # чтобы пропеваемые артистом окончания (особенно гласные в конце строк) не обрезались ИИ!
            jobs[job_id]["status"] = "Сглаживание вокальных окончаний (продление пропеваемых букв)..."
            num_lines = len(lyrics_karaoke)
            for line_idx, line_data in enumerate(lyrics_karaoke):
                words = line_data["words"]
                num_words = len(words)
            
                for w_idx in range(num_words):
                    w_end = words[w_idx]["end"]
                    w_start = words[w_idx]["start"]
                
                    # Продлеваем каждое слово на 12% от его длины или минимум 0.08с для мягкого затухания
                    duration_word = w_end - w_start
                    padding = max(0.08, duration_word * 0.12)
                
                    # 1. Если это НЕ последнее слово в строке
                    if w_idx < num_words - 1:
                        next_start = words[w_idx + 1]["start"]
                        # Продлеваем до начала следующего слова, но оставляем зазор (минимум 50% паузы)
                        gap = next_start - w_end
                        if gap > 0:
                            extend = min(padding, gap * 0.5)
                            words[w_idx]["end"] = round(w_end + extend, 3)
                
                    # 2. Если это последнее слово в строке
                    else:
                        # Если есть следующая строка
                        if line_idx < num_lines - 1:
                            next_line_start = lyrics_karaoke[line_idx + 1]["start"]
                            gap = next_line_start - w_end
                            if gap > 0:
                                # Для последнего слова в строке даем большее продление (до 0.45с), 
                                # так как гласные в конце фраз часто пропеваются очень долго!
                                extend = min(0.45, gap * 0.6)
                                words[w_idx]["end"] = round(w_end + extend, 3)
                        else:
                            # Если это самое последнее слово всей песни, просто продлим его на 0.5с для красоты
                            words[w_idx]["end"] = round(w_end + 0.5, 3)
            
                # Корректируем общие границы строки после изменения слов
                line_data["start"] = words[0]["start"]
                line_data["end"] = words[-1]["end"]

            # === АЛГОРИТМ ПРЕДОТВРАЩЕНИЯ ПЕРЕХЛЕСТОВ СТРОК (OVERLAP PREVENTION FILTER) ===
            # Этот алгоритм жестко устраняет перекрытия между строками, если ИИ Whisper ошибся
            # и поставил начало новой строки раньше, чем фактически закончилась предыдущая строка!
            jobs[job_id]["status"] = "Применение фильтра устранения перекрытий строк..."
            for i in range(1, num_lines):
                prev_end = lyrics_karaoke[i - 1]["end"]
                curr_start = lyrics_karaoke[i]["start"]
            
                # Если следующая строка начинается раньше, чем завершилась предыдущая
                if curr_start < prev_end + 0.05:
                    corrected_start = prev_end + 0.05
                    lyrics_karaoke[i]["start"] = corrected_start
                
                    # Корректируем тайминги каждого слова в сползающей строке
                    for w in lyrics_karaoke[i]["words"]:
                        if w["start"] < corrected_start:
                            w["start"] = corrected_start
                        if w["end"] < w["start"]:
                            w["end"] = w["start"] + 0.1
                
                    # Корректируем общее время окончания текущей строки
                    lyrics_karaoke[i]["end"] = max(lyrics_karaoke[i]["end"], lyrics_karaoke[i]["words"][-1]["end"])

            lyrics_karaoke = redistribute_repeated_tail_lines(lyrics_karaoke)
            lyrics_karaoke = repair_stretched_short_lines(lyrics_karaoke)
            lyrics_karaoke = repair_vocalization_lines(lyrics_karaoke)
            lyrics_karaoke = sanitize_word_timings(lyrics_karaoke)

        # ДАМП ФИНАЛЬНЫХ ТАЙМИНГОВ для отладки и будущего Rust-рендера.
        # Важно писать его после всех smoothing/overlap фильтров: renderer должен получать
        # ровно те же тайминги, которые рисует текущий Python-путь.
        try:
            write_timings_and_report(
                job_id,
                lyrics_karaoke,
                timings_output=timings_output,
                audio_duration=locals().get("audio_duration", audio_duration_for_lrc),
                source="whisper_forced_alignment",
                text_match=text_match_report,
            )
        except Exception:
            pass

        if timings_only:
            jobs[job_id]["progress"] = 1.0
            jobs[job_id]["status"] = "✅ Тайминги подготовлены для Rust-рендера."
            jobs[job_id]["done"] = True
            return

        jobs[job_id]["progress"] = 0.5

        # 3. РЕНДЕРИНГ ВИДЕО С ЭФФЕКТАМИ И КАСТОМИЗАЦИЕЙ
        jobs[job_id]["status"] = "Подготовка рендеринга видео через FFmpeg..."
        
        # Настройка масштаба разрешения и сжатия видео
        size_scale = 1.0
        crf = '23'
        preset = 'fast'
        
        if quality == 'high':
            size_scale = 1.0
            crf = '17'
            preset = 'medium'
        elif quality == 'ultra':
            size_scale = 2.0
            crf = '12'
            preset = 'slow'
            
        width = int(1352 * size_scale)
        height = int(224 * size_scale)
        line_spacing = int(62 * size_scale)
        safe_line_w = max(width * 0.72, width - int(128 * size_scale))
        
        # Размеры шрифтов
        font_size_max = int(42 * size_scale)
        font_size_min = int(26 * size_scale)
        
        # Центры и смещения
        y_center = height // 2
        y_text_center = int(31 * size_scale)
        line_y_cutoff = int(110 * size_scale)
        dist_cutoff = int(95 * size_scale)
        
        # Высота строки и картинок
        line_img_h = int(75 * size_scale)
        y_draw = int(10 * size_scale)
        
        rgba_active = hex_to_rgba(color_active, 255)
        rgba_inactive = hex_to_rgba(color_inactive, 255)
        rgba_bg = hex_to_rgba(color_bg, 255)
        
        fps = 30

        duration = 30.0
        try:
            cmd = [
                'ffprobe', '-v', 'error', '-show_entries', 'format=duration',
                '-of', 'default=noprint_wrappers=1:nokey=1', audio_path
            ]
            res_dur = subprocess.run(
                cmd,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                check=True,
                **subprocess_no_window_kwargs()
            )
            duration = float(res_dur.stdout.strip())
        except Exception:
            pass

        total_frames = int(duration * fps)
        
        font_path_reg = get_system_font(font_name=font_family, bold=False)
        font_path_bold = get_system_font(font_name=font_family, bold=True)
        font_path_black = font_path_bold

        mode_suffix = "karaoke-lines" if plain_lines else "karaoke-word"
        clean_filename = f"{artist} - {title} ({mode_suffix}).mp4".replace("/", "_").replace("\\", "_")
        output_mp4_path = os.path.join(EXPORT_FOLDER, clean_filename)

        ffmpeg_cmd = [
            'ffmpeg', '-y',
            '-f', 'rawvideo',
            '-pix_fmt', 'rgb24',
            '-s', f'{width}x{height}',
            '-r', str(fps),
            '-i', '-',
            '-i', audio_path,
            '-map', '0:v:0',
            '-map', '1:a:0',
            '-c:v', 'libx264',
            '-pix_fmt', 'yuv420p',
            '-preset', preset,
            '-crf', crf,
            '-bf', '0',
            '-vsync', 'cfr',
            '-avoid_negative_ts', 'make_zero',
            '-c:a', 'aac',
            '-b:a', '192k',
            '-movflags', '+faststart',
            '-t', f'{duration:.3f}',
            output_mp4_path
        ]
        
        process = subprocess.Popen(
            ffmpeg_cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            **subprocess_no_window_kwargs()
        )

        transition_duration = 0.80
        transition_total_frames = max(1, int(round(transition_duration * fps)))
        last_target_y = 0.0
        transition_start_y = 0.0
        transition_frame = transition_total_frames
        current_scroll_y = 0.0

        font_cache = {}
        def get_font_at_size(style, size):
            # style can be 'reg', 'bold', 'black'
            key = (style, size)
            if key not in font_cache:
                if style == 'black':
                    path = font_path_black
                elif style == 'bold':
                    path = font_path_bold
                else:
                    path = font_path_reg
                
                if path and os.path.exists(path):
                    try:
                        font_cache[key] = ImageFont.truetype(path, size)
                    except Exception:
                        font_cache[key] = ImageFont.load_default()
                else:
                    font_cache[key] = ImageFont.load_default()
            return font_cache[key]

        try:
            resampling_filter = Image.Resampling.BILINEAR
        except AttributeError:
            resampling_filter = Image.BILINEAR

        font_max_bold = get_font_at_size(style='bold', size=font_size_max)
        word_pad = int(20 * size_scale)
        word_active_offset = int(10 * size_scale)
        line_pad_x = int(40 * size_scale)
        line_text_x = int(20 * size_scale)

        jobs[job_id]["status"] = "Подготовка кеша строк для быстрого рендера..."
        line_render_cache = []
        for line_data in lyrics_karaoke:
            words = line_data["words"]
            
            widths_bold, space_w_bold = get_word_widths(words, font_max_bold)
            total_w_bold = sum(widths_bold) + space_w_bold * max(0, len(words) - 1)
            line_img_w_bold = max(1, total_w_bold + line_pad_x)
            
            inactive_img = Image.new("RGBA", (line_img_w_bold, line_img_h), (0, 0, 0, 0))
            active_plain_img = Image.new("RGBA", (line_img_w_bold, line_img_h), (0, 0, 0, 0))
            inactive_draw = ImageDraw.Draw(inactive_img)
            active_plain_draw = ImageDraw.Draw(active_plain_img)

            x_draw_bold = line_text_x
            word_layers = []
            for w_idx, w_data in enumerate(words):
                word = w_data["word"]
                word_w_bold = widths_bold[w_idx]
                inactive_draw.text((x_draw_bold, y_draw), word, fill=rgba_inactive, font=font_max_bold)
                active_plain_draw.text((x_draw_bold, y_draw), word, fill=rgba_active, font=font_max_bold)

                active_word_img = Image.new("RGBA", (word_w_bold + word_pad, line_img_h), (0, 0, 0, 0))
                active_word_draw = ImageDraw.Draw(active_word_img)
                active_word_draw.text((word_active_offset, y_draw), word, fill=rgba_active, font=font_max_bold)
                word_layers.append({
                    "start": w_data["start"],
                    "end": w_data["end"],
                    "paste_x": x_draw_bold - word_active_offset,
                    "image": active_word_img,
                    "width": active_word_img.width,
                })
                x_draw_bold += word_w_bold + space_w_bold

            line_render_cache.append({
                "inactive": inactive_img,
                "active_plain": active_plain_img,
                "word_layers": word_layers,
                "width": line_img_w_bold,
                "height": line_img_h,
            })

        # Моменты, когда скролл переключается на следующую строку.
        visual_lag = float(os.environ.get("KARAOKE_VISUAL_LAG_SECONDS", "0.25") or 0.25)
        visual_lag = max(0.0, min(2.0, visual_lag))
        transition_times = []
        for idx, line_data in enumerate(lyrics_karaoke):
                if idx == 0:
                    transition_times.append(float("-inf"))
                else:
                    transition_times.append(line_data["start"])

        bg_template = Image.new('RGB', (width, height), (rgba_bg[0], rgba_bg[1], rgba_bg[2]))
        non_active_cache = {}

        scroll_alpha = float(os.environ.get("KARAOKE_SCROLL_SMOOTHING", "0.065") or 0.065)
        scroll_alpha = max(0.01, min(0.25, scroll_alpha))
        for frame_idx in range(total_frames):
            display_t = frame_idx / fps - audio_delay
            highlight_t = frame_idx / fps - audio_delay - visual_lag
            
            # Быстрое копирование RGB шаблона фона
            image = bg_template.copy()
            
            # Интеллектуальный алгоритм превентивного скроллинга (Anticipatory Scrolling)
            active_line_idx = 0
            if transition_times:
                active_line_idx = max(0, min(len(lyrics_karaoke) - 1, bisect.bisect_right(transition_times, display_t) - 1))

            target_scroll_y = active_line_idx * line_spacing
            if target_scroll_y != last_target_y:
                transition_start_y = current_scroll_y
                transition_frame = 0
                last_target_y = target_scroll_y
                
            if transition_frame < transition_total_frames:
                transition_frame += 1
                x = transition_frame / transition_total_frames
                p = x * x * x * (x * (x * 6.0 - 15.0) + 10.0) # Perlin's smootherstep
                current_scroll_y = transition_start_y + (target_scroll_y - transition_start_y) * p
            else:
                current_scroll_y = target_scroll_y
            
            for idx, line_data in enumerate(lyrics_karaoke):
                line_y = y_center + (idx * line_spacing) - current_scroll_y
                if line_y < y_center - line_y_cutoff or line_y > y_center + line_y_cutoff:
                    continue
                    
                dist_from_center = abs(line_y - y_center)
                weight = max(0.0, min(1.0, 1.0 - (dist_from_center / line_spacing)))
                
                is_active = (idx == active_line_idx)
                cached_line = line_render_cache[idx]
                
                # Масштабируем холст строки методом субпиксельной интерполяции BILINEAR
                target_scale = (font_size_min + (font_size_max - font_size_min) * weight) / font_size_max
                fit_scale = min(1.0, safe_line_w / max(1, cached_line["width"]))
                scale = min(target_scale, fit_scale)
                
                # Применяем плавное изменение прозрачности в зависимости от положения на экране
                flat_ratio = 0.7
                if dist_from_center < dist_cutoff * flat_ratio:
                    opacity = 1.0
                else:
                    opacity = max(0.0, min(1.0, 1.0 - (dist_from_center - dist_cutoff * flat_ratio) / (dist_cutoff * (1.0 - flat_ratio))))
                if not is_active:
                    opacity *= inactive_opacity
                
                scale_key = round(scale, 2)
                opacity_key = round(opacity, 2)
                
                if not is_active:
                    # Для неактивных строк используем кэш отмасштабированных изображений
                    cache_key = (idx, scale_key, opacity_key)
                    if cache_key in non_active_cache:
                        resized_img = non_active_cache[cache_key]
                    else:
                        line_img = cached_line["inactive"]
                        new_w = max(1, int(cached_line["width"] * scale))
                        new_h = max(1, int(cached_line["height"] * scale))
                        resized_img = line_img.resize((new_w, new_h), resampling_filter)
                        
                        if opacity < 1.0:
                            alpha = resized_img.getchannel('A')
                            lut = [int(p * opacity) for p in range(256)]
                            new_alpha = alpha.point(lut)
                            resized_img.putalpha(new_alpha)
                        
                        non_active_cache[cache_key] = resized_img
                else:
                    # Для активной строки строим картинку с пословной заливкой
                    if not plain_lines:
                        line_img = cached_line["inactive"].copy()
                        for layer in cached_line["word_layers"]:
                            w_start = layer["start"]
                            w_end = layer["end"]
                            if highlight_t < w_start:
                                continue
                            elif highlight_t > w_end:
                                line_img.paste(layer["image"], (layer["paste_x"], 0), layer["image"])
                            else:
                                # Плавный цветной накат
                                progress = max(0.0, min(1.0, (highlight_t - w_start) / max(0.001, w_end - w_start)))
                                fill_w = int(layer["width"] * progress)
                                if fill_w > 0:
                                    filled_part = layer["image"].crop((0, 0, fill_w, line_img_h))
                                    line_img.paste(filled_part, (layer["paste_x"], 0), filled_part)
                    else:
                        line_img = cached_line["active_plain"]
                        
                    new_w = max(1, int(cached_line["width"] * scale))
                    new_h = max(1, int(cached_line["height"] * scale))
                    resized_img = line_img.resize((new_w, new_h), resampling_filter)
                    
                    if opacity < 1.0:
                        alpha = resized_img.getchannel('A')
                        lut = [int(p * opacity) for p in range(256)]
                        new_alpha = alpha.point(lut)
                        resized_img.putalpha(new_alpha)
                
                # Вычисляем субпиксельные координаты для точной центральной вставки без дрожания
                x_paste = width // 2 - resized_img.width // 2
                y_center_in_resized = y_text_center * scale
                y_paste = int(line_y - y_center_in_resized)
                
                image.paste(resized_img, (x_paste, y_paste), resized_img)
                    
            process.stdin.write(image.tobytes())
            
            if frame_idx % (fps // 2) == 0:
                prog_val = 0.5 + (frame_idx / total_frames) * 0.5
                jobs[job_id]["progress"] = round(prog_val, 2)
                jobs[job_id]["status"] = f"Рендеринг караоке (плавная заливка): {int((frame_idx/total_frames)*100)}%..."

        process.stdin.close()
        process.wait()

        jobs[job_id]["progress"] = 1.0
        jobs[job_id]["status"] = f"✅ Готово! Караоке-видео успешно создано!"
        jobs[job_id]["done"] = True
        jobs[job_id]["file"] = clean_filename

    except Exception as e:
        jobs[job_id]["error"] = str(e)
        jobs[job_id]["status"] = f"❌ Ошибка: {str(e)}"
        traceback.print_exc()
    finally:
        if cleanup_align_audio_path:
            try:
                os.remove(cleanup_align_audio_path)
            except OSError:
                pass


if __name__ == '__main__':
    import sys
    if len(sys.argv) > 1 and sys.argv[1] in ("batch", "parse-sheet", "download", "resolve", "candidates", "lyrics", "lyrics-discover"):
        from unified_resolver.__main__ import main as resolver_main
        sys.exit(resolver_main(sys.argv[1:]))

    if '--help' in sys.argv or '-h' in sys.argv:
        import argparse
        parser = argparse.ArgumentParser(description="Караоке-Генератор CLI")
        parser.add_argument('--cli', action='store_true')
        parser.add_argument('--audio', required=False)
        parser.add_argument('--batch-align-queue')
        parser.add_argument('--artist', default='Исполнитель')
        parser.add_argument('--title', default='Песня')
        parser.add_argument('--lyrics-file')
        parser.add_argument('--model', default='small')
        parser.add_argument('--quality', default='medium')
        parser.add_argument('--font', default='montserrat')
        parser.add_argument('--color-active', default='#000000')
        parser.add_argument('--color-inactive', default='#B4B9C3')
        parser.add_argument('--color-bg', default='#FFFFFF')
        parser.add_argument('--audio-delay', type=float, default=0.0)
        parser.add_argument('--inactive-opacity', type=float, default=0.65)
        parser.add_argument('--vocal-start', type=float, default=0.0)
        parser.add_argument('--auto-vocal-start', action='store_true')
        parser.add_argument('--detect-vocal-start', action='store_true')
        parser.add_argument('--detect-window', type=float, default=45.0)
        parser.add_argument('--timings-only', action='store_true')
        parser.add_argument('--timings-output')
        parser.add_argument('--plain-lines', action='store_true', default=False)
        parser.add_argument('--no-scrolling', action='store_true', dest='plain_lines')
        parser.add_argument('--verify-lrc-with-whisper', action='store_true')
        parser.add_argument('--separate-vocals-for-alignment', action='store_true')
        parser.print_help()
        sys.exit(0)

class ObservableDict(dict):
    def __init__(self, parent, key, *args, **kwargs):
        self.parent = parent
        self.key = key
        super().__init__(*args, **kwargs)
    def __setitem__(self, k, v):
        super().__setitem__(k, v)
        self.parent.notify(self.key, self)
    def update(self, *args, **kwargs):
        super().update(*args, **kwargs)
        self.parent.notify(self.key, self)

class CLIJobsDict(dict):
    def __init__(self, batch_align_index=None, *args, **kwargs):
        self.batch_align_index = batch_align_index
        super().__init__(*args, **kwargs)
    def __getitem__(self, key):
        if key not in self:
            super().__setitem__(key, ObservableDict(self, key))
        return super().__getitem__(key)
    def __setitem__(self, key, val):
        if not isinstance(val, ObservableDict):
            val = ObservableDict(self, key, val)
        super().__setitem__(key, val)
        self.notify(key, val)
    def notify(self, key, val):
        out = dict(val)
        if self.batch_align_index is not None:
            out["batch_align_index"] = self.batch_align_index
        print(json.dumps(out), flush=True)

def run_batch_align(queue_path, model_name, quality='medium', font_family='montserrat',
                    color_active='#000000', color_inactive='#B4B9C3', color_bg='#FFFFFF',
                    audio_delay=0.0, plain_lines=False, inactive_opacity=0.65,
                    verify_lrc_with_whisper=False, separate_vocals_for_alignment=False):
    with open(queue_path, 'r', encoding='utf-8') as f:
        queue = json.load(f)

    for item in queue:
        idx = item["index"]
        audio_path = item["audio"]
        artist = item["artist"]
        title = item["title"]
        lyrics_file = item["lyrics_file"]
        timings_output = item["timings_output"]

        # Read lyrics
        with open(lyrics_file, 'r', encoding='utf-8') as lf:
            lyrics_text = lf.read()

        # Set up globals for this item
        globals()['jobs'] = CLIJobsDict(batch_align_index=idx)
        job_id = f"batch_job_{idx}"
        jobs[job_id] = {
            "progress": 0.0,
            "status": f"Начало синхронизации...",
            "done": False,
            "error": None,
            "file": None
        }

        try:
            generate_karaoke_thread(
                job_id=job_id,
                audio_path=audio_path,
                artist=artist,
                title=title,
                lyrics=lyrics_text,
                model_name=model_name,
                quality=quality,
                font_family=font_family,
                color_active=color_active,
                color_inactive=color_inactive,
                color_bg=color_bg,
                audio_delay=audio_delay,
                timings_only=True,
                timings_output=timings_output,
                plain_lines=plain_lines,
                inactive_opacity=inactive_opacity,
                verify_lrc_with_whisper=verify_lrc_with_whisper,
                separate_vocals_for_alignment=separate_vocals_for_alignment,
            )
            # Ensure done message is sent
            jobs[job_id]["progress"] = 1.0
            jobs[job_id]["status"] = "Синхронизация завершена"
            jobs[job_id]["done"] = True
        except Exception as e:
            jobs[job_id]["progress"] = 1.0
            jobs[job_id]["status"] = f"Ошибка: {str(e)}"
            jobs[job_id]["error"] = str(e)
            jobs[job_id]["done"] = True

def run_cli_entrypoint():
    import argparse
    parser = argparse.ArgumentParser(description="Караоке-Генератор CLI")
    parser.add_argument('--cli', action='store_true')
    parser.add_argument('--audio', required=False)
    parser.add_argument('--batch-align-queue')
    parser.add_argument('--artist', default='Исполнитель')
    parser.add_argument('--title', default='Песня')
    parser.add_argument('--lyrics-file')
    parser.add_argument('--model', default='small')
    parser.add_argument('--quality', default='medium')
    parser.add_argument('--font', default='montserrat')
    parser.add_argument('--color-active', default='#000000')
    parser.add_argument('--color-inactive', default='#B4B9C3')
    parser.add_argument('--color-bg', default='#FFFFFF')
    parser.add_argument('--audio-delay', type=float, default=0.0)
    parser.add_argument('--inactive-opacity', type=float, default=0.65)
    parser.add_argument('--vocal-start', type=float, default=0.0)
    parser.add_argument('--auto-vocal-start', action='store_true')
    parser.add_argument('--detect-vocal-start', action='store_true')
    parser.add_argument('--detect-window', type=float, default=45.0)
    parser.add_argument('--timings-only', action='store_true')
    parser.add_argument('--timings-output')
    parser.add_argument('--plain-lines', action='store_true', default=False)
    parser.add_argument('--no-scrolling', action='store_true', dest='plain_lines')
    parser.add_argument('--verify-lrc-with-whisper', action='store_true')
    parser.add_argument('--separate-vocals-for-alignment', action='store_true')

    args = parser.parse_args()

    if args.batch_align_queue:
        try:
            run_batch_align(
                args.batch_align_queue,
                args.model,
                quality=args.quality,
                font_family=args.font,
                color_active=args.color_active,
                color_inactive=args.color_inactive,
                color_bg=args.color_bg,
                audio_delay=args.audio_delay,
                plain_lines=args.plain_lines,
                inactive_opacity=args.inactive_opacity,
                verify_lrc_with_whisper=args.verify_lrc_with_whisper,
                separate_vocals_for_alignment=args.separate_vocals_for_alignment,
            )
            sys.exit(0)
        except Exception as e:
            print(json.dumps({"progress": 1.0, "status": f"❌ Ошибка пакетного выравнивания: {str(e)}", "done": True, "error": str(e)}), flush=True)
            sys.exit(1)

    if args.detect_vocal_start:
        try:
            print(json.dumps({"progress": 0.05, "status": "Предобработка: поиск первого вокала...", "done": False}), flush=True)
            lyrics_for_detect = ''
            if args.lyrics_file:
                try:
                    with open(args.lyrics_file, 'r', encoding='utf-8') as f:
                        lyrics_for_detect = f.read()
                except Exception:
                    lyrics_for_detect = ''
            detected = detect_vocal_start(
                args.audio,
                args.model,
                args.detect_window,
                language=infer_lyrics_language(lyrics_for_detect),
                lyrics_text=lyrics_for_detect,
            )
            print(json.dumps({
                "progress": 1.0,
                "status": f"Первый вокал найден: {detected['vocal_start']:.1f} сек.",
                "done": True,
                "vocal_start": detected["vocal_start"],
                "confidence": detected["confidence"],
                "segments": detected["segments"],
            }), flush=True)
            sys.exit(0)
        except Exception as e:
            print(json.dumps({"progress": 1.0, "status": f"Предобработка не удалась: {str(e)}", "done": True, "error": str(e), "vocal_start": 0.0}), flush=True)
            sys.exit(1)

    if not args.lyrics_file:
        print(json.dumps({"progress": 1.0, "status": "❌ Ошибка: не указан файл текста", "done": True, "error": "lyrics-file is required"}), flush=True)
        sys.exit(1)

    with open(args.lyrics_file, 'r', encoding='utf-8') as f:
        lyrics_text = f.read()

    globals()['jobs'] = CLIJobsDict()
    job_id = "cli_job"
    jobs[job_id] = {
        "progress": 0.0,
        "status": "Инициализация CLI-генерации...",
        "done": False,
        "error": None,
        "file": None
    }

    try:
        generate_karaoke_thread(
            job_id=job_id,
            audio_path=args.audio,
            artist=args.artist,
            title=args.title,
            lyrics=lyrics_text,
            model_name=args.model,
            quality=args.quality,
            font_family=args.font,
            color_active=args.color_active,
            color_inactive=args.color_inactive,
            color_bg=args.color_bg,
            audio_delay=args.audio_delay,
            vocal_start=args.vocal_start,
            auto_vocal_start=args.auto_vocal_start,
            timings_only=args.timings_only,
            timings_output=args.timings_output,
            plain_lines=args.plain_lines,
            inactive_opacity=args.inactive_opacity,
            verify_lrc_with_whisper=args.verify_lrc_with_whisper,
            separate_vocals_for_alignment=args.separate_vocals_for_alignment,
        )
        if jobs[job_id].get("error"):
            sys.exit(1)
    except Exception as e:
        print(json.dumps({"progress": 1.0, "status": f"❌ Ошибка: {str(e)}", "done": True, "error": str(e)}), flush=True)
        sys.exit(1)
    sys.exit(0)



if __name__ == '__main__':
    import sys as _sys
    if '--cli' in _sys.argv:
        run_cli_entrypoint()

    if getattr(_sys, "frozen", False):
        print("This bundled worker is intended to be launched by Karaoke Generator with --cli.", file=_sys.stderr)
        _sys.exit(2)
