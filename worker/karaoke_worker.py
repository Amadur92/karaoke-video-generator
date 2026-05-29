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
import webbrowser

# ----------------- MONKEY PATCH ДЛЯ СОВМЕСТИМОСТИ FLASK И WERKZEUG -----------------
# Werkzeug >= 3.0.0 удалил url_quote, но старые версии Flask используют его.
# Этот патч восстанавливает url_quote, обеспечивая 100% стабильность запуска Flask!
import urllib.parse
try:
    import werkzeug.urls
    if not hasattr(werkzeug.urls, 'url_quote'):
        werkzeug.urls.url_quote = urllib.parse.quote
except Exception:
    pass

from flask import Flask, request, jsonify, render_template_string, send_file

# ----------------- НАСТРОЙКА FLASK -----------------
app = Flask(__name__)
ssl._create_default_https_context = ssl._create_unverified_context

if getattr(sys, "frozen", False):
    BASE_DIR = os.path.dirname(os.path.abspath(sys.executable))
    RESOURCE_DIR = getattr(sys, "_MEIPASS", BASE_DIR)
else:
    BASE_DIR = os.path.dirname(os.path.abspath(__file__))
    RESOURCE_DIR = BASE_DIR

UPLOAD_FOLDER = os.environ.get("KARAOKE_UPLOAD_DIR", os.path.join(BASE_DIR, "web_uploads"))
EXPORT_FOLDER = os.environ.get("KARAOKE_EXPORT_DIR", os.path.join(BASE_DIR, "web_exports"))
os.makedirs(UPLOAD_FOLDER, exist_ok=True)
os.makedirs(EXPORT_FOLDER, exist_ok=True)

app.config['UPLOAD_FOLDER'] = UPLOAD_FOLDER

# Глобальный словарь для фоновых задач
# Формат: { job_id: { "progress": float, "status": str, "done": bool, "error": str, "file": str } }
jobs = {}

# ----------------- ВСПОМОГАТЕЛЬНЫЕ ФУНКЦИИ -----------------
def replace_special_spaces(text):
    if not isinstance(text, str):
        return text
    # Заменяем все виды Unicode-пробелов на обычный пробел \u0020
    special_spaces = [
        '\u2000', '\u2001', '\u2002', '\u2003', '\u2004', '\u2005', 
        '\u2006', '\u2007', '\u2008', '\u2009', '\u200a', '\u200b', 
        '\u202f', '\u205f', '\u3000', '\u00a0'
    ]
    for sp in special_spaces:
        text = text.replace(sp, ' ')
    # Также убираем нулевой ширины пробелы и другие мусорные символы
    text = text.replace('\u200b', '').replace('\u200e', '').replace('\u200f', '')
    return text

def clean_word(w):
    return re.sub(r'[^\w\s]', '', replace_special_spaces(w).strip().lower())

def infer_lyrics_language(text):
    return 'ru' if re.search(r'[А-Яа-яЁё]', text or '') else 'en'

def get_system_font(font_name='montserrat', bold=False):
    font_name = font_name.lower().strip()
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
        font_file = "Montserrat-Bold.ttf" if bold else "Montserrat-Regular.ttf"
        for search_dir in font_search_dirs:
            montserrat_path = os.path.join(search_dir, font_file)
            if os.path.exists(montserrat_path):
                return montserrat_path
            
    # 2. Arial
    if font_name == 'arial':
        if sys.platform == "win32":
            path = os.path.join(os.environ.get("SystemRoot", "C:\\Windows"), "Fonts", "arialbd.ttf" if bold else "arial.ttf")
            if os.path.exists(path): return path
        elif sys.platform == "darwin":
            paths = [
                "/Library/Fonts/Arial Bold.ttf" if bold else "/Library/Fonts/Arial.ttf",
                "/System/Library/Fonts/Supplemental/Arial Bold.ttf" if bold else "/System/Library/Fonts/Supplemental/Arial.ttf"
            ]
            for p in paths:
                if os.path.exists(p): return p
                
    # 3. Helvetica
    if font_name == 'helvetica':
        if sys.platform == "darwin":
            path = "/System/Library/Fonts/Helvetica.ttc"
            if os.path.exists(path): return path
        return get_system_font(font_name='arial', bold=bold)
        
    # 4. Georgia
    if font_name == 'georgia':
        if sys.platform == "win32":
            path = os.path.join(os.environ.get("SystemRoot", "C:\\Windows"), "Fonts", "georgiab.ttf" if bold else "georgia.ttf")
            if os.path.exists(path): return path
        elif sys.platform == "darwin":
            paths = [
                "/Library/Fonts/Georgia Bold.ttf" if bold else "/Library/Fonts/Georgia.ttf",
                "/System/Library/Fonts/Supplemental/Georgia Bold.ttf" if bold else "/System/Library/Fonts/Supplemental/Georgia.ttf"
            ]
            for p in paths:
                if os.path.exists(p): return p
                
    # Default fallback to Montserrat
    font_file = "Montserrat-Bold.ttf" if bold else "Montserrat-Regular.ttf"
    for search_dir in font_search_dirs:
        montserrat_path = os.path.join(search_dir, font_file)
        if os.path.exists(montserrat_path):
            return montserrat_path
        
    # Final generic fallback
    if sys.platform == "win32":
        return os.path.join(os.environ.get("SystemRoot", "C:\\Windows"), "Fonts", "arialbd.ttf" if bold else "arial.ttf")
    elif sys.platform == "darwin":
        return "/Library/Fonts/Arial Bold.ttf" if bold else "/Library/Fonts/Arial.ttf"
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
    if model_name not in loaded_models:
        if status_callback:
            status_callback(f"Загрузка ИИ-модели Whisper '{model_name}' в память...")
        loaded_models[model_name] = stable_whisper.load_model(model_name)
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
            check=True
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
    subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)

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
            result = model.transcribe(scan_path, language=language)

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

# ----------------- РЕНДЕРИНГ (ФОНОВЫЙ ПОТОК) -----------------
def generate_karaoke_thread(job_id, audio_path, artist, title, lyrics, model_name, quality='medium', font_family='montserrat', color_active='#000000', color_inactive='#B4B9C3', color_bg='#FFFFFF', audio_delay=0.0, vocal_start=0.0, auto_vocal_start=False):
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
        lyrics = replace_special_spaces(lyrics)
        artist = replace_special_spaces(artist)
        title = replace_special_spaces(title)
        lyrics_language = infer_lyrics_language(lyrics)
        
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
                    lyrics_text=lyrics,
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

        model = loaded_models[model_name]
        jobs[job_id]["progress"] = 0.2

        jobs[job_id]["status"] = "Запуск пословного выравнивания ИИ по аудио..."
        result = model.align(align_audio_path, lyrics, language=lyrics_language, original_split=True)
        jobs[job_id]["progress"] = 0.4

        # Очищаем паузы и тишину: если между словами пауза, сжимаем границы слов, чтобы они не горели заранее
        jobs[job_id]["status"] = "Постобработка таймингов ИИ (вырезание пауз тишины)..."
        try:
            result.regroup(by_gap=True)
        except Exception:
            pass

        # Извлечение ВСЕХ слов из Whisper (включая битые с нулевой длительностью)
        whisper_words = []
        for segment in result.segments:
            if hasattr(segment, 'words') and segment.words:
                for w in segment.words:
                    whisper_words.append(w)

        # ДАМП СЫРЫХ ДАННЫХ WHISPER для отладки
        try:
            raw_dump = []
            for seg_idx, segment in enumerate(result.segments):
                seg_data = {
                    "segment_idx": seg_idx,
                    "start": round(segment.start, 3) if hasattr(segment, 'start') else None,
                    "end": round(segment.end, 3) if hasattr(segment, 'end') else None,
                    "text": segment.text if hasattr(segment, 'text') else "",
                    "words": []
                }
                if hasattr(segment, 'words') and segment.words:
                    for w in segment.words:
                        seg_data["words"].append({
                            "word": w.word if hasattr(w, 'word') else "?",
                            "start": round(w.start, 3) if hasattr(w, 'start') else -1,
                            "end": round(w.end, 3) if hasattr(w, 'end') else -1,
                        })
                raw_dump.append(seg_data)
            dump_path = os.path.join(EXPORT_FOLDER, f"{job_id}_whisper_raw.json")
            with open(dump_path, 'w', encoding='utf-8') as f:
                json.dump(raw_dump, f, ensure_ascii=False, indent=2)
        except Exception:
            pass

        # ИНТЕРПОЛЯЦИЯ БИТЫХ СЛОВ (перевернутые, нулевые или слишком длинные тайминги)
        # Whisper иногда сжимает целые припевы в одну точку времени.
        # Находим группы битых слов и равномерно распределяем их между валидными якорями.
        jobs[job_id]["status"] = "Интерполяция таймингов для сбойных сегментов ИИ..."
        
        n_total = len(whisper_words)
        def word_duration_limit(word):
            clean = clean_word(getattr(word, 'word', '') or '')
            # Певческие хвосты бывают длинными, но одно слово на полкуплета почти всегда сбой.
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
        
        # Получаем длительность аудио для крайнего случая
        audio_duration = 120.0
        try:
            cmd_dur = ['ffprobe', '-v', 'error', '-show_entries', 'format=duration',
                       '-of', 'default=noprint_wrappers=1:nokey=1', audio_path]
            res_dur = subprocess.run(cmd_dur, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            audio_duration = float(res_dur.stdout.strip())
        except Exception:
            pass
        
        interpolated_count = 0
        i = 0
        while i < n_total:
            if is_valid[i]:
                i += 1
                continue
            
            # Нашли начало группы битых слов
            group_start = i
            while i < n_total and not is_valid[i]:
                i += 1
            group_end = i  # не включительно
            num_broken = group_end - group_start
            
            # Левый якорь: конец последнего валидного слова перед группой.
            # Для группы в самом начале нельзя слепо брать auto vocal_start:
            # если детектор промахнулся поздно, он сдвинет первую строку поверх второй.
            left_time = 0.0
            for j in range(group_start - 1, -1, -1):
                if is_valid[j]:
                    left_time = whisper_words[j].end
                    break

            # Правый якорь: начало первого валидного слова после группы
            right_time = None
            for j in range(group_end, n_total):
                if is_valid[j]:
                    right_time = whisper_words[j].start
                    break
            if group_start == 0 and right_time is not None:
                total_chars_before = sum(max(len(clean_word(whisper_words[group_start + k].word)), 1) for k in range(num_broken))
                estimated_span = min(8.0, max(0.45 * num_broken, total_chars_before * 0.16))
                left_time = max(0.0, right_time - estimated_span)
            if right_time is None:
                # Если справа нет надежного якоря, не растягиваем хвост до конца трека.
                total_chars = sum(max(len(clean_word(whisper_words[group_start + k].word)), 1) for k in range(num_broken))
                right_time = left_time + min(8.0, max(0.45 * num_broken, total_chars * 0.16))
            
            # Пропорциональное распределение времени по длине слов
            max_span = max(0.45 * num_broken, min(8.0, num_broken * 0.9))
            span = min(max(right_time - left_time, 0.2), max_span)
            
            # Считаем суммарную длину всех битых слов в группе
            total_chars = sum(max(len(clean_word(whisper_words[group_start + k].word)), 1) for k in range(num_broken))
            if total_chars == 0:
                total_chars = num_broken
                
            current_time = left_time
            for k in range(num_broken):
                idx = group_start + k
                word_clean = clean_word(whisper_words[idx].word)
                char_len = max(len(word_clean), 1) if word_clean else 1
                
                # Доля времени для этого слова
                word_share = char_len / total_chars
                w_dur = span * word_share
                
                # Ограничиваем разумными пределами (от 120мс до 2.0с)
                w_dur = min(max(w_dur, 0.12), 2.0)
                
                new_start = current_time
                new_end = new_start + w_dur * 0.85  # 15% зазор
                
                whisper_words[idx].start = round(new_start, 3)
                whisper_words[idx].end = round(new_end, 3)
                
                current_time += w_dur
                
            interpolated_count += num_broken
        
        # Гарантируем монотонность
        for i in range(1, n_total):
            if whisper_words[i].start < whisper_words[i-1].end:
                whisper_words[i].start = round(whisper_words[i-1].end + 0.01, 3)
            max_end = whisper_words[i].start + word_duration_limit(whisper_words[i])
            if whisper_words[i].end <= whisper_words[i].start:
                whisper_words[i].end = round(whisper_words[i].start + 0.15, 3)
            elif whisper_words[i].end > max_end:
                whisper_words[i].end = round(max_end, 3)
        
        num_whisper_words = len(whisper_words)
        jobs[job_id]["status"] = f"Обработано {num_whisper_words} слов (интерполировано: {interpolated_count})"

        # Сопоставляем выровненные ИИ слова со строгой структурой исходного текста lyrics
        raw_lines = lyrics.split('\n')
        lyrics_karaoke = []
        whisper_idx = 0
        LOOKAHEAD = 5  # Узкое окно — защита от перескока на повторы (припевы)

        for line in raw_lines:
            line_cleaned = line.strip()
            if not line_cleaned:
                continue
            
            orig_words = line_cleaned.split()
            line_words_timing = []
            
            for orig_w in orig_words:
                orig_w_clean = clean_word(orig_w)
                if not orig_w_clean:
                    continue
                
                # Защита от перепрыгивания через гигантские инструментальные паузы (instrumental gaps):
                # Если у нас уже есть слова в этой строке, и следующее слово в Whisper
                # находится более чем на 3.5 секунды позже (или общая длина строки превысит 8.5 сек),
                # мы прекращаем считывать слова из Whisper и синтезируем оставшуюся часть строки локально.
                if line_words_timing:
                    last_end = line_words_timing[-1]["end"]
                    gap_too_large = False
                    
                    if whisper_idx < num_whisper_words:
                        next_w = whisper_words[whisper_idx]
                        if next_w.start - last_end > 3.5:
                            gap_too_large = True
                        elif next_w.start - line_words_timing[0]["start"] > 8.5:
                            gap_too_large = True
                            
                    if gap_too_large:
                        current_t = last_end
                        try:
                            start_idx = orig_words.index(orig_w)
                        except ValueError:
                            start_idx = 0
                            
                        for rem_w in orig_words[start_idx:]:
                            line_words_timing.append({
                                "word": rem_w,
                                "start": round(current_t + 0.05, 3),
                                "end": round(current_t + 0.45, 3)
                            })
                            current_t += 0.45
                        break

                
                # Приоритет 1: точное совпадение на текущей позиции (без поиска)
                matched = False
                if whisper_idx < num_whisper_words:
                    w_word_clean = clean_word(whisper_words[whisper_idx].word)
                    if orig_w_clean == w_word_clean:
                        line_words_timing.append({
                            "word": orig_w,
                            "start": round(whisper_words[whisper_idx].start, 3),
                            "end": round(whisper_words[whisper_idx].end, 3)
                        })
                        whisper_idx += 1
                        matched = True
                
                # Приоритет 2: поиск в узком окне (до 5 слов вперёд)
                if not matched:
                    best_k = -1
                    for k in range(whisper_idx, min(whisper_idx + LOOKAHEAD, num_whisper_words)):
                        w_word_clean = clean_word(whisper_words[k].word)
                        if orig_w_clean == w_word_clean or orig_w_clean in w_word_clean or w_word_clean in orig_w_clean:
                            best_k = k
                            break
                    
                    if best_k >= 0:
                        line_words_timing.append({
                            "word": orig_w,
                            "start": round(whisper_words[best_k].start, 3),
                            "end": round(whisper_words[best_k].end, 3)
                        })
                        whisper_idx = best_k + 1
                        matched = True
                
                # Фолбэк: берём следующее слово как есть (строго +1)
                if not matched and whisper_idx < num_whisper_words:
                    line_words_timing.append({
                        "word": orig_w,
                        "start": round(whisper_words[whisper_idx].start, 3),
                        "end": round(whisper_words[whisper_idx].end, 3)
                    })
                    whisper_idx += 1
            
            if line_words_timing:
                lyrics_karaoke.append({
                    "text": line_cleaned,
                    "start": line_words_timing[0]["start"],
                    "end": line_words_timing[-1]["end"],
                    "words": line_words_timing
                })

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

        # ДАМП ФИНАЛЬНЫХ ТАЙМИНГОВ для отладки
        try:
            final_dump_path = os.path.join(EXPORT_FOLDER, f"{job_id}_timings_final.json")
            with open(final_dump_path, 'w', encoding='utf-8') as f:
                json.dump(lyrics_karaoke, f, ensure_ascii=False, indent=2)
        except Exception:
            pass

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
            res_dur = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=True)
            duration = float(res_dur.stdout.strip())
        except Exception:
            pass

        total_frames = int(duration * fps)
        
        font_path_reg = get_system_font(font_name=font_family, bold=False)
        font_path_bold = get_system_font(font_name=font_family, bold=True)

        clean_filename = f"{artist} - {title} (karaoke).mp4".replace("/", "_").replace("\\", "_")
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
        
        process = subprocess.Popen(ffmpeg_cmd, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

        current_scroll_y = 0.0

        font_cache = {}
        def get_font_at_size(bold, size):
            key = (bold, size)
            if key not in font_cache:
                path = font_path_bold if bold else font_path_reg
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

        font_max = get_font_at_size(bold=True, size=font_size_max)
        word_pad = int(20 * size_scale)
        word_active_offset = int(10 * size_scale)
        line_pad_x = int(40 * size_scale)
        line_text_x = int(20 * size_scale)

        jobs[job_id]["status"] = "Подготовка кеша строк для быстрого рендера..."
        line_render_cache = []
        for line_data in lyrics_karaoke:
            words = line_data["words"]
            widths, space_w = get_word_widths(words, font_max)
            total_w = sum(widths) + space_w * max(0, len(words) - 1)
            line_img_w = max(1, total_w + line_pad_x)
            inactive_img = Image.new("RGBA", (line_img_w, line_img_h), (0, 0, 0, 0))
            inactive_draw = ImageDraw.Draw(inactive_img)

            x_draw = line_text_x
            word_layers = []
            for w_idx, w_data in enumerate(words):
                word = w_data["word"]
                word_w = widths[w_idx]
                inactive_draw.text((x_draw, y_draw), word, fill=rgba_inactive, font=font_max)

                active_word_img = Image.new("RGBA", (word_w + word_pad, line_img_h), (0, 0, 0, 0))
                active_word_draw = ImageDraw.Draw(active_word_img)
                active_word_draw.text((word_active_offset, y_draw), word, fill=rgba_active, font=font_max)
                word_layers.append({
                    "start": w_data["start"],
                    "end": w_data["end"],
                    "paste_x": x_draw - word_active_offset,
                    "image": active_word_img,
                    "width": active_word_img.width,
                })
                x_draw += word_w + space_w

            line_render_cache.append({
                "inactive": inactive_img,
                "word_layers": word_layers,
                "width": line_img_w,
                "height": line_img_h,
            })

        # Моменты, когда скролл переключается на следующую строку.
        # В паузах берем ту же 40% точку, что и прежний покадровый алгоритм.
        transition_times = []
        for idx, line_data in enumerate(lyrics_karaoke):
            if idx == 0:
                transition_times.append(float("-inf"))
            else:
                prev_line = lyrics_karaoke[idx - 1]
                if line_data["start"] > prev_line["end"]:
                    transition_times.append(prev_line["end"] + (line_data["start"] - prev_line["end"]) * 0.4)
                else:
                    transition_times.append(line_data["start"])

        for frame_idx in range(total_frames):
            t = frame_idx / fps - audio_delay
            
            # Задний фон пользователя
            image = Image.new('RGBA', (width, height), rgba_bg)
            
            # Интеллектуальный алгоритм превентивного скроллинга (Anticipatory Scrolling)
            active_line_idx = 0
            if transition_times:
                active_line_idx = max(0, min(len(lyrics_karaoke) - 1, bisect.bisect_right(transition_times, t) - 1))

            target_scroll_y = active_line_idx * line_spacing
            current_scroll_y += (target_scroll_y - current_scroll_y) * 0.15
            
            for idx, line_data in enumerate(lyrics_karaoke):
                line_y = y_center + (idx * line_spacing) - current_scroll_y
                
                if line_y < y_center - line_y_cutoff or line_y > y_center + line_y_cutoff:
                    continue
                    
                dist_from_center = abs(line_y - y_center)
                weight = max(0.0, min(1.0, 1.0 - (dist_from_center / line_spacing)))
                
                is_active = (idx == active_line_idx)
                cached_line = line_render_cache[idx]

                if is_active:
                    line_img = cached_line["inactive"].copy()
                    for layer in cached_line["word_layers"]:
                        w_start = layer["start"]
                        w_end = layer["end"]
                        if t < w_start:
                            continue
                        elif t > w_end:
                            line_img.paste(layer["image"], (layer["paste_x"], 0), layer["image"])
                        else:
                            # Плавный цветной накат
                            progress = max(0.0, min(1.0, (t - w_start) / max(0.001, w_end - w_start)))
                            fill_w = int(layer["width"] * progress)
                            if fill_w > 0:
                                filled_part = layer["image"].crop((0, 0, fill_w, line_img_h))
                                line_img.paste(filled_part, (layer["paste_x"], 0), filled_part)
                else:
                    line_img = cached_line["inactive"]
                
                # Масштабируем холст строки методом субпиксельной интерполяции BILINEAR
                scale = (font_size_min + (font_size_max - font_size_min) * weight) / font_size_max
                new_w = max(1, int(cached_line["width"] * scale))
                new_h = max(1, int(cached_line["height"] * scale))
                    
                resized_img = line_img.resize((new_w, new_h), resampling_filter)
                
                # Применяем плавное изменение прозрачности в зависимости от положения на экране
                opacity = max(0.0, min(1.0, 1.0 - (dist_from_center / dist_cutoff)))
                if not is_active:
                    opacity *= 0.5
                
                if opacity < 1.0:
                    alpha = resized_img.getchannel('A')
                    new_alpha = alpha.point(lambda p: int(p * opacity))
                    resized_img.putalpha(new_alpha)
                
                # Вычисляем субпиксельные координаты для точной центральной вставки без дрожания
                x_paste = width // 2 - new_w // 2
                y_center_in_resized = y_text_center * scale
                y_paste = int(line_y - y_center_in_resized)
                
                image.paste(resized_img, (x_paste, y_paste), resized_img)
                    
            rgb_image = image.convert('RGB')
            frame_bytes = rgb_image.tobytes()
            process.stdin.write(frame_bytes)
            
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

# ----------------- FLASK МАРШРУТЫ И API -----------------

@app.route('/')
def index():
    # Роскошный HTML5/CSS/JS интерфейс
    return render_template_string("""
<!DOCTYPE html>
<html lang="ru">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Караоке-Видео Генератор (Word-Level)</title>
    <link href="https://fonts.googleapis.com/css2?family=Outfit:wght@300;400;600;700&display=swap" rel="stylesheet">
    <style>
        * {
            box-sizing: border-box;
            margin: 0;
            padding: 0;
        }
        body {
            background-color: #0b0f19;
            color: #f8fafc;
            font-family: 'Outfit', sans-serif;
            overflow-x: hidden;
            min-height: 100vh;
            position: relative;
        }
        /* Анимированные неоновые сферы */
        .blob {
            position: absolute;
            width: 600px;
            height: 600px;
            border-radius: 50%;
            filter: blur(160px);
            z-index: -1;
            opacity: 0.12;
            animation: float 25s infinite alternate;
        }
        .blob-cyan {
            background: #06b6d4;
            top: -150px;
            left: -150px;
        }
        .blob-magenta {
            background: #d946ef;
            bottom: -150px;
            right: -150px;
            animation-delay: -12s;
        }
        @keyframes float {
            0% { transform: translate(0, 0) scale(1); }
            100% { transform: translate(120px, 80px) scale(1.15); }
        }

        .container {
            max-width: 1200px;
            margin: 0 auto;
            padding: 40px 20px;
        }

        header {
            margin-bottom: 40px;
            text-align: center;
        }
        header h1 {
            font-size: 2.5rem;
            font-weight: 700;
            background: linear-gradient(135deg, #22d3ee, #3b82f6);
            -webkit-background-clip: text;
            -webkit-text-fill-color: transparent;
            letter-spacing: 1px;
            margin-bottom: 8px;
        }
        header p {
            color: #94a3b8;
            font-size: 1.1rem;
        }

        .grid {
            display: grid;
            grid-template-columns: 1.1fr 1fr;
            gap: 30px;
            align-items: start;
        }

        .glass-card {
            background: rgba(30, 41, 59, 0.45);
            backdrop-filter: blur(20px);
            -webkit-backdrop-filter: blur(20px);
            border: 1px solid rgba(255, 255, 255, 0.08);
            border-radius: 16px;
            padding: 28px;
            box-shadow: 0 10px 40px rgba(0, 0, 0, 0.4);
        }

        .form-group {
            margin-bottom: 20px;
        }
        .form-group label {
            display: block;
            margin-bottom: 8px;
            font-size: 0.95rem;
            font-weight: 600;
            color: #cbd5e1;
        }

        /* Drag & Drop */
        .drag-drop-zone {
            border: 2px dashed rgba(255, 255, 255, 0.2);
            border-radius: 12px;
            padding: 30px 20px;
            text-align: center;
            cursor: pointer;
            transition: all 0.3s ease;
            background: rgba(15, 23, 42, 0.4);
            position: relative;
        }
        .drag-drop-zone:hover, .drag-drop-zone.dragover {
            border-color: #06b6d4;
            background: rgba(6, 182, 212, 0.06);
            box-shadow: 0 0 15px rgba(6, 182, 212, 0.15);
        }
        .drag-drop-zone svg {
            width: 44px;
            height: 44px;
            stroke: #06b6d4;
            margin-bottom: 12px;
        }
        .drag-drop-zone p {
            font-size: 0.95rem;
            color: #94a3b8;
        }
        .drag-drop-zone .file-name {
            font-weight: 600;
            color: #22d3ee;
            margin-top: 10px;
            display: none;
        }

        /* Input Styles */
        .input-control {
            width: 100%;
            background: rgba(15, 23, 42, 0.6);
            border: 1px solid rgba(255, 255, 255, 0.15);
            border-radius: 8px;
            padding: 12px 16px;
            color: #f8fafc;
            font-family: inherit;
            font-size: 1rem;
            transition: all 0.3s ease;
        }
        .input-control:focus {
            outline: none;
            border-color: #22d3ee;
            box-shadow: 0 0 10px rgba(34, 211, 238, 0.2);
            background: rgba(15, 23, 42, 0.85);
        }
        .meta-grid {
            display: grid;
            grid-template-columns: 1fr 1fr;
            gap: 15px;
        }

        textarea.input-control {
            height: 400px;
            resize: none;
            font-size: 1.05rem;
            line-height: 1.6;
        }

        select.input-control {
            appearance: none;
            background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 24 24' stroke='%2394a3b8'%3E%3Cpath stroke-linecap='round' stroke-linejoin='round' stroke-width='2' d='M19 9l-7 7-7-7'/%3E%3C/svg%3E");
            background-repeat: no-repeat;
            background-position: right 16px center;
            background-size: 16px;
            cursor: pointer;
        }

        /* Кнопка запуска */
        .btn-generate {
            display: block;
            width: 100%;
            background: linear-gradient(135deg, #06b6d4, #3b82f6);
            color: #0b0f19;
            font-weight: 700;
            font-size: 1.05rem;
            text-transform: uppercase;
            letter-spacing: 1px;
            border: none;
            padding: 16px;
            border-radius: 8px;
            cursor: pointer;
            transition: all 0.3s ease;
            margin-top: 10px;
            box-shadow: 0 4px 15px rgba(6, 182, 212, 0.2);
        }
        .btn-generate:hover {
            transform: translateY(-2px);
            box-shadow: 0 0 25px rgba(6, 182, 212, 0.45);
        }
        .btn-generate:disabled {
            background: #334155;
            color: #94a3b8;
            cursor: not-allowed;
            transform: none;
            box-shadow: none;
        }

        /* Прогресс-бар */
        .progress-card {
            margin-top: 30px;
            display: none;
        }
        .progress-info {
            display: flex;
            justify-content: space-between;
            margin-bottom: 10px;
            font-weight: 600;
            font-size: 0.95rem;
        }
        .progress-container {
            background: rgba(15, 23, 42, 0.7);
            border-radius: 9999px;
            height: 12px;
            overflow: hidden;
            border: 1px solid rgba(255, 255, 255, 0.08);
            margin-bottom: 15px;
        }
        .progress-bar {
            background: linear-gradient(90deg, #06b6d4, #3b82f6);
            box-shadow: 0 0 10px rgba(6, 182, 212, 0.4);
            height: 100%;
            width: 0%;
            transition: width 0.4s ease;
        }
        .log-box {
            background: rgba(15, 23, 42, 0.8);
            border: 1px solid rgba(255, 255, 255, 0.05);
            border-radius: 8px;
            padding: 15px;
            height: 100px;
            overflow-y: auto;
            font-family: monospace;
            font-size: 0.85rem;
            color: #38bdf8;
            line-height: 1.5;
        }

        /* Видеоплеер и результат */
        .result-card {
            margin-top: 30px;
            display: none;
            text-align: center;
        }
        .result-card h3 {
            font-size: 1.3rem;
            margin-bottom: 15px;
            color: #22d3ee;
        }
        .result-card video {
            width: 100%;
            border-radius: 12px;
            border: 1px solid rgba(255, 255, 255, 0.1);
            box-shadow: 0 10px 30px rgba(0,0,0,0.5);
            margin-bottom: 20px;
        }
        .btn-download {
            display: inline-block;
            background: #10b981;
            color: #ffffff;
            font-weight: 700;
            padding: 14px 28px;
            border-radius: 8px;
            text-decoration: none;
            transition: all 0.3s ease;
            box-shadow: 0 4px 15px rgba(16, 185, 129, 0.2);
        }
        .btn-download:hover {
            background: #059669;
            transform: translateY(-2px);
            box-shadow: 0 0 25px rgba(16, 185, 129, 0.45);
        }
    </style>
</head>
<body>
    <div class="blob blob-cyan"></div>
    <div class="blob blob-magenta"></div>

    <div class="container">
        <header>
            <h1>КАРАОКЕ-ВИДЕО ГЕНЕРАТОР</h1>
            <p>Создание плавных караоке-видеороликов с пословной заливкой</p>
        </header>

        <div class="grid">
            <!-- ЛЕВАЯ КОЛОНКА -->
            <div class="glass-card">
                <!-- Загрузка аудио -->
                <div class="form-group">
                    <label>1. Загрузите аудиофайл песни (.mp3):</label>
                    <div class="drag-drop-zone" id="drop-zone">
                        <svg fill="none" viewBox="0 0 24 24" stroke="currentColor">
                            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="1.5" d="M7 16a4 4 0 01-.88-7.903A5 5 0 1115.9 6L16 6a5 5 0 011 9.9M15 13l-3-3m0 0l-3 3m3-3v12" />
                        </svg>
                        <p id="drop-text">Перетащите сюда .mp3 файл или кликните для выбора</p>
                        <div class="file-name" id="file-name"></div>
                        <input type="file" id="file-input" accept=".mp3" style="display: none;">
                    </div>
                </div>

                <!-- Метаданные -->
                <div class="form-group">
                    <label>2. Метаданные песни:</label>
                    <div class="meta-grid">
                        <div>
                            <input type="text" id="artist" class="input-control" placeholder="Исполнитель">
                        </div>
                        <div>
                            <input type="text" id="title" class="input-control" placeholder="Название песни">
                        </div>
                    </div>
                </div>

                <!-- Выбор модели -->
                <div class="form-group">
                    <label>3. Модель выравнивания ИИ Whisper:</label>
                    <select id="model-select" class="input-control">
                        <option value="medium">medium (профессиональная точность вокала, ~1.5 GB)</option>
                        <option value="small" selected>small (высокая точность, ~460 MB)</option>
                        <option value="base">base (средняя точность, ~140 MB)</option>
                    </select>
                </div>

                <!-- Качество и стилизация -->
                <div class="form-group">
                    <label>4. Качество видео:</label>
                    <select id="quality-select" class="input-control">
                        <option value="medium" selected>Стандартное (1352x224, Быстрый рендер, CRF 23)</option>
                        <option value="high">Высокое (1352x224, Высокий битрейт, CRF 17)</option>
                        <option value="ultra">Ультра HD (2704x448, 2x Супер-разрешение, CRF 12)</option>
                    </select>
                </div>

                <div class="form-group">
                    <label>5. Шрифт текста:</label>
                    <select id="font-select" class="input-control">
                        <option value="montserrat" selected>Montserrat (Рекомендуется, Bold)</option>
                        <option value="arial">Arial (Стандартный)</option>
                        <option value="helvetica">Helvetica (macOS стиль)</option>
                        <option value="georgia">Georgia (С засечками)</option>
                    </select>
                </div>

                <div class="form-group">
                    <label>6. Настройка цветов:</label>
                    <div style="display: grid; grid-template-columns: 1fr 1fr 1fr; gap: 10px; background: rgba(15, 23, 42, 0.4); padding: 12px; border-radius: 8px; border: 1px solid rgba(255, 255, 255, 0.08);">
                        <div style="text-align: center;">
                            <label style="font-size: 0.8rem; display: block; margin-bottom: 6px; color: #94a3b8;">Поющий</label>
                            <input type="color" id="color-active" value="#000000" style="width: 100%; height: 32px; border: none; border-radius: 4px; cursor: pointer; background: none;">
                        </div>
                        <div style="text-align: center;">
                            <label style="font-size: 0.8rem; display: block; margin-bottom: 6px; color: #94a3b8;">Будущий</label>
                            <input type="color" id="color-inactive" value="#B4B9C3" style="width: 100%; height: 32px; border: none; border-radius: 4px; cursor: pointer; background: none;">
                        </div>
                        <div style="text-align: center;">
                            <label style="font-size: 0.8rem; display: block; margin-bottom: 6px; color: #94a3b8;">Фон</label>
                            <input type="color" id="color-bg" value="#FFFFFF" style="width: 100%; height: 32px; border: none; border-radius: 4px; cursor: pointer; background: none;">
                        </div>
                    </div>
                </div>

                <div class="form-group">
                    <label>7. Сдвиг синхронизации звука (мс):</label>
                    <div style="display: flex; align-items: center; gap: 12px; background: rgba(15, 23, 42, 0.4); padding: 12px; border-radius: 8px; border: 1px solid rgba(255, 255, 255, 0.08);">
                        <input type="range" id="audio-delay" min="-500" max="500" value="0" step="10" style="flex: 1; cursor: pointer; accent-color: #22d3ee;">
                        <span id="audio-delay-val" style="font-weight: bold; color: #22d3ee; width: 60px; text-align: right;">0 мс</span>
                    </div>
                </div>

                <!-- Запуск -->
                <button id="btn-generate" class="btn-generate" disabled>СГЕНЕРИРОВАТЬ ВИДЕО</button>

                <!-- Блок прогресса -->
                <div class="progress-card" id="progress-card">
                    <div class="progress-info">
                        <span id="status-label">Запуск выравнивания...</span>
                        <span id="percent-label">0%</span>
                    </div>
                    <div class="progress-container">
                        <div class="progress-bar" id="progress-bar"></div>
                    </div>
                    <div class="log-box" id="log-box"></div>
                </div>
            </div>

            <!-- ПРАВАЯ КОЛОНКА -->
            <div class="glass-card">
                <div class="form-group" style="margin-bottom: 0;">
                    <label>4. Вставьте текст песни (построчно):</label>
                    <textarea id="lyrics" class="input-control" placeholder="Вставьте сюда текст песни... Каждый куплет или фраза с новой строки."></textarea>
                </div>
            </div>
        </div>

        <!-- КАРТОЧКА РЕЗУЛЬТАТА -->
        <div class="glass-card result-card" id="result-card">
            <h3>🎉 Ваше караоке-видео готово!</h3>
            <video id="video-player" controls></video>
            <div>
                <a href="#" id="btn-download" class="btn-download" download>Скачать видеоролик (.mp4)</a>
            </div>
        </div>
    </div>

    <script>
        const dropZone = document.getElementById('drop-zone');
        const fileInput = document.getElementById('file-input');
        const dropText = document.getElementById('drop-text');
        const fileNameDiv = document.getElementById('file-name');
        
        const artistInput = document.getElementById('artist');
        const titleInput = document.getElementById('title');
        const lyricsInput = document.getElementById('lyrics');
        const modelSelect = document.getElementById('model-select');
        const btnGenerate = document.getElementById('btn-generate');
        
        const progressCard = document.getElementById('progress-card');
        const progressBar = document.getElementById('progress-bar');
        const statusLabel = document.getElementById('status-label');
        const percentLabel = document.getElementById('percent-label');
        const logBox = document.getElementById('log-box');
        
        const resultCard = document.getElementById('result-card');
        const videoPlayer = document.getElementById('video-player');
        const btnDownload = document.getElementById('btn-download');

        let uploadedFilePath = '';
        let currentJobId = '';

        // Настройка Drag & Drop
        dropZone.addEventListener('click', () => fileInput.click());
        
        dropZone.addEventListener('dragover', (e) => {
            e.preventDefault();
            dropZone.classList.add('dragover');
        });

        dropZone.addEventListener('dragleave', () => {
            dropZone.classList.remove('dragover');
        });

        dropZone.addEventListener('drop', (e) => {
            e.preventDefault();
            dropZone.classList.remove('dragover');
            const files = e.dataTransfer.files;
            if (files.length > 0) {
                handleFile(files[0]);
            }
        });

        fileInput.addEventListener('change', (e) => {
            const files = e.target.files;
            if (files.length > 0) {
                handleFile(files[0]);
            }
        });

        // Загрузка файла
        function handleFile(file) {
            if (!file.name.endsWith('.mp3')) {
                alert('Пожалуйста, выберите файл в формате .mp3!');
                return;
            }

            dropText.innerText = 'Загрузка файла...';
            fileNameDiv.style.display = 'none';

            const formData = new FormData();
            formData.append('file', file);

            fetch('/upload', {
                method: 'POST',
                body: formData
            })
            .then(res => res.json())
            .then(data => {
                if (data.error) {
                    alert('Ошибка загрузки: ' + data.error);
                    dropText.innerText = 'Перетащите сюда .mp3 файл или кликните для выбора';
                    return;
                }
                uploadedFilePath = data.filepath;
                dropText.innerText = 'Файл успешно загружен!';
                fileNameDiv.innerText = file.name;
                fileNameDiv.style.display = 'block';

                // Заполняем метаданные
                artistInput.value = data.artist || '';
                titleInput.value = data.title || '';
                
                checkReady();
            })
            .catch(err => {
                console.error(err);
                alert('Произошла ошибка при загрузке файла!');
                dropText.innerText = 'Перетащите сюда .mp3 файл или кликните для выбора';
            });
        }

        // Проверка готовности к генерации
        function checkReady() {
            if (uploadedFilePath && lyricsInput.value.trim()) {
                btnGenerate.disabled = false;
            } else {
                btnGenerate.disabled = true;
            }
        }

        lyricsInput.addEventListener('input', checkReady);

        // Клик по Генерации
        btnGenerate.addEventListener('click', () => {
            btnGenerate.disabled = true;
            progressCard.style.display = 'block';
            resultCard.style.display = 'none';
            logBox.innerHTML = '';
            
            addLog('Инициализация процесса генерации...');

            const payload = {
                audio_path: uploadedFilePath,
                artist: artistInput.value.trim() || 'Исполнитель',
                title: titleInput.value.trim() || 'Песня',
                lyrics: lyricsInput.value.trim(),
                model: modelSelect.value,
                quality: document.getElementById('quality-select').value,
                font: document.getElementById('font-select').value,
                color_active: document.getElementById('color-active').value,
                color_inactive: document.getElementById('color-inactive').value,
                color_bg: document.getElementById('color-bg').value,
                audio_delay: parseFloat(document.getElementById('audio-delay').value) / 1000.0
            };

            fetch('/generate', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify(payload)
            })
            .then(res => res.json())
            .then(data => {
                if (data.error) {
                    addLog('❌ Ошибка: ' + data.error);
                    btnGenerate.disabled = false;
                    return;
                }
                currentJobId = data.job_id;
                addLog('🚀 Задача запущена. ID: ' + currentJobId);
                pollStatus();
            })
            .catch(err => {
                console.error(err);
                addLog('❌ Не удалось отправить запрос на генерацию!');
                btnGenerate.disabled = false;
            });
        });

        // Настройка интерактивного обновления значения слайдера задержки звука
        const audioDelayInput = document.getElementById('audio-delay');
        const audioDelayValSpan = document.getElementById('audio-delay-val');
        audioDelayInput.addEventListener('input', () => {
            let val = parseInt(audioDelayInput.value);
            audioDelayValSpan.textContent = (val > 0 ? '+' : '') + val + ' мс';
        });

        function addLog(msg) {
            const time = new Date().toLocaleTimeString();
            logBox.innerHTML += `[${time}] ${msg}<br>`;
            logBox.scrollTop = logBox.scrollHeight;
        }

        // Опрос статуса
        function pollStatus() {
            if (!currentJobId) return;

            fetch(`/status/${currentJobId}`)
            .then(res => res.json())
            .then(data => {
                if (data.error) {
                    addLog('❌ Ошибка: ' + data.error);
                    statusLabel.innerText = 'Ошибка генерации';
                    btnGenerate.disabled = false;
                    return;
                }

                const percent = Math.round(data.progress * 100);
                progressBar.style.width = percent + '%';
                percentLabel.innerText = percent + '%';
                statusLabel.innerText = data.status || 'Обработка...';
                
                if (data.status) {
                    // Добавляем лог, если он отличается от последнего
                    const lastLog = logBox.innerHTML.split('<br>').slice(-2)[0] || '';
                    if (!lastLog.includes(data.status)) {
                        addLog(data.status);
                    }
                }

                if (data.done) {
                    addLog('🎉 Генерация караоке-видео полностью завершена!');
                    
                    // Показываем плеер
                    const videoUrl = `/download/${currentJobId}`;
                    videoPlayer.src = videoUrl;
                    btnDownload.href = videoUrl;
                    btnDownload.setAttribute('download', `${artistInput.value} - ${titleInput.value} (karaoke).mp4`);
                    
                    resultCard.style.display = 'block';
                    btnGenerate.disabled = false;
                    
                    // Прокрутка вниз к видеоплееру
                    resultCard.scrollIntoView({ behavior: 'smooth' });
                    return;
                }

                // Продолжаем опрос
                setTimeout(pollStatus, 800);
            })
            .catch(err => {
                console.error(err);
                addLog('⚠️ Временный сбой связи с сервером...');
                setTimeout(pollStatus, 1500);
            });
        }
    </script>
</body>
</html>
""")

@app.route('/upload', methods=['POST'])
def upload():
    try:
        if 'file' not in request.files:
            return jsonify({"error": "Файл не отправлен!"}), 400
        file = request.files['file']
        if file.filename == '':
            return jsonify({"error": "Пустое имя файла!"}), 400
        
        file_ext = os.path.splitext(file.filename)[1]
        unique_name = f"{uuid.uuid4()}{file_ext}"
        filepath = os.path.join(app.config['UPLOAD_FOLDER'], unique_name)
        file.save(filepath)

        # Парсим исполнителя и название
        basename = os.path.splitext(file.filename)[0]
        artist = ""
        title = ""
        if " - " in basename:
            parts = basename.split(" - ", 1)
            artist = parts[0].strip()
            title = parts[1].strip()
        else:
            title = basename.strip()

        return jsonify({
            "filepath": filepath,
            "artist": artist,
            "title": title
        })
    except Exception as e:
        return jsonify({"error": str(e)}), 500

@app.route('/generate', methods=['POST'])
def generate():
    try:
        data = request.json
        audio_path = data.get('audio_path')
        artist = data.get('artist', 'Исполнитель')
        title = data.get('title', 'Песня')
        lyrics = data.get('lyrics')
        model_name = data.get('model', 'base')
        quality = data.get('quality', 'medium')
        font_family = data.get('font', 'montserrat')
        color_active = data.get('color_active', '#000000')
        color_inactive = data.get('color_inactive', '#B4B9C3')
        color_bg = data.get('color_bg', '#FFFFFF')
        audio_delay = float(data.get('audio_delay', 0.0))
        vocal_start = float(data.get('vocal_start', 0.0))
        auto_vocal_start = bool(data.get('auto_vocal_start', True))

        if not audio_path or not os.path.exists(audio_path):
            return jsonify({"error": "Аудиофайл не найден на сервере!"}), 400
        if not lyrics:
            return jsonify({"error": "Текст песни отсутствует!"}), 400

        job_id = str(uuid.uuid4())
        jobs[job_id] = {
            "progress": 0.0,
            "status": "Инициализация фоновой задачи...",
            "done": False,
            "error": None,
            "file": None
        }

        # Запускаем фоновый поток генерации с параметрами оформления и качества
        thread = threading.Thread(
            target=generate_karaoke_thread,
            args=(job_id, audio_path, artist, title, lyrics, model_name, quality, font_family, color_active, color_inactive, color_bg, audio_delay, vocal_start, auto_vocal_start)
        )
        thread.daemon = True
        thread.start()

        return jsonify({"job_id": job_id})
    except Exception as e:
        return jsonify({"error": str(e)}), 500

@app.route('/status/<job_id>', methods=['GET'])
def status(job_id):
    if job_id not in jobs:
        return jsonify({"error": "Задача не найдена!"}), 404
    return jsonify(jobs[job_id])

@app.route('/download/<job_id>', methods=['GET'])
def download(job_id):
    if job_id not in jobs or not jobs[job_id]["done"]:
        return jsonify({"error": "Видео еще не готово или задача не найдена!"}), 404
    
    filename = jobs[job_id]["file"]
    filepath = os.path.join(EXPORT_FOLDER, filename)
    if not os.path.exists(filepath):
        return jsonify({"error": "Файл видео не найден на сервере!"}), 404
        
    return send_file(filepath, mimetype='video/mp4', as_attachment=False)

# ----------------- СТАРТ ПРИЛОЖЕНИЯ -----------------
def open_browser():
    # Небольшая задержка перед открытием браузера
    import time
    time.sleep(1.5)
    webbrowser.open("http://127.0.0.1:5050")

if __name__ == '__main__':
    import sys
    if '--help' in sys.argv or '-h' in sys.argv:
        import argparse
        parser = argparse.ArgumentParser(description="Караоке-Генератор CLI")
        parser.add_argument('--cli', action='store_true')
        parser.add_argument('--audio', required=True)
        parser.add_argument('--artist', default='Исполнитель')
        parser.add_argument('--title', default='Песня')
        parser.add_argument('--lyrics-file')
        parser.add_argument('--model', default='base')
        parser.add_argument('--quality', default='medium')
        parser.add_argument('--font', default='montserrat')
        parser.add_argument('--color-active', default='#000000')
        parser.add_argument('--color-inactive', default='#B4B9C3')
        parser.add_argument('--color-bg', default='#FFFFFF')
        parser.add_argument('--audio-delay', type=float, default=0.0)
        parser.add_argument('--vocal-start', type=float, default=0.0)
        parser.add_argument('--auto-vocal-start', action='store_true')
        parser.add_argument('--detect-vocal-start', action='store_true')
        parser.add_argument('--detect-window', type=float, default=45.0)
        parser.print_help()
        sys.exit(0)

    if '--cli' in sys.argv:
        import argparse
        parser = argparse.ArgumentParser(description="Караоке-Генератор CLI")
        parser.add_argument('--cli', action='store_true')
        parser.add_argument('--audio', required=True)
        parser.add_argument('--artist', default='Исполнитель')
        parser.add_argument('--title', default='Песня')
        parser.add_argument('--lyrics-file')
        parser.add_argument('--model', default='base')
        parser.add_argument('--quality', default='medium')
        parser.add_argument('--font', default='montserrat')
        parser.add_argument('--color-active', default='#000000')
        parser.add_argument('--color-inactive', default='#B4B9C3')
        parser.add_argument('--color-bg', default='#FFFFFF')
        parser.add_argument('--audio-delay', type=float, default=0.0)
        parser.add_argument('--vocal-start', type=float, default=0.0)
        parser.add_argument('--auto-vocal-start', action='store_true')
        parser.add_argument('--detect-vocal-start', action='store_true')
        parser.add_argument('--detect-window', type=float, default=45.0)
        
        args = parser.parse_args()

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
        
        # Читаем текст песни из файла
        with open(args.lyrics_file, 'r', encoding='utf-8') as f:
            lyrics_text = f.read()
            
        # Создаем эмулятор-выводитель прогресса в stdout с поддержкой глубокого отслеживания мутаций
        class ObservableDict(dict):
            def __init__(self, parent, key, *args, **kwargs):
                self.parent = parent
                self.key = key
                super().__init__(*args, **kwargs)
            def __setitem__(self, k, v):
                super().__setitem__(k, v)
                self.parent.notify(self.key, self)

        class CLIJobsDict(dict):
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
                print(json.dumps(val), flush=True)
                
        # Переопределяем глобальный словарь jobs
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
                auto_vocal_start=args.auto_vocal_start
            )
        except Exception as e:
            print(json.dumps({"progress": 1.0, "status": f"❌ Ошибка: {str(e)}", "done": True, "error": str(e)}), flush=True)
            sys.exit(1)
        sys.exit(0)

    if getattr(sys, "frozen", False):
        print("This bundled worker is intended to be launched by Karaoke Generator with --cli.", file=sys.stderr)
        sys.exit(2)

    print("==================================================================")
    print("🚀 Запуск ЛОКАЛЬНОГО ВЕБ-СЕРВЕРА Караоке-Генератора...")
    print("Браузер откроется автоматически через несколько секунд.")
    print("Вы также можете перейти по адресу вручную: http://127.0.0.1:5050")
    print("==================================================================")
    
    # Запускаем автоматическое открытие браузера в отдельном потоке
    threading.Thread(target=open_browser, daemon=True).start()
    
    # Запускаем Flask сервер локально на порту 5050
    app.run(host='127.0.0.1', port=5050, debug=False)
