#!/usr/bin/env python3
"""Yggdrasil Voice Server — RNNoise + Whisper STT + Kokoro TTS.

Endpoints:
  POST /api/v1/tts  — { text, voice? } → raw PCM i16 bytes + x-sample-rate header
  POST /api/v1/stt  — WAV audio bytes → { text } (RNNoise denoise + Whisper transcription)
  GET  /health      — health check
"""

import io
import logging
import sys
import time
import wave

import numpy as np
from flask import Flask, Response, jsonify, request

logging.basicConfig(level=logging.INFO, format="%(asctime)s [%(levelname)s] %(message)s", stream=sys.stdout)
log = logging.getLogger("voice-server")

app = Flask(__name__)
kokoro = None
whisper_model = None
denoiser = None


def load_models():
    global kokoro, whisper_model, denoiser

    from kokoro_onnx import Kokoro
    log.info("Loading Kokoro TTS...")
    kokoro = Kokoro("kokoro-v1.0.onnx", "voices-v1.0.bin")
    log.info("Kokoro TTS ready")

    from faster_whisper import WhisperModel
    log.info("Loading Whisper STT (small)...")
    whisper_model = WhisperModel("small", device="cpu", compute_type="int8")
    log.info("Whisper STT ready")

    from pyrnnoise import RNNoise
    denoiser = RNNoise(sample_rate=48000)
    log.info("RNNoise denoiser ready")


def denoise_audio(wav_bytes: bytes) -> bytes:
    """Apply RNNoise to WAV audio. Returns denoised WAV bytes."""
    # Write input to temp file — pyrnnoise.denoise_wav works with file paths
    import tempfile, os, soundfile as sf
    with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tmp_in:
        tmp_in.write(wav_bytes)
        in_path = tmp_in.name
    out_path = in_path.replace(".wav", "_clean.wav")

    try:
        # denoise_wav is a generator — must consume it
        for _ in denoiser.denoise_wav(in_path, out_path):
            pass
        with open(out_path, "rb") as f:
            return f.read()
    finally:
        os.unlink(in_path)
        if os.path.exists(out_path):
            os.unlink(out_path)


@app.route("/api/v1/tts", methods=["POST"])
def tts_endpoint():
    data = request.json
    if not data or "text" not in data:
        return jsonify({"error": "JSON body with 'text' required"}), 400

    text = data["text"]
    voice = data.get("voice", "bm_george")

    t0 = time.monotonic()
    try:
        audio, sr = kokoro.create(text, voice=voice, speed=1.0)
    except Exception as e:
        log.error(f"TTS failed: {e}")
        return jsonify({"error": str(e)}), 500

    elapsed = time.monotonic() - t0
    pcm = (np.clip(audio, -1.0, 1.0) * 32767).astype(np.int16)
    pcm_bytes = pcm.tobytes()

    log.info(f"TTS: {len(text)} chars -> {len(pcm_bytes)} bytes ({elapsed:.2f}s) voice={voice}")

    return Response(
        pcm_bytes,
        mimetype="application/octet-stream",
        headers={"x-sample-rate": str(sr)},
    )


@app.route("/api/v1/stt", methods=["POST"])
def stt_endpoint():
    """Denoise with RNNoise then transcribe with Whisper."""
    audio_data = request.get_data()
    if not audio_data:
        return jsonify({"error": "raw WAV bytes required in body"}), 400

    t0 = time.monotonic()
    try:
        # Skip RNNoise for now — Whisper's VAD handles noise well on its own.
        # RNNoise was stripping speech along with noise at this mic's characteristics.
        denoise_ms = 0

        # Whisper transcription with built-in VAD noise filtering
        t1 = time.monotonic()
        segments, info = whisper_model.transcribe(
            io.BytesIO(audio_data),
            language="en",
            initial_prompt="Hey Fergus, can you help me?",
            vad_filter=True,
            vad_parameters=dict(
                min_silence_duration_ms=300,
                speech_pad_ms=200,
                threshold=0.5,
            ),
            no_speech_threshold=0.6,
            condition_on_previous_text=False,
        )
        text = " ".join(seg.text.strip() for seg in segments).strip()
        whisper_ms = (time.monotonic() - t1) * 1000
    except Exception as e:
        log.error(f"STT failed: {e}")
        return jsonify({"error": str(e)}), 500

    total_ms = (time.monotonic() - t0) * 1000
    log.info(f"STT: '{text}' (denoise={denoise_ms:.0f}ms whisper={whisper_ms:.0f}ms total={total_ms:.0f}ms)")

    return jsonify({"text": text})


@app.route("/health", methods=["GET"])
def health():
    return jsonify({"status": "ok", "service": "voice-server", "models": ["rnnoise", "whisper-base", "kokoro-v1.0"]})


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=9098)
    parser.add_argument("--host", default="0.0.0.0")
    args = parser.parse_args()

    load_models()
    log.info(f"Starting voice server on {args.host}:{args.port}")
    app.run(host=args.host, port=args.port, threaded=False)
