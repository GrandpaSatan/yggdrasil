"""
Qwen3-ASR-1.7B STT server — drop-in replacement for ygg-voice /api/v1/stt.

Accepts raw PCM s16le audio at 16kHz mono, returns {"text": "transcription"}.
Uses the qwen-asr Python package with transformers backend on CPU.

Usage:
    pip install qwen-asr fastapi uvicorn
    python server.py --model Qwen/Qwen3-ASR-1.7B --port 9097
"""
import argparse
import io
import logging
import struct
import time

import numpy as np
import uvicorn
from fastapi import FastAPI, Request, Response
from fastapi.responses import JSONResponse

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
log = logging.getLogger("qwen3-asr")

app = FastAPI(title="Qwen3-ASR Server")
asr_model = None

SAMPLE_RATE = 16000

@app.get("/health")
async def health():
    return {"status": "ok", "model_loaded": asr_model is not None}

@app.post("/api/v1/stt")
async def stt(request: Request):
    """Drop-in replacement for ygg-voice STT endpoint.

    Accepts: raw PCM s16le bytes (16kHz, mono) as application/octet-stream
    Returns: {"text": "transcribed text"}
    """
    if asr_model is None:
        return JSONResponse(status_code=503, content={"error": "model not loaded"})

    body = await request.body()
    if len(body) < 2:
        return JSONResponse(status_code=400, content={"error": "body too small"})

    # Decode PCM s16le to float32 numpy array
    n_samples = len(body) // 2
    samples_i16 = struct.unpack(f"<{n_samples}h", body[:n_samples * 2])
    audio = np.array(samples_i16, dtype=np.float32) / 32768.0

    # Energy gate — skip near-silence
    rms = np.sqrt(np.mean(audio ** 2))
    if rms < 0.005:
        log.info("silence detected (rms=%.4f), skipping", rms)
        return {"text": ""}

    t0 = time.time()

    try:
        result = asr_model.transcribe(
            (audio, SAMPLE_RATE),
        )
        # result may be ASRTranscription object, list, or string
        if isinstance(result, list):
            item = result[0] if result else None
        else:
            item = result
        # Extract .text from ASRTranscription object if present
        text = getattr(item, "text", str(item)) if item else ""
        text = text.strip()
        elapsed = time.time() - t0
        log.info("transcribed in %.2fs: %s", elapsed, text[:100])
        return {"text": text}
    except Exception as e:
        log.exception("transcription failed")
        return JSONResponse(status_code=500, content={"error": str(e)})

def load_model(model_id: str, device: str):
    global asr_model
    from qwen_asr import Qwen3ASRModel

    log.info("loading Qwen3-ASR model: %s on %s", model_id, device)
    asr_model = Qwen3ASRModel.from_pretrained(model_id)
    log.info("model loaded")

def main():
    parser = argparse.ArgumentParser(description="Qwen3-ASR STT Server")
    parser.add_argument("--model", default="Qwen/Qwen3-ASR-1.7B")
    parser.add_argument("--device", default="cpu", help="cpu or cuda or xpu")
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=9097)
    args = parser.parse_args()

    load_model(args.model, args.device)
    uvicorn.run(app, host=args.host, port=args.port, log_level="info")

if __name__ == "__main__":
    main()
