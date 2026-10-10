"""Content identities and atomic metadata writes for reusable audio artifacts."""

import hashlib
import json
import os
import tempfile


def file_sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def alignment_identity(audio, lyrics, model, plain_lines, verify_lrc, separate_vocals,
                       acoustic_text_verification=True):
    return {
        "schema": 1, "algorithm": "alignment-v2",
        "audio_sha256": file_sha256(audio),
        "lyrics_sha256": hashlib.sha256(lyrics.encode("utf-8")).hexdigest(),
        "model": model, "plain_lines": bool(plain_lines),
        "verify_lrc": bool(verify_lrc), "separate_vocals": bool(separate_vocals),
        "acoustic_text_verification": bool(acoustic_text_verification),
    }


def atomic_write_json(path, payload):
    directory = os.path.dirname(os.path.abspath(path))
    os.makedirs(directory, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=".karaoke-", suffix=".json", dir=directory)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            json.dump(payload, output, ensure_ascii=False, indent=2)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.remove(temporary)
