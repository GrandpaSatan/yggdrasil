"""
SpeechT5 TTS server via OpenVINO GenAI on Intel NPU.

Drop-in replacement for MiniCPM-o TTS endpoint and Kokoro TTS.
Serves POST /api/v1/tts compatible with Odin's voice pipeline.

Usage:
    python server.py --model-path ./speecht5_tts --device NPU --port 9095
"""
import argparse
import logging
import struct
import time

import numpy as np
import soundfile as sf
import uvicorn
from fastapi import FastAPI, HTTPException, Response
from pydantic import BaseModel

logging.basicConfig(
    level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s"
)
log = logging.getLogger("tts-speechT5")

app = FastAPI(title="SpeechT5 TTS (OpenVINO)")

_pipeline = None
_speaker_embedding = None
_device = "NPU"
_sample_rate = 16000  # SpeechT5 outputs 16kHz


class TtsRequest(BaseModel):
    text: str
    voice: str | None = None
    speed: float = 1.0


@app.get("/health")
async def health():
    return {
        "status": "ok" if _pipeline is not None else "loading",
        "model": "SpeechT5-OpenVINO",
        "device": _device,
        "sample_rate": _sample_rate,
    }


@app.get("/keepalive")
async def keepalive():
    if _pipeline is None:
        raise HTTPException(503, "model not loaded")
    t0 = time.time()
    _pipeline.generate("Hello.")
    elapsed_ms = (time.time() - t0) * 1000
    log.info("Keepalive: %.0fms", elapsed_ms)
    return {"latency_ms": round(elapsed_ms, 1)}


@app.post("/api/v1/tts")
async def tts(req: TtsRequest):
    """TTS endpoint — drop-in replacement for Kokoro/MiniCPM-o TTS.

    Returns raw PCM s16le bytes with x-sample-rate header.
    """
    if _pipeline is None:
        raise HTTPException(503, "model not loaded")

    t0 = time.time()

    if _speaker_embedding is not None:
        result = _pipeline.generate(req.text, _speaker_embedding)
    else:
        result = _pipeline.generate(req.text)

    elapsed = time.time() - t0

    # result.data is a list of numpy arrays (one per batch)
    audio = result.data[0] if hasattr(result, "data") else np.array(result, dtype=np.float32)

    # Convert float32 → PCM s16le
    clipped = np.clip(audio, -1.0, 1.0)
    pcm_bytes = (clipped * 32767).astype(np.int16).tobytes()

    log.info("TTS: %d samples, %.1fs, text='%s'", len(audio), elapsed, req.text[:60])

    return Response(
        content=pcm_bytes,
        media_type="application/octet-stream",
        headers={"x-sample-rate": str(_sample_rate)},
    )


def load_model(model_path: str, device: str, speaker_embedding_path: str | None):
    global _pipeline, _speaker_embedding, _device

    import openvino_genai as ov_genai

    _device = device
    log.info("Loading SpeechT5 from %s on %s", model_path, device)

    _pipeline = ov_genai.Text2SpeechPipeline(model_path, device)

    if speaker_embedding_path:
        log.info("Loading speaker embedding from %s", speaker_embedding_path)
        _speaker_embedding = np.fromfile(speaker_embedding_path, dtype=np.float32)
        if len(_speaker_embedding) != 512:
            log.warning(
                "Speaker embedding has %d values (expected 512)", len(_speaker_embedding)
            )
    else:
        log.info("No speaker embedding — using default voice")

    # Warmup
    log.info("Running warmup inference...")
    t0 = time.time()
    if _speaker_embedding is not None:
        _pipeline.generate("Hello.", _speaker_embedding)
    else:
        _pipeline.generate("Hello.")
    log.info("Warmup done in %.1fs", time.time() - t0)
    log.info("Server ready on device %s", device)


def main():
    parser = argparse.ArgumentParser(description="SpeechT5 TTS Server (OpenVINO)")
    parser.add_argument(
        "--model-path", default="./speecht5_tts", help="Path to exported OpenVINO model"
    )
    parser.add_argument(
        "--device", default="NPU", help="Inference device: NPU, GPU, CPU"
    )
    parser.add_argument(
        "--speaker-embedding",
        default=None,
        help="Path to .bin file with 512 float32 speaker embedding values",
    )
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=9095)
    args = parser.parse_args()

    load_model(args.model_path, args.device, args.speaker_embedding)
    uvicorn.run(app, host=args.host, port=args.port, log_level="info")


if __name__ == "__main__":
    main()
