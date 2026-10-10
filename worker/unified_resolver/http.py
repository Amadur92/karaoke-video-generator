from __future__ import annotations

import json
import os
import urllib.parse
import urllib.request
import urllib.error
from typing import Any, Optional

try:
    import certifi

    os.environ.setdefault("SSL_CERT_FILE", certifi.where())
except ImportError:
    pass


DEFAULT_UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36"


def safe_urlopen(req: Any, timeout: int = 10) -> Any:
    # TLS verification is deliberately never disabled here. Search metadata is
    # part of the verification chain; accepting a forged API response would be
    # worse than reporting a certificate configuration error to the user.
    return urllib.request.urlopen(req, timeout=timeout)


def get_json(url: str, headers: Optional[dict[str, str]] = None, timeout: int = 30) -> Any:
    req = urllib.request.Request(url, headers={"User-Agent": DEFAULT_UA, **(headers or {})})
    with safe_urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def post_json(url: str, payload: dict[str, Any], headers: Optional[dict[str, str]] = None, timeout: int = 30) -> Any:
    body = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=body,
        headers={"User-Agent": DEFAULT_UA, "Content-Type": "application/json", **(headers or {})},
    )
    with safe_urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))



def urlencode(params: dict[str, str]) -> str:
    return urllib.parse.urlencode(params)


PUBLIC_PAGE_HOSTS = {
    "www.youtube.com",
    "youtube.com",
    "music.youtube.com",
    "youtu.be",
    "www.jiosaavn.com",
    "jiosaavn.com",
    "www.deezer.com",
    "deezer.com",
    "open.spotify.com",
    "open.qobuz.com",
    "www.qobuz.com",
    "qobuz.com",
    "song.link",
    "album.link",
    "soundcloud.com",
    "www.soundcloud.com",
    "m.soundcloud.com",
    "tidal.com",
    "music.amazon.com",
    "lyrics.lyricfind.com",
    "www.shazam.com",
    "shazam.com",
    "music.apple.com",
    "itunes.apple.com",
    "genius.com",
    "www.musixmatch.com",
    "musixmatch.com",
}


def is_safe_public_page(url: str) -> bool:
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme not in {"http", "https"}:
        return False
    return (parsed.hostname or "").lower() in PUBLIC_PAGE_HOSTS


def check_public_url(url: str, timeout: int = 10) -> Optional[bool]:
    if not is_safe_public_page(url):
        return False
    headers = {"User-Agent": DEFAULT_UA}
    for method in ("HEAD", "GET"):
        try:
            req = urllib.request.Request(url, headers=headers, method=method)
            with safe_urlopen(req, timeout=timeout) as resp:
                return 200 <= resp.status < 400
        except urllib.error.HTTPError as exc:
            if exc.code in {401, 403, 405, 429}:
                # The resource may exist, but the generic preflight cannot
                # prove it. Preserve that distinction instead of reporting a
                # false positive.
                return None
            if exc.code in {404, 410}:
                return False
        except Exception:
            pass
    return False
