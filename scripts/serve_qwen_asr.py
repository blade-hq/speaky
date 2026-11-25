"""Expose local Qwen3-ASR through the OpenAI-compatible endpoints used by Speaky."""

import argparse
import io
from pathlib import Path
from threading import Lock

import soundfile as sf
import torch
import uvicorn
from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from fastapi.responses import PlainTextResponse
from qwen_asr import Qwen3ASRModel


MODEL_ID = "Qwen3-ASR-0.6B"
MAX_PROMPT_BYTES = 1024
DEFAULT_MODEL = Path(__file__).resolve().parents[1] / "models" / MODEL_ID
LANGUAGES = {
    "ar": "Arabic", "cs": "Czech", "da": "Danish", "de": "German",
    "el": "Greek", "en": "English", "es": "Spanish", "fa": "Persian",
    "fi": "Finnish", "fil": "Filipino", "fr": "French", "hi": "Hindi",
    "hu": "Hungarian", "id": "Indonesian", "it": "Italian", "ja": "Japanese",
    "ko": "Korean", "mk": "Macedonian", "ms": "Malay", "nl": "Dutch",
    "pl": "Polish", "pt": "Portuguese", "ro": "Romanian", "ru": "Russian",
    "sv": "Swedish", "th": "Thai", "tr": "Turkish", "vi": "Vietnamese",
    "yue": "Cantonese", "zh": "Chinese",
}


def create_app(checkpoint: Path, device: str) -> FastAPI:
    from local_models import qwen_id
    model_id = qwen_id(checkpoint)
    if not model_id:
        raise ValueError("Expected a complete Qwen3-ASR checkpoint")
    recognizer = Qwen3ASRModel.from_pretrained(
        str(checkpoint.resolve(strict=True)),
        dtype=torch.float16 if device.startswith("cuda") else torch.float32,
        device_map=device,
        max_inference_batch_size=1,
        max_new_tokens=4096,
    )
    lock = Lock()
    app = FastAPI(title="Speaky local Qwen3-ASR")

    @app.get("/health")
    def health():
        return {"status": "ok", "model": model_id, "device": device}

    @app.get("/v1/models")
    def list_models():
        return {"object": "list", "data": [
            {"id": model_id, "object": "model", "created": 0, "owned_by": "Qwen"},
        ]}

    @app.post("/v1/audio/transcriptions")
    def transcribe(file: UploadFile = File(...), model: str = Form(...),
                   language: str = Form("auto"), response_format: str = Form("json"),
                   prompt: str = Form("")):
        if model not in (model_id, "Qwen/" + model_id):
            raise HTTPException(404, "Unknown model")
        if response_format not in ("json", "text"):
            raise HTTPException(400, "Expected response_format=json or text")
        if len(prompt.encode("utf-8")) > MAX_PROMPT_BYTES:
            raise HTTPException(413, f"Prompt exceeds {MAX_PROMPT_BYTES} UTF-8 bytes")
        lang = language.lower().replace("_", "-").split("-")[0]
        if lang not in ("", "auto") and lang not in LANGUAGES:
            raise HTTPException(400, "Unsupported language code")
        # Speaky records 16 kHz mono PCM. Limit decoding to Qwen's 30-minute window.
        max_bytes = 64 * 1024 * 1024
        if file.size is not None and file.size > max_bytes:
            raise HTTPException(413, "Audio exceeds 64 MiB")
        try:
            with sf.SoundFile(file.file) as audio:
                if not audio.frames or audio.channels > 2:
                    raise HTTPException(400, "Expected non-empty mono or stereo audio")
                if audio.frames / audio.samplerate > 1800 or audio.frames * audio.channels > max_bytes:
                    raise HTTPException(413, "Audio is too long")
                samples = audio.read(dtype="float32", always_2d=True).mean(axis=1)
                rate = audio.samplerate
        except (ValueError, RuntimeError, io.UnsupportedOperation) as exc:
            raise HTTPException(400, "Invalid audio file") from exc
        with lock:
            result = recognizer.transcribe(
                audio=(samples, rate), language=LANGUAGES.get(lang), context=prompt.strip(),
            )
        text = result[0].text.strip()
        return PlainTextResponse(text) if response_format == "text" else {"text": text}

    return app


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model-dir", type=Path, default=DEFAULT_MODEL)
    parser.add_argument("--port", type=int, default=18107)
    parser.add_argument("--device", choices=["cpu", "cuda:0"],
                        default="cuda:0" if torch.cuda.is_available() else "cpu")
    args = parser.parse_args()
    uvicorn.run(create_app(args.model_dir, args.device), host="127.0.0.1", port=args.port)
