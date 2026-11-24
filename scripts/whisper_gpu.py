"""Run a pinned whisper.cpp v1.8.3 CUDA context inside the model worker."""
import ctypes as ct
import math
import os
from pathlib import Path
import re


DEFAULT_WEIGHT = "ggml-large-v3-turbo-q8_0.bin"
BRIDGE = Path(__file__).with_name("whisper_gpu_bridge.dll")
RUNTIME_FILES = ("whisper.dll", "ggml.dll", "ggml-base.dll", "ggml-cpu.dll", "ggml-cuda.dll")


def installed(directory, weight=DEFAULT_WEIGHT):
    runtime = directory / "runtime-cuda"
    return ((directory / weight).is_file() and BRIDGE.is_file()
            and all((runtime / name).is_file() for name in RUNTIME_FILES))


def bind(library, name, result, *arguments):
    fn = getattr(library, name)
    fn.restype, fn.argtypes = result, arguments
    return fn


class WhisperGPU:
    def __init__(self, directory, device_index, weight=DEFAULT_WEIGHT):
        model = directory / weight
        runtime = (directory / "runtime-cuda").resolve(strict=True)
        self._dirs = [os.add_dll_directory(str(runtime))]
        # Python's DLL loader does not automatically include CUDA_PATH/bin.
        toolkit = os.environ.get("CUDA_PATH_V11_8") or os.environ.get("CUDA_PATH_V11_3") or os.environ.get("CUDA_PATH")
        if toolkit and (Path(toolkit) / "bin").is_dir():
            self._dirs.append(os.add_dll_directory(str(Path(toolkit) / "bin")))
        try:
            whisper = ct.CDLL(str(runtime / "whisper.dll"))
            ggml = ct.CDLL(str(runtime / "ggml.dll"))
            ggml_base = ct.CDLL(str(runtime / "ggml-base.dll"))
            bridge = ct.CDLL(str(BRIDGE.resolve(strict=True)))
        except OSError as error:
            raise RuntimeError("无法加载 Whisper CUDA 11.8 运行库，请检查 runtime-cuda 和 cuBLAS 11。") from error
        version = bind(whisper, "whisper_version", ct.c_char_p)().decode()
        if version != "1.8.3":
            raise RuntimeError(f"Whisper GPU 接口需要 whisper.cpp 1.8.3，当前为 {version}。")
        cuda = f"CUDA{device_index}"
        device = bind(ggml, "ggml_backend_dev_by_name", ct.c_void_p, ct.c_char_p)(cuda.encode())
        if not device:
            raise RuntimeError(f"Whisper 无法访问 {cuda}。")
        self.device_name = bind(ggml_base, "ggml_backend_dev_description", ct.c_char_p, ct.c_void_p)(device).decode()
        expected = os.environ.get("SPEAKY_GPU_NAME")
        if expected and self.device_name != expected:
            raise RuntimeError(f"Whisper 检测到 {self.device_name}，需要 {expected}。")

        log_fn = ct.CFUNCTYPE(None, ct.c_int, ct.c_char_p, ct.c_void_p)
        messages = []
        # Capture initialization only. Audio and transcription text never enter this log.
        capture = log_fn(lambda _level, msg, _data: messages.append(msg.decode("utf-8", "replace")))
        set_log = bind(whisper, "whisper_log_set", None, ct.c_void_p, ct.c_void_p)
        self._free = bind(whisper, "whisper_free", None, ct.c_void_p)
        load = bind(bridge, "speaky_whisper_open", ct.c_void_p, ct.c_char_p, ct.c_int)
        set_log(ct.cast(capture, ct.c_void_p), None)
        try:
            self._context = load(str(model.resolve()).encode("utf-8"), device_index)
        finally:
            set_log(None, None)
        log = "".join(messages)
        weights = re.search(rf"\b{cuda} total size\s*=\s*([\d.]+) MB", log)
        if (not self._context or f"using {cuda} backend" not in log or not weights
                or float(weights.group(1)) * 1e6 < model.stat().st_size * 0.9):
            self.close()
            raise RuntimeError(f"Whisper 未能将模型权重加载到 {self.device_name}，请检查可用显存。")
        self._run = bind(bridge, "speaky_whisper_transcribe", ct.c_int, ct.c_void_p, ct.c_void_p,
                         ct.c_int, ct.c_char_p, ct.c_char_p)
        self._count = bind(whisper, "whisper_full_n_segments", ct.c_int, ct.c_void_p)
        self._segment = bind(whisper, "whisper_full_get_segment_text", ct.c_char_p, ct.c_void_p, ct.c_int)
        self._lang_id = bind(whisper, "whisper_lang_id", ct.c_int, ct.c_char_p)

    def close(self):
        if self._context:
            self._free(self._context)
            self._context = None

    def transcribe(self, samples, rate, lang, prompt):
        import numpy as np
        if rate != 16000:
            from scipy.signal import resample_poly
            divisor = math.gcd(rate, 16000)
            samples = resample_poly(samples, 16000 // divisor, rate // divisor)
        audio = np.ascontiguousarray(samples, dtype=np.float32)
        language = (lang or "auto").encode()
        if language != b"auto" and self._lang_id(language) < 0:
            raise ValueError(f"Whisper 不支持语言 {lang}。")
        if self._run(self._context, audio.ctypes.data, audio.size, language, (prompt or "").encode()) != 0:
            raise RuntimeError(f"Whisper 在 {self.device_name} 上转写失败。")
        segments = [self._segment(self._context, i) for i in range(self._count(self._context))]
        return b"".join(segments).decode("utf-8").strip()
