"""Run with python -m unittest discover -s scripts. No GPU or model files needed."""
import contextlib
from concurrent.futures import ThreadPoolExecutor
import os
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import Mock, patch

import local_models
import gpu_worker
from whisper_gpu import WhisperGPU


class TranscriptionPriorityTests(unittest.TestCase):
    def test_final_cancels_the_running_preview_and_rejects_new_previews(self):
        models = local_models.Recognizers()
        cancelled = threading.Event()
        models.worker = Mock(preview_cancelled=cancelled)
        final_started = threading.Event()
        release_final = threading.Event()

        def final():
            with models.gpu_slot():
                final_started.set()
                return release_final.wait(3)

        with ThreadPoolExecutor(max_workers=1) as executor:
            try:
                with models.gpu_slot(preview=True):
                    job = executor.submit(final)
                    self.assertTrue(cancelled.wait(3))
                    self.assertFalse(final_started.is_set())
                    with self.assertRaises(local_models.PreviewSuperseded), models.gpu_slot(preview=True):
                        self.fail("A waiting final must take precedence over a preview")
                self.assertTrue(final_started.wait(3))
                with self.assertRaises(local_models.PreviewSuperseded), models.gpu_slot(preview=True):
                    self.fail("Preview must not run alongside a final")
            finally:
                release_final.set()
            self.assertTrue(job.result(timeout=3))
        with models.gpu_slot(preview=True):
            self.assertFalse(cancelled.is_set())


class VocabularyPromptTests(unittest.TestCase):
    def test_whisper_files_in_one_directory_are_independent_choices(self):
        with tempfile.TemporaryDirectory() as root:
            folder = Path(root) / "whisper"
            folder.mkdir()
            (folder / "ggml-small.bin").touch()
            (folder / "ggml-large-v3-turbo-q5_0.bin").touch()
            with patch.dict(os.environ, {"SPEAKY_MODELS_DIR": root}):
                catalog = local_models.discover()
            self.assertEqual(catalog["Whisper Small"], folder)
            self.assertEqual(catalog["Whisper Large v3 Turbo (Q5)"], folder)
            self.assertNotIn("Whisper Base", catalog)

    def test_selected_whisper_uses_its_own_weight(self):
        with (
            patch("whisper_gpu.installed", return_value=True) as installed,
            patch("whisper_gpu.WhisperGPU") as runner,
        ):
            gpu_worker._open_model("Whisper Small", Path("whisper"), 0)
            installed.assert_called_once_with(Path("whisper"), "ggml-small.bin")
            runner.assert_called_once_with(Path("whisper"), 0, "ggml-small.bin")

    def test_model_selection_reports_decoders_that_accept_prompts(self):
        expected = {
            "Qwen3-ASR-0.6B": True, "Qwen3-ASR-1.7B": True,
            local_models.WHISPER: True, "Whisper Small": True,
            local_models.SENSE: False,
        }
        with (
            tempfile.TemporaryDirectory() as profile,
            patch.dict(os.environ, {"SPEAKY_CONFIG_DIR": profile}),
            patch.object(local_models, "startup_lock", return_value=contextlib.nullcontext()),
            patch.object(local_models, "connection", return_value=("http://127.0.0.1:12345", "test")),
            patch.object(local_models, "request_json"),
        ):
            catalog = dict.fromkeys(expected, Path(profile))
            for model, enabled in expected.items():
                with self.subTest(model=model):
                    endpoint = local_models.ensure(model, 123, catalog)
                    self.assertEqual(endpoint["use_vocabulary_prompt"], enabled)

    def test_whisper_carries_terms_and_clears_them_on_the_next_utterance(self):
        import numpy as np
        runner = WhisperGPU.__new__(WhisperGPU)
        runner._context = None
        runner._run = Mock(return_value=0)
        runner._count = Mock(return_value=0)
        for hint in ("Agent, RAG, 大模型", ""):
            runner.transcribe(np.zeros(8000, dtype=np.float32), 16000, "auto", hint)
            self.assertEqual(runner._run.call_args.args[-2:], (b"auto", hint.encode()))


if __name__ == "__main__":
    unittest.main()
