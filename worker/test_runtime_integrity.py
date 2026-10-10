"""Regression tests for failed encodes, stale caches and batch stop behavior."""
import contextlib
import io
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from cache_identity import alignment_identity, atomic_write_json, file_sha256
from render_output import finish_encoder, validate_video
import karaoke_worker as worker
import audio_loudness


class RuntimeIntegrityTests(unittest.TestCase):
    def test_encoder_failure_keeps_previous_export_and_reports_stderr(self):
        with tempfile.TemporaryDirectory() as root:
            destination = Path(root) / "out.mp4"
            temporary = Path(root) / "part.mp4"
            destination.write_bytes(b"previous export")
            temporary.write_bytes(b"incomplete")
            process = SimpleNamespace(stdin=io.BytesIO(), wait=lambda: 9)
            with self.assertRaisesRegex(RuntimeError, "кодом 9.*encoder failed"):
                finish_encoder(process, io.BytesIO(b"encoder failed"), str(temporary), str(destination), 2.0)
            self.assertEqual(destination.read_bytes(), b"previous export")

    def test_invalid_video_does_not_replace_export_even_with_zero_exit(self):
        with tempfile.TemporaryDirectory() as root:
            destination = Path(root) / "out.mp4"
            destination.write_bytes(b"previous")
            process = SimpleNamespace(stdin=io.BytesIO(), wait=lambda: 0)
            with self.assertRaisesRegex(RuntimeError, "непустой"):
                finish_encoder(process, io.BytesIO(), str(Path(root) / "missing.mp4"), str(destination), 2.0)
            self.assertEqual(destination.read_bytes(), b"previous")

    def test_identity_changes_for_audio_text_model_and_options(self):
        with tempfile.TemporaryDirectory() as root:
            audio = Path(root) / "audio.wav"
            audio.write_bytes(b"audio A")
            def identity(**changes):
                args = dict(audio=str(audio), lyrics="first text", model="base", plain_lines=False, verify_lrc=True, separate_vocals=False)
                args.update(changes)
                return alignment_identity(**args)
            first = identity()
            audio.write_bytes(b"audio B")
            self.assertNotEqual(first, identity())
            audio.write_bytes(b"audio A")
            self.assertEqual(first, identity())
            for changes in [dict(lyrics="other text"), dict(model="small"), dict(plain_lines=True), dict(verify_lrc=False), dict(separate_vocals=True)]:
                self.assertNotEqual(first, identity(**changes))

    def test_report_records_provenance_and_timings_content(self):
        with tempfile.TemporaryDirectory() as root:
            timings = str(Path(root) / "timings.json")
            identity = {"schema": 1}
            with patch.object(worker, "jobs", {"test": {"cache_identity": identity}}):
                worker.write_timings_and_report("test", [{"text": "hello", "start": 0, "end": 1, "words": []}], timings_output=timings, audio_duration=1)
            report = json.loads(Path(worker.alignment_report_path_for_timings(timings)).read_text())
            self.assertEqual(report["cache_identity"], identity)
            self.assertEqual(report["timings_sha256"], file_sha256(timings))

    def test_graceful_stop_does_not_begin_next_alignment(self):
        with tempfile.TemporaryDirectory() as root:
            stop = Path(root) / "stop"
            lyrics = Path(root) / "lyrics.txt"
            lyrics.write_text("hello")
            queue = Path(root) / "queue.json"
            atomic_write_json(queue, [dict(index=i, audio="unused", artist="A", title="T", lyrics_file=str(lyrics), timings_output="unused") for i in range(3)])
            called = []
            def generate(**kwargs):
                called.append(kwargs["job_id"])
                stop.touch()
            with patch.object(worker, "generate_karaoke_thread", generate), patch.object(worker, "jobs", {}), contextlib.redirect_stdout(io.StringIO()):
                worker.run_batch_align(str(queue), "base", stop_after_current_file=str(stop))
            self.assertEqual(called, ["batch_job_0"])

    def test_batch_does_not_turn_worker_error_into_success(self):
        with tempfile.TemporaryDirectory() as root:
            lyrics = Path(root) / "lyrics.txt"
            lyrics.write_text("hello")
            queue = Path(root) / "queue.json"
            atomic_write_json(queue, [dict(index=0, audio="unused", artist="A", title="T", lyrics_file=str(lyrics), timings_output="unused")])
            def fail(**kwargs):
                worker.jobs[kwargs["job_id"]]["error"] = "alignment failed"
            output = io.StringIO()
            with patch.object(worker, "generate_karaoke_thread", fail), patch.object(worker, "jobs", {}), contextlib.redirect_stdout(output):
                worker.run_batch_align(str(queue), "base")
                final = worker.jobs["batch_job_0"].copy()
            self.assertTrue(final["done"])
            self.assertEqual(final["error"], "alignment failed")
            self.assertIn("Ошибка", final["status"])

    def test_demucs_rejects_old_audio_with_equal_duration(self):
        with tempfile.TemporaryDirectory() as root:
            audio = Path(root) / "audio.wav"
            vocals = Path(root) / "vocals.wav"
            audio.write_bytes(b"audio A")
            vocals.write_bytes(b"cached" * 1024)
            atomic_write_json(str(vocals) + ".cache.json", {"schema": 1, "algorithm": "demucs-mdx_q-v1", "audio_sha256": file_sha256(audio)})
            audio.write_bytes(b"audio B")
            with patch.object(worker, "vocal_cache_path_for_audio", return_value=str(vocals)), patch.object(worker, "audio_duration_seconds", return_value=30), patch.object(worker, "find_demucs_command", side_effect=RuntimeError("fresh separation required")):
                with self.assertRaisesRegex(RuntimeError, "fresh separation required"):
                    worker.separate_vocals_with_demucs(str(audio))

    def test_loudness_scan_cache_is_independent_of_target(self):
        with tempfile.TemporaryDirectory() as root:
            cache = str(Path(root) / "analysis.txt")
            report = "track_gain = -6.00 dB\ntrack_peak = 0.8"
            with patch.object(audio_loudness, "analysis_cache_path", return_value=cache), patch.object(audio_loudness, "scan_loudness", return_value=report) as scan:
                self.assertIn("volume=-6.000000dB", audio_loudness.normalization_filter("unused", 89))
                self.assertIn("volume=-3.000000dB", audio_loudness.normalization_filter("unused", 92))
                self.assertEqual(scan.call_count, 1)


if __name__ == "__main__":
    unittest.main()
