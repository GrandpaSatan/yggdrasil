#!/usr/bin/env python3
"""LFM2.5-Audio-1.5B voice server — single model for STT + LLM + TTS.

Endpoints:
  POST /api/v1/chat   — audio_b64 (WAV) + system_prompt → { text, audio_b64 }
  POST /api/v1/tts    — { text } → raw PCM i16 bytes + x-sample-rate header
  GET  /health        — health check
  GET  /keepalive     — keepalive ping (prevents GPU idle clock-down)

Requires: pip install liquid-audio flask soundfile soxr
"""

import argparse
import base64
import io
import logging
import os
import sys
import time

# Enable experimental Flash Attention on AMD RDNA 4 (gfx1200) for ~4x speedup.
os.environ.setdefault("TORCH_ROCM_AOTRITON_ENABLE_EXPERIMENTAL", "1")

import numpy as np
import soundfile as sf
import torch
from flask import Flask, Response, jsonify, request

from liquid_audio import LFM2AudioModel, LFM2AudioProcessor, ChatState, LFMModality

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    stream=sys.stdout,
)
log = logging.getLogger("lfm-audio")

app = Flask(__name__)

# Globals — set in main()
model = None
processor = None
device = None


def load_model(device_str: str):
    """Load LFM2.5-Audio-1.5B onto the specified device."""
    global model, processor, device

    repo = "LiquidAI/LFM2.5-Audio-1.5B"
    log.info(f"Loading processor from {repo}...")
    processor = LFM2AudioProcessor.from_pretrained(repo).eval()

    log.info(f"Loading model from {repo} onto {device_str}...")
    model = LFM2AudioModel.from_pretrained(repo).eval()
    if device_str == "cpu":
        device = torch.device("cpu")
    else:
        model = model.to(device_str)
        device = torch.device(device_str)

    log.info(f"Model loaded on {device}. VRAM: {torch.cuda.memory_allocated(device) / 1e9:.2f} GB" if device.type == "cuda" else f"Model loaded on {device}.")


def generate_speech_response(audio_wav: torch.Tensor, sr: int, system_prompt: str):
    """Run interleaved generation: audio in → text + audio out.

    Returns (text_response, audio_wav_tensor, sample_rate).
    """
    chat = ChatState(processor)

    # System prompt
    chat.new_turn("system")
    chat.add_text(system_prompt)
    chat.end_turn()

    # User turn: audio input
    chat.new_turn("user")
    chat.add_audio(audio_wav, sr)
    chat.end_turn()

    # Generate response
    chat.new_turn("assistant")
    text_tokens = []
    audio_tokens = []
    modality_flags = []

    t0 = time.monotonic()
    for t in model.generate_interleaved(
        **chat,
        max_new_tokens=512,
        audio_temperature=0.6,
        audio_top_k=8,
    ):
        if t.numel() == 1:
            # Text token
            text_tokens.append(t)
            modality_flags.append(LFMModality.TEXT)
        else:
            # Audio token
            audio_tokens.append(t)
            modality_flags.append(LFMModality.AUDIO_OUT)

    elapsed = time.monotonic() - t0

    # Decode text
    if text_tokens:
        text_ids = torch.stack(text_tokens, dim=1)
        text_response = processor.text.decode(text_ids.squeeze(0).tolist())
    else:
        text_response = ""

    # Decode audio
    audio_wav_out = None
    if audio_tokens:
        # Stack audio codes and decode via Mimi (24kHz output)
        # Remove end-of-audio token if present
        codes = audio_tokens[:-1] if len(audio_tokens) > 1 else audio_tokens
        if codes:
            audio_codes = torch.stack(codes, dim=1).unsqueeze(0)
            audio_wav_out = processor.decode(audio_codes)

    log.info(
        f"Generated {len(text_tokens)} text + {len(audio_tokens)} audio tokens "
        f"in {elapsed:.2f}s — text: {text_response[:80]!r}"
    )

    return text_response, audio_wav_out, 24000


def generate_tts(text: str):
    """Text-only → audio generation (sequential mode).

    Returns (audio_wav_tensor, sample_rate).
    """
    chat = ChatState(processor)

    chat.new_turn("system")
    chat.add_text("You are a helpful voice assistant. Respond with speech.")
    chat.end_turn()

    chat.new_turn("user")
    chat.add_text(text)
    chat.end_turn()

    chat.new_turn("assistant")
    audio_tokens = []

    t0 = time.monotonic()
    for t in model.generate_interleaved(
        **chat,
        max_new_tokens=512,
        audio_temperature=0.6,
        audio_top_k=8,
    ):
        if t.numel() > 1:
            audio_tokens.append(t)

    elapsed = time.monotonic() - t0

    audio_wav_out = None
    if audio_tokens:
        codes = audio_tokens[:-1] if len(audio_tokens) > 1 else audio_tokens
        if codes:
            audio_codes = torch.stack(codes, dim=1).unsqueeze(0)
            audio_wav_out = processor.decode(audio_codes)

    log.info(f"TTS generated {len(audio_tokens)} audio tokens in {elapsed:.2f}s")
    return audio_wav_out, 24000


def wav_tensor_to_pcm_i16(wav: torch.Tensor, sr: int) -> bytes:
    """Convert a float32 waveform tensor to raw PCM i16 little-endian bytes."""
    # Ensure 1D
    if wav.dim() > 1:
        wav = wav.squeeze()
    # Clamp and convert
    pcm = (wav.clamp(-1.0, 1.0).cpu() * 32767.0).to(torch.int16)
    return pcm.numpy().tobytes()


def wav_tensor_to_wav_b64(wav: torch.Tensor, sr: int) -> str:
    """Convert a float32 waveform tensor to base64-encoded WAV."""
    audio_np = wav.squeeze().cpu().numpy()
    buf = io.BytesIO()
    sf.write(buf, audio_np, sr, format="WAV")
    return base64.b64encode(buf.getvalue()).decode("ascii")


@app.route("/api/v1/chat", methods=["POST"])
def chat_endpoint():
    """Speech-to-speech: audio in → text + audio out."""
    data = request.json
    if not data:
        return jsonify({"error": "JSON body required"}), 400

    audio_b64 = data.get("audio_b64")
    if not audio_b64:
        # Text-only input — treat as text chat
        text_input = data.get("text", "")
        if not text_input:
            return jsonify({"error": "audio_b64 or text required"}), 400

        system_prompt = data.get("system_prompt", "You are Fergus, a helpful voice assistant.")
        # For text input, generate audio response via TTS path
        audio_out, sr = generate_tts(text_input)
        response = {"text": text_input}
        if audio_out is not None:
            response["audio_b64"] = wav_tensor_to_wav_b64(audio_out, sr)
        return jsonify(response)

    system_prompt = data.get("system_prompt", "You are Fergus, a helpful voice assistant.")

    # Decode audio via soundfile (avoids torchcodec/libnvrtc on ROCm)
    try:
        audio_bytes = base64.b64decode(audio_b64)
        buf = io.BytesIO(audio_bytes)
        audio_np, sr = sf.read(buf, dtype="float32")
        # soundfile returns (samples,) or (samples, channels) — ensure mono
        if audio_np.ndim > 1:
            audio_np = audio_np.mean(axis=1)
        wav = torch.from_numpy(audio_np).unsqueeze(0)  # (1, samples)
    except Exception as e:
        log.error(f"Failed to decode audio: {e}")
        return jsonify({"error": f"audio decode failed: {e}"}), 400

    # Resample to 16kHz if needed (LFM2.5-Audio expects 16kHz input)
    expected_sr = 16000
    if sr != expected_sr:
        import soxr
        audio_resampled = soxr.resample(audio_np, sr, expected_sr)
        wav = torch.from_numpy(audio_resampled).unsqueeze(0)
        sr = expected_sr

    # Move to device if GPU
    if device.type != "cpu":
        wav = wav.to(device)

    text_response, audio_out, out_sr = generate_speech_response(wav, sr, system_prompt)

    response = {"text": text_response}
    if audio_out is not None:
        response["audio_b64"] = wav_tensor_to_wav_b64(audio_out, out_sr)

    return jsonify(response)


@app.route("/api/v1/tts", methods=["POST"])
def tts_endpoint():
    """Text → raw PCM audio (for alerts/text-only TTS)."""
    data = request.json
    if not data or "text" not in data:
        return jsonify({"error": "text required"}), 400

    text = data["text"]
    audio_out, sr = generate_tts(text)

    if audio_out is None:
        return jsonify({"error": "TTS generation produced no audio"}), 500

    pcm_bytes = wav_tensor_to_pcm_i16(audio_out, sr)
    return Response(
        pcm_bytes,
        mimetype="application/octet-stream",
        headers={"x-sample-rate": str(sr)},
    )


@app.route("/health", methods=["GET"])
def health():
    return jsonify({
        "status": "ok",
        "model": "LFM2.5-Audio-1.5B",
        "device": str(device),
    })


@app.route("/keepalive", methods=["GET"])
def keepalive():
    return jsonify({"status": "alive"})


def main():
    parser = argparse.ArgumentParser(description="LFM2.5-Audio voice server")
    parser.add_argument("--port", type=int, default=9098, help="Listen port (default: 9098)")
    parser.add_argument("--host", type=str, default="0.0.0.0", help="Listen host")
    parser.add_argument(
        "--device",
        type=str,
        default="cuda",
        help="PyTorch device: cuda (eGPU via ROCm/HIP), cpu",
    )
    args = parser.parse_args()

    # Detect device
    device_str = args.device
    if device_str == "cuda" and not torch.cuda.is_available():
        log.warning("CUDA/ROCm not available, falling back to CPU")
        device_str = "cpu"

    load_model(device_str)

    log.info(f"Starting LFM-Audio server on {args.host}:{args.port}")
    app.run(host=args.host, port=args.port, threaded=False)


if __name__ == "__main__":
    main()
