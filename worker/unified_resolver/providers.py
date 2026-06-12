from __future__ import annotations

from abc import ABC, abstractmethod
import hashlib
import time
from typing import Iterable
import urllib.error
import urllib.parse

from .http import get_json, urlencode
from .models import Candidate, TrackMetadata


class Provider(ABC):
    name: str

    @abstractmethod
    def search(self, track: TrackMetadata, limit: int = 5) -> list[Candidate]:
        raise NotImplementedError


class JioSaavnProvider(Provider):
    name = "jiosaavn"

    def search(self, track: TrackMetadata, limit: int = 5) -> list[Candidate]:
        params = urlencode(
            {
                "__call": "autocomplete.get",
                "_format": "json",
                "_marker": "0",
                "cc": "in",
                "includeMetaTags": "1",
                "query": track.title,
            }
        )
        data = get_json("https://www.jiosaavn.com/api.php?" + params)
        songs = (data.get("songs") or {}).get("data") or []
        candidates: list[Candidate] = []
        for idx, item in enumerate(songs[:limit], 1):
            more = item.get("more_info") or {}
            artist_text = more.get("primary_artists") or more.get("singers") or ""
            candidates.append(
                Candidate(
                    source=self.name,
                    title=item.get("title") or "",
                    artists=[part.strip() for part in artist_text.split(",") if part.strip()],
                    public_url=item.get("url") or item.get("perma_url"),
                    external_id=item.get("id"),
                    album=more.get("album"),
                    quality_hint="up to 160/320 kbps if matched",
                    raw_rank=idx,
                    next_step="Would call JioSaavn song.getDetails and decrypt media_url",
                    blocked_at="direct media_url resolution",
                    route_kind="resolvable_blocked",
                )
            )
        return candidates


class DeezerISRCProvider(Provider):
    name = "deezer-isrc"

    def search(self, track: TrackMetadata, limit: int = 5) -> list[Candidate]:
        if not track.isrc:
            return []
        data = get_json(f"https://api.deezer.com/track/isrc:{track.isrc.upper()}")
        if data.get("error") or not data.get("id"):
            return []

        artist = data.get("artist") or {}
        album = data.get("album") or {}
        return [
            Candidate(
                source=self.name,
                title=data.get("title") or "",
                artists=[artist.get("name", "")] if artist.get("name") else [],
                duration_sec=data.get("duration"),
                public_url=data.get("link"),
                external_id=str(data.get("id")),
                album=album.get("title"),
                quality_hint="metadata/public page only",
                raw_rank=1,
                next_step="Deezer match by ISRC found; this prototype does not implement Deezer audio retrieval",
                blocked_at="audio source unsupported in this combined resolver",
                route_kind="metadata_only",
            )
        ]


class QobuzISRCProvider(Provider):
    name = "qobuz-isrc"
    app_id = "712109809"
    secret = "589be88e4538daea11f509d29e4a23b1"

    def search(self, track: TrackMetadata, limit: int = 5) -> list[Candidate]:
        if not track.isrc:
            return []
        params = {"query": track.isrc, "limit": str(limit)}
        ts = str(int(time.time()))
        signed = {
            **params,
            "app_id": self.app_id,
            "request_ts": ts,
            "request_sig": self._signature("track/search", params, ts),
        }
        data = get_json("https://www.qobuz.com/api.json/0.2/track/search?" + urlencode(signed))
        items = (data.get("tracks") or {}).get("items") or []
        candidates: list[Candidate] = []
        for idx, item in enumerate(items[:limit], 1):
            performers = item.get("performer") or {}
            album = item.get("album") or {}
            public_url = item.get("url") or item.get("relative_url")
            if public_url and public_url.startswith("/"):
                public_url = "https://www.qobuz.com" + public_url
            if not public_url and item.get("id"):
                public_url = f"https://open.qobuz.com/track/{item.get('id')}"
            candidates.append(
                Candidate(
                    source=self.name,
                    title=item.get("title") or "",
                    artists=[performers.get("name", "")] if performers.get("name") else [],
                    duration_sec=item.get("duration"),
                    public_url=public_url,
                    external_id=str(item.get("id")) if item.get("id") else None,
                    album=album.get("title"),
                    quality_hint=self._quality_hint(item),
                    raw_rank=idx,
                    next_step="Would call a Qobuz download provider with the matched track id",
                    blocked_at="direct download/provider stream URL resolution",
                    route_kind="resolvable_blocked",
                )
            )
        return candidates

    def _signature(self, path: str, params: dict[str, str], ts: str) -> str:
        normalized_path = path.strip("/").replace("/", "")
        payload = normalized_path
        for key in sorted(params):
            payload += key + str(params[key])
        payload += ts + self.secret
        return hashlib.md5(payload.encode("utf-8")).hexdigest()

    def _quality_hint(self, item: dict) -> str:
        depth = item.get("maximum_bit_depth")
        rate = item.get("maximum_sampling_rate")
        if depth and rate:
            return f"up to {depth}-bit/{rate} kHz if downloadable"
        return "Qobuz catalog match if available"


class SonglinkProvider(Provider):
    name = "songlink"

    def search(self, track: TrackMetadata, limit: int = 5) -> list[Candidate]:
        if not track.spotify_url:
            return []
        try:
            data = get_json(
                "https://api.song.link/v1-alpha.1/links?"
                + urlencode({"url": track.spotify_url, "userCountry": "US"})
            )
        except urllib.error.HTTPError:
            return []

        links = data.get("linksByPlatform") or {}
        candidates: list[Candidate] = []
        for platform in ("tidal", "amazonMusic", "deezer", "youtubeMusic", "youtube", "qobuz"):
            link = links.get(platform) or {}
            url = link.get("url")
            if not url:
                continue
            candidates.append(
                Candidate(
                    source=f"{self.name}:{platform}",
                    title=track.title,
                    artists=track.artists,
                    duration_sec=track.duration_sec,
                    public_url=url,
                    external_id=link.get("entityUniqueId"),
                    album=track.album,
                    quality_hint="cross-platform public page",
                    raw_rank=len(candidates) + 1,
                    next_step=f"Would pass this {platform} public page to the provider-specific resolver",
                    blocked_at="provider-specific direct media/download URL resolution",
                    route_kind="resolvable_blocked" if platform in {"tidal", "amazonMusic", "youtubeMusic", "youtube", "qobuz"} else "metadata_only",
                )
            )
            if len(candidates) >= limit:
                break
        return candidates


class YouTubeSearchProvider(Provider):
    name = "youtube"

    def search(self, track: TrackMetadata, limit: int = 5) -> list[Candidate]:
        try:
            try:
                from yt_dlp import YoutubeDL
            except ImportError:
                from youtube_dl import YoutubeDL
        except Exception:
            return []

        query = f"ytsearch{limit}:{track.query_text}"
        options = {
            "quiet": True,
            "skip_download": True,
            "extract_flat": True,
            "noplaylist": True,
            "ignoreerrors": True,
            "nocheckcertificate": True,
        }
        with YoutubeDL(options) as ydl:
            info = ydl.extract_info(query, download=False)

        candidates: list[Candidate] = []
        for idx, item in enumerate((info or {}).get("entries") or [], 1):
            if not item:
                continue
            video_id = item.get("id")
            candidates.append(
                Candidate(
                    source=self.name,
                    title=item.get("title") or "",
                    artists=[item.get("uploader") or ""],
                    duration_sec=round(item.get("duration") or 0) or None,
                    public_url=f"https://www.youtube.com/watch?v={video_id}" if video_id else item.get("webpage_url"),
                    external_id=video_id,
                    quality_hint="lossy YouTube candidate",
                    raw_rank=idx,
                    next_step="Would call yt-dlp/youtube-api-dl format extraction and then audio conversion",
                    blocked_at="direct YouTube media format URL resolution",
                    route_kind="resolvable_blocked",
                )
            )
        return candidates


def default_providers() -> Iterable[Provider]:
    return [
        DeezerISRCProvider(),
        QobuzISRCProvider(),
        SonglinkProvider(),
        JioSaavnProvider(),
        YouTubeSearchProvider(),
    ]
