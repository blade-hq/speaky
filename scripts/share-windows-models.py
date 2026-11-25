"""Share a Windows host's Speaky models with an OpenAI-compatible client."""

import argparse
from contextlib import asynccontextmanager
import hmac
import ipaddress
import logging
import os
import shutil
import sys
import threading
import wave
from pathlib import Path

import requests
import uvicorn
from fastapi import Depends, FastAPI, File, Form, Header, HTTPException, UploadFile

from shared_windows_models import MIB, ROOT, SharedModels, model_roots


LOG = logging.getLogger("speaky.share")


def create_app(models, token):
    @asynccontextmanager
    async def lifespan(_app):
        stop = threading.Event()

        def unload_idle():
            while not stop.wait(5):
                try:
                    models.release_idle(30)
                except (OSError, RuntimeError):
                    LOG.exception("Could not release the idle shared model")

        thread = threading.Thread(target=unload_idle, daemon=True)
        thread.start()
        try:
            yield
        finally:
            stop.set()
            thread.join()
            models.release_idle(0)

    app = FastAPI(docs_url=None, redoc_url=None, openapi_url=None, lifespan=lifespan)

    def authorize(authorization: str = Header(default="")):
        if token and not hmac.compare_digest(authorization.encode(), ("Bearer " + token).encode()):
            raise HTTPException(401, "Invalid API key.", headers={"WWW-Authenticate": "Bearer"})

    @app.get("/v1/models", dependencies=[Depends(authorize)])
    def list_models():
        return {"object": "list", "data": [{"id": name, "object": "model"} for name in models.catalog()]}

    @app.post("/v1/audio/transcriptions", dependencies=[Depends(authorize)])
    def transcribe(file: UploadFile = File(...), model: str = Form(...), language: str = Form("auto"),
                   response_format: str = Form("json")):
        if response_format != "json":
            raise HTTPException(400, "Use response_format=json.")
        if file.size is not None and file.size > 64 * MIB:
            raise HTTPException(413, "Audio exceeds 64 MiB.")
        try:
            with wave.open(file.file, "rb") as audio:
                if (audio.getnchannels(), audio.getsampwidth(), audio.getframerate()) != (1, 2, 16000):
                    raise ValueError("Invalid PCM WAV format.")
                frames = audio.getnframes()
                if not 1 <= frames <= 16000 * 1800:
                    raise ValueError("Invalid audio duration.")
                audio.setpos(frames - 1)
                if len(audio.readframes(1)) != 2:
                    raise ValueError("Incomplete audio data.")
        except (EOFError, OSError, ValueError, wave.Error) as error:
            raise HTTPException(400, "Expected 16 kHz mono 16-bit PCM WAV audio, up to 30 minutes.") from error
        finally:
            file.file.seek(0)
        try:
            text = models.transcribe(model, file.file, language)
        except KeyError as error:
            raise HTTPException(404, "The requested model is not installed.") from error
        except (OSError, RuntimeError, ValueError, requests.RequestException) as error:
            LOG.warning("Model %s failed: %s", model, error)
            detail = str(error) if isinstance(error, RuntimeError) else "识别服务未就绪。"
            raise HTTPException(503, detail) from error
        return {"text": text, "model": model}

    return app


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="127.0.0.1", help="Bind to the Windows host's private VM network IP.")
    parser.add_argument("--port", type=int, default=18769)
    parser.add_argument("--packs-dir", action="append", type=Path, default=[], help="Additional trusted pack directory.")
    parser.add_argument("--cpu-python", type=Path, help="Python with CPU SenseVoice and Vosk dependencies.")
    parser.add_argument("--whisper-server", type=Path, help="Trusted whisper.cpp server executable.")
    args = parser.parse_args()
    if sys.platform != "win32":
        parser.error("Run the sharing service on the Windows host.")
    try:
        local = ipaddress.ip_address(args.host).is_loopback
    except ValueError:
        local = args.host.lower() == "localhost"
    token = os.environ.get("SPEAKY_ASR_TOKEN", "")
    if not local and not token:
        parser.error("Set SPEAKY_ASR_TOKEN before binding beyond loopback.")
    if not 1 <= args.port <= 65535:
        parser.error("Port must be between 1 and 65535.")
    bundled = ROOT / "whisper-runtime/whisper-server.exe"
    server = args.whisper_server or (bundled if bundled.is_file() else Path(shutil.which("whisper-server") or bundled))
    cpu_python = args.cpu_python or next(
        (path for root in model_roots() if (path := root / "bridge-env/Scripts/python.exe").is_file()),
        Path(sys.executable),
    )
    models = SharedModels(args.packs_dir, cpu_python, server)
    uvicorn.run(create_app(models, token), host=args.host, port=args.port, proxy_headers=False)


if __name__ == "__main__":
    main()
