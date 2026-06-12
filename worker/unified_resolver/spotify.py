from __future__ import annotations

import base64
import hashlib
import hmac
import re
import struct
import time
from typing import Optional

from .http import get_json, urlencode
from .models import TrackMetadata


SPOTIFY_TOTP_SECRET = "GM3TMMJTGYZTQNZVGM4DINJZHA4TGOBYGMZTCMRTGEYDSMJRHE4TEOBUG4YTCMRUGQ4DQOJUGQYTAMRRGA2TCMJSHE3TCMBY"
SPOTIFY_TOTP_VERSION = 61
SPOTIFY_BASE62 = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"


def extract_spotify_track_id(value: str) -> Optional[str]:
    value = value.strip()
    if re.fullmatch(r"[A-Za-z0-9]{22}", value):
        return value
    match = re.search(r"open\.spotify\.com/track/([A-Za-z0-9]{22})", value)
    if match:
        return match.group(1)
    match = re.search(r"spotify:track:([A-Za-z0-9]{22})", value)
    if match:
        return match.group(1)
    return None


def generate_totp(secret: str = SPOTIFY_TOTP_SECRET, timestep: int = 30, digits: int = 6) -> str:
    key = base64.b32decode(secret, casefold=True)
    counter = int(time.time() // timestep)
    msg = struct.pack(">Q", counter)
    digest = hmac.new(key, msg, hashlib.sha1).digest()
    offset = digest[-1] & 0x0F
    code = struct.unpack(">I", digest[offset : offset + 4])[0] & 0x7FFFFFFF
    return str(code % (10**digits)).zfill(digits)


def spotify_id_to_gid(track_id: str) -> str:
    value = 0
    for ch in track_id:
        value = value * 62 + SPOTIFY_BASE62.index(ch)
    return value.to_bytes(16, "big").hex()


class SpotifyResolver:
    def get_anonymous_token(self) -> str:
        code = generate_totp()
        params = urlencode(
            {
                "reason": "init",
                "productType": "web-player",
                "totp": code,
                "totpServer": code,
                "totpVer": str(SPOTIFY_TOTP_VERSION),
            }
        )
        data = get_json("https://open.spotify.com/api/token?" + params)
        return data["accessToken"]

    def enrich_from_spotify_id(self, track_id: str, fallback_title: str = "", fallback_artists: Optional[list[str]] = None) -> TrackMetadata:
        token = self.get_anonymous_token()
        gid = spotify_id_to_gid(track_id)
        try:
            track_data = get_json(
                f"https://spclient.wg.spotify.com/metadata/4/track/{gid}?market=from_token",
                headers={"Authorization": "Bearer " + token, "Accept": "application/json"},
            )
        except Exception:
            track_data = {}

        title = track_data.get("name") or fallback_title
        artists = [a.get("name") for a in track_data.get("artist", []) if a.get("name")]
        if not artists and fallback_artists:
            artists = fallback_artists
            
        duration_sec = None
        if track_data.get("duration"):
            duration_sec = round(track_data.get("duration") / 1000)
            
        album_data = track_data.get("album") or {}
        album_name = album_data.get("name")
        
        release_date = None
        date_data = album_data.get("date") or {}
        if date_data.get("year"):
            y = date_data.get("year")
            m = date_data.get("month", 1)
            d = date_data.get("day", 1)
            release_date = f"{y:04d}-{m:02d}-{d:02d}"

        cover_url = None
        cover_group = album_data.get("cover_group") or {}
        images = cover_group.get("image") or []
        large_image = None
        for img in images:
            if img.get("size") == "LARGE":
                large_image = img.get("file_id")
                break
        if not large_image and images:
            large_image = images[0].get("file_id")
        if large_image:
            cover_url = f"https://i.scdn.co/image/{large_image}"

        isrc = None
        for ext in track_data.get("external_id", []):
            if ext.get("type") == "isrc":
                isrc = ext.get("id")

        upc = None
        album_gid = album_data.get("gid")
        if album_gid:
            try:
                album_details = get_json(
                    f"https://spclient.wg.spotify.com/metadata/4/album/{album_gid}?market=from_token",
                    headers={"Authorization": "Bearer " + token, "Accept": "application/json"},
                )
                for ext in album_details.get("external_id", []):
                    if ext.get("type") == "upc":
                        upc = ext.get("id")
            except Exception:
                pass

        return TrackMetadata(
            title=title,
            artists=artists,
            duration_sec=duration_sec,
            album=album_name,
            release_date=release_date,
            spotify_id=track_id,
            spotify_url=f"https://open.spotify.com/track/{track_id}",
            isrc=isrc,
            upc=upc,
            cover_url=cover_url,
        )
