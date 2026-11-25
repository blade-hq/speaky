"""Serve one locally provisioned ASR model to Speaky."""

import argparse
import hmac
import ipaddress
import json
import logging
import os
import shutil
import tempfile
import threading
import wave
from pathlib import Path

from fastapi import Depends, FastAPI, File, Form, Header, HTTPException, UploadFile


PORTS = {
    "funasr": 18761, "sherpa-onnx": 18762, "wenet": 18763, "vosk": 18764,
    "nemo": 18765, "espnet": 18766, "speechbrain": 18767, "kaldi": 18768,
}
LOG = logging.getLogger("speaky.asr")


def model_identity(framework, model_dir, explicit_name=None):
    """Resolve a name only from an explicit setting or a configured local preset."""
    if explicit_name is not None:
        return explicit_name.strip(), None
    root = model_dir.resolve()
    scripts = Path(__file__).resolve().parent
    app = scripts.parent
    model_roots = [app / "ASR-Models", app.parent / "ASR-Models"]
    model_roots.extend(Path(p) for p in os.environ.get("SPEAKY_MODELS_DIR", "").split(os.pathsep) if p)
    if any(root.parent == p.resolve() for p in model_roots):
        presets = json.loads((scripts / "asr-presets.json").read_text(encoding="utf-8"))
        for item in presets:
            if item["framework"] == framework and root == (root.parent / item["directory"]):
                return item["model"], framework
    return root.name, None


def check_wav(path):
    try:
        with wave.open(str(path), "rb") as wav:
            if (wav.getnchannels(), wav.getsampwidth(), wav.getframerate(), wav.getcomptype()) != (1, 2, 16000, "NONE"):
                raise ValueError("Expected uncompressed 16 kHz mono 16-bit PCM WAV.")
            frames = wav.getnframes()
            if frames < 1:
                raise ValueError("The WAV contains no audio.")
            wav.setpos(frames - 1)
            if len(wav.readframes(1)) != 2:
                raise ValueError("Incomplete WAV audio.")
    except (wave.Error, EOFError) as error:
        raise ValueError("Invalid PCM WAV file.") from error


def create_app(framework, model_id, recognize, token="", model_alias=None):
    app = FastAPI(title="Speaky ASR", docs_url=None, redoc_url=None)
    inference = threading.Lock()

    def authorize(authorization: str = Header(default="")):
        if token and not hmac.compare_digest(authorization.encode(), ("Bearer " + token).encode()):
            raise HTTPException(401, detail="Invalid API key.", headers={"WWW-Authenticate": "Bearer"})

    identity = {"framework": framework, "model": model_id}
    catalog_entry = {"id": model_id, "object": "model", "framework": framework}
    if model_alias and model_alias != model_id:
        catalog_entry["aliases"] = [model_alias]

    @app.get("/v1/models", dependencies=[Depends(authorize)])
    def models():
        return {"object": "list", "data": [catalog_entry]}

    @app.post("/v1/audio/transcriptions", dependencies=[Depends(authorize)])
    def transcribe(file: UploadFile = File(...), model: str = Form(...),
                   language: str = Form("auto"), response_format: str = Form("json")):
        if model != model_id and model != model_alias:
            raise HTTPException(404, detail=f"{framework} serves model {model_id!r}, requested {model!r}.")
        if response_format != "json":
            raise HTTPException(400, detail="Use response_format=json to receive text and model identity.")
        with tempfile.TemporaryDirectory(prefix="speaky-asr-") as directory:
            audio = Path(directory) / "audio.wav"
            with audio.open("wb") as out:
                shutil.copyfileobj(file.file, out)
            try:
                check_wav(audio)
            except ValueError as error:
                raise HTTPException(400, detail=str(error)) from error
            try:
                with inference:
                    transcript = recognize(str(audio), language)
                if not isinstance(transcript, str):
                    raise TypeError("The recognizer returned a non-text result.")
            except Exception as error:
                LOG.exception("%s / %s transcription failed", framework, model_id)
                raise HTTPException(500, detail=f"{framework} / {model_id}: {error}") from error
        return {"text": transcript, **identity}

    return app


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--framework", required=True, choices=PORTS)
    parser.add_argument("--model-dir", required=True, type=Path, help="Local directory of a trusted model")
    parser.add_argument("--model-id", help="Model name shown to Speaky (default: installed preset name or model directory name)")
    parser.add_argument("--model-alias", help="Accept this previous model ID when migrating existing clients")
    parser.add_argument("--options", type=Path, help="Optional framework-specific UTF-8 JSON file")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, help="Default: framework-specific port between 18761 and 18768")
    args = parser.parse_args()
    model_id, preset_alias = model_identity(args.framework, args.model_dir, args.model_id)
    if not model_id or not args.model_dir.is_dir():
        parser.error("A model directory and a nonempty model ID are required.")
    token = os.environ.get("SPEAKY_ASR_TOKEN", "")
    try:
        local = ipaddress.ip_address(args.host).is_loopback
    except ValueError:
        local = args.host.lower() == "localhost"
    if not local and not token:
        parser.error("Set SPEAKY_ASR_TOKEN before binding beyond loopback.")
    if args.port is not None and not 1 <= args.port <= 65535:
        parser.error("Port must be between 1 and 65535.")

    # Each bridge has its own process and already-provisioned weights.
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    if args.framework in ("funasr", "sherpa-onnx", "wenet", "vosk"):
        from asr_adapters_light import load_adapter
    else:
        from asr_adapters_research import load_adapter
    try:
        options = json.loads(args.options.read_text(encoding="utf-8-sig")) if args.options else {}
        if not isinstance(options, dict):
            raise ValueError("Expected a JSON object in the options file.")
        recognize = load_adapter(args.framework, args.model_dir.resolve(), options)
    except Exception as error:
        parser.exit(1, f"{args.framework} / {model_id}: {error}\n")

    import uvicorn
    uvicorn.run(create_app(args.framework, model_id, recognize, token, args.model_alias or preset_alias),
                host=args.host, port=args.port or PORTS[args.framework])


if __name__ == "__main__":
    main()
