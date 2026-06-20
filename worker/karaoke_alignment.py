import re


def replace_special_spaces(text):
    if not isinstance(text, str):
        return text
    special_spaces = [
        '\u2000', '\u2001', '\u2002', '\u2003', '\u2004', '\u2005',
        '\u2006', '\u2007', '\u2008', '\u2009', '\u200a', '\u200b',
        '\u202f', '\u205f', '\u3000', '\u00a0'
    ]
    for sp in special_spaces:
        text = text.replace(sp, ' ')
    return text.replace('\u200b', '').replace('\u200e', '').replace('\u200f', '')


CYRILLIC_CONFUSABLES = str.maketrans({
    "A": "А", "a": "а",
    "B": "В",
    "C": "С", "c": "с",
    "E": "Е", "e": "е",
    "H": "Н",
    "K": "К", "k": "к",
    "M": "М",
    "O": "О", "o": "о",
    "P": "Р", "p": "р",
    "T": "Т",
    "X": "Х", "x": "х",
    "Y": "У", "y": "у",
})


def normalize_mixed_cyrillic_word(word):
    if not isinstance(word, str) or not word:
        return word
    has_cyrillic = bool(re.search(r"[А-Яа-яЁё]", word))
    has_latin = bool(re.search(r"[A-Za-z]", word))
    if has_cyrillic and has_latin:
        return word.translate(CYRILLIC_CONFUSABLES)
    return word


def normalize_mixed_cyrillic_text(text):
    if not isinstance(text, str):
        return text
    return re.sub(r"[A-Za-zА-Яа-яЁё]+", lambda m: normalize_mixed_cyrillic_word(m.group(0)), text)


def normalize_lyrics_text(text):
    text = replace_special_spaces(text)
    if not isinstance(text, str):
        return text
    text = normalize_mixed_cyrillic_text(text)
    text = re.sub(r'([A-Za-zА-Яа-яЁё]{2,})([.!?…]{2,})(?=([A-ZА-ЯЁ]))', r'\1\2\n', text)
    text = re.sub(r'(?i)(\bA\s+denial[.!?…]*)(?:\s+|(?=A\s+denial))', r'\1\n', text)
    text = re.sub(r'\n{3,}', '\n\n', text)

    grouped_lines = []
    denial_run = []
    for line in text.splitlines():
        stripped = line.strip()
        if re.fullmatch(r'(?i)A\s+denial[.!?…]*', stripped):
            denial_run.append(stripped)
            continue
        for idx in range(0, len(denial_run), 2):
            grouped_lines.append(" ".join(denial_run[idx:idx + 2]))
        denial_run = []
        grouped_lines.append(line.rstrip())
    for idx in range(0, len(denial_run), 2):
        grouped_lines.append(" ".join(denial_run[idx:idx + 2]))

    return "\n".join(grouped_lines).strip()


def clean_word(w):
    w = normalize_mixed_cyrillic_text(replace_special_spaces(w))
    return re.sub(r'[^\w\s]', '', w.strip().lower())


def bounded_levenshtein(a, b, limit):
    if abs(len(a) - len(b)) > limit:
        return limit + 1
    previous = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        current = [i]
        row_min = current[0]
        for j, cb in enumerate(b, 1):
            insert = current[j - 1] + 1
            delete = previous[j] + 1
            replace = previous[j - 1] + (ca != cb)
            value = min(insert, delete, replace)
            current.append(value)
            row_min = min(row_min, value)
        if row_min > limit:
            return limit + 1
        previous = current
    return previous[-1]


def parse_lrc_timestamp(raw):
    match = re.match(r'^\s*(\d{1,2}):(\d{2})(?:[.:](\d{1,3}))?\s*$', raw or '')
    if not match:
        return None
    minutes = int(match.group(1))
    seconds = int(match.group(2))
    fraction = match.group(3) or '0'
    fraction_seconds = int(fraction.ljust(3, '0')[:3]) / 1000.0
    return minutes * 60.0 + seconds + fraction_seconds


def parse_timestamped_lyrics(lyrics):
    entries = []
    has_timestamps = False
    for raw_line in (lyrics or '').splitlines():
        line = replace_special_spaces(raw_line).strip()
        if not line:
            continue
        matches = list(re.finditer(r'\[([0-9]{1,2}:[0-9]{2}(?:[.:][0-9]{1,3})?)\]', line))
        if not matches:
            continue
        text = re.sub(r'\[[0-9]{1,2}:[0-9]{2}(?:[.:][0-9]{1,3})?\]', '', line).strip()
        for match in matches:
            timestamp = parse_lrc_timestamp(match.group(1))
            if timestamp is not None:
                entries.append({"time": timestamp, "text": text})
                has_timestamps = True
    if not has_timestamps:
        return None
    return sorted(entries, key=lambda item: item["time"])


def strip_lrc_timestamps(lyrics):
    cleaned_lines = []
    for raw_line in (lyrics or '').splitlines():
        line = replace_special_spaces(raw_line)
        line = re.sub(r'\[[0-9]{1,2}:[0-9]{2}(?:[.:][0-9]{1,3})?\]', '', line).strip()
        cleaned_lines.append(line)
    return "\n".join(cleaned_lines).strip()


def estimate_line_duration(words, available):
    if not words:
        return 0.0
    total_chars = sum(max(len(clean_word(word)), 1) for word in words)
    estimated = 0.34 * len(words) + 0.045 * total_chars
    estimated = min(5.2, max(0.75, estimated))
    if available is not None:
        estimated = min(estimated, max(0.25, available - 0.12))
    return estimated


def fuzzy_word_match(a, b):
    a = clean_word(a)
    b = clean_word(b)
    if not a or not b:
        return False
    if a == b:
        return True
    if len(a) >= 4 and len(b) >= 4:
        if a in b or b in a:
            return True
        max_len = max(len(a), len(b))
        limit = 1 if max_len <= 5 else 2
        return bounded_levenshtein(a, b, limit) <= limit
    return False


def lyric_text_score(expected_text, heard_text):
    expected = [w for w in re.split(r'\s+', expected_text or '') if clean_word(w)]
    heard = [w for w in re.split(r'\s+', heard_text or '') if clean_word(w)]
    if not expected or not heard:
        return 0.0
    score = 0.0
    for ew in expected:
        if any(fuzzy_word_match(ew, hw) for hw in heard):
            score += 1.0
    return score / max(1, len(expected))


def build_karaoke_from_timestamped_lyrics(lyrics, audio_duration=None):
    entries = parse_timestamped_lyrics(lyrics)
    if not entries:
        return None

    lyrics_karaoke = []
    for idx, entry in enumerate(entries):
        text = entry["text"].strip()
        if not text:
            continue
        start = float(entry["time"])
        next_time = None
        for next_entry in entries[idx + 1:]:
            if next_entry["time"] > start + 0.05:
                next_time = float(next_entry["time"])
                break
        if next_time is None and audio_duration:
            next_time = float(audio_duration)

        words = text.split()
        if not words:
            continue
        available = None if next_time is None else next_time - start
        duration = estimate_line_duration(words, available)
        if duration <= 0:
            continue

        total_chars = sum(max(len(clean_word(word)), 1) for word in words) or len(words)
        cursor = start
        line_words = []
        for word in words:
            char_len = max(len(clean_word(word)), 1)
            share = char_len / total_chars
            word_slot = max(0.16, duration * share)
            word_end = min(start + duration, cursor + word_slot * 0.88)
            line_words.append({
                "word": word,
                "start": round(cursor, 3),
                "end": round(max(cursor + 0.08, word_end), 3),
            })
            cursor += word_slot

        if line_words:
            lyrics_karaoke.append({
                "text": text,
                "start": line_words[0]["start"],
                "end": line_words[-1]["end"],
                "words": line_words,
            })
    return lyrics_karaoke


def shift_karaoke_timings(karaoke, shift_seconds):
    shifted = []
    for line in karaoke or []:
        new_line = dict(line)
        new_line["start"] = round(max(0.0, float(new_line.get("start", 0.0)) + shift_seconds), 3)
        new_line["end"] = round(max(new_line["start"] + 0.05, float(new_line.get("end", 0.0)) + shift_seconds), 3)
        new_words = []
        for word in new_line.get("words", []) or []:
            new_word = dict(word)
            new_word["start"] = round(max(0.0, float(new_word.get("start", 0.0)) + shift_seconds), 3)
            new_word["end"] = round(max(new_word["start"] + 0.05, float(new_word.get("end", 0.0)) + shift_seconds), 3)
            new_words.append(new_word)
        new_line["words"] = new_words
        if new_words:
            new_line["start"] = new_words[0]["start"]
            new_line["end"] = new_words[-1]["end"]
        shifted.append(new_line)
    return shifted


def evaluate_alignment_quality(karaoke, audio_duration=None, source=None, text_match=None):
    """Builds a machine-readable quality report for generated karaoke timings.

    The report intentionally uses only final timing JSON, so it can be reused by
    future render-only, import, batch, and manual-editor flows.
    """
    lines = karaoke or []
    report = {
        "source": source or "unknown",
        "line_count": len(lines),
        "word_count": 0,
        "duration": round(float(audio_duration), 3) if audio_duration else None,
        "metrics": {
            "zero_or_tiny_words": 0,
            "long_words": 0,
            "line_overlaps": 0,
            "word_overlaps": 0,
            "large_internal_gaps": 0,
            "long_lines": 0,
            "out_of_bounds": 0,
            "empty_lines": 0,
            "max_word_duration": 0.0,
            "max_line_duration": 0.0,
            "max_internal_gap": 0.0,
            "text_match_score": None,
        },
        "issues": [],
        "summary": "ok",
        "score": 1.0,
    }

    def issue(kind, severity, message, line_index=None, word_index=None, **extra):
        item = {
            "kind": kind,
            "severity": severity,
            "message": message,
        }
        if line_index is not None:
            item["line_index"] = line_index
        if word_index is not None:
            item["word_index"] = word_index
        item.update(extra)
        report["issues"].append(item)

    previous_line_end = None
    for line_idx, line in enumerate(lines):
        words = line.get("words") or []
        if not words:
            report["metrics"]["empty_lines"] += 1
            issue("empty_line", "warning", "Line has no words.", line_idx)
            continue

        report["word_count"] += len(words)
        try:
            line_start = float(line.get("start", words[0].get("start", 0.0)))
            line_end = float(line.get("end", words[-1].get("end", line_start)))
        except Exception:
            report["metrics"]["empty_lines"] += 1
            issue("invalid_line_time", "error", "Line start/end is not numeric.", line_idx)
            continue

        line_duration = max(0.0, line_end - line_start)
        report["metrics"]["max_line_duration"] = round(
            max(report["metrics"]["max_line_duration"], line_duration), 3
        )
        if line_duration > 9.0:
            report["metrics"]["long_lines"] += 1
            issue(
                "long_line",
                "warning",
                "Line duration is unusually long.",
                line_idx,
                duration=round(line_duration, 3),
            )

        if previous_line_end is not None and line_start < previous_line_end - 0.03:
            report["metrics"]["line_overlaps"] += 1
            issue(
                "line_overlap",
                "error",
                "Line starts before the previous line ends.",
                line_idx,
                overlap=round(previous_line_end - line_start, 3),
            )
        previous_line_end = max(previous_line_end or 0.0, line_end)

        if audio_duration and (line_start < -0.05 or line_end > float(audio_duration) + 0.5):
            report["metrics"]["out_of_bounds"] += 1
            issue(
                "line_out_of_bounds",
                "warning",
                "Line extends outside the audio duration.",
                line_idx,
                start=round(line_start, 3),
                end=round(line_end, 3),
            )

        previous_word_end = None
        for word_idx, word in enumerate(words):
            try:
                start = float(word.get("start", 0.0))
                end = float(word.get("end", start))
            except Exception:
                report["metrics"]["zero_or_tiny_words"] += 1
                issue("invalid_word_time", "error", "Word start/end is not numeric.", line_idx, word_idx)
                continue

            duration = end - start
            report["metrics"]["max_word_duration"] = round(
                max(report["metrics"]["max_word_duration"], max(0.0, duration)), 3
            )
            if duration <= 0.04:
                report["metrics"]["zero_or_tiny_words"] += 1
                issue(
                    "tiny_word",
                    "warning",
                    "Word duration is too short for reliable highlighting.",
                    line_idx,
                    word_idx,
                    duration=round(duration, 3),
                )
            elif duration > 2.8:
                report["metrics"]["long_words"] += 1
                issue(
                    "long_word",
                    "warning",
                    "Word duration is unusually long.",
                    line_idx,
                    word_idx,
                    duration=round(duration, 3),
                )

            if previous_word_end is not None:
                gap = start - previous_word_end
                report["metrics"]["max_internal_gap"] = round(
                    max(report["metrics"]["max_internal_gap"], max(0.0, gap)), 3
                )
                if start < previous_word_end - 0.02:
                    report["metrics"]["word_overlaps"] += 1
                    issue(
                        "word_overlap",
                        "error",
                        "Word starts before the previous word ends.",
                        line_idx,
                        word_idx,
                        overlap=round(previous_word_end - start, 3),
                    )
                elif gap > 2.5:
                    report["metrics"]["large_internal_gaps"] += 1
                    issue(
                        "large_internal_gap",
                        "warning",
                        "Large pause inside one lyric line.",
                        line_idx,
                        word_idx,
                        gap=round(gap, 3),
                    )
            previous_word_end = max(previous_word_end or 0.0, end)

    penalty = (
        report["metrics"]["line_overlaps"] * 0.16
        + report["metrics"]["word_overlaps"] * 0.10
        + report["metrics"]["zero_or_tiny_words"] * 0.025
        + report["metrics"]["long_words"] * 0.025
        + report["metrics"]["large_internal_gaps"] * 0.04
        + report["metrics"]["long_lines"] * 0.035
        + report["metrics"]["out_of_bounds"] * 0.04
        + report["metrics"]["empty_lines"] * 0.04
    )

    if text_match is not None:
        try:
            text_score = float(text_match.get("score", text_match))
        except Exception:
            text_score = None
        if text_score is not None:
            text_score = max(0.0, min(1.0, text_score))
            report["metrics"]["text_match_score"] = round(text_score, 3)
            report["text_match"] = text_match if isinstance(text_match, dict) else {"score": text_score}
            if text_score < 0.35:
                penalty += 0.55
                issue(
                    "text_mismatch",
                    "error",
                    "Lyrics text poorly matches recognized vocal.",
                    score=round(text_score, 3),
                )
            elif text_score < 0.55:
                penalty += 0.28
                issue(
                    "text_mismatch",
                    "warning",
                    "Lyrics text weakly matches recognized vocal.",
                    score=round(text_score, 3),
                )
            elif text_score < 0.72:
                penalty += 0.12
                issue(
                    "text_match_low",
                    "warning",
                    "Lyrics text partially matches recognized vocal.",
                    score=round(text_score, 3),
                )

    report["score"] = round(max(0.0, 1.0 - penalty), 3)
    if any(item["severity"] == "error" for item in report["issues"]):
        report["summary"] = "needs_repair"
    elif report["score"] < 0.82:
        report["summary"] = "suspicious"
    elif report["issues"]:
        report["summary"] = "minor_warnings"
    return report


def timestamped_whisper_probe_decision(timestamped_karaoke, whisper_words, close_avg_limit=0.35, close_max_limit=0.65, shift_spread_limit=0.45, max_shift=3.0):
    if not timestamped_karaoke or not whisper_words:
        return {"action": "whisper", "shift": 0.0, "reason": "нет данных для сравнения"}

    lrc_words = []
    for line in timestamped_karaoke:
        for word in line.get("words", []) or []:
            clean = clean_word(word.get("word", ""))
            if clean:
                lrc_words.append((clean, float(line.get("start", word.get("start", 0.0)))))
                break
        if len(lrc_words) >= 8:
            break

    whisper_probe = []
    for word in whisper_words[:45]:
        clean = clean_word(getattr(word, "word", "") or "")
        if clean and hasattr(word, "start"):
            try:
                whisper_probe.append((clean, float(word.start)))
            except Exception:
                pass
        if len(whisper_probe) >= 18:
            break

    if not lrc_words or not whisper_probe:
        return {"action": "whisper", "shift": 0.0, "reason": "нет первых слов для сравнения"}

    diffs = []
    signed_diffs = []
    for lrc_word, lrc_start in lrc_words:
        matched = False
        best_signed = None
        best_abs = None
        for whisper_word, whisper_start in whisper_probe:
            if whisper_start < lrc_start - 4.0:
                continue
            if whisper_start > lrc_start + 4.0:
                break
            if fuzzy_word_match(lrc_word, whisper_word):
                signed = lrc_start - whisper_start
                diff = abs(signed)
                if best_abs is None or diff < best_abs:
                    best_abs = diff
                    best_signed = signed
        if best_signed is not None:
            signed_diffs.append(best_signed)
            diffs.append(best_abs)
            matched = True
        else:
            for whisper_word, whisper_start in whisper_probe:
                if whisper_start < lrc_start - 7.0:
                    continue
                if whisper_start > lrc_start + 7.0:
                    break
                if fuzzy_word_match(lrc_word, whisper_word):
                    signed = lrc_start - whisper_start
                    signed_diffs.append(signed)
                    diffs.append(abs(signed))
                    matched = True
                    break
        if len(diffs) >= 5:
            break
        if not matched and diffs:
            continue

    if len(diffs) < 3:
        return {"action": "whisper", "shift": 0.0, "reason": "первые слова не совпали по тексту"}

    avg_diff = sum(diffs) / len(diffs)
    max_seen = max(diffs)
    avg_signed = sum(signed_diffs) / len(signed_diffs)
    direction = "LRC раньше Whisper" if avg_signed < -0.05 else "LRC позже Whisper" if avg_signed > 0.05 else "без сдвига"
    base_reason = f"совпало {len(diffs)} первых строк, среднее {avg_diff:.2f}с, максимум {max_seen:.2f}с, сдвиг {avg_signed:+.2f}с ({direction})"

    if avg_diff <= close_avg_limit and max_seen <= close_max_limit:
        return {"action": "lrc", "shift": 0.0, "reason": base_reason}
    if avg_diff <= 1.6 and max_seen <= 3.0:
        return {"action": "lrc_confined", "shift": 0.0, "reason": f"{base_reason}, LRC достаточно близок для выравнивания слов внутри строк"}

    sorted_signed = sorted(signed_diffs)
    median_signed = sorted_signed[len(sorted_signed) // 2]
    spread = max(abs(signed - median_signed) for signed in signed_diffs)
    if abs(median_signed) <= max_shift and spread <= shift_spread_limit:
        return {"action": "shift_lrc", "shift": round(-median_signed, 3), "reason": f"{base_reason}, стабильный сдвиг {median_signed:+.2f}с"}

    return {"action": "whisper", "shift": 0.0, "reason": f"{base_reason}, сдвиг нестабилен"}


def timestamped_matches_whisper_probe(timestamped_karaoke, whisper_words, avg_limit=0.35, max_limit=0.65):
    decision = timestamped_whisper_probe_decision(timestamped_karaoke, whisper_words, close_avg_limit=avg_limit, close_max_limit=max_limit)
    return decision["action"] == "lrc", decision["reason"]


def clamp_word_timing(start, end, line_start, line_end):
    start = max(line_start, min(line_end - 0.05, float(start)))
    end = max(start + 0.05, min(line_end, float(end)))
    return round(start, 3), round(end, 3)


def distribute_words_between_anchors(words, start_time, end_time):
    if not words:
        return []
    start_time = float(start_time)
    end_time = max(start_time + 0.08, float(end_time))
    total_chars = sum(max(len(clean_word(word.get("word", ""))), 1) for word in words) or len(words)
    span = end_time - start_time
    cursor = start_time
    distributed = []
    for idx, word in enumerate(words):
        updated = dict(word)
        share = max(len(clean_word(word.get("word", ""))), 1) / total_chars
        slot = span * share
        word_end = end_time if idx == len(words) - 1 else min(end_time, cursor + max(0.08, slot * 0.88))
        updated["start"] = round(cursor, 3)
        updated["end"] = round(max(cursor + 0.05, word_end), 3)
        distributed.append(updated)
        cursor = min(end_time, cursor + max(0.08, slot))
    return distributed


def refine_timestamped_words_with_whisper(timestamped_karaoke, whisper_words, whisper_time_offset=0.0, line_left_pad=0.0):
    if not timestamped_karaoke or not whisper_words:
        return timestamped_karaoke, {"refined_lines": 0, "matched_words": 0, "total_words": 0}

    refined = []
    whisper_cursor = 0
    refined_lines = 0
    matched_total = 0
    total_words = 0
    whisper_data = []

    for word in whisper_words:
        clean = clean_word(getattr(word, "word", "") or "")
        if not clean or not hasattr(word, "start") or not hasattr(word, "end"):
            continue
        try:
            start = float(word.start) + float(whisper_time_offset or 0.0)
            end = float(word.end) + float(whisper_time_offset or 0.0)
        except Exception:
            continue
        if end > start:
            whisper_data.append({"clean": clean, "start": start, "end": end})

    for line_idx, line in enumerate(timestamped_karaoke):
        new_line = dict(line)
        base_words = [dict(word) for word in line.get("words", []) or []]
        if not base_words:
            refined.append(new_line)
            continue

        line_start = float(new_line.get("start", base_words[0].get("start", 0.0)) or 0.0)
        if line_idx + 1 < len(timestamped_karaoke):
            next_start = float(timestamped_karaoke[line_idx + 1].get("start", line.get("end", line_start + 1.0)) or line_start + 1.0)
            line_end = max(line_start + 0.2, next_start - 0.05)
        else:
            line_end = max(line_start + 0.2, float(new_line.get("end", line_start + 1.0) or line_start + 1.0))

        align_start = max(0.0, line_start - float(line_left_pad or 0.0))
        if line_idx > 0:
            prev_start = float(timestamped_karaoke[line_idx - 1].get("start", 0.0) or 0.0)
            align_start = max(align_start, prev_start + 0.05)

        window_start = max(0.0, align_start - 0.45)
        window_end = line_end + 0.45
        while whisper_cursor < len(whisper_data) and whisper_data[whisper_cursor]["end"] < window_start:
            whisper_cursor += 1

        search_start = whisper_cursor
        matched_indices = {}
        used_whisper = set()
        for word_idx, base_word in enumerate(base_words):
            target = clean_word(base_word.get("word", "") or "")
            if not target:
                continue
            try:
                expected_start = float(base_word.get("start", line_start) or line_start)
            except Exception:
                expected_start = line_start
            max_late_drift = max(1.75, (line_end - line_start) * 0.45)
            max_early_drift = float(line_left_pad or 0.0) + 0.75
            best_idx = None
            for probe_idx in range(search_start, min(search_start + 24, len(whisper_data))):
                if probe_idx in used_whisper:
                    continue
                candidate = whisper_data[probe_idx]
                if candidate["start"] > window_end:
                    break
                if candidate["start"] > expected_start + max_late_drift:
                    continue
                if candidate["end"] < expected_start - max_early_drift:
                    continue
                if fuzzy_word_match(target, candidate["clean"]):
                    best_idx = probe_idx
                    break
            if best_idx is not None:
                matched_indices[word_idx] = best_idx
                used_whisper.add(best_idx)
                search_start = best_idx + 1

        matched_count = len(matched_indices)
        total_words += len(base_words)
        matched_total += matched_count
        min_matches = 1 if len(base_words) <= 2 else max(2, int(len(base_words) * 0.45))
        if matched_count < min_matches:
            new_line["words"] = base_words
            refined.append(new_line)
            continue

        new_words = []
        for word_idx, base_word in enumerate(base_words):
            updated = dict(base_word)
            if word_idx in matched_indices:
                match = whisper_data[matched_indices[word_idx]]
                updated["start"], updated["end"] = clamp_word_timing(match["start"], match["end"], align_start, line_end)
            else:
                updated["start"], updated["end"] = clamp_word_timing(base_word.get("start", line_start), base_word.get("end", line_end), line_start, line_end)
            new_words.append(updated)

        matched_word_indices = sorted(matched_indices)
        if matched_word_indices:
            first_matched_word = matched_word_indices[0]
            first_match_start = float(new_words[first_matched_word]["start"])
            if first_matched_word == 0:
                if first_match_start > line_start + 0.18 or first_match_start < align_start:
                    new_words[0]["start"] = round(line_start, 3)
                    new_words[0]["end"] = round(max(line_start + 0.08, float(new_words[0]["end"])), 3)
            elif first_match_start > line_start + 0.08:
                lead_end = max(line_start + 0.08, min(first_match_start - 0.03, line_end))
                new_words[:first_matched_word] = distribute_words_between_anchors(new_words[:first_matched_word], line_start, lead_end)
        elif new_words:
            new_words = distribute_words_between_anchors(new_words, line_start, line_end)

        if new_words and float(new_words[0]["start"]) != line_start and float(new_words[0]["start"]) > line_start:
            new_words[0]["start"] = round(line_start, 3)
            new_words[0]["end"] = round(max(line_start + 0.08, float(new_words[0]["end"])), 3)

        for word_idx in range(1, len(new_words)):
            prev = new_words[word_idx - 1]
            curr = new_words[word_idx]
            if curr["start"] < prev["end"]:
                curr["start"] = round(prev["end"] + 0.01, 3)
            if curr["end"] <= curr["start"]:
                curr["end"] = round(min(line_end, curr["start"] + 0.12), 3)

        new_line["words"] = new_words
        new_line["start"] = round(min(line_start, float(new_words[0]["start"])), 3)
        new_line["end"] = round(max(line_start + 0.05, min(line_end, float(new_words[-1]["end"]))), 3)
        refined_lines += 1
        refined.append(new_line)
        if used_whisper:
            whisper_cursor = max(whisper_cursor, max(used_whisper) + 1)

    return refined, {"refined_lines": refined_lines, "matched_words": matched_total, "total_words": total_words}


def align_timestamped_lrc_words(model, audio_path, timestamped_karaoke, language):
    line_left_pad = 1.5
    segments = []
    segment_line_indices = []
    for line_idx, line in enumerate(timestamped_karaoke or []):
        base_words = line.get("words", []) or []
        text = (line.get("text", "") or "").strip()
        if not text or not base_words:
            continue
        line_start = float(line.get("start", base_words[0].get("start", 0.0)) or 0.0)
        align_start = max(0.0, line_start - line_left_pad)
        if line_idx > 0:
            prev_start = float(timestamped_karaoke[line_idx - 1].get("start", 0.0) or 0.0)
            align_start = max(align_start, prev_start + 0.05)
        if line_idx + 1 < len(timestamped_karaoke):
            next_start = float(timestamped_karaoke[line_idx + 1].get("start", line.get("end", line_start + 1.0)) or line_start + 1.0)
            line_end = max(line_start + 0.2, next_start - 0.05)
        else:
            line_end = max(line_start + 0.2, float(line.get("end", line_start + 1.0) or line_start + 1.0))
        segments.append({"start": round(align_start, 3), "end": round(line_end, 3), "text": text})
        segment_line_indices.append(line_idx)

    if not segments:
        return timestamped_karaoke, {"refined_lines": 0, "matched_words": 0, "total_words": 0}

    result = model.align_words(audio_path, segments, language=language, inplace=False, verbose=None, regroup=False, suppress_silence=False)
    aligned_segments = list(getattr(result, "segments", []) or [])
    refined = [dict(line) for line in timestamped_karaoke]
    refined_lines = 0
    matched_total = 0
    total_words = 0

    for segment_idx, line_idx in enumerate(segment_line_indices):
        if segment_idx >= len(aligned_segments):
            break
        line = dict(refined[line_idx])
        base_words = [dict(word) for word in line.get("words", []) or []]
        total_words += len(base_words)
        aligned_words = list(getattr(aligned_segments[segment_idx], "words", []) or [])
        if not aligned_words:
            continue

        line_start = float(line.get("start", base_words[0].get("start", 0.0)) or 0.0)
        align_start = max(0.0, line_start - line_left_pad)
        if line_idx > 0:
            prev_start = float(timestamped_karaoke[line_idx - 1].get("start", 0.0) or 0.0)
            align_start = max(align_start, prev_start + 0.05)
        if line_idx + 1 < len(timestamped_karaoke):
            next_start = float(timestamped_karaoke[line_idx + 1].get("start", line.get("end", line_start + 1.0)) or line_start + 1.0)
            line_end = max(line_start + 0.2, next_start - 0.05)
        else:
            line_end = max(line_start + 0.2, float(line.get("end", line_start + 1.0) or line_start + 1.0))

        new_words = []
        used_aligned = set()
        search_from = 0
        line_matched = 0
        for base_word in base_words:
            updated = dict(base_word)
            target = clean_word(base_word.get("word", "") or "")
            match_idx = None
            try:
                expected_start = float(base_word.get("start", line_start) or line_start)
            except Exception:
                expected_start = line_start
            max_late_drift = max(1.75, (line_end - line_start) * 0.45)
            max_early_drift = line_left_pad + 0.75
            if target:
                for idx in range(search_from, len(aligned_words)):
                    if idx in used_aligned:
                        continue
                    candidate = aligned_words[idx]
                    try:
                        candidate_start = float(candidate.start)
                        candidate_end = float(candidate.end)
                    except Exception:
                        continue
                    if candidate_start > expected_start + max_late_drift:
                        continue
                    if candidate_end < expected_start - max_early_drift:
                        continue
                    if fuzzy_word_match(target, clean_word(getattr(candidate, "word", "") or "")):
                        match_idx = idx
                        break
            if match_idx is not None:
                candidate = aligned_words[match_idx]
                updated["start"], updated["end"] = clamp_word_timing(candidate.start, candidate.end, align_start, line_end)
                used_aligned.add(match_idx)
                search_from = match_idx + 1
                matched_total += 1
                line_matched += 1
            new_words.append(updated)

        if line_matched >= max(1, min(len(base_words), len(aligned_words)) // 2):
            for word_idx in range(1, len(new_words)):
                prev = new_words[word_idx - 1]
                curr = new_words[word_idx]
                if curr["start"] < prev["end"]:
                    curr["start"] = round(prev["end"] + 0.01, 3)
                if curr["end"] <= curr["start"]:
                    curr["end"] = round(min(line_end, curr["start"] + 0.12), 3)
            line["words"] = new_words
            line["start"] = round(min(line_start, float(new_words[0]["start"])), 3)
            line["end"] = round(max(line_start + 0.05, min(line_end, float(new_words[-1]["end"]))), 3)
            refined[line_idx] = line
            refined_lines += 1

    return refined, {"refined_lines": refined_lines, "matched_words": matched_total, "total_words": total_words}
