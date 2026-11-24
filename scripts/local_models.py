"""Discover local ASR installations and serve them for the lifetime of one Windows GUI."""

import argparse
import contextlib
import ctypes
from ctypes import wintypes
import json
import multiprocessing
import os
from pathlib import Path
import secrets
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
QWEN_SIZES = {1024: "Qwen3-ASR-0.6B", 2048: "Qwen3-ASR-1.7B"}
SENSE = "SenseVoiceSmall-FP32"
WHISPER = "Whisper-large-v3-turbo"
WHISPER_FILE = "ggml-large-v3-turbo-q8_0.bin"
WHISPER_MODELS = {
    "Whisper Tiny": "ggml-tiny.bin",
    "Whisper Base": "ggml-base.bin",
    "Whisper Small": "ggml-small.bin",
    "Whisper Large v3 Turbo (Q5)": "ggml-large-v3-turbo-q5_0.bin",
    "Whisper Large v3 Turbo": "ggml-large-v3-turbo.bin",
    WHISPER: WHISPER_FILE,  # Existing installations keep their saved selection.
}
HTTP = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def qwen_id(directory):
    try:
        cfg = json.loads((directory / "config.json").read_bytes())
        if cfg.get("model_type") != "qwen3_asr":
            return None
        for name in ("preprocessor_config.json", "tokenizer_config.json", "vocab.json", "merges.txt"):
            if not (directory / name).is_file():
                return None
        if not (directory / "model.safetensors").is_file():
            index = json.loads((directory / "model.safetensors.index.json").read_bytes())
            shards = set(index["weight_map"].values())
            if not shards or not all(Path(f).name == f and (directory / f).is_file() for f in shards):
                return None
        return QWEN_SIZES.get(cfg["thinker_config"]["text_config"]["hidden_size"])
    except (OSError, ValueError, TypeError, KeyError):
        return None


def sense_model(path):
    if not (path / "tokens.txt").is_file():
        return False
    try:
        # The published sherpa export keeps its model_type metadata after the graph.
        with (path / "model.onnx").open("rb") as f:
            f.seek(max(0, f.seek(0, os.SEEK_END) - 65536))
            return b"model_type\x12\x0fsense_voice_ctc" in f.read()
    except OSError:
        return False


def discover():
    roots = [ROOT / "models", ROOT.parent / "models"]
    for base in (ROOT, ROOT.parent):
        configured = base / "models/model-roots.txt"
        try:
            roots.extend(Path(line.strip()) for line in configured.read_text(encoding="utf-8-sig").splitlines()
                         if line.strip() and not line.lstrip().startswith("#"))
        except FileNotFoundError:
            pass
    roots += [Path(x) for x in os.environ.get("SPEAKY_MODELS_DIR", "").split(os.pathsep) if x]
    roots += [ROOT / "ASR-Models", ROOT.parent / "ASR-Models", ROOT.parent / "speaky/models"]
    candidates = []
    if os.environ.get("SPEAKY_QWEN_MODEL"):
        candidates.append(Path(os.environ["SPEAKY_QWEN_MODEL"]))
    for folder in roots:
        try:
            candidates.extend(sorted(p for p in folder.iterdir() if p.is_dir()))
        except OSError:
            continue
    found = {}
    for path in candidates:
        if sense_model(path):
            found.setdefault(SENSE, path)
        else:
            for name, filename in WHISPER_MODELS.items():
                if (path / filename).is_file():
                    found.setdefault(name, path)
            name = qwen_id(path)
            if name:
                found.setdefault(name, path)
    return found


def request_json(url, data=None, timeout=2):
    req = urllib.request.Request(url, data=None if data is None else json.dumps(data).encode(),
                                 headers={"Content-Type": "application/json"})
    with HTTP.open(req, timeout=timeout) as response:
        return json.load(response)


def winfunc(name, result, *args):
    fn = getattr(ctypes.WinDLL("kernel32", use_last_error=True), name)
    fn.argtypes, fn.restype = args, result
    return fn


def close_handle(handle):
    winfunc("CloseHandle", wintypes.BOOL, wintypes.HANDLE)(handle)


def wait_handle(handle, millis):
    return winfunc("WaitForSingleObject", wintypes.DWORD, wintypes.HANDLE, wintypes.DWORD)(handle, millis)


@contextlib.contextmanager
def startup_lock(owner):
    mutex = winfunc("CreateMutexW", wintypes.HANDLE, ctypes.c_void_p, wintypes.BOOL, wintypes.LPCWSTR)(
        None, False, f"Local\\SpeakyLocalASR-{owner}")
    if not mutex:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        if wait_handle(mutex, 45000) not in (0, 0x80):
            raise RuntimeError("等待本地识别服务超时。")
        try:
            yield
        finally:
            winfunc("ReleaseMutex", wintypes.BOOL, wintypes.HANDLE)(mutex)
    finally:
        close_handle(mutex)


def connection(state, owner):
    saved = {}
    try:
        saved = json.loads(state.read_bytes())
        url = f"http://127.0.0.1:{int(saved['port'])}"
        health = request_json(url + "/health", timeout=0.7)
        if health.get("owner") == owner and health.get("instance") == saved["instance"]:
            return url, saved["instance"]
    except (KeyError, ValueError, OSError):
        pass
    token = saved.get("instance") if isinstance(saved, dict) else None
    if isinstance(token, str) and len(token) == 32 and all(c in "0123456789abcdef" for c in token):
        handle = winfunc("OpenEventW", wintypes.HANDLE, wintypes.DWORD, wintypes.BOOL, wintypes.LPCWSTR)(
            0x00100000, False, f"Local\\SpeakyManager-{owner}-{token}")
        if handle:
            close_handle(handle)
            raise RuntimeError("原有本地模型服务暂未响应，请稍后重试。")
    return None


def ensure(model, owner, catalog):
    if owner <= 0 or (model is not None and model not in catalog):
        raise ValueError("当前未找到所选模型的权重或运行程序，请刷新模型列表。")
    profile = Path(os.environ.get("SPEAKY_CONFIG_DIR") or Path(os.environ["APPDATA"]) / "speaky/gpui")
    profile.mkdir(parents=True, exist_ok=True)
    state = profile / f"local-asr-{owner}.json"
    with startup_lock(owner):
        address = connection(state, owner)
        if not address:
            if model is None:
                return {"released": True}
            # The service owns its socket. A second GUI can select a free port independently.
            log_path = profile / f"local-asr-{owner}.log"
            with log_path.open("w", encoding="utf-8") as log:
                child = subprocess.Popen([sys.executable, "-B", "-X", "utf8", __file__, "Serve",
                                          "--owner", str(owner), "--state", str(state)],
                                         stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                         creationflags=subprocess.CREATE_NO_WINDOW)
            deadline = time.monotonic() + 35
            while time.monotonic() < deadline and child.poll() is None:
                try:
                    address = connection(state, owner)
                except RuntimeError:
                    address = None  # Service has published its port and is starting HTTP.
                if address:
                    break
                time.sleep(0.12)
            if not address:
                raise RuntimeError(f"无法启动本地识别服务，请检查 Python 运行环境。日志：{log_path}")
    url, instance = address
    try:
        request_json(url + "/local/prepare", {"model": model, "instance": instance}, timeout=300)
    except urllib.error.HTTPError as error:
        raise RuntimeError(json.load(error).get("detail", str(error))) from error
    if model is None:
        return {"released": True}
    supports_prompt = model.startswith("Qwen3-ASR-") or model in WHISPER_MODELS
    return {"id": model, "base_url": url + "/v1", "use_vocabulary_prompt": supports_prompt}


class PreviewSuperseded(Exception):
    pass


class Recognizers:
    def __init__(self):
        self.worker = None
        self.last_pid = None
        self.free_after_release = None
        self._gpu_ready = threading.Condition()
        self._gpu_busy = False
        self._priority_waiters = 0
        self._preview_worker = None

    @contextlib.contextmanager
    def gpu_slot(self, preview=False):
        with self._gpu_ready:
            if preview:
                if self._gpu_busy or self._priority_waiters:
                    raise PreviewSuperseded
                if self.worker is not None:
                    self.worker.preview_cancelled.clear()
                    self._preview_worker = self.worker
            else:
                # A final request or model switch can supersede a live draft.
                # Keep exclusive access until the old GPU call actually returns.
                self._priority_waiters += 1
                try:
                    if self._preview_worker is not None:
                        self._preview_worker.preview_cancelled.set()
                    self._gpu_ready.wait_for(lambda: not self._gpu_busy)
                finally:
                    self._priority_waiters -= 1
            self._gpu_busy = True
        try:
            yield
        finally:
            with self._gpu_ready:
                self._preview_worker = None
                self._gpu_busy = False
                self._gpu_ready.notify_all()

    def status(self):
        # Health must remain responsive during loading and long transcriptions.
        worker = self.worker
        if worker is not None and worker.process.is_alive():
            active = dict(worker.info, model=worker.name, worker_pid=worker.process.pid)
        else:
            active = dict(model=None, worker_pid=None, device=None, gpu_index=None, gpu_uuid=None)
        return dict(active, last_released_pid=self.last_pid, free_after_release_mib=self.free_after_release)

    def release(self):
        worker = self.worker
        if worker is not None:
            worker.stop()
            self.last_pid = worker.process.pid
            self.free_after_release = worker.free_after_stop()
            self.worker = None

    def prepare(self, name):
        from gpu_worker import Worker
        catalog = discover() if name is not None else {}
        if name is not None and name not in catalog:
            raise ValueError("模型文件已移动，请重新选择。")
        if self.worker is not None and self.worker.name == name and self.worker.process.is_alive():
            return
        self.release()
        if name is not None:
            self.worker = Worker(name, catalog[name])

    def transcribe(self, name, samples, rate, lang, prompt, preview=False):
        worker = self.worker
        if worker is None or worker.name != name or not worker.process.is_alive():
            raise ValueError("该模型尚未加载到 GPU，请重新选择。")
        return worker.transcribe(samples, rate, lang, prompt, preview=preview)


def serve(owner, state):
    import soundfile as sf
    import uvicorn
    from fastapi import Body, FastAPI, File, Form, HTTPException, UploadFile
    from fastapi.responses import PlainTextResponse

    owner_handle = winfunc("OpenProcess", wintypes.HANDLE, wintypes.DWORD, wintypes.BOOL, wintypes.DWORD)(
        0x00100000, False, owner)
    if not owner_handle:
        raise RuntimeError("Speaky 主窗口已经关闭。")
    models = Recognizers()
    token = secrets.token_hex(16)
    app = FastAPI(docs_url=None, redoc_url=None)

    @app.get("/health")
    def health():
        return dict(models.status(), owner=owner, instance=token)

    @app.get("/v1/models")
    def listed():
        name = models.status()["model"]
        return {"object": "list", "data": [{"id": name, "object": "model", "speaky_owner": owner}] if name else []}

    @app.post("/local/prepare")
    def prepare(payload: dict = Body(...)):
        if payload.get("instance") != token:
            raise HTTPException(403, "Invalid instance")
        with models.gpu_slot():
            try:
                models.prepare(payload["model"])
            except Exception as error:
                raise HTTPException(503, str(error)) from error
        return {"ready": True}

    @app.post("/v1/audio/transcriptions")
    def transcribe(file: UploadFile = File(...), model: str = Form(...), language: str = Form("auto"),
                   response_format: str = Form("json"), prompt: str = Form(""),
                   preview: bool = Form(False)):
        if response_format not in ("text", "json") or len(prompt.encode("utf-8")) > 1024:
            raise HTTPException(400, "Unsupported format or prompt")
        if model != models.status()["model"]:
            raise HTTPException(409, "The requested GPU model is not active")
        if file.size and file.size > 64 * 1024**2:
            raise HTTPException(413, "Audio exceeds 64 MiB")
        try:
            with sf.SoundFile(file.file) as audio:
                if not audio.frames or audio.channels > 2:
                    raise HTTPException(400, "Expected non-empty mono or stereo audio")
                if audio.frames > audio.samplerate * 1800 or audio.frames * audio.channels > 64 * 1024**2:
                    raise HTTPException(413, "Audio exceeds maximum duration")
                samples = audio.read(dtype="float32", always_2d=True).mean(axis=1)
                rate = audio.samplerate
        except (ValueError, RuntimeError, OSError) as error:
            raise HTTPException(400, "Invalid audio file") from error
        lang = language.lower().replace("_", "-").split("-")[0] or "auto"
        try:
            with models.gpu_slot(preview=preview):
                text = models.transcribe(model, samples, rate, lang, prompt, preview=preview)
        except PreviewSuperseded:
            text = ""
        except Exception as error:
            raise HTTPException(503, str(error)) from error
        return PlainTextResponse(text) if response_format == "text" else {"text": text}

    sock = socket.socket()
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
    try:
        sock.bind(("127.0.0.1", 18118))
    except OSError:
        sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    server = uvicorn.Server(uvicorn.Config(app, log_level="warning", access_log=False))
    # This marker distinguishes a busy manager from a stale state file.
    marker = winfunc("CreateEventW", wintypes.HANDLE, ctypes.c_void_p, wintypes.BOOL, wintypes.BOOL,
                     wintypes.LPCWSTR)(None, True, True, f"Local\\SpeakyManager-{owner}-{token}")
    if not marker:
        raise ctypes.WinError(ctypes.get_last_error())
    def clear_state():
        try:
            if json.loads(state.read_bytes())["instance"] == token:
                state.unlink(missing_ok=True)
        except (FileNotFoundError, KeyError, ValueError):
            pass

    def watch_owner():
        wait_handle(owner_handle, 0xffffffff)
        server.should_exit = True
        # A GPU kernel or model load can hold an in-flight HTTP request.
        # Closing our process handles also closes the kill-on-close worker Job.
        time.sleep(2)
        clear_state()
        os._exit(0)
    threading.Thread(target=watch_owner, daemon=True).start()
    pending = state.with_suffix(".tmp")
    pending.write_text(json.dumps({"instance": token, "port": port, "service_pid": os.getpid()}), encoding="utf-8")
    os.replace(pending, state)
    try:
        server.run(sockets=[sock])
    finally:
        with models.gpu_slot():
            models.release()
        close_handle(marker)
        close_handle(owner_handle)
        sock.close()
        clear_state()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("List", "Ensure", "Release", "Serve"))
    parser.add_argument("--owner", type=int)
    parser.add_argument("--model")
    parser.add_argument("--state", type=Path)
    args = parser.parse_args()
    if args.action == "Serve":
        serve(args.owner, args.state)
        return
    models = {} if args.action == "Release" else discover()
    if args.action == "List":
        result = [{"id": model, "label": model} for model in models]
    else:
        result = ensure(args.model, args.owner, models)
    print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        print(str(exc), file=sys.stderr)
        sys.exit(1)
