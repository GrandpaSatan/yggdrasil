#!/usr/bin/env python3
"""Yggdrasil Voice Server — Qwen2.5-Omni-7B for native speech-to-speech.

Endpoints:
  POST /api/v1/stt   — WAV bytes → { text }
  POST /api/v1/tts   — { text } → PCM i16 bytes + x-sample-rate header
  POST /api/v1/chat  — { audio_b64?, text?, system_prompt? } → { text, audio_b64? }
  GET  /health
"""

import base64
import io
import logging
import os
import sys
import time

import numpy as np
import soundfile as sf
import torch
from flask import Flask, Response, jsonify, request

os.environ.setdefault("TORCH_ROCM_AOTRITON_ENABLE_EXPERIMENTAL", "1")

logging.basicConfig(level=logging.INFO, format="%(asctime)s [%(levelname)s] %(message)s", stream=sys.stdout)
log = logging.getLogger("voice-server")

app = Flask(__name__)

model = None
processor = None

MODEL_PATH = "/opt/yggdrasil/models/qwen25-omni-7b"
SPEAKER = "Ethan"
SAMPLE_RATE = 24000

# MUST use this exact system prompt for audio output to work
DEFAULT_SYSTEM_PROMPT = (
    "You are Qwen, a virtual human developed by the Qwen Team, Alibaba Group, "
    "capable of perceiving auditory and visual inputs, as well as generating text and speech."
)


def load_model():
    global model, processor
    from transformers import Qwen2_5OmniForConditionalGeneration, Qwen2_5OmniProcessor

    log.info(f"Loading processor from {MODEL_PATH}...")
    processor = Qwen2_5OmniProcessor.from_pretrained(MODEL_PATH)

    log.info(f"Loading model (fp16) onto GPU...")
    model = Qwen2_5OmniForConditionalGeneration.from_pretrained(
        MODEL_PATH,
        torch_dtype=torch.float16,
        device_map="auto",
    )

    # Move audio/visual towers to CPU to save VRAM for the main LM
    if hasattr(model, 'thinker') and hasattr(model.thinker, 'audio_tower'):
        model.thinker.audio_tower = model.thinker.audio_tower.to("cpu")
        log.info("Moved audio_tower to CPU")
    if hasattr(model, 'thinker') and hasattr(model.thinker, 'visual'):
        model.thinker.visual = model.thinker.visual.to("cpu")
        log.info("Moved visual tower to CPU")

    log.info("Model loaded.")


def build_inputs(conversation):
    from qwen_omni_utils import process_mm_info
    audios, images, videos = process_mm_info(conversation, use_audio_in_video=True)
    text = processor.apply_chat_template(conversation, add_generation_prompt=True, tokenize=False)
    inputs = processor(
        text=text, audio=audios, images=images, videos=videos,
        return_tensors="pt", padding=True, use_audio_in_video=True,
    )
    inputs = inputs.to(model.device).to(model.dtype)
    return inputs


# ─── Endpoints ──────────────────────────────────────────────────

@app.route("/api/v1/stt", methods=["POST"])
def stt_endpoint():
    audio_data = request.get_data()
    if not audio_data:
        return jsonify({"error": "WAV bytes required"}), 400

    import tempfile
    with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
        f.write(audio_data)
        audio_path = f.name

    try:
        conversation = [
            {"role": "system", "content": [{"type": "text", "text": DEFAULT_SYSTEM_PROMPT}]},
            {"role": "user", "content": [
                {"type": "audio", "audio": audio_path},
                {"type": "text", "text": "Transcribe this audio."},
            ]},
        ]
        inputs = build_inputs(conversation)

        t0 = time.monotonic()
        with torch.no_grad():
            text_ids = model.generate(**inputs, use_audio_in_video=True, return_audio=False, max_new_tokens=256)
        text = processor.batch_decode(text_ids, skip_special_tokens=True, clean_up_tokenization_spaces=False)[0]
        elapsed = time.monotonic() - t0

        log.info(f"STT: '{text}' ({elapsed:.2f}s)")
        return jsonify({"text": text})
    except Exception as e:
        log.error(f"STT failed: {e}")
        return jsonify({"error": str(e)}), 500
    finally:
        os.unlink(audio_path)


@app.route("/api/v1/tts", methods=["POST"])
def tts_endpoint():
    data = request.json
    if not data or "text" not in data:
        return jsonify({"error": "JSON with 'text' required"}), 400

    try:
        conversation = [
            {"role": "system", "content": [{"type": "text", "text": DEFAULT_SYSTEM_PROMPT}]},
            {"role": "user", "content": [{"type": "text", "text": data["text"]}]},
        ]
        inputs = build_inputs(conversation)

        t0 = time.monotonic()
        with torch.no_grad():
            text_ids, audio_out = model.generate(**inputs, return_audio=True, speaker=SPEAKER, max_new_tokens=512)
        elapsed = time.monotonic() - t0

        if audio_out is None:
            return jsonify({"error": "No audio generated"}), 500

        wav = audio_out.reshape(-1).detach().cpu().float().numpy()
        pcm = (np.clip(wav, -1.0, 1.0) * 32767).astype(np.int16).tobytes()

        log.info(f"TTS: {len(data['text'])} chars → {len(pcm)} bytes ({elapsed:.2f}s)")
        return Response(pcm, mimetype="application/octet-stream", headers={"x-sample-rate": str(SAMPLE_RATE)})
    except Exception as e:
        log.error(f"TTS failed: {e}")
        return jsonify({"error": str(e)}), 500


@app.route("/api/v1/chat", methods=["POST"])
def chat_endpoint():
    data = request.json
    if not data:
        return jsonify({"error": "JSON body required"}), 400

    # Always use default system prompt for audio output compatibility
    # Inject persona via the user message instead
    persona_prefix = data.get("persona", "")

    try:
        audio_b64 = data.get("audio_b64")
        text_input = data.get("text")

        if audio_b64:
            import tempfile
            audio_bytes = base64.b64decode(audio_b64)
            with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
                f.write(audio_bytes)
                audio_path = f.name

            try:
                user_content = [{"type": "audio", "audio": audio_path}]
                if persona_prefix:
                    user_content.append({"type": "text", "text": persona_prefix})
                conversation = [
                    {"role": "system", "content": [{"type": "text", "text": DEFAULT_SYSTEM_PROMPT}]},
                    {"role": "user", "content": user_content},
                ]
                inputs = build_inputs(conversation)

                t0 = time.monotonic()
                with torch.no_grad():
                    text_ids, audio_out = model.generate(
                        **inputs, use_audio_in_video=True,
                        return_audio=True, speaker=SPEAKER,
                        max_new_tokens=512,
                    )
                elapsed = time.monotonic() - t0
            finally:
                os.unlink(audio_path)

        elif text_input:
            msg = f"{persona_prefix} {text_input}".strip() if persona_prefix else text_input
            conversation = [
                {"role": "system", "content": [{"type": "text", "text": DEFAULT_SYSTEM_PROMPT}]},
                {"role": "user", "content": [{"type": "text", "text": msg}]},
            ]
            inputs = build_inputs(conversation)

            t0 = time.monotonic()
            with torch.no_grad():
                text_ids, audio_out = model.generate(
                    **inputs, return_audio=True, speaker=SPEAKER,
                    max_new_tokens=512,
                )
            elapsed = time.monotonic() - t0
        else:
            return jsonify({"error": "audio_b64 or text required"}), 400

        text_response = processor.batch_decode(text_ids, skip_special_tokens=True, clean_up_tokenization_spaces=False)[0]

        response = {"text": text_response}
        if audio_out is not None:
            wav = audio_out.reshape(-1).detach().cpu().float().numpy()
            buf = io.BytesIO()
            sf.write(buf, wav, SAMPLE_RATE, format="WAV")
            response["audio_b64"] = base64.b64encode(buf.getvalue()).decode("ascii")

        log.info(f"Chat: '{text_response[:80]}' ({elapsed:.2f}s, audio={'yes' if audio_out is not None else 'no'})")
        return jsonify(response)
    except Exception as e:
        log.error(f"Chat failed: {e}")
        return jsonify({"error": str(e)}), 500


@app.route("/health", methods=["GET"])
def health():
    return jsonify({"status": "ok", "service": "voice-server", "model": "Qwen2.5-Omni-7B", "speaker": SPEAKER})


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=9098)
    parser.add_argument("--host", default="0.0.0.0")
    args = parser.parse_args()

    load_model()
    log.info(f"Starting Qwen2.5-Omni voice server on {args.host}:{args.port}")
    app.run(host=args.host, port=args.port, threaded=False)
