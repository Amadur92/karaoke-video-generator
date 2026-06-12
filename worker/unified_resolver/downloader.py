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


class Downloader:
    def __init__(self, output_dir: str = ".", format: str = "mp3"):
        self.output_dir = output_dir
        self.format = format.lower()
        if self.format not in {"mp3", "m4a", "flac"}:
            self.format = "mp3"
        self.temp_dir = os.path.join(self.output_dir, ".tmp")
        os.makedirs(self.temp_dir, exist_ok=True)

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
            if scored.score < 20:
                print(f"[#] Skipping candidate '{candidate.title}' due to low score ({scored.score} < 20).")
                continue
                
            print(f"[*] Trying candidate from '{candidate.source}' (Score: {scored.score})...")
            try:
                if candidate.source == "jiosaavn" and candidate.external_id:
                    temp_audio = self._download_jiosaavn(candidate.external_id)
                elif candidate.source == "qobuz-isrc" and candidate.external_id:
                    temp_audio = self._download_qobuz(candidate.external_id)
                elif "youtube" in candidate.source:
                    if candidate.public_url:
                        temp_audio = self._download_youtube(candidate.public_url)
                
                if temp_audio:
                    resolved_candidate = candidate
                    print(f"[+] Successfully downloaded stream from '{candidate.source}'!")
                    break
            except Exception as e:
                print(f"[!] Failed to download candidate from '{candidate.source}': {e}", file=sys.stderr)
                
        # Fallback to YouTube search if all candidates failed or no candidates were found
        if not temp_audio:
            print(f"[!] Direct candidate downloads failed or unavailable. Falling back to YouTube search...")
            query = f"{track.title} - {track.primary_artist} audio"
            try:
                temp_audio = self._download_youtube_search(query)
            except Exception as e:
                print(f"[!] YouTube search download failed: {e}", file=sys.stderr)
                
        if not temp_audio:
            print("[-] Error: Could not download audio stream from any source.", file=sys.stderr)
            return False
            
        try:
            # Determine downloaded audio duration and enrich track metadata
            actual_duration = get_audio_duration(temp_audio)
            if actual_duration:
                track.duration_sec = actual_duration
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
                    
                return temp_path
        except Exception as e:
            print(f"[!] JioSaavn download failed: {e}", file=sys.stderr)
            return None

    def _download_qobuz(self, track_id: str) -> Optional[str]:
        """
        Fetches Qobuz stream URL via WJHE community API and downloads it.
        """
        try:
            url = f"https://music.wjhe.top/api/music/qobuz/url?ID={track_id}&quality=1000&format=flac"
            req = urllib.request.Request(url, headers={"User-Agent": DEFAULT_UA})
            with safe_urlopen(req, timeout=10) as resp:
                res = json.loads(resp.read().decode("utf-8"))
                stream_url = res.get("url") or res.get("download_url")
                if not stream_url and "data" in res:
                    stream_url = res["data"].get("url") or res["data"].get("download_url")
                
                if not stream_url:
                    return None
                
                print(f"[+] Found Qobuz WJHE stream URL: {stream_url}")
                
                # Download to temp file
                temp_file = tempfile.NamedTemporaryFile(dir=self.temp_dir, delete=False, suffix=".flac")
                temp_path = temp_file.name
                temp_file.close()
                
                req_audio = urllib.request.Request(stream_url, headers={"User-Agent": DEFAULT_UA})
                with safe_urlopen(req_audio, timeout=30) as audio_resp, open(temp_path, "wb") as f_out:
                    f_out.write(audio_resp.read())
                    
                return temp_path
        except Exception as e:
            print(f"[!] Qobuz download failed: {e}", file=sys.stderr)
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
            
            print(f"[*] Downloading from YouTube URL: {video_url}...")
            with YoutubeDL(options) as ydl:
                info = ydl.extract_info(video_url, download=True)
                video_id = info.get("id")
                ext = info.get("ext")
                downloaded_path = os.path.join(temp_dir, f"{video_id}.{ext}")
                if os.path.exists(downloaded_path):
                    return downloaded_path
        except Exception as e:
            print(f"[!] YouTube download failed: {e}", file=sys.stderr)
        return None

    def _download_youtube_search(self, query: str) -> Optional[str]:
        """
        Searches YouTube (up to 5 results), filters out karaoke/instrumentals/covers if original search query doesn't have them,
        and downloads the first suitable result.
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
            
            # Simple check for critical words
            CRITICAL_BAD_MARKERS = [
                "karaoke", "instrumental", "minus", "minusovka", "backing track",
                "кавер", "cover", "минус", "караоке", "инструментал"
            ]
            
            print(f"[*] Searching YouTube for: '{query}'...")
            search_query = f"ytsearch5:{query}"
            with YoutubeDL(options) as ydl:
                info = ydl.extract_info(search_query, download=False)
                entries = info.get("entries") or []
                
                selected_entry = None
                for entry in entries:
                    if not entry:
                        continue
                    title_lower = (entry.get("title") or "").lower()
                    
                    # check if title has critical bad words
                    has_bad = False
                    for marker in CRITICAL_BAD_MARKERS:
                        # only flag as bad if the marker is NOT in the query
                        if marker in title_lower and marker not in query.lower():
                            has_bad = True
                            break
                    if not has_bad:
                        selected_entry = entry
                        break
                
                if not selected_entry and entries:
                    # if all had bad markers, fallback to first anyway
                    selected_entry = entries[0]
                
                if selected_entry:
                    video_id = selected_entry.get("id")
                    webpage_url = selected_entry.get("webpage_url") or f"https://www.youtube.com/watch?v={video_id}"
                    print(f"[*] Selected fallback YouTube video: '{selected_entry.get('title')}'")
                    
                    # Download the selected video
                    download_info = ydl.extract_info(webpage_url, download=True)
                    ext = download_info.get("ext") or "webm"  # fallback
                    downloaded_path = os.path.join(temp_dir, f"{video_id}.{ext}")
                    if os.path.exists(downloaded_path):
                        return downloaded_path
        except Exception as e:
            print(f"[!] YouTube search download failed: {e}", file=sys.stderr)
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
