"""Discover and serve the Windows desktop's installed models without changing its settings."""

import ctypes
import json
import logging
import os
import secrets
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

import requests

from local_models import SENSE, WHISPER, WHISPER_MODELS, discover


ROOT = Path(__file__).resolve().parent.parent
SCRIPTS = ROOT / "scripts"
MIB = 1024 ** 2
LOG = logging.getLogger("speaky.share")


def public_name(name):
    return "Whisper Large v3 Turbo (Q8)" if name == WHISPER else name


def model_roots():
    for directory in (ROOT, ROOT.parent):
        yield directory / "ASR-Models"
        yield directory / "models"
        config = directory / "models/model-roots.txt"
        if config.is_file():
            yield from (Path(line.strip()) for line in config.read_text(encoding="utf-8-sig").splitlines()
                        if line.strip() and not line.lstrip().startswith("#"))
    yield from (Path(path) for path in os.environ.get("SPEAKY_MODELS_DIR", "").split(os.pathsep) if path)


def desktop_paths(packs):
    profile = Path(os.environ.get("SPEAKY_CONFIG_DIR") or Path(os.environ["APPDATA"]) / "speaky/gpui")
    configured = os.environ.get("SPEAKY_PACKS_DIR")
    roots = [*packs, Path(configured) if configured else ROOT / "packs", ROOT / "packs", profile / "packs"]
    return profile, tuple(dict.fromkeys(path.resolve() for path in roots))


def session():
    http = requests.Session()
    http.trust_env = False
    return http


def read_json(http, url, headers=None):
    with http.get(url, headers=headers, timeout=0.5, allow_redirects=False) as response:
        if response.status_code == 200:
            value = response.json()
            if isinstance(value, dict):
                return value
    return {}


def loopback(port):
    if type(port) is not int or not 1 <= port <= 65535:
        raise ValueError("Invalid local ASR port.")
    return f"http://127.0.0.1:{port}"


def local_api_path(path):
    if not path.startswith("/") or path.startswith("//") or any(c in path for c in "?#\\"):
        raise ValueError("Invalid model pack API path.")
    return path.rstrip("/")


def desktop_endpoints(profile, packs):
    """Read only healthy loopback services; never use the desktop's prepare endpoint."""
    active = {}
    with session() as http:
        for path in profile.glob("local-asr-*.json"):
            try:
                owner = int(path.stem.rsplit("-", 1)[1])
                state = json.loads(path.read_bytes())
                origin = loopback(state["port"])
                health = read_json(http, origin + "/health")
                model = health.get("model")
                if (isinstance(model, str) and model and health.get("device")
                        and health.get("owner") == owner and health.get("instance") == state["instance"]):
                    active[model] = origin + "/v1/audio/transcriptions"
            except (OSError, ValueError, TypeError, KeyError, requests.RequestException):
                continue
        for root in packs:
            for path in root.glob("*/.runtime-status.json"):
                try:
                    state = json.loads(path.read_bytes())
                    manifest = json.loads(path.with_name("manifest.json").read_bytes())
                    model = state["model"]
                    if not isinstance(model, str) or state["state"] != "ready" or model != manifest["model_id"]:
                        continue
                    origin = loopback(state["port"])
                    health = read_json(http, origin + local_api_path(manifest.get("health_path", "/health")))
                    if health.get("ready") is True and health.get("model") == model and health.get("device"):
                        api = local_api_path(manifest.get("api_base_path", "/v1"))
                        active.setdefault(model, origin + api + "/audio/transcriptions")
                except (OSError, ValueError, TypeError, AttributeError, KeyError, requests.RequestException):
                    continue
    return active


def transcribe_http(http, url, audio, model, language, headers=None):
    with http.post(url, files={"file": ("audio.wav", audio, "audio/wav")},
                   data={"model": model, "language": language, "response_format": "json"},
                   headers=headers, timeout=(2, 1800), allow_redirects=False) as response:
        if response.status_code != 200:
            raise RuntimeError(f"{model} 的转录服务返回 HTTP {response.status_code}。")
        result = response.json()
    if not isinstance(result, dict) or not isinstance(result.get("text"), str):
        raise RuntimeError(f"{model} 的转录服务未返回文字。")
    return result["text"].strip()


class CpuModel:
    def __init__(self, name, command, *, whisper=False, auth=False):
        self.name = name
        self.process = None
        self.http = session()
        self.headers = {"Authorization": "Bearer " + secrets.token_urlsafe(24)} if auth else {}
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        origin = loopback(port)
        command = [*map(str, command), "--port", str(port)]
        if whisper:
            route = "/" + secrets.token_urlsafe(18)
            command += ["--host", "127.0.0.1", "--request-path", route, "--language", "auto", "--no-gpu"]
            self.url = origin + route + "/inference"
            ready_url = origin + route + "/health"
        else:
            self.url = origin + "/v1/audio/transcriptions"
            ready_url = origin + "/v1/models"
        env = dict(os.environ, HF_HUB_OFFLINE="1", TRANSFORMERS_OFFLINE="1", PYTHONUTF8="1",
                   OMP_NUM_THREADS="4", MKL_NUM_THREADS="4")
        env["SPEAKY_ASR_TOKEN"] = self.headers.get("Authorization", "").removeprefix("Bearer ")
        try:
            self.process = subprocess.Popen(
                command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                cwd=Path(command[0]).parent, env=env, creationflags=subprocess.CREATE_NO_WINDOW,
            )
            deadline = time.monotonic() + 75
            while self.process.poll() is None and time.monotonic() < deadline:
                try:
                    ready = read_json(self.http, ready_url, self.headers)
                    if (whisper and ready.get("status") == "ok") or any(
                        item.get("id") == name for item in ready.get("data", [])
                    ):
                        return
                except (OSError, ValueError, requests.RequestException):
                    pass
                time.sleep(0.15)
            raise RuntimeError(f"{name} 的 CPU 进程未能启动，请检查模型和运行环境。")
        except BaseException:
            self.close()
            raise

    def transcribe(self, audio, language):
        return transcribe_http(self.http, self.url, audio, self.name, language, self.headers)

    def close(self):
        try:
            process, self.process = self.process, None
            if process is not None and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        finally:
            self.http.close()


class GpuModel:
    def __init__(self, name, directory):
        from gpu_worker import Worker
        self.worker = Worker(name, directory)

    def transcribe(self, audio, language):
        import soundfile
        with soundfile.SoundFile(audio) as source:
            samples = source.read(dtype="float32", always_2d=True).mean(axis=1)
            rate = source.samplerate
        return self.worker.transcribe(samples, rate, language, "")

    def close(self):
        self.worker.stop()


def require_memory(name, needed):
    class MemoryStatus(ctypes.Structure):
        _fields_ = [("length", ctypes.c_uint32), ("load", ctypes.c_uint32),
                    ("total", ctypes.c_uint64), ("available", ctypes.c_uint64), ("other", ctypes.c_uint64 * 5)]

    status = MemoryStatus()
    status.length = ctypes.sizeof(status)
    if not ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(status)):
        raise ctypes.WinError()
    if status.available < needed:
        raise RuntimeError(f"{name} 的 CPU 推理约需 {(needed + MIB - 1) // MIB} MiB 可用内存，"
                           f"当前约有 {status.available // MIB} MiB。")


class SharedModels:
    def __init__(self, packs, cpu_python, whisper_server):
        self.profile, self.packs = desktop_paths(packs)
        self.cpu_python = cpu_python
        self.whisper_server = whisper_server
        self.lock = threading.Lock()
        self.active = None
        self.last_used = 0.0

    def installed(self):
        found = {public_name(name): (name, folder, None) for name, folder in discover().items()}
        for pack in self.packs:
            for name, weight in WHISPER_MODELS.items():
                for path in sorted(pack.glob(f"*/{weight}")):
                    found.setdefault(public_name(name), (name, path.parent, None))
        for root in model_roots():
            for name, framework, weight in (
                ("SenseVoiceSmall-int8", "sherpa-onnx", "model.int8.onnx"),
                ("vosk-model-small-cn-0.22", "vosk", "am/final.mdl"),
            ):
                folder = root / name
                if (folder / weight).is_file() and (framework == "vosk" or (folder / "tokens.txt").is_file()):
                    found.setdefault(name, (name, folder, framework))
        return found

    def catalog(self):
        return dict.fromkeys([*self.installed(), *(public_name(name) for name in desktop_endpoints(self.profile, self.packs))])

    def _release(self):
        previous, self.active = self.active, None
        if previous is not None:
            previous[1].close()

    def release_idle(self, seconds):
        with self.lock:
            if time.monotonic() - self.last_used >= seconds:
                self._release()

    def _load(self, name, folder, framework, desktop_busy):
        if not desktop_busy and framework is None:
            try:
                return GpuModel(name, folder)
            except (OSError, ImportError, RuntimeError) as error:
                LOG.info("GPU unavailable for %s, trying CPU: %s", name, error)
        if name in WHISPER_MODELS:
            weight = folder / WHISPER_MODELS[name]
            require_memory(name, weight.stat().st_size + 300 * MIB)
            return CpuModel(name, [self.whisper_server.resolve(strict=True), "--model", weight.resolve(strict=True)],
                            whisper=True)
        if name.startswith("Qwen3-ASR-"):
            size = sum(path.stat().st_size for path in folder.glob("*.safetensors"))
            require_memory(name, size * 2 + 850 * MIB)
            return CpuModel(name, [sys.executable, "-B", SCRIPTS / "serve_qwen_asr.py", "--model-dir",
                                   folder.resolve(strict=True), "--device", "cpu"])
        if framework or name == SENSE:
            args = [self.cpu_python.resolve(strict=True), "-B", SCRIPTS / "asr_bridge.py", "--framework",
                    framework or "sherpa-onnx", "--model-dir", folder.resolve(strict=True), "--model-id", name]
            if name == SENSE:
                require_memory(name, (folder / "model.onnx").stat().st_size + 350 * MIB)
                with tempfile.TemporaryDirectory(prefix="speaky-shared-sense-") as directory:
                    options = Path(directory) / "options.json"
                    options.write_text('{"model_file":"model.onnx"}', encoding="utf-8")
                    return CpuModel(name, [*args, "--options", options], auth=True)
            return CpuModel(name, args, auth=True)
        raise RuntimeError(f"未找到 {name} 的本机识别程序。")

    def transcribe(self, model, audio, language):
        lang = language.strip().lower().replace("_", "-").split("-", 1)[0] or "auto"
        with self.lock:
            desktop = desktop_endpoints(self.profile, self.packs)
            for name, endpoint in desktop.items():
                if public_name(name) == model:
                    self._release()
                    with session() as http:
                        return transcribe_http(http, endpoint, audio, name, lang)
            entry = self.installed().get(model)
            if entry is None:
                raise KeyError(model)
            # If the desktop starts its own model, relinquish any sharing worker on the next request.
            if self.active is not None and (self.active[0] != entry or (desktop and isinstance(self.active[1], GpuModel))):
                self._release()
            if self.active is None:
                self.active = (entry, self._load(*entry, desktop_busy=bool(desktop)))
            try:
                return self.active[1].transcribe(audio, lang)
            except BaseException:
                self._release()
                raise
            finally:
                self.last_used = time.monotonic()
