"""
MiniCPM-o 4.5 speech-to-speech server on AMD RX 9060 XT (RDNA 4, gfx1200).

Loads MiniCPM-o with INT4 quantization via custom bitsandbytes (gfx1200 kernels),
exposes STT, TTS, and full speech-to-speech endpoints compatible with Odin's
voice pipeline.

Required environment:
    LD_LIBRARY_PATH=$TORCH_LIB  (PyTorch's bundled ROCm 6.3 libs)

Usage:
    python server.py --port 9098 --ref-audio /path/to/fergus_voice.wav
"""
import argparse
import base64
import io
import logging
import struct
import tempfile
import time

import numpy as np
import soundfile as sf
import torch

# ── Patch torchaudio BEFORE any imports that trigger it ───────
# torchaudio 2.9.1 requires torchcodec which needs ffmpeg 4.
# Ubuntu 24.04 ships ffmpeg 7. Patch load/save to use soundfile.
import torchaudio


def _sf_load(filepath, **kwargs):
    if filepath is None:
        raise ValueError("torchaudio.load: filepath is None")
    data, sr = sf.read(str(filepath), dtype="float32")
    t = torch.from_numpy(data)
    if t.dim() == 1:
        t = t.unsqueeze(0)
    else:
        t = t.T
    return t, sr


def _sf_save(filepath, src, sample_rate, **kwargs):
    if isinstance(src, torch.Tensor):
        src = src.cpu().numpy()
    if src.ndim == 2:
        src = src.T
    fmt = kwargs.get("format", None)
    if isinstance(filepath, io.BytesIO):
        sf.write(filepath, src, sample_rate, format=fmt or "WAV")
    else:
        sf.write(str(filepath), src, sample_rate)


torchaudio.load = _sf_load
torchaudio.save = _sf_save

# Patch s3tokenizer audio loading (same torchaudio dependency)
import s3tokenizer.utils as _s3u


def _sf_load_audio(file, sr=16000):
    if file is None:
        raise ValueError("s3tokenizer.load_audio: file is None")
    data, sample_rate = sf.read(str(file), dtype="float32")
    if len(data.shape) > 1:
        data = data.mean(axis=1)
    if sample_rate != sr:
        import librosa

        data = librosa.resample(data, orig_sr=sample_rate, target_sr=sr)
    return torch.from_numpy(data)


_s3u.load_audio = _sf_load_audio

# ── Now safe to import everything else ────────────────────────
import uvicorn
from fastapi import FastAPI, HTTPException, Request, Response
from pydantic import BaseModel, Field
from transformers import AutoModel, AutoProcessor, BitsAndBytesConfig

logging.basicConfig(
    level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s"
)
log = logging.getLogger("minicpm-server")

# ── Pydantic models ──────────────────────────────────────────

class SttResponse(BaseModel):
    text: str
    duration: float = 0.0


class ChatRequest(BaseModel):
    text: str = ""
    audio_b64: str | None = None
    session_id: str | None = None
    generate_audio: bool = False
    system_prompt: str | None = None


class ChatResponse(BaseModel):
    text: str
    audio_b64: str | None = None
    sample_rate: int = 24000
    latency_ms: float = 0.0


class TtsRequest(BaseModel):
    text: str
    voice: str | None = None
    speed: float = 1.0


class HealthResponse(BaseModel):
    status: str
    model: str
    vram_gb: float
    tts_ready: bool


# ── Global state ─────────────────────────────────────────────

app = FastAPI(title="MiniCPM-o 4.5 Server (RDNA4)")
_model = None
_processor = None
_ref_audio = None  # 16kHz float32 numpy array
_tts_ready = False
_model_name = "MiniCPM-o-4.5"

# ── Audio helpers ────────────────────────────────────────────


def pcm_s16le_to_float32(pcm_bytes: bytes, sample_rate: int = 16000) -> np.ndarray:
    """Convert raw PCM s16le bytes to float32 numpy array."""
    samples = np.frombuffer(pcm_bytes, dtype=np.int16)
    return samples.astype(np.float32) / 32768.0


def float32_to_pcm_s16le(audio: np.ndarray) -> bytes:
    """Convert float32 numpy array to raw PCM s16le bytes."""
    clipped = np.clip(audio, -1.0, 1.0)
    samples = (clipped * 32767).astype(np.int16)
    return samples.tobytes()


def decode_audio_b64(b64_data: str) -> tuple[np.ndarray, int]:
    """Decode base64 WAV to (float32 numpy, sample_rate)."""
    raw = base64.b64decode(b64_data)
    audio, sr = sf.read(io.BytesIO(raw), dtype="float32")
    if audio.ndim > 1:
        audio = audio.mean(axis=1)
    return audio, sr


def resample_to_16k(audio: np.ndarray, sr: int) -> np.ndarray:
    """Resample audio to 16kHz if needed."""
    if sr == 16000:
        return audio
    import librosa

    return librosa.resample(audio, orig_sr=sr, target_sr=16000)


def encode_wav_b64(audio: np.ndarray, sr: int = 24000) -> str:
    """Encode float32 audio to base64 WAV string."""
    buf = io.BytesIO()
    sf.write(buf, audio, sr, format="WAV")
    return base64.b64encode(buf.getvalue()).decode("ascii")


# ── Endpoints ────────────────────────────────────────────────


@app.get("/health")
async def health():
    vram = torch.cuda.memory_allocated() / 1024**3 if torch.cuda.is_available() else 0
    return HealthResponse(
        status="ok" if _model is not None else "loading",
        model=_model_name,
        vram_gb=round(vram, 1),
        tts_ready=_tts_ready,
    )


@app.post("/api/v1/stt", response_model=SttResponse)
async def stt(request: Request):
    """STT endpoint — drop-in replacement for SenseVoice.

    Accepts raw PCM s16le 16kHz mono bytes (Content-Type: application/octet-stream).
    Returns {"text": "transcription"}.
    """
    if _model is None:
        raise HTTPException(503, "model not loaded")

    pcm_bytes = await request.body()
    if len(pcm_bytes) < 100:
        return SttResponse(text="")

    audio = pcm_s16le_to_float32(pcm_bytes)

    # Energy gate — skip silence
    rms = np.sqrt(np.mean(audio**2))
    if rms < 0.005:
        return SttResponse(text="")

    t0 = time.time()
    result = _model.chat(
        msgs=[
            {"role": "system", "content": "You are a speech-to-text transcription system. Output ONLY the exact words spoken. No explanations, no apologies, no commentary. If the audio is unclear, output your best guess. Never say 'I'm sorry' or 'I cannot'."},
            {"role": "user", "content": "Transcribe:"},
        ],
        tokenizer=_processor.tokenizer,
        audio=[(audio, 16000)],
    )
    elapsed = time.time() - t0

    text = result.strip() if isinstance(result, str) else ""
    log.info("STT: '%s' (%.1fs)", text[:80], elapsed)
    return SttResponse(text=text, duration=elapsed)


@app.post("/api/v1/chat", response_model=ChatResponse)
async def chat(req: ChatRequest):
    """Full speech-to-speech chat endpoint.

    Accepts text and/or base64 audio input.
    Returns text response and optionally base64 audio output.
    """
    if _model is None:
        raise HTTPException(503, "model not loaded")

    system = req.system_prompt or (
        "Listen to the user's audio and respond naturally. "
        "You are Fergus, a British butler. Address the user as 'sir'. "
        "Be brief — one to three sentences."
    )

    msgs = [{"role": "system", "content": system}]

    # Audio goes INTO the message content as np.ndarray — MiniCPM-o's chat()
    # scans content for np.ndarray instances and injects <audio> tokens.
    if req.audio_b64:
        audio_np, sr = decode_audio_b64(req.audio_b64)
        audio_np = resample_to_16k(audio_np, sr)
        log.info(
            "Audio input: %d samples (%.1fs), dtype=%s, range=[%.3f, %.3f], rms=%.4f",
            len(audio_np), len(audio_np) / 16000,
            audio_np.dtype, audio_np.min(), audio_np.max(),
            np.sqrt(np.mean(audio_np ** 2)),
        )
        # MiniCPM-o expects: content = [text_prompt, np.ndarray_audio]
        # The text prompt must reference the audio or the model ignores it
        prompt = req.text if req.text else "Listen and respond to what I just said."
        msgs.append({"role": "user", "content": [prompt, audio_np]})
    elif req.text:
        msgs.append({"role": "user", "content": req.text})

    t0 = time.time()

    chat_kwargs = {
        "msgs": msgs,
        "tokenizer": _processor.tokenizer,
    }

    if req.generate_audio and _tts_ready:
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tmp:
            tmp_path = tmp.name

        chat_kwargs["generate_audio"] = True
        chat_kwargs["use_tts_template"] = True
        chat_kwargs["output_audio_path"] = tmp_path

        result = _model.chat(**chat_kwargs)

        elapsed = time.time() - t0
        text = result.strip() if isinstance(result, str) else ""

        audio_b64 = None
        import os

        if os.path.exists(tmp_path) and os.path.getsize(tmp_path) > 0:
            data, sr = sf.read(tmp_path, dtype="float32")
            audio_b64 = encode_wav_b64(data, sr)
            os.unlink(tmp_path)
            log.info("Chat+TTS: '%s' (%d samples, %.1fs)", text[:60], len(data), elapsed)
        else:
            log.info("Chat (no audio): '%s' (%.1fs)", text[:60], elapsed)
            if os.path.exists(tmp_path):
                os.unlink(tmp_path)

        return ChatResponse(text=text, audio_b64=audio_b64, latency_ms=elapsed * 1000)
    else:
        result = _model.chat(**chat_kwargs)
        elapsed = time.time() - t0
        text = result.strip() if isinstance(result, str) else ""
        log.info("Chat: '%s' (%.1fs)", text[:60], elapsed)
        return ChatResponse(text=text, latency_ms=elapsed * 1000)


@app.post("/api/v1/tts")
async def tts(req: TtsRequest):
    """TTS endpoint — drop-in replacement for Kokoro.

    Returns raw PCM audio bytes with x-sample-rate header.
    """
    if _model is None or not _tts_ready:
        raise HTTPException(503, "TTS not ready")

    audio_input = [(_ref_audio, 16000)] if _ref_audio is not None else None

    with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tmp:
        tmp_path = tmp.name

    t0 = time.time()
    _model.chat(
        msgs=[{"role": "user", "content": req.text}],
        tokenizer=_processor.tokenizer,
        audio=audio_input,
        generate_audio=True,
        use_tts_template=True,
        output_audio_path=tmp_path,
    )
    elapsed = time.time() - t0

    import os

    if os.path.exists(tmp_path) and os.path.getsize(tmp_path) > 0:
        data, sr = sf.read(tmp_path, dtype="float32")
        os.unlink(tmp_path)
        pcm_bytes = float32_to_pcm_s16le(data)
        log.info("TTS: %d samples, %dHz, %.1fs", len(data), sr, elapsed)
        return Response(
            content=pcm_bytes,
            media_type="application/octet-stream",
            headers={"x-sample-rate": str(sr)},
        )
    else:
        if os.path.exists(tmp_path):
            os.unlink(tmp_path)
        raise HTTPException(500, "TTS produced no audio")


# ── Model loading ────────────────────────────────────────────


def load_model(model_id: str, ref_audio_path: str | None):
    global _model, _processor, _ref_audio, _tts_ready

    log.info("Loading processor from %s", model_id)
    _processor = AutoProcessor.from_pretrained(model_id, trust_remote_code=True)

    log.info("Loading model INT4 (NF4) with TTS modules in FP16...")
    _model = AutoModel.from_pretrained(
        model_id,
        quantization_config=BitsAndBytesConfig(
            load_in_4bit=True,
            bnb_4bit_compute_dtype=torch.float16,
            bnb_4bit_quant_type="nf4",
            llm_int8_enable_fp32_cpu_offload=True,
            llm_int8_skip_modules=["tts", "head_code", "token2wav", "apm", "audio_avg_pooler", "audio_projection_layer", "vpm", "resampler"],
        ),
        device_map="auto",
        trust_remote_code=True,
        attn_implementation="eager",
        offload_buffers=True,
    )

    vram = torch.cuda.memory_allocated() / 1024**3
    log.info("Model loaded. VRAM: %.1f GB", vram)

    # Initialize TTS
    log.info("Initializing TTS module...")
    _model.init_tts()

    # Load voice reference for cloning
    if ref_audio_path:
        log.info("Loading voice reference from %s", ref_audio_path)
        ref, ref_sr = sf.read(ref_audio_path, dtype="float32")
        _ref_audio = resample_to_16k(ref, ref_sr)
        _model.init_token2wav_cache(_ref_audio)
        log.info("Voice reference loaded: %d samples at 16kHz", len(_ref_audio))

    _tts_ready = True
    log.info("TTS ready")

    # Warmup inference
    log.info("Running warmup...")
    t0 = time.time()
    _model.chat(
        msgs=[{"role": "user", "content": "Hello."}],
        tokenizer=_processor.tokenizer,
    )
    log.info("Warmup done in %.1fs", time.time() - t0)
    log.info("Server ready. VRAM: %.1f GB", torch.cuda.memory_allocated() / 1024**3)


# ── Main ─────────────────────────────────────────────────────


def main():
    parser = argparse.ArgumentParser(description="MiniCPM-o 4.5 Server (RDNA4)")
    parser.add_argument("--model", default="openbmb/MiniCPM-o-4_5")
    parser.add_argument("--ref-audio", default=None, help="Voice reference WAV for cloning")
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=9098)
    args = parser.parse_args()

    load_model(args.model, args.ref_audio)
    uvicorn.run(app, host=args.host, port=args.port, log_level="info")


if __name__ == "__main__":
    main()
