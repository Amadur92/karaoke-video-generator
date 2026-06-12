from __future__ import annotations

from dataclasses import dataclass, field
from typing import Optional


@dataclass
class TrackMetadata:
    title: str
    artists: list[str]
    duration_sec: Optional[int] = None
    album: Optional[str] = None
    release_date: Optional[str] = None
    spotify_id: Optional[str] = None
    spotify_url: Optional[str] = None
    isrc: Optional[str] = None
    upc: Optional[str] = None
    cover_url: Optional[str] = None

    @property
    def primary_artist(self) -> str:
        return self.artists[0] if self.artists else ""

    @property
    def query_text(self) -> str:
        if self.primary_artist:
            return f"{self.title} - {self.primary_artist}"
        return self.title


@dataclass
class Candidate:
    source: str
    title: str
    artists: list[str] = field(default_factory=list)
    duration_sec: Optional[int] = None
    public_url: Optional[str] = None
    external_id: Optional[str] = None
    album: Optional[str] = None
    quality_hint: Optional[str] = None
    raw_rank: int = 0
    next_step: Optional[str] = None
    blocked_at: Optional[str] = None
    route_kind: str = "metadata"
    public_url_ok: Optional[bool] = None


@dataclass
class ScoredCandidate:
    candidate: Candidate
    score: float
    title_score: float
    artist_score: float
    duration_score: float
    album_score: float
    flags: list[str] = field(default_factory=list)


@dataclass
class ProviderRun:
    provider: str
    status: str
    candidates: int = 0
    error: Optional[str] = None


@dataclass
class ResolutionReport:
    track: TrackMetadata
    candidates: list[ScoredCandidate]
    provider_runs: list[ProviderRun]
