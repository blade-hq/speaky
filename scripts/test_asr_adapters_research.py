"""Check local model/archive contracts without requiring optional speech SDKs."""
import subprocess
import sys
import tempfile
import unittest
import wave
from pathlib import Path
from unittest.mock import Mock, patch

import asr_adapters_research as adapters


class AdapterContracts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="speaky asr ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()

    def test_kaldi_uses_separate_archives_and_decodes_utf8_symbol_ids(self):
        (self.root / "conf").mkdir()
        (self.root / "graph").mkdir()
        for name in ("final.mdl", "conf/online.conf", "graph/HCLG.fst"):
            (self.root / name).touch()
        (self.root / "graph/words.txt").write_text("<eps> 0\n你好 7\nworld 5\n", encoding="utf8")
        original = self.root / "有空格的 录音.wav"
        original.write_bytes(b"isolated PCM fixture")

        def run(argv, **kwargs):
            self.assertEqual(kwargs["cwd"], self.root)
            self.assertEqual(kwargs["timeout"], 21)
            self.assertIn("--acoustic-scale=0.5", argv)
            if argv[0] == "decoder":
                self.assertEqual(argv[-5:-3], [str(self.root / "final.mdl"), str(self.root / "graph/HCLG.fst")])
                self.assertIn("--frame-subsampling-factor=1", argv)
                self.assertEqual(Path(argv[-3].removeprefix("ark:")).read_bytes(), b"spk utt\n")
                entry = Path(argv[-2].removeprefix("scp:")).read_text(encoding="utf8").strip()
                key, wav = entry.split(" ", 1)
                self.assertEqual(key, "utt")
                self.assertEqual(Path(wav).name, "input.wav")
                self.assertEqual(Path(wav).read_bytes(), original.read_bytes())
                Path(argv[-1].removeprefix("ark:")).write_bytes(b"lattice fixture")
                return subprocess.CompletedProcess(argv, 0, b"", b"")
            self.assertEqual(argv[0], "best-path")
            self.assertEqual(argv[-1], "ark,t:-")
            self.assertEqual(Path(argv[-2].removeprefix("ark:")).read_bytes(), b"lattice fixture")
            return subprocess.CompletedProcess(argv, 0, b"utt 7 5\n", b"")

        with patch.object(adapters, "_kaldi_program", side_effect=["decoder", "best-path"]):
            recognize = adapters.load_adapter("kaldi", self.root, {
                "acoustic_scale": 0.5, "timeout": 21, "frame_subsampling_factor": 1,
            })
        with patch.object(subprocess, "run", side_effect=run):
            self.assertEqual(recognize(str(original), "auto"), "你好 world")

    def test_espnet_passes_normalized_mono_waveform_and_uses_first_hypothesis(self):
        import numpy as np

        (self.root / "config.yaml").touch()
        (self.root / "asr.pth").touch()
        wav_path = self.root / "录音.wav"
        with wave.open(str(wav_path), "wb") as wav:
            wav.setparams((1, 2, 16000, 0, "NONE", "not compressed"))
            wav.writeframes(np.array([-32768, 0, 16384], dtype="<i2").tobytes())
        module = Mock()
        decoder = module.Speech2Text.return_value
        decoder.return_value = [("  测试 A  ", [], [], object())]
        with patch.dict(sys.modules, {"espnet2.bin.asr_inference": module}):
            recognize = adapters.load_adapter("espnet", self.root, {})
        self.assertEqual(recognize(str(wav_path), "auto"), "测试 A")
        samples = decoder.call_args.args[0]
        self.assertEqual(samples.dtype, np.float32)
        np.testing.assert_array_equal(samples, [-1.0, 0.0, 0.5])

    def test_nemo_rejects_ambiguous_checkpoint_before_importing_sdk(self):
        (self.root / "a.nemo").touch()
        (self.root / "b.nemo").touch()
        with self.assertRaisesRegex(ValueError, "options.checkpoint"):
            adapters.load_adapter("nemo", self.root, {})

    def test_speechbrain_receives_forward_slashes_for_local_audio(self):
        (self.root / "hyperparams.yaml").touch()
        audio = self.root / "带 空格.wav"
        interfaces, fetch = Mock(), Mock()
        model = interfaces.EncoderDecoderASR.from_hparams.return_value
        model.transcribe_file.return_value = "words"
        with patch.dict(sys.modules, {"speechbrain.inference.ASR": interfaces, "speechbrain.utils.fetching": fetch}):
            recognize = adapters.load_adapter("speechbrain", self.root, {})
        self.assertEqual(recognize(str(audio), "en"), "words")
        model.transcribe_file.assert_called_once_with(audio.as_posix())
        fetch.FetchConfig.assert_called_once_with(allow_network=False)


if __name__ == "__main__":
    unittest.main()
