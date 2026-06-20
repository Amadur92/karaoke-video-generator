from __future__ import annotations

import base64
import json
import os
import re
import subprocess
import sys
import tempfile
import urllib.parse
import urllib.request
from typing import Optional, Any

from .models import TrackMetadata, ScoredCandidate
from .http import DEFAULT_UA, safe_urlopen

def decrypt_jiosaavn_url(encrypted_url: str) -> str:
    """
    Decrypts JioSaavn encrypted_media_url using OpenSSL DES-ECB.
    """
    enc_data = base64.b64decode(encrypted_url)
    
    # We use openssl command because cryptography/pycryptodome might not be installed.
    # OpenSSL on macOS 3.0+ requires -provider legacy -provider default for DES.
    cmd = [
        "openssl", "enc", "-d", "-des-ecb",
        "-provider", "legacy",
        "-provider", "default",
        "-K", "3338333436353931",
        "-nosalt"
    ]
    
    try:
        proc = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE
        )
        stdout, stderr = proc.communicate(input=enc_data)
        if proc.returncode != 0:
            raise Exception(f"OpenSSL failed: {stderr.decode('utf-8', errors='ignore')}")
        
        decrypted = stdout.decode('utf-8', errors='ignore').strip()
        # Replace _96.mp4 with _320.mp4 for high-quality audio
        decrypted = decrypted.replace("_96.mp4", "_320.mp4").replace("_96_p.mp4", "_320.mp4")
        return decrypted
    except Exception as e:
        raise RuntimeError(f"Failed to decrypt JioSaavn URL: {e}")


def fetch_lyrics(title: str, artist: str) -> Optional[dict[str, str]]:
    """
    Fetches lyrics from LRCLIB for the given title and artist.
    Returns a dict with 'plain' and 'synced' lyrics if found, else None.
    """
    # Try exact match first
    try:
        url = f"https://lrclib.net/api/get?artist_name={urllib.parse.quote(artist)}&track_name={urllib.parse.quote(title)}"
        req = urllib.request.Request(url, headers={"User-Agent": DEFAULT_UA})
        with safe_urlopen(req, timeout=10) as resp:
            data = json.loads(resp.read().decode("utf-8"))
            if data.get("plainLyrics") or data.get("syncedLyrics"):
                return {
                    "plain": data.get("plainLyrics") or "",
                    "synced": data.get("syncedLyrics") or ""
                }
    except Exception:
        pass
        
    # If exact match fails, try search
    try:
        url = f"https://lrclib.net/api/search?artist_name={urllib.parse.quote(artist)}&track_name={urllib.parse.quote(title)}"
        req = urllib.request.Request(url, headers={"User-Agent": DEFAULT_UA})
        with safe_urlopen(req, timeout=10) as resp:
            results = json.loads(resp.read().decode("utf-8"))
            if results:
                # Find first with synced, then first with plain
                best = None
                for res in results:
                    if res.get("syncedLyrics"):
                        best = res
                        break
                if not best:
                    best = results[0]
                if best.get("plainLyrics") or best.get("syncedLyrics"):
                    return {
                        "plain": best.get("plainLyrics") or "",
                        "synced": best.get("syncedLyrics") or ""
                    }
    except Exception:
        pass
        
def clean_genius_lyrics(lyrics: str) -> str:
    if not lyrics:
        return ""
    
    # 1. Clean the header: e.g. "3 ContributorsЧёрно-белый цвет (Black And White Color) Lyrics[Текст песни «Чёрно-белый цвет»]" -> ""
    # We clean the entire first line if it contains the word "Contributors" or "Contributor"
    cleaned = re.sub(r'^\s*\d*\s*Contributors?[^\n]*\s*', '', lyrics, flags=re.IGNORECASE)
    
    # In case [Текст песни...] starts on its own line or wasn't caught
    cleaned = re.sub(r'^\[Текст песни\s+[^\]]+\]\s*', '', cleaned, flags=re.IGNORECASE)
    
    # Remove lines containing only bracketed section headers like [Куплет 1], [Припев], [Chorus], [Intro], etc.
    cleaned = re.sub(r'^\s*\[[^\]]+\]\s*\n?', '', cleaned, flags=re.MULTILINE)
    
    # 2. Remove "You might also like"
    cleaned = re.sub(r'You might also like', '', cleaned, flags=re.IGNORECASE)
    
    # 3. Clean the footer: e.g. "54Embed" or "Embed" at the very end
    cleaned = re.sub(r'\d*\s*Embed\s*$', '', cleaned, flags=re.IGNORECASE)
    
    cleaned = cleaned.strip()
    if cleaned.endswith("Embed"):
        cleaned = cleaned[:-5].strip()
        
    return cleaned


def search_genius(title: str, artist: str) -> Optional[str]:
    from .lyrics import _search_variants
    from .models import TrackMetadata
    
    track = TrackMetadata(title=title, artists=[artist])
    variants = _search_variants(track)
    
    BAD_KEYWORDS = {
        "translation", "translate", "перевод", "cover", "кавер", 
        "instrumental", "minus", "минус", "backing track", "karaoke", "караоке",
        "remix", "ремикс", "tribute", "трибьют"
    }
    
    # Generate search queries based on variants
    queries = []
    for var_title, var_artist in variants:
        queries.append(f"{var_artist} {var_title}")
        
        # Simplify artist if multi-artist
        first_art = var_artist
        for sep in [",", " feat", " ft.", " and ", " & ", " with "]:
            if sep in first_art.lower():
                first_art = first_art.split(sep)[0].strip()
        m = re.match(r"^([а-яА-ЯёЁ\s\-]+)\s+[a-zA-Z]", first_art)
        if m:
            first_art = m.group(1).strip()
        
        if first_art.lower() != var_artist.lower():
            queries.append(f"{first_art} {var_title}")
            
    # Fallback to searching by title only
    for var_title, _ in variants:
        if var_title not in queries:
            queries.append(var_title)
            
    # Deduplicate queries while preserving order
    seen_queries = set()
    unique_queries = []
    for q in queries:
        q_low = q.lower().strip()
        if q_low and q_low not in seen_queries:
            seen_queries.add(q_low)
            unique_queries.append(q)
            
    from .scoring import ratio
    
    for query in unique_queries:
        url = f"https://genius.com/api/search/multi?q={urllib.parse.quote(query)}"
        req = urllib.request.Request(url, headers={"User-Agent": DEFAULT_UA})
        try:
            with safe_urlopen(req, timeout=10) as resp:
                data = json.loads(resp.read().decode("utf-8"))
                sections = data.get("response", {}).get("sections", [])
                for sec in sections:
                    if sec.get("type") != "song":
                        continue
                    hits = sec.get("hits", [])
                    for hit in hits[:5]:
                        result = hit.get("result", {})
                        hit_title = (result.get("title") or "").lower()
                        hit_artist = (result.get("primary_artist", {}).get("name") or "").lower()
                        path = result.get("path")
                        
                        if not path:
                            continue
                            
                        has_bad_keyword = False
                        for kw in BAD_KEYWORDS:
                            if kw in hit_title and kw not in title.lower() and kw not in artist.lower():
                                has_bad_keyword = True
                                break
                        
                        if not has_bad_keyword:
                            best_sim = 0.0
                            for var_title, var_artist in variants:
                                sim = ratio(var_artist, result.get("primary_artist", {}).get("name") or "")
                                if sim > best_sim:
                                    best_sim = sim
                                    
                            if best_sim >= 38.0:
                                return f"https://genius.com{path}"
        except Exception:
            pass
            
    # Ultimate fallback: first hit of the first query
    if unique_queries:
        try:
            url = f"https://genius.com/api/search/multi?q={urllib.parse.quote(unique_queries[0])}"
            req = urllib.request.Request(url, headers={"User-Agent": DEFAULT_UA})
            with safe_urlopen(req, timeout=10) as resp:
                data = json.loads(resp.read().decode("utf-8"))
                sections = data.get("response", {}).get("sections", [])
                for sec in sections:
                    if sec.get("type") == "song":
                        hits = sec.get("hits", [])
                        if hits:
                            path = hits[0].get("result", {}).get("path")
                            if path:
                                return f"https://genius.com{path}"
        except Exception:
            pass
            
    return None


def scrape_genius_lyrics(url: str) -> Optional[str]:
    import lxml.html
    req = urllib.request.Request(url, headers={"User-Agent": DEFAULT_UA})
    try:
        with safe_urlopen(req, timeout=10) as resp:
            html = resp.read().decode("utf-8")
            tree = lxml.html.fromstring(html)
            
            # Find all divs with data-lyrics-container="true"
            containers = tree.xpath('//div[@data-lyrics-container="true"]')
            if not containers:
                # Fallback to class search
                containers = tree.xpath('//div[contains(@class, "Lyrics__Container")]')
            
            if not containers:
                # Legacy fallback
                legacy = tree.xpath('//div[contains(@class, "lyrics")]')
                if legacy:
                    containers = legacy
            
            if not containers:
                return None
                
            parts = []
            for container in containers:
                # Convert <br> tags to newlines before getting text content
                for br in container.xpath('.//br'):
                    br.tail = "\n" + (br.tail or "")
                
                parts.append(container.text_content().strip())
                
            raw = "\n\n".join(parts).strip()
            return clean_genius_lyrics(raw)
    except Exception:
        pass
def get_audio_duration(path: str) -> Optional[int]:
    cmd = [
        "ffprobe", "-v", "error", "-show_entries", "format=duration",
        "-of", "default=noprint_wrappers=1:nokey=1", path
    ]
    try:
        proc = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        if proc.returncode == 0:
            return round(float(proc.stdout.strip()))
    except Exception:
        pass
    return None


# Минимальный размер файла, который считаем валидным аудио.
# Меньше — это, скорее всего, HTML-страница с ошибкой или пустой ответ.
_MIN_AUDIO_BYTES = 50_000


def is_valid_audio_file(path: Optional[str], min_duration_sec: int = 10) -> bool:
    """
    Проверяет, что по пути лежит действительно валидный аудиофайл:
    файл существует, достаточно велик и ffprobe умеет его прочитать
    с разумной длительностью. Предохраняет от случаев, когда провайдер
    вернул HTML с ошибкой вместо аудио — тогда ffmpeg падал бы позже.
    """
    if not path or not os.path.exists(path):
        return False
    try:
        if os.path.getsize(path) < _MIN_AUDIO_BYTES:
            return False
    except OSError:
        return False
    duration = get_audio_duration(path)
    return duration is not None and duration >= min_duration_sec


def _safe_remove(path: Optional[str]) -> None:
    """Удаляет файл, игнорируя ошибки (используется при очистке битых загрузок)."""
    if path and os.path.exists(path):
        try:
            os.remove(path)
        except OSError:
            pass


class Downloader:
    def __init__(
        self,
        output_dir: str = ".",
        format: str = "mp3",
        cookies_from_browser: str | None = None,
        duration_tolerance: int | None = None,
    ):
        self.output_dir = output_dir
        self.format = format.lower()
        if self.format not in {"mp3", "m4a", "flac"}:
            self.format = "mp3"
        self.cookies_from_browser = (cookies_from_browser or "").strip() or None
        self.duration_tolerance = duration_tolerance if duration_tolerance and duration_tolerance > 0 else None
        self.temp_dir = os.path.join(self.output_dir, ".tmp")
        os.makedirs(self.temp_dir, exist_ok=True)

    def _add_cookie_options(self, options: dict) -> dict:
        if self.cookies_from_browser:
            options = dict(options)
            options["cookiesfrombrowser"] = (self.cookies_from_browser,)
        return options

    def _duration_tolerance(self, default: int) -> int:
        return self.duration_tolerance or default

    def download(self, track: TrackMetadata, candidates: list[ScoredCandidate], override_dir: Optional[str] = None) -> bool:
        """
        Attempts to download candidates in order of score. Falls back to YouTube search if all fail.
        Also fetches lyrics from LRCLIB and embeds/saves them.
        """
        if override_dir:
            track_dir = override_dir
        else:
            safe_title = "".join(c for c in track.title if c.isalnum() or c in " -_().")
            safe_artist = "".join(c for c in track.primary_artist if c.isalnum() or c in " -_().")
            track_folder = f"{safe_artist} - {safe_title}"
            track_dir = os.path.join(self.output_dir, track_folder)
            
        os.makedirs(track_dir, exist_ok=True)
        
        temp_audio: Optional[str] = None
        temp_cover: Optional[str] = None
        resolved_candidate = None
        
        # Try candidates in order
        for scored in candidates:
            candidate = scored.candidate
            
            # Skip candidates with critical bad markers or very low scores
            if any(f.startswith("critical_marker:") for f in scored.flags):
                print(f"[#] Skipping candidate '{candidate.title}' because it contains a critical bad marker (karaoke/minus/instrumental).")
                continue
            allow_low_score_exact = (
                self.duration_tolerance
                and scored.title_score >= 78
                and scored.artist_score >= 40
            )
            if scored.score < 20 and not allow_low_score_exact:
                print(f"[#] Skipping candidate '{candidate.title}' due to low score ({scored.score} < 20).")
                continue
                
            print(f"[*] Trying candidate from '{candidate.source}' (Score: {scored.score})...")
            try:
                if candidate.source == "jiosaavn" and candidate.external_id:
                    temp_audio = self._download_jiosaavn(candidate.external_id)
                elif candidate.source == "qobuz-isrc" and candidate.external_id:
                    temp_audio = self._download_qobuz(candidate.external_id)
                elif candidate.source == "soundcloud" and candidate.public_url:
                    temp_audio = self._download_soundcloud(candidate.public_url)
                elif "youtube" in candidate.source:
                    if candidate.public_url:
                        temp_audio = self._download_youtube(candidate.public_url)
                
                if temp_audio:
                    # Постскачивательная проверка длительности: некоторые источники
                    # (JioSaavn, Deezer metadata) не сообщают duration в кандидате,
                    # поэтому фильтр по длительности не мог их отсечь заранее. Если
                    # скачанный файл сильно расходится с эталоном (>25с) — это почти
                    # наверняка другая версия (radio edit, кавер). Отвергаем и идём
                    # к следующему кандидату, а не принимаем заведомо не тот трек.
                    actual_dur = get_audio_duration(temp_audio)
                    if track.duration_sec and actual_dur and abs(track.duration_sec - actual_dur) > self._duration_tolerance(25):
                        print(
                            f"[!] Downloaded duration ({actual_dur}s) differs from reference "
                            f"({track.duration_sec}s) by {abs(track.duration_sec - actual_dur)}s "
                            f"— likely wrong version, discarding and trying next candidate."
                        )
                        _safe_remove(temp_audio)
                        temp_audio = None
                        continue
                    resolved_candidate = candidate
                    print(f"[+] Successfully downloaded stream from '{candidate.source}'!")
                    break
            except Exception as e:
                print(f"[!] Failed to download candidate from '{candidate.source}': {e}", file=sys.stderr)
                
        # Fallback to YouTube search if all candidates failed or no candidates were found
        if not temp_audio:
            print(f"[!] Direct candidate downloads failed or unavailable. Falling back to YouTube search...")
            # Поиск по «артист - название». Суффикс «audio» раньше тянул
            # низкокачественные загрузки; теперь правильная версия выбирается
            # по длительности и официальности канала, а не по запросу.
            query = f"{track.primary_artist} - {track.title}".strip(" -")
            try:
                temp_audio = self._download_youtube_search(query, track)
            except Exception as e:
                print(f"[!] YouTube search download failed: {e}", file=sys.stderr)

        # Второй fallback: SoundCloud-поиск (без токена). Полезен для русского
        # хип-хопа/инди и эксклюзивов, отсутствующих на YouTube/Qobuz.
        if not temp_audio:
            print(f"[!] YouTube unavailable/failed. Trying SoundCloud search...")
            sc_query = f"{track.primary_artist} {track.title}".strip()
            try:
                temp_audio = self._download_soundcloud_search(sc_query, track)
            except Exception as e:
                print(f"[!] SoundCloud search download failed: {e}", file=sys.stderr)

        if not temp_audio:
            print("[-] Error: Could not download audio stream from any source.", file=sys.stderr)
            return False

        # Финальный guard длительности: даже если какой-то источник сообщил
        # неверную версию (например, direct-кандидат без duration в списке),
        # и fallback-поиск тоже промахнулся — не отдаём пользователю файл,
        # который на >40с расходится с эталоном. Лучше сообщить об ошибке,
        # чем подсунуть extended/радио-edit другого размера.
        _ref_dur = track.duration_sec
        _actual = get_audio_duration(temp_audio)
        if _ref_dur and _actual and abs(_ref_dur - _actual) > self._duration_tolerance(40):
            print(
                f"[-] Final check failed: downloaded {_actual}s vs reference {_ref_dur}s "
                f"(diff {abs(_ref_dur - _actual)}s). Rejecting likely wrong version."
            )
            _safe_remove(temp_audio)
            return False
            
        try:
            # Determine downloaded audio duration and enrich track metadata.
            # Сохраняем исходный эталон, чтобы не затереть его реальной
            # длительностью до окончания выбора (постскачивательная проверка
            # кандидатов опирается на эталон из enrichment).
            actual_duration = get_audio_duration(temp_audio)
            if actual_duration:
                print(f"[*] Actual downloaded audio duration: {actual_duration}s")
                
            # Fetch and save lyrics
            lyrics_data = self._get_lyrics(track, resolved_candidate)
            plain_lyrics = lyrics_data.get("plain") if lyrics_data else None
            
            if lyrics_data:
                self._save_lrc_file(track, lyrics_data, track_dir)
            
            # Cover art download
            temp_cover = self._download_cover(track)
            
            # Post-processing (conversion, tagging & lyrics embedding)
            self._post_process(track, temp_audio, temp_cover, plain_lyrics, track_dir)
            return True
        except Exception as e:
            print(f"[-] Error during post-processing: {e}", file=sys.stderr)
            return False
        finally:
            # Clean up temp files
            if temp_audio and os.path.exists(temp_audio):
                try:
                    os.remove(temp_audio)
                except OSError:
                    pass
            if temp_cover and os.path.exists(temp_cover):
                try:
                    os.remove(temp_cover)
                except OSError:
                    pass

    def _get_lyrics(self, track: TrackMetadata, candidate: Optional[Any]) -> Optional[dict[str, str]]:
        """
        Tries different title/artist combinations to query LRCLIB for lyrics,
        with a fallback to Genius search and scraping.
        """
        from .lyrics import resolve_lyrics

        print(f"[*] Resolving LRCLIB lyrics for '{track.title}' by '{track.primary_artist}'...")
        resolved = resolve_lyrics(track)
        if resolved:
            print(
                f"[+] Lyrics match: {resolved.artist} - {resolved.title} "
                f"(score {resolved.score}, synced={resolved.has_synced})"
            )
            return {
                "plain": resolved.plain,
                "synced": resolved.synced,
            }

        print("[-] Lyrics not found in LRCLIB. Trying Genius fallback...")
        try:
            genius_url = search_genius(track.title, track.primary_artist)
            if genius_url:
                print(f"[+] Found Genius URL: {genius_url}")
                plain_lyrics = scrape_genius_lyrics(genius_url)
                if plain_lyrics:
                    print(f"[+] Successfully scraped lyrics from Genius!")
                    return {
                        "plain": plain_lyrics,
                        "synced": "",
                    }
                else:
                    print("[-] Failed to scrape lyrics from Genius URL.")
            else:
                print("[-] Track not found on Genius.")
        except Exception as e:
            print(f"[!] Genius fallback error: {e}", file=sys.stderr)

        return None

    def _save_lrc_file(self, track: TrackMetadata, lyrics_data: dict[str, str], target_dir: str) -> None:
        """
        Saves synced lyrics (or plain if synced not available) as a .lrc file.
        """
        safe_title = "".join(c for c in track.title if c.isalnum() or c in " -_().")
        safe_artist = "".join(c for c in track.primary_artist if c.isalnum() or c in " -_().")
        lrc_filename = f"{safe_artist} - {safe_title}.lrc"
        lrc_path = os.path.join(target_dir, lrc_filename)
        
        # We prefer synced lyrics, otherwise fallback to plain
        content = lyrics_data.get("synced") or lyrics_data.get("plain")
        if content:
            try:
                with open(lrc_path, "w", encoding="utf-8") as f_out:
                    f_out.write(content)
                print(f"[+] Saved lyrics to: {lrc_path}")
            except Exception as e:
                print(f"[!] Failed to save LRC file: {e}", file=sys.stderr)

    def _download_jiosaavn(self, pid: str) -> Optional[str]:
        """
        Fetches song details from JioSaavn, decrypts the media URL, and downloads it.
        """
        try:
            url = f"https://www.jiosaavn.com/api.php?__call=song.getDetails&cc=in&_marker=0%3F_marker%3D0&_format=json&pids={pid}"
            req = urllib.request.Request(url, headers={"User-Agent": DEFAULT_UA})
            with safe_urlopen(req, timeout=10) as resp:
                res = json.loads(resp.read().decode("utf-8"))
                song_data = res.get(pid)
                if not song_data:
                    return None
                
                encrypted_url = song_data.get("encrypted_media_url")
                if not encrypted_url:
                    return None
                
                decrypted_url = decrypt_jiosaavn_url(encrypted_url)
                print(f"[+] Decrypted JioSaavn URL: {decrypted_url}")
                
                # Download to temp file
                temp_file = tempfile.NamedTemporaryFile(dir=self.temp_dir, delete=False, suffix=".mp4")
                temp_path = temp_file.name
                temp_file.close()
                
                req_audio = urllib.request.Request(decrypted_url, headers={"User-Agent": DEFAULT_UA})
                with safe_urlopen(req_audio, timeout=30) as audio_resp, open(temp_path, "wb") as f_out:
                    f_out.write(audio_resp.read())

                if is_valid_audio_file(temp_path):
                    return temp_path
                print("[!] JioSaavn returned invalid audio data, discarding...")
                _safe_remove(temp_path)
                return None
        except Exception as e:
            print(f"[!] JioSaavn download failed: {e}", file=sys.stderr)
            return None

    def _download_qobuz(self, track_id: str) -> Optional[str]:
        """
        Fetches Qobuz stream URL via WJHE community API and downloads it.

        Пробует качества по убыванию (hi-res → lossless → lossy), потому что
        hi-res (quality=1000) доступен не для всех треков, а ошибка часто
        возвращается как пустой ответ или HTML. Валидирует результат через
        is_valid_audio_file, чтобы не отдавать ffmpeg битый файл.
        """
        # 27 = 320 kbps MP3 (lossy), 6/7 = FLAC lossless, 1000 = hi-res.
        # Берём 1000 → 320 (надёжнее, чем 27-профиль) как разумный каскад.
        for quality in (1000, 320, 27):
            try:
                url = f"https://music.wjhe.top/api/music/qobuz/url?ID={track_id}&quality={quality}&format=flac"
                req = urllib.request.Request(url, headers={"User-Agent": DEFAULT_UA})
                with safe_urlopen(req, timeout=10) as resp:
                    res = json.loads(resp.read().decode("utf-8"))
                    stream_url = res.get("url") or res.get("download_url")
                    if not stream_url and isinstance(res.get("data"), dict):
                        stream_url = res["data"].get("url") or res["data"].get("download_url")

                    if not stream_url:
                        continue

                    print(f"[+] Found Qobuz WJHE stream URL (quality={quality}): {stream_url}")

                    temp_file = tempfile.NamedTemporaryFile(dir=self.temp_dir, delete=False, suffix=".flac")
                    temp_path = temp_file.name
                    temp_file.close()

                    req_audio = urllib.request.Request(stream_url, headers={"User-Agent": DEFAULT_UA})
                    with safe_urlopen(req_audio, timeout=30) as audio_resp, open(temp_path, "wb") as f_out:
                        f_out.write(audio_resp.read())

                    if is_valid_audio_file(temp_path):
                        return temp_path
                    print(f"[!] Qobuz response at quality={quality} is not valid audio, trying lower quality...")
                    _safe_remove(temp_path)
            except Exception as e:
                print(f"[!] Qobuz download attempt (quality={quality}) failed: {e}", file=sys.stderr)
        return None

    def _download_soundcloud(self, track_url: str) -> Optional[str]:
        """
        Скачивает публичный трек SoundCloud через yt-dlp (extractor «soundcloud»).
        Токен не требуется. Результат валидируется, битые ответы отбрасываются.
        """
        try:
            try:
                from yt_dlp import YoutubeDL
            except ImportError:
                from youtube_dl import YoutubeDL

            outtmpl = os.path.join(self.temp_dir, "%(id)s.%(ext)s")
            options = {
                "format": "bestaudio/best",
                "outtmpl": outtmpl,
                "quiet": True,
                "nooverwrites": True,
                "noplaylist": True,
                "nocheckcertificate": True,
            }
            options = self._add_cookie_options(options)

            print(f"[*] Downloading from SoundCloud: {track_url}...")
            with YoutubeDL(options) as ydl:
                info = ydl.extract_info(track_url, download=True)
                if not info:
                    return None
                track_id = info.get("id")
                ext = info.get("ext") or "mp3"
                downloaded_path = os.path.join(self.temp_dir, f"{track_id}.{ext}")
                if downloaded_path and is_valid_audio_file(downloaded_path):
                    return downloaded_path
                print("[!] SoundCloud download produced invalid audio, discarding...")
                _safe_remove(downloaded_path)
        except Exception as e:
            print(f"[!] SoundCloud download failed: {e}", file=sys.stderr)
        return None

    def _download_youtube(self, video_url: str) -> Optional[str]:
        """
        Downloads a YouTube video as an audio file using yt-dlp or youtube-dl.
        """
        try:
            try:
                from yt_dlp import YoutubeDL
            except ImportError:
                from youtube_dl import YoutubeDL
            
            temp_dir = self.temp_dir
            outtmpl = os.path.join(temp_dir, "%(id)s.%(ext)s")
            
            options = {
                "format": "bestaudio/best",
                "outtmpl": outtmpl,
                "quiet": True,
                "nooverwrites": True,
                "noplaylist": True,
                "nocheckcertificate": True,
            }
            options = self._add_cookie_options(options)
            
            print(f"[*] Downloading from YouTube URL: {video_url}...")
            with YoutubeDL(options) as ydl:
                info = ydl.extract_info(video_url, download=True)
                video_id = info.get("id")
                ext = info.get("ext")
                downloaded_path = os.path.join(temp_dir, f"{video_id}.{ext}")
                if downloaded_path and is_valid_audio_file(downloaded_path):
                    return downloaded_path
                print("[!] YouTube download produced invalid audio, discarding...")
                _safe_remove(downloaded_path)
        except Exception as e:
            print(f"[!] YouTube download failed: {e}", file=sys.stderr)
        return None

    def _rank_youtube_entries(
        self, entries: list, track: TrackMetadata
    ) -> list[tuple[float, dict]]:
        """
        Ранжирует результаты поиска YouTube по совокупности сигналов:
          - близость длительности к эталонной (главный сигнал);
          - сходство названия с «артист - название»;
          - официальность канала (VEVO / Topic / лейбл);
          - отсутствие критических bad-маркеров (караоке, минус, ремикс ...).

        Возвращает список (score, entry), отсортированный по убыванию score.
        Кандидаты с критическими bad-маркерами полностью исключаются
        (кроме случая, когда искомый трек сам содержит этот маркер).
        """
        from .scoring import ratio
        from .markers import has_critical_marker, find_soft_markers, is_official_channel

        target_title = f"{track.primary_artist} {track.title}".strip()
        expected_dur = track.duration_sec
        ranked: list[tuple[float, dict]] = []

        for entry in entries:
            if not entry:
                continue
            title = entry.get("title") or ""
            # Жёсткий отсев по критическим bad-маркерам.
            if has_critical_marker(title, track.title):
                continue

            score = 0.0
            # 1. Сходство названия.
            title_sim = max(ratio(target_title, title), ratio(track.title, title))
            score += title_sim * 0.30

            # 2. Длительность — главный сигнал для выбора правильной версии.
            dur = entry.get("duration")
            if expected_dur and dur:
                diff = abs(expected_dur - dur)
                tolerance = self._duration_tolerance(25)
                if self.duration_tolerance and diff > tolerance:
                    continue
                if diff <= 4:
                    score += 40
                elif diff <= 8:
                    score += 28
                elif diff <= 15:
                    score += 12
                elif diff <= tolerance:
                    score += 4
                else:
                    # Сильно отличающаяся длительность — подозрение на другую версию.
                    score -= 35
            elif expected_dur and not dur:
                score -= 5  # нет длительности — небольшая неуверенность

            # 3. Бонус за официальный канал (VEVO, Topic, лейбл ...).
            uploader = entry.get("uploader") or entry.get("channel") or entry.get("uploader_id") or ""
            if is_official_channel(uploader):
                score += 15

            # 4. Мягкий штраф за «lyrics»/«audio»-видео (часто низкое качество).
            score -= 4 * len(find_soft_markers(title, track.title))

            ranked.append((score, entry))

        ranked.sort(key=lambda pair: pair[0], reverse=True)
        return ranked

    def _youtube_search_queries(self, primary_query: str, track: TrackMetadata) -> list[str]:
        """
        Формирует список поисковых запросов для YouTube.

        Если исполнитель указан латиницей, а кириллицы в запросе нет, добавляем
        транслитерированный вариант — русские треки часто лучше ищутся по
        оригинальному написанию (Zemfira → Земфира).
        """
        from .text_norm import has_cyrillic, translit_lat_to_cyr

        queries = [primary_query]
        if not has_cyrillic(primary_query):
            cyr = translit_lat_to_cyr(primary_query)
            if cyr and cyr.lower() != primary_query.lower():
                queries.append(cyr)
        return queries

    def _download_youtube_search(self, query: str, track: TrackMetadata) -> Optional[str]:
        """
        Ищет трек на YouTube и скачивает лучший кандидат.
        Ранжирование учитывает длительность, официальность канала и bad-маркеры,
        а не просто берёт первый результат — это критично для русскоязычных
        треков, где у одного произведения бывает 5+ версий (оригинал, ремиксы,
        лирик-видео, караоке).
        """
        try:
            try:
                from yt_dlp import YoutubeDL
            except ImportError:
                from youtube_dl import YoutubeDL

            temp_dir = self.temp_dir
            outtmpl = os.path.join(temp_dir, "%(id)s.%(ext)s")

            options = {
                "format": "bestaudio/best",
                "outtmpl": outtmpl,
                "quiet": True,
                "nooverwrites": True,
                "noplaylist": True,
                "nocheckcertificate": True,
            }
            options = self._add_cookie_options(options)

            with YoutubeDL(options) as ydl:
                # Объединяем кандидатов из нескольких запросов (оригинал +
                # транслитерация), чтобы ранжирование шло по полной выборке.
                all_entries: list[dict] = []
                seen_ids: set[str] = set()
                for q in self._youtube_search_queries(query, track):
                    print(f"[*] Searching YouTube for: '{q}'...")
                    try:
                        info = ydl.extract_info(f"ytsearch8:{q}", download=False)
                    except Exception as exc:
                        print(f"[!] YouTube search '{q}' failed: {exc}", file=sys.stderr)
                        continue
                    for entry in (info.get("entries") or []):
                        if not entry:
                            continue
                        eid = entry.get("id")
                        if eid and eid in seen_ids:
                            continue
                        if eid:
                            seen_ids.add(eid)
                        all_entries.append(entry)

                ranked = self._rank_youtube_entries(all_entries, track)
                if ranked:
                    # Отчитываемся о выборе для прозрачности.
                    best_score, best_entry = ranked[0]
                    dur = best_entry.get("duration")
                    uploader = best_entry.get("uploader") or best_entry.get("channel") or ""
                    print(
                        f"[*] Selected YouTube video: '{best_entry.get('title')}' "
                        f"({dur}s, score={best_score:.1f}, channel='{uploader}')"
                    )
                    webpage_url = (
                        best_entry.get("webpage_url")
                        or f"https://www.youtube.com/watch?v={best_entry.get('id')}"
                    )

                    download_info = ydl.extract_info(webpage_url, download=True)
                    video_id = (download_info or best_entry).get("id")
                    ext = (download_info or {}).get("ext") or "webm"
                    downloaded_path = os.path.join(temp_dir, f"{video_id}.{ext}")
                    if downloaded_path and is_valid_audio_file(downloaded_path):
                        return downloaded_path
                    print("[!] YouTube search download produced invalid audio, discarding...")
                    _safe_remove(downloaded_path)
                else:
                    print("[-] All YouTube candidates filtered out by bad markers.")
        except Exception as e:
            print(f"[!] YouTube search download failed: {e}", file=sys.stderr)
        return None

    def _download_soundcloud_search(self, query: str, track: TrackMetadata) -> Optional[str]:
        """
        Ищет трек на SoundCloud и скачивает лучший кандидат.
        Повторно использует _rank_youtube_entries — сигналы ранжирования
        (длительность, официальность канала, bad-маркеры) универсальны.
        """
        try:
            try:
                from yt_dlp import YoutubeDL
            except ImportError:
                from youtube_dl import YoutubeDL

            outtmpl = os.path.join(self.temp_dir, "%(id)s.%(ext)s")
            options = {
                "format": "bestaudio/best",
                "outtmpl": outtmpl,
                "quiet": True,
                "nooverwrites": True,
                "noplaylist": True,
                "nocheckcertificate": True,
            }

            with YoutubeDL(options) as ydl:
                all_entries: list[dict] = []
                seen_ids: set[str] = set()
                for q in self._youtube_search_queries(query, track):
                    print(f"[*] Searching SoundCloud for: '{q}'...")
                    try:
                        info = ydl.extract_info(f"scsearch8:{q}", download=False)
                    except Exception as exc:
                        print(f"[!] SoundCloud search '{q}' failed: {exc}", file=sys.stderr)
                        continue
                    for entry in (info.get("entries") or []):
                        if not entry:
                            continue
                        eid = entry.get("id")
                        if eid and eid in seen_ids:
                            continue
                        if eid:
                            seen_ids.add(eid)
                        all_entries.append(entry)

                ranked = self._rank_youtube_entries(all_entries, track)
                if ranked:
                    best_score, best_entry = ranked[0]
                    dur = best_entry.get("duration")
                    uploader = best_entry.get("uploader") or best_entry.get("channel") or ""
                    print(
                        f"[*] Selected SoundCloud track: '{best_entry.get('title')}' "
                        f"({dur}s, score={best_score:.1f}, channel='{uploader}')"
                    )
                    track_url = best_entry.get("url") or best_entry.get("webpage_url")
                    if track_url:
                        return self._download_soundcloud(track_url)
                else:
                    print("[-] All SoundCloud candidates filtered out by bad markers.")
        except Exception as e:
            print(f"[!] SoundCloud search download failed: {e}", file=sys.stderr)
        return None

    def _download_cover(self, track: TrackMetadata) -> Optional[str]:
        """
        Downloads cover art from track.cover_url to a temporary image file.
        """
        if not track.cover_url:
            return None
        
        try:
            print(f"[*] Downloading cover art: {track.cover_url}...")
            temp_file = tempfile.NamedTemporaryFile(dir=self.temp_dir, delete=False, suffix=".jpg")
            temp_path = temp_file.name
            temp_file.close()
            
            req = urllib.request.Request(track.cover_url, headers={"User-Agent": DEFAULT_UA})
            with safe_urlopen(req, timeout=30) as resp, open(temp_path, "wb") as f_out:
                f_out.write(resp.read())
            return temp_path
        except Exception as e:
            print(f"[!] Cover art download failed: {e}", file=sys.stderr)
            return None

    def _post_process(self, track: TrackMetadata, audio_path: str, cover_path: Optional[str], lyrics: Optional[str], target_dir: str) -> None:
        """
        Converts the audio to the requested format and applies metadata, cover art, and lyrics tags via ffmpeg.
        """
        safe_title = "".join(c for c in track.title if c.isalnum() or c in " -_().")
        safe_artist = "".join(c for c in track.primary_artist if c.isalnum() or c in " -_().")
        filename = f"{safe_artist} - {safe_title}.{self.format}"
        output_path = os.path.join(target_dir, filename)
        
        print(f"[*] Post-processing and converting to {self.format.upper()}...")
        
        # Build ffmpeg command
        cmd = ["ffmpeg", "-y", "-i", audio_path]
        if cover_path:
            cmd.extend(["-i", cover_path])
            cmd.extend(["-map", "0:a", "-map", "1:v"])
        else:
            cmd.extend(["-map", "0:a"])
            
        # Audio codec options
        if self.format == "mp3":
            cmd.extend(["-c:a", "libmp3lame", "-q:a", "2"])
            if cover_path:
                cmd.extend(["-c:v", "copy", "-id3v2_version", "3", "-metadata:s:v", "title=Album cover", "-metadata:s:v", "comment=Cover (front)"])
        elif self.format == "m4a":
            cmd.extend(["-c:a", "aac", "-b:a", "256k"])
            if cover_path:
                cmd.extend(["-c:v", "copy"])
        elif self.format == "flac":
            cmd.extend(["-c:a", "flac"])
            if cover_path:
                cmd.extend(["-c:v", "copy"])
                
        # Metadata tags
        cmd.extend(["-metadata", f"title={track.title}"])
        if track.artists:
            cmd.extend(["-metadata", f"artist={', '.join(track.artists)}"])
        if track.album:
            cmd.extend(["-metadata", f"album={track.album}"])
        if track.release_date:
            # Extract year from release_date (e.g. 2006-06-05)
            year = track.release_date.split("-")[0]
            cmd.extend(["-metadata", f"date={year}"])
            
        # Embed unsynced lyrics in metadata if available
        if lyrics:
            cmd.extend(["-metadata", f"lyrics={lyrics}"])
            cmd.extend(["-metadata", f"USLT={lyrics}"])
            cmd.extend(["-metadata", f"unsyncedlyrics={lyrics}"])
            
        cmd.append(output_path)
        
        # Run ffmpeg
        try:
            proc = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            if proc.returncode != 0:
                raise Exception(f"FFmpeg failed with exit code {proc.returncode}: {proc.stderr.decode('utf-8', errors='ignore')}")
            print(f"[+] Downloaded and processed: {output_path}")
        except Exception as e:
            raise RuntimeError(f"FFmpeg processing failed: {e}")
