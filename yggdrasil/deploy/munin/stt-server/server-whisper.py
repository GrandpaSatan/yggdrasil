"""
Faster-Whisper STT server — drop-in replacement for SenseVoice/MiniCPM-o STT.

Runs Whisper large-v3 on CPU via faster-whisper (CTranslate2 INT8).
Accepts raw PCM s16le 16kHz mono bytes, returns {"text": "transcription"}.

Usage:
    python server-whisper.py --model large-v3 --port 9097
"""
import argparse
import logging
import time

import numpy as np
import uvicorn
from fastapi import FastAPI, HTTPException, Request
from pydantic import BaseModel

logging.basicConfig(
    level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s"
)
log = logging.getLogger("stt-whisper")

app = FastAPI(title="Whisper STT (faster-whisper)")

_model = None
_model_size = "large-v3"


class SttResponse(BaseModel):
    text: str
    duration: float = 0.0


@app.get("/health")
async def health():
    return {
        "status": "ok" if _model is not None else "loading",
        "model": f"whisper-{_model_size}",
        "backend": "faster-whisper-cpu",
    }


@app.post("/api/v1/stt", response_model=SttResponse)
async def stt(request: Request):
    """STT endpoint — accepts raw PCM s16le 16kHz mono bytes."""
    if _model is None:
        raise HTTPException(503, "model not loaded")

    pcm_bytes = await request.body()
    if len(pcm_bytes) < 100:
        return SttResponse(text="")

    # Convert PCM s16le to float32
    audio = np.frombuffer(pcm_bytes, dtype=np.int16).astype(np.float32) / 32768.0

    # Energy gate
    rms = np.sqrt(np.mean(audio**2))
    if rms < 0.005:
        return SttResponse(text="")

    t0 = time.time()
    segments, info = _model.transcribe(
        audio,
        language="en",
        beam_size=5,
        vad_filter=True,
        vad_parameters=dict(min_silence_duration_ms=500),
    )

    text = " ".join(seg.text.strip() for seg in segments).strip()
    elapsed = time.time() - t0

    log.info("STT: '%s' (%.1fs, rms=%.4f)", text[:80], elapsed, rms)
    return SttResponse(text=text, duration=elapsed)


def load_model(model_size: str, device: str, compute_type: str):
    global _model, _model_size
    from faster_whisper import WhisperModel

    _model_size = model_size
    log.info("Loading Whisper %s on %s (%s)...", model_size, device, compute_type)
    t0 = time.time()
    _model = WhisperModel(model_size, device=device, compute_type=compute_type)
    log.info("Model loaded in %.1fs", time.time() - t0)

    # Warmup
    log.info("Warmup...")
    warmup_audio = np.zeros(16000, dtype=np.float32)  # 1s silence
    _model.transcribe(warmup_audio, language="en")
    log.info("Ready")


def main():
    parser = argparse.ArgumentParser(description="Whisper STT Server")
    parser.add_argument("--model", default="large-v3", help="Whisper model size")
    parser.add_argument("--device", default="cpu", help="cpu or cuda")
    parser.add_argument("--compute-type", default="int8", help="int8, float16, etc.")
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=9097)
    args = parser.parse_args()

    load_model(args.model, args.device, args.compute_type)
    uvicorn.run(app, host=args.host, port=args.port, log_level="info")


if __name__ == "__main__":
    main()
