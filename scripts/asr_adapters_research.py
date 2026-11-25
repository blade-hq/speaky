"""Offline inference adapters for trusted, locally installed ASR model bundles."""

import math
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import wave


def _artifact(root, options, key, default=None, pattern=None):
    name = options.get(key, default)
    if name is None:
        matches = sorted(root.glob(pattern))
        if len(matches) != 1:
            raise ValueError(f"请在 options.{key} 指定模型文件（找到 {len(matches)} 个 {pattern}）。")
        name = matches[0]
    path = (root / name).resolve()
    if not path.is_relative_to(root) or not path.is_file():
        raise ValueError(f"options.{key} 须指向模型目录内已有的文件：{name}")
    return path


def _text(value):
    if not isinstance(value, str):
        raise ValueError("模型未返回可读文本，请检查解码器和分词器配置。")
    return value.strip()


def _nemo(root, options):
    checkpoint = _artifact(root, options, "checkpoint", pattern="*.nemo")
    from nemo.collections.asr.models import ASRModel

    device = options.get("device", "cpu")
    model = ASRModel.restore_from(str(checkpoint), map_location=device)
    model.to(device).eval()

    def transcribe(audio_path, language):
        # Classic CTC/RNNT/TDT models use the checkpoint's language configuration.
        result = model.transcribe([audio_path], batch_size=1, return_hypotheses=True)
        if isinstance(result, tuple):  # Some RNNT models also return beam hypotheses.
            result = result[0]
        if not result:
            raise ValueError("NeMo 未返回该音频的识别结果。")
        best = result[0]
        return _text(getattr(best, "text", best))

    return transcribe


def _espnet(root, options):
    config = _artifact(root, options, "config", "config.yaml")
    checkpoint = _artifact(root, options, "checkpoint", pattern="*.pth")
    if (root / "nltk_data").is_dir():
        # ESPnet releases using g2p_en check these resources at import time.
        import nltk
        nltk.data.path.insert(0, str(root / "nltk_data"))
    from espnet2.bin.asr_inference import Speech2Text
    import numpy as np

    kwargs = dict(asr_train_config=str(config), asr_model_file=str(checkpoint),
                  device=options.get("device", "cpu"))
    if "beam_size" in options:
        kwargs["beam_size"] = _positive_int(options["beam_size"], "beam_size")
    if "bpemodel" in options:
        kwargs["bpemodel"] = str(_artifact(root, options, "bpemodel"))
    if "stats_file" in options:
        # Exported training configs may reference the training machine's CMVN file.
        import yaml
        settings = yaml.safe_load(config.read_text(encoding="utf-8"))
        settings.setdefault("normalize_conf", {})["stats_file"] = str(_artifact(root, options, "stats_file"))
        with tempfile.TemporaryDirectory(prefix="speaky-espnet-") as directory:
            local_config = Path(directory) / "config.yaml"
            local_config.write_text(yaml.safe_dump(settings, allow_unicode=True), encoding="utf-8")
            kwargs["asr_train_config"] = str(local_config)
            model = Speech2Text(**kwargs)
    else:
        model = Speech2Text(**kwargs)

    def transcribe(audio_path, language):
        with wave.open(audio_path, "rb") as wav:
            if (wav.getframerate(), wav.getnchannels(), wav.getsampwidth()) != (16000, 1, 2):
                raise ValueError("ESPnet 输入需要 16 kHz 单声道 16 位 PCM WAV。")
            samples = np.frombuffer(wav.readframes(wav.getnframes()), dtype="<i2")
        hypotheses = model(samples.astype(np.float32) / 32768.0)
        if not hypotheses:
            raise ValueError("ESPnet 未返回该音频的识别结果。")
        return _text(hypotheses[0][0])

    return transcribe


def _speechbrain(root, options):
    hparams = _artifact(root, options, "hparams", "hyperparams.yaml")
    from speechbrain.inference.ASR import EncoderASR, EncoderDecoderASR
    from speechbrain.utils.fetching import FetchConfig, LocalStrategy

    classes = {"EncoderASR": EncoderASR, "EncoderDecoderASR": EncoderDecoderASR}
    interface = options.get("interface", "EncoderDecoderASR")
    if interface not in classes:
        raise ValueError("options.interface 仅支持 EncoderASR 或 EncoderDecoderASR。")
    # SpeechBrain 1.1+: the built-in interface reads the local HyperPyYAML recipe
    # and its state dicts. No foreign_class/custom.py or Hub repository import.
    model = classes[interface].from_hparams(
        source=str(root), hparams_file=hparams.relative_to(root).as_posix(),
        savedir=str(root), local_strategy=LocalStrategy.NO_LINK,
        fetch_config=FetchConfig(allow_network=False),
        run_opts={"device": options.get("device", "cpu")},
    )

    def transcribe(audio_path, language):
        # SpeechBrain splits source/filename on "/" even on Windows.
        return _text(model.transcribe_file(Path(audio_path).resolve().as_posix()))

    return transcribe


def _positive_int(value, key):
    if isinstance(value, bool) or not isinstance(value, int) or value < 1:
        raise ValueError(f"options.{key} 必须是正整数。")
    return value


def _positive_number(value, key):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value <= 0:
        raise ValueError(f"options.{key} 必须是有限正数。")
    return value


def _kaldi_program(name, bin_dir):
    filename = name + (".exe" if os.name == "nt" else "")
    # Only the named installed program is accepted, never a command string or
    # a shell/batch file found beside a checkpoint.
    if bin_dir:
        directory = Path(bin_dir)
        if not directory.is_absolute():
            raise ValueError("options.bin_dir 需要使用已安装 Kaldi 程序目录的绝对路径。")
        program = directory / filename
    else:
        found = shutil.which(filename)
        program = Path(found) if found else None
    if not program or not program.is_file() or not os.access(program, os.X_OK):
        raise ValueError(f"找不到 Kaldi 程序 {filename}，请配置 PATH 或 options.bin_dir。")
    return str(program.resolve())


def _run_kaldi(argv, root, timeout):
    try:
        result = subprocess.run(argv, cwd=root, stdin=subprocess.DEVNULL, capture_output=True,
                                timeout=timeout, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    except subprocess.TimeoutExpired as error:
        raise RuntimeError(f"{Path(argv[0]).name} 超过 {timeout} 秒未完成。") from error
    if result.returncode:
        detail = result.stderr[-4096:].decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"{Path(argv[0]).name} 失败（{result.returncode}）：{detail}")
    return result.stdout


def _kaldi(root, options):
    if options.get("device", "cpu") != "cpu":
        raise ValueError("Kaldi online2-wav-nnet3-latgen-faster 使用 CPU，请将 options.device 设为 cpu。")
    decoder = _kaldi_program("online2-wav-nnet3-latgen-faster", options.get("bin_dir"))
    best_path = _kaldi_program("lattice-best-path", options.get("bin_dir"))
    model = _artifact(root, options, "model", "final.mdl")
    graph = _artifact(root, options, "graph", "graph/HCLG.fst")
    config = _artifact(root, options, "config", "conf/online.conf")
    words = _artifact(root, options, "words", "graph/words.txt")
    acoustic_scale = _positive_number(options.get("acoustic_scale", 1.0), "acoustic_scale")
    subsampling = _positive_int(options.get("frame_subsampling_factor", 3), "frame_subsampling_factor")
    timeout = _positive_number(options.get("timeout", 600), "timeout")
    dictionary = {}
    with words.open(encoding="utf-8-sig") as stream:
        for line in stream:
            if line.strip():
                symbol, integer_id = line.split()
                dictionary[int(integer_id)] = symbol
    if not dictionary:
        raise ValueError(f"Kaldi 词表为空：{words}")

    def transcribe(audio_path, language):
        # Kaldi's scp values may name shell pipes. Use a generated filename
        # instead of interpreting the provided WAV path as an rxfilename.
        with tempfile.TemporaryDirectory(prefix="speaky-kaldi-") as directory:
            work = Path(directory).resolve()
            wav = work / "input.wav"
            shutil.copyfile(audio_path, wav)
            scp, speakers, lattice = (work / name for name in ("wav.scp", "spk2utt", "result.ark"))
            scp.write_bytes(f"utt {wav.as_posix()}\n".encode("utf-8"))
            speakers.write_bytes(b"spk utt\n")
            _run_kaldi([decoder, f"--config={config}", f"--frame-subsampling-factor={subsampling}",
                        f"--acoustic-scale={acoustic_scale}", str(model), str(graph),
                        f"ark:{speakers}", f"scp:{scp}", f"ark:{lattice}"], root, timeout)
            # The decoder writes lattices with the acoustic scaling removed.
            output = _run_kaldi([best_path, f"--acoustic-scale={acoustic_scale}",
                                 f"ark:{lattice}", "ark,t:-"], root, timeout)
        records = [line.split() for line in output.decode("ascii").splitlines() if line.strip()]
        if len(records) != 1 or records[0][0] != "utt":
            raise ValueError("Kaldi 未返回本次识别的 utt 记录。")
        return " ".join(dictionary[int(word)] for word in records[0][1:])

    return transcribe


def load_adapter(framework: str, model_dir: Path, options: dict):
    """Load one offline recognizer and return its (WAV path, language) callable."""
    loaders = {"nemo": _nemo, "espnet": _espnet, "speechbrain": _speechbrain, "kaldi": _kaldi}
    if framework not in loaders:
        raise ValueError(f"不支持的 ASR 框架：{framework}")
    root = Path(model_dir).resolve(strict=True)
    if not root.is_dir():
        raise ValueError(f"模型目录无效：{root}")
    return loaders[framework](root, options)
