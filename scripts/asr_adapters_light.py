"""Local-file ASR adapters. Import an engine only when its profile is selected."""

import json
from inspect import signature
from pathlib import Path
import threading
import wave


def _file(root, name):
    path = (root / name).resolve()
    if not path.is_relative_to(root) or not path.is_file():
        raise ValueError(f"模型文件不存在或不在所选目录中：{name}")
    return path


def _open_pcm(audio_path):
    audio = wave.open(str(Path(audio_path).resolve(strict=True)), "rb")
    if (audio.getnchannels(), audio.getframerate(), audio.getsampwidth(),
            audio.getcomptype()) != (1, 16000, 2, "NONE"):
        audio.close()
        raise ValueError("需要单声道、16 kHz、16 位 PCM WAV。")
    return audio


def _torch_device(options):
    import torch

    device = torch.device(options.get("device", "cpu"))
    if device.type == "cuda":
        if not torch.cuda.is_available() or (
                device.index is not None and device.index >= torch.cuda.device_count()):
            raise ValueError(f"所选 CUDA 设备不可用：{device}")
    elif device.type == "cpu":
        return "cpu"
    else:
        raise ValueError("options.device 支持 cpu 或 cuda[:设备编号]。")
    return str(device)


def _require_cpu(options):
    if options.get("device", "cpu") != "cpu" or options.get("provider", "cpu") != "cpu":
        raise ValueError("此适配器使用 CPU，请将 options.device 和 options.provider 设为 cpu。")


def _boolean(options, key, default):
    value = options.get(key, default)
    if not isinstance(value, bool):
        raise ValueError(f"options.{key} 必须是布尔值。")
    return value


def _text(value):
    if not isinstance(value, str):
        raise ValueError("模型返回结果缺少文本。")
    return value.strip()


def _funasr(root, options):
    # The toolkit accepts both a local directory and a Hub identifier. Resolve
    # and validate the checkpoint first so a misspelling cannot trigger a fetch
    # or leave a randomly initialized model running.
    if not any((root / name).is_file() for name in ("configuration.json", "config.yaml")):
        raise ValueError("FunASR 目录需要 configuration.json 或 config.yaml。")
    checkpoint = _file(root, options.get("checkpoint", "model.pt"))
    from funasr import AutoModel

    model = AutoModel(
        model=str(root), model_path=str(root), init_param=str(checkpoint),
        device=_torch_device(options), disable_update=True, check_latest=False,
        trust_remote_code=False, disable_pbar=True, vad_model=None, punc_model=None, spk_model=None,
    )
    use_itn = _boolean(options, "use_itn", True)
    sense_voice = model.kwargs.get("model") == "SenseVoiceSmall"

    def transcribe(audio_path, language):
        path = str(Path(audio_path).resolve(strict=True))
        requested = language or "auto"
        if sense_voice:
            requested = _sense_language(requested) or "auto"
        result = model.generate(input=path, language=requested, use_itn=use_itn)
        if not isinstance(result, list) or not result or not isinstance(result[0], dict):
            raise ValueError("FunASR 未返回该录音的识别结果。")
        text = _text(result[0].get("text"))
        if text.startswith("<|"):
            from funasr.utils.postprocess_utils import rich_transcription_postprocess
            text = rich_transcription_postprocess(text)
        return text

    return transcribe


def _sense_language(language):
    value = (language or "auto").strip().lower().replace("_", "-")
    value = value.split("-")[0]
    if value == "auto":
        return ""
    if value not in {"zh", "en", "yue", "ja", "ko"}:
        raise ValueError(f"SenseVoice 不支持指定的语言：{language}")
    return value


def _sherpa(root, options):
    _require_cpu(options)
    default_model = "model.int8.onnx" if (root / "model.int8.onnx").is_file() else "model.onnx"
    model = _file(root, options.get("model_file", default_model))
    tokens = _file(root, options.get("tokens_file", "tokens.txt"))
    threads = options.get("num_threads", 4)
    if isinstance(threads, bool) or not isinstance(threads, int) or threads < 1:
        raise ValueError("options.num_threads 必须是正整数。")
    use_itn = _boolean(options, "use_itn", True)
    import numpy as np
    import sherpa_onnx

    def create(language):
        return sherpa_onnx.OfflineRecognizer.from_sense_voice(
            model=str(model), tokens=str(tokens), provider="cpu", num_threads=threads,
            language=language, use_itn=use_itn,
        )

    default_language = options.get("language", "auto")
    loaded_language = _sense_language(default_language)
    recognizer = create(loaded_language)
    lock = threading.Lock()

    def transcribe(audio_path, language):
        nonlocal recognizer, loaded_language
        requested = _sense_language(language or default_language)
        with _open_pcm(audio_path) as audio:
            samples = np.frombuffer(audio.readframes(audio.getnframes()), dtype="<i2")
        waveform = samples.astype(np.float32) / 32768.0
        with lock:
            # SenseVoice's language is a recognizer option, not a stream option.
            if recognizer is None or loaded_language != requested:
                recognizer = None
                recognizer = create(requested)
                loaded_language = requested
            stream = recognizer.create_stream()
            stream.accept_waveform(16000, waveform)
            recognizer.decode_stream(stream)
            return _text(stream.result.text)

    return transcribe


def _vosk(root, options):
    _require_cpu(options)
    if not any((root / name).is_file() for name in ("am/final.mdl", "final.mdl")):
        raise ValueError("Vosk 目录需要已解压的声学模型 am/final.mdl 或 final.mdl。")
    from vosk import KaldiRecognizer, Model

    # The model_path form does not invoke Vosk's model-name/language downloader.
    model = Model(model_path=str(root))

    def transcribe(audio_path, language):
        with _open_pcm(audio_path) as audio:
            decoder = KaldiRecognizer(model, audio.getframerate())
            segments = []
            while data := audio.readframes(4000):
                if decoder.AcceptWaveform(data):
                    segments.append(_text(json.loads(decoder.Result()).get("text")))
            segments.append(_text(json.loads(decoder.FinalResult()).get("text")))
        return " ".join(segment for segment in segments if segment)

    return transcribe


def _wenet(root, options):
    _file(root, "units.txt")
    import torch
    from wenet.cli.model import load_model

    device = _torch_device(options)
    parameters = signature(load_model).parameters
    if "model_dir" in parameters:
        # WeNet v3.1.0 loads the TorchScript export with torch.jit.load.
        # The model_dir keyword bypasses Hub.get_model_by_lang.
        _file(root, "final.zip")
        gpu = -1 if device == "cpu" else (torch.device(device).index or 0)
        model = load_model(model_dir=str(root), gpu=gpu)
    elif "model_name_or_path" in parameters:
        # The newer Python API uses a training checkpoint and config.
        _file(root, "train.yaml")
        _file(root, "final.pt")
        model = load_model(str(root), device=device)
        model.eval()
    else:
        raise ValueError("需要 wenet-e2e/wenet 官方 Python SDK（支持 v3.1.0）。")

    def transcribe(audio_path, language):
        with torch.inference_mode():
            result = model.transcribe(str(Path(audio_path).resolve(strict=True)))
        return _text(result.get("text") if isinstance(result, dict) else getattr(result, "text", None))

    return transcribe


def load_adapter(framework: str, model_dir: Path, options: dict):
    root = Path(model_dir).resolve(strict=True)
    if not root.is_dir():
        raise ValueError(f"请选择本地模型目录：{root}")
    engines = {"funasr": _funasr, "sherpa-onnx": _sherpa, "vosk": _vosk, "wenet": _wenet}
    if framework not in engines:
        raise ValueError(f"不支持的识别框架：{framework}")
    return engines[framework](root, options)
