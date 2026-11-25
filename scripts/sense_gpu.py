"""SenseVoiceSmall using the sherpa-onnx FP32 export and ONNX Runtime CUDA."""

import json
import math
from pathlib import Path
import sys
import tempfile

import numpy as np


class SenseGPU:
    def __init__(self, directory: Path, device_index: int):
        # Portable GPU dependencies can live alongside the weights.
        deps = directory / "runtime/python"
        if deps.is_dir():
            sys.path.insert(0, str(deps))
        import torch
        import kaldi_native_fbank as knf
        import onnxruntime as ort

        self.device_name = torch.cuda.get_device_name(device_index)
        if "CUDAExecutionProvider" not in ort.get_available_providers():
            raise RuntimeError("SenseVoice 需要支持 CUDA 的 onnxruntime-gpu。")
        # Import PyTorch first to load matching CUDA 12 / cuDNN 9 libraries.
        options = ort.SessionOptions()
        options.log_severity_level = 3
        options.intra_op_num_threads = 1
        options.inter_op_num_threads = 1
        cuda_options = {"device_id": device_index, "arena_extend_strategy": "kSameAsRequested",
                        "cudnn_conv_algo_search": "HEURISTIC", "cudnn_conv_use_max_workspace": "0"}
        self.knf = knf
        self.fbank = knf.FbankOptions()
        self.fbank.frame_opts.dither = 0
        self.fbank.frame_opts.snip_edges = True
        self.fbank.frame_opts.window_type = "hamming"
        self.fbank.frame_opts.samp_freq = 16000
        self.fbank.mel_opts.num_bins = 80
        self.fbank.mel_opts.high_freq = 0
        with (directory / "tokens.txt").open(encoding="utf-8") as tokens:
            self.tokens = {}
            for line in tokens:
                token, index = line.rstrip("\r\n").rsplit(" ", 1)
                self.tokens[int(index)] = token
        with tempfile.TemporaryDirectory(prefix="speaky-sense-gpu-") as scratch:
            # Provider availability alone cannot prove that the graph runs on CUDA.
            options.enable_profiling = True
            options.profile_file_prefix = str(Path(scratch) / "placement")
            self.session = ort.InferenceSession(str(directory / "model.onnx"), options,
                                                providers=[("CUDAExecutionProvider", cuda_options)])
            self.session.disable_fallback()
            if "CUDAExecutionProvider" not in self.session.get_providers():
                raise RuntimeError("SenseVoice 未能初始化 CUDA 推理。请检查 CUDA 和 cuDNN 运行库。")
            metadata = self.session.get_modelmeta().custom_metadata_map
            if metadata.get("model_type") != "sense_voice_ctc":
                raise ValueError("所选权重不属于 SenseVoiceSmall CTC。")
            self.sample_scale = 1 if int(metadata["normalize_samples"]) else 32768
            self.width = int(metadata["lfr_window_size"])
            self.step = int(metadata["lfr_window_shift"])
            self.mean = np.fromstring(metadata["neg_mean"], sep=",", dtype=np.float32)
            self.scale = np.fromstring(metadata["inv_stddev"], sep=",", dtype=np.float32)
            self.languages = {lang: int(metadata["lang_" + lang])
                              for lang in ("auto", "zh", "en", "yue", "ja", "ko")}
            self.itn = int(metadata["with_itn"])
            self.blank = int(metadata.get("blank_id", "0"))
            self.inputs = [item.name for item in self.session.get_inputs()]
            self.output = self.session.get_outputs()[0].name
            self._decode(np.zeros(8000, dtype=np.float32), "auto")
            profile = Path(self.session.end_profiling())
            with profile.open(encoding="utf-8") as stream:
                events = json.load(stream)
            heavy = [e["args"] for e in events if e.get("cat") == "Node"
                     and e.get("args", {}).get("provider") and
                     any(s in e["args"].get("op_name", "") for s in
                         ("MatMul", "Gemm", "Conv", "Attention", "Softmax"))]
            if not heavy or any(node["provider"] != "CUDAExecutionProvider" for node in heavy):
                raise RuntimeError("SenseVoice 的神经网络计算没有全部使用 CUDA。")
            self.cuda_kernel_count = len(heavy)

    def _decode(self, samples, lang):
        fbank = self.knf.OnlineFbank(self.fbank)
        fbank.accept_waveform(16000, (samples * self.sample_scale).tolist())
        fbank.input_finished()
        if not fbank.num_frames_ready:
            return ""
        frames = np.stack([fbank.get_frame(i) for i in range(fbank.num_frames_ready)])
        # Each LFR vector is centered at 0, step, 2*step, ... . As in
        # sherpa-onnx ApplyLfr, repeat boundary frames even for a one-frame clip.
        centers = np.arange(0, len(frames), self.step)
        offsets = np.arange(self.width) - (self.width - 1) // 2
        indices = np.clip(centers[:, None] + offsets, 0, len(frames) - 1)
        features = frames[indices].reshape(len(centers), -1)
        features = np.ascontiguousarray((features + self.mean) * self.scale)
        args = [features[None], np.array([len(features)], dtype=np.int32),
                np.array([self.languages[lang]], dtype=np.int32), np.array([self.itn], dtype=np.int32)]
        logits, = self.session.run([self.output], dict(zip(self.inputs, args)))
        ids = logits[0, :len(features) + 4].argmax(axis=-1)
        # Collapse repeated CTC labels before removing the blank or a duplicate
        # separated by a blank would disappear. First four labels are metadata.
        unique = ids[np.r_[True, ids[1:] != ids[:-1]]]
        labels = unique[unique != self.blank]
        text = "".join(self.tokens[int(i)] for i in labels[4:]).replace("▁", " ")
        # An independently decoded English/Korean chunk may start with a plain
        # token. Its language tag remains available even in automatic mode.
        if text[:1].isalnum() and self.tokens[int(labels[0])] in ("<|en|>", "<|ko|>"):
            text = " " + text
        return text

    def transcribe(self, samples, rate, lang, prompt=""):
        if lang not in self.languages:
            raise ValueError(f"SenseVoice 不支持此语言：{lang}")
        if rate != 16000:
            from scipy.signal import resample_poly
            divisor = math.gcd(rate, 16000)
            samples = resample_poly(samples, 16000 // divisor, rate // divisor)
        # Bound the size of the encoder attention matrix on longer dictations.
        results = []
        while len(samples) > 30 * 16000:
            start = 27 * 16000
            tail = samples[start:30 * 16000].reshape(-1, 320)
            cut = start + int(np.mean(np.square(tail), axis=1).argmin()) * 320 + 160
            results.append(self._decode(samples[:cut], lang))
            samples = samples[cut:]
        if len(samples):
            results.append(self._decode(samples, lang))
        return "".join(results).strip()
