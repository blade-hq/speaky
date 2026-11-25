import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
import wave

from asr_adapters_light import load_adapter


class LocalAdapterTests(unittest.TestCase):
    def test_vosk_keeps_each_endpoint_and_the_pending_tail(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "am").mkdir()
            (root / "am/final.mdl").touch()
            audio = root / "three-chunks.wav"
            with wave.open(str(audio), "wb") as wav:
                wav.setparams((1, 2, 16000, 0, "NONE", ""))
                wav.writeframes(bytes(9000 * 2))

            sdk = Mock()
            recognizer = sdk.KaldiRecognizer.return_value
            recognizer.AcceptWaveform.side_effect = [True, True, False]
            recognizer.Result.side_effect = [json.dumps({"text": s}) for s in ("first", "second")]
            recognizer.FinalResult.return_value = json.dumps({"text": "last"})
            with patch.dict(sys.modules, vosk=sdk):
                transcribe = load_adapter("vosk", root, {})
                self.assertEqual(transcribe(str(audio), "zh"), "first second last")
            sdk.Model.assert_called_once_with(model_path=str(root.resolve()))
            self.assertEqual(recognizer.Result.call_count, 2)
            recognizer.FinalResult.assert_called_once()

    def test_incomplete_model_folder_does_not_contact_hub(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for framework in ("funasr", "sherpa-onnx", "vosk", "wenet"):
                with self.subTest(framework=framework), self.assertRaises(ValueError):
                    load_adapter(framework, root, {})


if __name__ == "__main__":
    unittest.main()
