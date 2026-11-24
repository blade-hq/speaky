import io
import os
import tempfile
import unittest
import wave
from pathlib import Path
from unittest.mock import patch

from fastapi.testclient import TestClient

from asr_bridge import create_app, model_identity


def sample(rate=16000):
    audio = io.BytesIO()
    with wave.open(audio, "wb") as stream:
        stream.setparams((1, 2, rate, 0, "NONE", ""))
        stream.writeframes(b"\x00\x00" * 16)
    return audio.getvalue()


class BridgeTest(unittest.TestCase):
    def setUp(self):
        self.calls = []

        def recognize(path, language):
            self.calls.append((path, language, Path(path).read_bytes()))
            return "测试文本"

        self.client = TestClient(create_app("vosk", "cn-small", recognize, token="secret"))
        self.client.headers["Authorization"] = "Bearer secret"

    def submit(self, data=None, model="cn-small"):
        return self.client.post("/v1/audio/transcriptions", data={"model": model, "language": "zh"},
                                files={"file": ("recording.wav", sample() if data is None else data, "audio/wav")})

    def test_model_catalog_and_transcript_agree(self):
        entry, = self.client.get("/v1/models").json()["data"]
        result = self.submit()
        self.assertEqual(result.status_code, 200, result.text)
        self.assertEqual(result.json(), {"text": "测试文本", "framework": entry["framework"], "model": entry["id"]})
        path, language, received = self.calls[0]
        self.assertEqual((language, received), ("zh", sample()))
        self.assertFalse(Path(path).exists())

    def test_preset_alias_preserves_canonical_identity(self):
        self.client = TestClient(create_app("funasr", "SenseVoiceSmall", lambda *_: "你好", model_alias="funasr"))
        entry, = self.client.get("/v1/models").json()["data"]
        self.assertEqual((entry["id"], entry["aliases"]), ("SenseVoiceSmall", ["funasr"]))
        for name in ("funasr", "SenseVoiceSmall"):
            self.assertEqual(self.submit(model=name).json(),
                             {"text": "你好", "framework": "funasr", "model": "SenseVoiceSmall"})

    def test_model_directory_defaults_and_explicit_names(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder)
            with patch.dict(os.environ, {"SPEAKY_MODELS_DIR": str(path / "models")}):
                preset = path / "models" / "funasr-model"
                self.assertEqual(model_identity("funasr", preset), ("SenseVoiceSmall", "funasr"))
                self.assertEqual(model_identity("funasr", preset, "My fine-tuned model"), ("My fine-tuned model", None))
                self.assertEqual(model_identity("vosk", preset), ("funasr-model", None))
                self.assertEqual(model_identity("funasr", path / "funasr-model"), ("funasr-model", None))
                self.assertEqual(model_identity("vosk", path / "Chinese 0.24"), ("Chinese 0.24", None))

    def test_wrong_model_and_invalid_audio_never_reach_sdk(self):
        self.assertEqual(self.submit(model="wrong").status_code, 404)
        self.assertEqual(self.submit(model="vosk").status_code, 404)
        for audio in (b"not audio", sample(8000), sample()[:-2]):
            self.assertEqual(self.submit(audio).status_code, 400)
        self.assertEqual(self.calls, [])

    def test_catalog_and_transcription_require_configured_token(self):
        self.client.headers.pop("Authorization")
        self.assertEqual(self.client.get("/v1/models").status_code, 401)
        self.assertEqual(self.submit().status_code, 401)
        self.assertEqual(self.calls, [])


if __name__ == "__main__":
    unittest.main()
