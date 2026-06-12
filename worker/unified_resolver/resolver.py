from __future__ import annotations

from .http import check_public_url, get_json, urlencode
from .models import ProviderRun, ResolutionReport, ScoredCandidate, TrackMetadata
from .providers import Provider, default_providers
from .scoring import score_candidate


def enrich_from_deezer(track: TrackMetadata) -> None:
    """
    Enriches TrackMetadata (duration, ISRC, album, cover_url) using Deezer public search API
    if they are not already set.
    """
    if not track.title:
        return
    
    primary_artist = track.primary_artist
    query = f'artist:"{primary_artist}" track:"{track.title}"' if primary_artist else track.title
    
    try:
        url = "https://api.deezer.com/search?" + urlencode({"q": query})
        data = get_json(url)
        results = data.get("data", [])
        
        if not results and primary_artist:
            # Fallback to simple query if strict search yields no results
            query_simple = f"{primary_artist} {track.title}"
            url = "https://api.deezer.com/search?" + urlencode({"q": query_simple})
            data = get_json(url)
            results = data.get("data", [])
            
        if results:
            # Filter out remixes if original title doesn't mention "remix"
            has_remix_in_query = "remix" in track.title.lower() or "remiks" in track.title.lower()
            
            best_match = None
            for item in results:
                title_lower = item.get("title", "").lower()
                is_remix = "remix" in title_lower or "remiks" in title_lower
                
                # If query doesn't have "remix", prefer non-remix results
                if not has_remix_in_query and is_remix:
                    continue
                best_match = item
                break
                
            # If all results were filtered out, just take the first one
            if not best_match:
                best_match = results[0]
                
            if not track.duration_sec and best_match.get("duration"):
                track.duration_sec = best_match.get("duration")
            if not track.isrc and best_match.get("isrc"):
                track.isrc = best_match.get("isrc")
            if not track.album and best_match.get("album", {}).get("title"):
                track.album = best_match.get("album", {}).get("title")
            if not track.cover_url and best_match.get("album", {}).get("cover_big"):
                track.cover_url = best_match.get("album", {}).get("cover_big")
    except Exception as e:
        print(f"[-] Failed to enrich track '{track.title}' from Deezer: {e}")


class Resolver:
    def __init__(self, providers: list[Provider] | None = None):
        self.providers = providers or list(default_providers())

    def candidates(self, track: TrackMetadata, limit_per_provider: int = 5) -> list[ScoredCandidate]:
        return self.resolve(track, limit_per_provider=limit_per_provider, check_urls=False).candidates

    def resolve(
        self,
        track: TrackMetadata,
        limit_per_provider: int = 5,
        check_urls: bool = True,
    ) -> ResolutionReport:
        # Automatically enrich metadata from Deezer if needed
        if not track.isrc or not track.duration_sec:
            enrich_from_deezer(track)

        scored: list[ScoredCandidate] = []
        provider_runs: list[ProviderRun] = []
        for provider in self.providers:
            try:
                candidates = provider.search(track, limit=limit_per_provider)
            except Exception as exc:
                provider_runs.append(ProviderRun(provider=provider.name, status="error", error=str(exc)))
                continue
            for candidate in candidates:
                if check_urls and candidate.public_url:
                    candidate.public_url_ok = check_public_url(candidate.public_url)
            scored.extend(score_candidate(track, candidate) for candidate in candidates)
            provider_runs.append(ProviderRun(provider=provider.name, status="ok", candidates=len(candidates)))
        return ResolutionReport(
            track=track,
            candidates=sorted(scored, key=lambda item: item.score, reverse=True),
            provider_runs=provider_runs,
        )
