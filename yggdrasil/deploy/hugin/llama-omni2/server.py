#!/usr/bin/env python3
"""Yggdrasil Voice Server — LLaMA-Omni2-3B for speech-to-speech.

Single voice model for the entire Yggdrasil ecosystem. Uses CosyVoice2
decoder for high-quality 24kHz speech synthesis.

Endpoints:
  POST /api/v1/stt   — WAV bytes -> { text }
  POST /api/v1/tts   — { text }   -> PCM i16 bytes + x-sample-rate header
  POST /api/v1/chat  — { audio_b64?, text?, system_prompt? } -> { text, audio_b64? }
  GET  /health

ROCm compatibility:
  - Uses soundfile+scipy for all WAV I/O (avoids torchaudio/torchcodec CUDA deps)
  - Patches torchaudio.set_audio_backend (removed in newer torchaudio)
  - Patches cosyvoice.utils.file_utils.load_wav to use soundfile
"""

import base64
import io
import logging
import math
import os
import sys
import tempfile
import time

# ---------------------------------------------------------------------------
# ROCm + torchaudio compatibility — MUST run before any model imports
# ---------------------------------------------------------------------------
os.environ.setdefault("TORCH_ROCM_AOTRITON_ENABLE_EXPERIMENTAL", "1")

import numpy as np
import soundfile as sf
import torch
from scipy.signal import resample_poly

# Shim: torchaudio.set_audio_backend was removed in torchaudio >= 2.5
import torchaudio
if not hasattr(torchaudio, "set_audio_backend"):
    torchaudio.set_audio_backend = lambda x: None

# ---------------------------------------------------------------------------
# Configuration — all paths via env vars, no hardcoded IPs
# ---------------------------------------------------------------------------
LLAMA_OMNI2_ROOT = os.environ.get(
    "LLAMA_OMNI2_ROOT", "/opt/yggdrasil/deploy/llama-omni2/LLaMA-Omni2"
)
MODEL_PATH = os.environ.get(
    "LLAMA_OMNI2_MODEL", "/opt/yggdrasil/models/llama-omni2-3b"
)
COSY2_DECODER_PATH = os.environ.get(
    "COSY2_DECODER_PATH", "/opt/yggdrasil/models/cosy2_decoder"
)
SPEECH_ENCODER_PATH = os.environ.get(
    "SPEECH_ENCODER_PATH", "/opt/yggdrasil/models/speech_encoder/large-v3.pt"
)
PROMPT_WAV_PATH = os.environ.get(
    "PROMPT_WAV_PATH",
    os.path.join(LLAMA_OMNI2_ROOT, "llama_omni2", "inference", "prompt_en.wav"),
)

SAMPLE_RATE = 24000  # CosyVoice2 output sample rate

# ---------------------------------------------------------------------------
# sys.path setup — LLaMA-Omni2 source + Matcha-TTS (CosyVoice2 dependency)
# ---------------------------------------------------------------------------
if LLAMA_OMNI2_ROOT not in sys.path:
    sys.path.insert(0, LLAMA_OMNI2_ROOT)
# CosyVoice2's flow model imports matcha.models from Matcha-TTS-full
matcha_path = os.path.join(LLAMA_OMNI2_ROOT, "third_party", "Matcha-TTS-full")
if not os.path.isdir(matcha_path):
    matcha_path = os.path.join(LLAMA_OMNI2_ROOT, "third_party", "Matcha-TTS")
if matcha_path not in sys.path:
    sys.path.insert(0, matcha_path)

# ---------------------------------------------------------------------------
# Soundfile-based WAV loader (replaces torchaudio.load — works on ROCm)
# ---------------------------------------------------------------------------
def load_wav_soundfile(wav_path, target_sr=16000):
    """Load WAV using soundfile + scipy resampling. Returns torch tensor [1, N]."""
    audio, orig_sr = sf.read(wav_path, dtype="float32", always_2d=False)
    if audio.ndim == 2:
        audio = audio.mean(axis=1)
    if orig_sr != target_sr:
        gcd = math.gcd(target_sr, orig_sr)
        audio = resample_poly(audio, target_sr // gcd, orig_sr // gcd).astype(np.float32)
    return torch.from_numpy(audio).unsqueeze(0)


# Monkey-patch cosyvoice's load_wav BEFORE it gets imported by SpeechDecoder.
# This avoids torchaudio.load → torchcodec → libnvrtc.so CUDA dep on ROCm.
import cosyvoice.utils.file_utils as _cosyvoice_file_utils
_cosyvoice_file_utils.load_wav = load_wav_soundfile

# ---------------------------------------------------------------------------
# Flask app + logging
# ---------------------------------------------------------------------------
from flask import Flask, Response, jsonify, request

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    stream=sys.stdout,
)
log = logging.getLogger("voice-server")

app = Flask(__name__)

# ---------------------------------------------------------------------------
# Globals populated by load_models()
# ---------------------------------------------------------------------------
tokenizer = None
s2s_model = None          # Omni2Speech2SQwen2ForCausalLM (text + speech units)
speech_decoder = None     # CosyVoice2 SpeechDecoder
prompt_speech_16k = None  # Pre-loaded prompt wav for CosyVoice2 speaker embedding


def load_models():
    """Load S2S model + CosyVoice2 decoder + prompt WAV.

    Single model instance handles STT, TTS, and chat (~10GB VRAM).
    """
    global tokenizer, s2s_model, speech_decoder, prompt_speech_16k

    from transformers import AutoTokenizer, AutoConfig
    from llama_omni2.model import Omni2Speech2SQwen2ForCausalLM
    from llama_omni2.inference.run_cosy2_decoder import SpeechDecoder as Cosy2Decoder

    # Tokenizer
    log.info("Loading tokenizer from %s", MODEL_PATH)
    tokenizer = AutoTokenizer.from_pretrained(MODEL_PATH, use_fast=False)

    # S2S model (text + speech units — used for all endpoints)
    log.info("Loading S2S model from %s", MODEL_PATH)
    config = AutoConfig.from_pretrained(MODEL_PATH)
    config.speech_encoder = SPEECH_ENCODER_PATH
    config.tts_tokenizer = os.path.join(MODEL_PATH, "tts_tokenizer")
    s2s_model = Omni2Speech2SQwen2ForCausalLM.from_pretrained(
        MODEL_PATH, config=config, torch_dtype=torch.bfloat16
    )
    s2s_model.cuda().eval()
    log.info("S2S model loaded (~%.1f GB VRAM)", torch.cuda.memory_allocated() / 1e9)

    # CosyVoice2 decoder (flow + HiFT vocoder)
    log.info("Loading CosyVoice2 decoder from %s", COSY2_DECODER_PATH)
    speech_decoder = Cosy2Decoder(model_dir=COSY2_DECODER_PATH)
    log.info("CosyVoice2 decoder loaded")

    # Prompt WAV (determines voice character — "Alfred" male voice)
    log.info("Loading prompt WAV from %s", PROMPT_WAV_PATH)
    prompt_speech_16k = load_wav_soundfile(PROMPT_WAV_PATH, 16000)
    log.info("All models loaded successfully")


# ---------------------------------------------------------------------------
# Inference helpers
# ---------------------------------------------------------------------------
def _wav_bytes_to_mel(wav_bytes: bytes):
    """Convert raw WAV bytes to mel spectrogram tensor for the model."""
    import whisper

    tmp = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
    try:
        tmp.write(wav_bytes)
        tmp.close()
        audio, orig_sr = sf.read(tmp.name, dtype="float32", always_2d=False)
    finally:
        os.unlink(tmp.name)

    if audio.ndim == 2:
        audio = audio.mean(axis=1)
    target_sr = whisper.audio.SAMPLE_RATE  # 16000
    if orig_sr != target_sr:
        gcd = math.gcd(target_sr, orig_sr)
        audio = resample_poly(audio, target_sr // gcd, orig_sr // gcd).astype(np.float32)

    audio = whisper.pad_or_trim(audio)
    mel = whisper.log_mel_spectrogram(audio, n_mels=128).permute(1, 0)
    mel_tensor = mel.unsqueeze(0).to(dtype=torch.bfloat16, device="cuda")
    lengths = torch.LongTensor([mel_tensor.shape[1]]).to("cuda")
    return mel_tensor, lengths


def _prepare_input_ids(text_content: str = None):
    """Build input_ids from chat template. Uses speech token for audio input."""
    from llama_omni2.constants import SPEECH_TOKEN_INDEX, DEFAULT_SPEECH_TOKEN

    if text_content:
        messages = [{"role": "user", "content": text_content}]
    else:
        messages = [{"role": "user", "content": DEFAULT_SPEECH_TOKEN}]

    input_ids = tokenizer.apply_chat_template(
        messages, add_generation_prompt=True, return_tensors="pt"
    )[0]

    if not text_content:
        speech_token_id = tokenizer.convert_tokens_to_ids(DEFAULT_SPEECH_TOKEN)
        input_ids[input_ids == speech_token_id] = SPEECH_TOKEN_INDEX

    return input_ids.unsqueeze(0).to("cuda")


def _generate_s2s(input_ids, speech_tensor, speech_lengths, max_new_tokens=512):
    """Run S2S model — returns (text, speech_units)."""
    with torch.inference_mode():
        output_ids, output_units = s2s_model.generate(
            input_ids,
            speech=speech_tensor,
            speech_lengths=speech_lengths,
            do_sample=False,
            temperature=None,
            top_p=None,
            num_beams=1,
            max_new_tokens=max_new_tokens,
            use_cache=True,
            pad_token_id=tokenizer.pad_token_id,
        )
    text = tokenizer.batch_decode(output_ids, skip_special_tokens=True)[0].strip()
    return text, output_units


def _decode_units_to_wav(units_str) -> np.ndarray:
    """Decode speech units to float32 numpy array at 24kHz via CosyVoice2."""
    from llama_omni2.inference.run_cosy2_decoder import process_units

    if isinstance(units_str, str):
        units = process_units(units_str)
    elif isinstance(units_str, (list, tuple)):
        units = list(units_str)
    else:
        units = process_units(str(units_str))

    if not units:
        return None

    x = torch.LongTensor(units).cuda()
    tts_speech = speech_decoder.entry(x, prompt_speech_16k, stream=False)
    return tts_speech.squeeze(0).numpy().astype(np.float32)


def _wav_to_pcm_bytes(wav: np.ndarray) -> bytes:
    """Convert float32 WAV to PCM int16 bytes."""
    return (np.clip(wav, -1.0, 1.0) * 32767).astype(np.int16).tobytes()


def _wav_to_b64(wav: np.ndarray) -> str:
    """Encode float32 WAV as base64 WAV file."""
    buf = io.BytesIO()
    sf.write(buf, wav, SAMPLE_RATE, format="WAV")
    return base64.b64encode(buf.getvalue()).decode("ascii")


# ---------------------------------------------------------------------------
# Endpoints
# ---------------------------------------------------------------------------
@app.route("/api/v1/stt", methods=["POST"])
def stt_endpoint():
    """Speech-to-text: raw WAV bytes -> {"text": "..."}."""
    audio_data = request.get_data()
    if not audio_data:
        return jsonify({"error": "WAV bytes required"}), 400

    try:
        mel_tensor, lengths = _wav_bytes_to_mel(audio_data)
        input_ids = _prepare_input_ids()

        t0 = time.monotonic()
        text, _ = _generate_s2s(input_ids, mel_tensor, lengths, max_new_tokens=256)
        elapsed = time.monotonic() - t0

        log.info("STT: '%s' (%.2fs)", text[:120], elapsed)
        return jsonify({"text": text})
    except Exception as e:
        log.error("STT failed: %s", e, exc_info=True)
        return jsonify({"error": str(e)}), 500


@app.route("/api/v1/tts", methods=["POST"])
def tts_endpoint():
    """Text-to-speech: {"text": "..."} -> PCM int16 bytes + x-sample-rate header."""
    data = request.json
    if not data or "text" not in data:
        return jsonify({"error": "JSON with 'text' required"}), 400

    try:
        text_input = data["text"]
        input_ids = _prepare_input_ids(text_content=text_input)

        dummy_speech = torch.zeros(1, 1, 128, dtype=torch.bfloat16, device="cuda")
        dummy_lengths = torch.LongTensor([1]).to("cuda")

        t0 = time.monotonic()
        _, units = _generate_s2s(input_ids, dummy_speech, dummy_lengths)
        wav = _decode_units_to_wav(units)
        elapsed = time.monotonic() - t0

        if wav is None:
            return jsonify({"error": "No audio generated"}), 500

        pcm = _wav_to_pcm_bytes(wav)
        log.info("TTS: %d chars -> %d bytes (%.2fs)", len(text_input), len(pcm), elapsed)
        return Response(
            pcm,
            mimetype="application/octet-stream",
            headers={"x-sample-rate": str(SAMPLE_RATE)},
        )
    except Exception as e:
        log.error("TTS failed: %s", e, exc_info=True)
        return jsonify({"error": str(e)}), 500


@app.route("/api/v1/chat", methods=["POST"])
def chat_endpoint():
    """Speech-to-speech chat: audio and/or text -> text + audio.

    Request: { audio_b64?, text?, system_prompt?, persona? }
    Response: { text, audio_b64? }
    """
    data = request.json
    if not data:
        return jsonify({"error": "JSON body required"}), 400

    try:
        audio_b64 = data.get("audio_b64")
        text_input = data.get("text")
        persona = data.get("persona", "")

        if audio_b64:
            audio_bytes = base64.b64decode(audio_b64)
            mel_tensor, lengths = _wav_bytes_to_mel(audio_bytes)
            input_ids = _prepare_input_ids()

            t0 = time.monotonic()
            text_out, units = _generate_s2s(input_ids, mel_tensor, lengths)
            elapsed_gen = time.monotonic() - t0
        elif text_input:
            msg = f"{persona} {text_input}".strip() if persona else text_input
            input_ids = _prepare_input_ids(text_content=msg)

            dummy_speech = torch.zeros(1, 1, 128, dtype=torch.bfloat16, device="cuda")
            dummy_lengths = torch.LongTensor([1]).to("cuda")

            t0 = time.monotonic()
            text_out, units = _generate_s2s(input_ids, dummy_speech, dummy_lengths)
            elapsed_gen = time.monotonic() - t0
        else:
            return jsonify({"error": "audio_b64 or text required"}), 400

        response = {"text": text_out}

        if units is not None:
            t0_decode = time.monotonic()
            wav = _decode_units_to_wav(units)
            elapsed_decode = time.monotonic() - t0_decode

            if wav is not None:
                response["audio_b64"] = _wav_to_b64(wav)
                log.info("Chat: '%s' (gen=%.2fs, decode=%.2fs)", text_out[:80], elapsed_gen, elapsed_decode)
            else:
                log.info("Chat: '%s' (gen=%.2fs, no audio)", text_out[:80], elapsed_gen)
        else:
            log.info("Chat: '%s' (gen=%.2fs, no units)", text_out[:80], elapsed_gen)

        return jsonify(response)
    except Exception as e:
        log.error("Chat failed: %s", e, exc_info=True)
        return jsonify({"error": str(e)}), 500


@app.route("/health", methods=["GET"])
def health():
    return jsonify({
        "status": "ok",
        "service": "voice-server",
        "model": "LLaMA-Omni2-3B",
        "voice": "Alfred",
        "sample_rate": SAMPLE_RATE,
    })


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
if __name__ == "__main__":
    import argparse

    parser = argparse.ArgumentParser(description="Yggdrasil LLaMA-Omni2 Voice Server")
    parser.add_argument("--port", type=int, default=9098)
    parser.add_argument("--host", default="0.0.0.0")
    args = parser.parse_args()

    load_models()
    log.info("Starting LLaMA-Omni2-3B voice server on %s:%d", args.host, args.port)
    app.run(host=args.host, port=args.port, threaded=False)
