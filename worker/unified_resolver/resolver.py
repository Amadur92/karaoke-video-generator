from __future__ import annotations

from .http import check_public_url, get_json, urlencode
from .itunes import enrich_from_itunes, is_weak_album
from .models import ProviderRun, ResolutionReport, ScoredCandidate, TrackMetadata
from .providers import Provider, default_providers
from .scoring import score_candidate


def _artist_matches_deezer(track: TrackMetadata, item: dict) -> bool:
    """
    Проверяет, что артист из выдачи Deezer действительно соответствует
    искомому. Раньше этого не было — и трек «Маша и Медведи - Любочка»
    обогащался из сборника «DJ Groove и все, все, все…», где артистом
    значился «DJ Groove», а длительность (318с) принадлежала чужой версии.
    Это напрямую ломало последующий duration-фильтр.
    """
    from .scoring import partial_word_match

    artist_val = (item.get("artist", {}) or {}).get("name") or ""
    if not artist_val:
        return False
    # Нормализованный word-match: «Маша и Медведи» vs «DJ Groove» → 0,
    # а vs «МАША И МЕДВЕДИ» → ~100. Порог 60 отсеивает чужие сборники.
    return partial_word_match(track.primary_artist, artist_val) >= 60


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
            # Фильтрация кандидатов:
            #   • по маркеру «remix» (как и раньше) — если ищем не ремикс, ремиксы пропускаем;
            #   • по артисту — отсекаем чужие сборники (например «DJ Groove и все, все…»),
            #     из-за которых enrichment привязывался к чужой длительности;
            #   • по weak-album — предпочитать студийные релизы сборникам/live/переизданиям
            #     («Новое и Лучшее», «Дискотека 80-х», «Greatest Hits»), у которых
            #     длительность часто отличается от оригинала (ремастер,radio edit).
            has_remix_in_query = "remix" in track.title.lower() or "remiks" in track.title.lower()

            strong_match = None
            fallback_match = None
            for item in results:
                title_lower = item.get("title", "").lower()
                is_remix = "remix" in title_lower or "remiks" in title_lower
                if not has_remix_in_query and is_remix:
                    continue

                album_title = (item.get("album", {}) or {}).get("title") or ""
                artist_ok = _artist_matches_deezer(track, item) if primary_artist else True

                # Студийный альбом нужного артиста = наилучший источник длительности.
                if artist_ok and not is_weak_album(album_title):
                    strong_match = item
                    break
                # Иначе запоминаем как fallback: возьмём, если ничего лучше нет.
                if fallback_match is None and artist_ok:
                    fallback_match = item

            best_match = strong_match or fallback_match
            # Если ничего не прошло фильтры — берём первый, как и раньше,
            # чтобы не ухудшать поведение для редких релизов.
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

        # iTunes fallback — хорошо индексирует русскоязычные релизы и закрывает
        # пробелы (длительность/обложка/альбом), которые Deezer не нашёл.
        if not track.duration_sec or not track.cover_url or not track.album:
            try:
                enrich_from_itunes(track)
            except Exception as exc:
                print(f"[-] iTunes enrichment failed for '{track.title}': {exc}")

        # MusicBrainz enrichment: перекрёсная проверка/обогащение duration_sec
        # по данным MusicBrainz. MusicBrainz хранит несколько recording-записей
        # для одного произведения (по альбомам), различает студийные релизы от
        # компиляций/live, и для русскоязычного каталога часто точнее Deezer.
        # Идёт ДО YouTube duration-validator, т.к. даёт более уверенный сигнал
        # (знает тип релиза, а не просто медиану поиска).
        try:
            from .musicbrainz import enrich_from_musicbrainz

            note = enrich_from_musicbrainz(track)
            if note and note.startswith("mb_overwrote_duration"):
                print(
                    f"[!] MusicBrainz: overwriting reference duration "
                    f"for '{track.title}' ({note})"
                )
            elif note and note.startswith("mb_set_duration"):
                print(f"[+] MusicBrainz: {note} for '{track.title}'")
        except Exception as exc:
            print(f"[-] MusicBrainz enrichment failed for '{track.title}': {exc}")

        # Перекрёсная валидация длительности: если enrichment привязался к
        # сборнику/live/переизданию, его duration может отличаться от реального
        # оригинала на 20–60%. В этом случае duration-фильтр ниже работал бы
        # против нас (штрафовал бы настоящий оригинал). Валидатор сравнивает
        # эталон с медианой топ-выдачи YouTube и при сильном расхождении
        # обнуляет duration_sec. Сетевые ошибки не ломают pipeline.
        if track.duration_sec:
            try:
                from .duration_check import DurationValidator

                note = DurationValidator().validate(track)
                if note and note.startswith("reference_duration_dropped"):
                    print(
                        f"[!] Duration validation: dropping reference duration "
                        f"for '{track.title}' ({note})"
                    )
            except Exception as exc:
                print(f"[-] Duration validation failed for '{track.title}': {exc}")

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
