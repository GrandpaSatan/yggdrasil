"""
Qwen3-Omni serving via IPEX-LLM on Intel iGPU.

Loads the model with 4-bit quantization using IPEX-LLM's transformers API,
exposes an OpenAI-compatible /v1/chat/completions endpoint that accepts
multimodal audio input.

Usage:
    python server.py --model /opt/models/Qwen3-Omni-30B-A3B-Instruct
"""
import argparse
import base64
import io
import logging
import time
import uuid
from pathlib import Path

import numpy as np
import soundfile as sf
import torch
import uvicorn
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
log = logging.getLogger("omni-server")

# ── Pydantic models (OpenAI-compatible) ────────────────────────

class AudioInputData(BaseModel):
    data: str  # base64-encoded audio
    format: str = "wav"

class ContentPart(BaseModel):
    type: str
    text: str | None = None
    input_audio: AudioInputData | None = None

class ChatMessage(BaseModel):
    role: str
    content: str | list[ContentPart]

class ChatCompletionRequest(BaseModel):
    model: str = ""
    messages: list[ChatMessage]
    temperature: float = 0.7
    max_tokens: int = 512
    stream: bool = False

class ChoiceMessage(BaseModel):
    role: str = "assistant"
    content: str

class Choice(BaseModel):
    index: int = 0
    message: ChoiceMessage
    finish_reason: str = "stop"

class Usage(BaseModel):
    prompt_tokens: int = 0
    completion_tokens: int = 0
    total_tokens: int = 0

class ChatCompletionResponse(BaseModel):
    id: str
    object: str = "chat.completion"
    created: int
    model: str
    choices: list[Choice]
    usage: Usage = Field(default_factory=Usage)

class ModelInfo(BaseModel):
    id: str
    object: str = "model"
    created: int = 0
    owned_by: str = "local"

class ModelList(BaseModel):
    object: str = "list"
    data: list[ModelInfo]

# ── Global state ───────────────────────────────────────────────

app = FastAPI(title="Qwen3-Omni Server (IPEX-LLM)")
model = None
processor = None
served_model_name = "Qwen3-Omni"

# ── Audio helpers ──────────────────────────────────────────────

def decode_audio_b64(b64_data: str, fmt: str = "wav") -> np.ndarray:
    """Decode base64 audio to numpy float32 array at native sample rate."""
    raw = base64.b64decode(b64_data)
    audio, sr = sf.read(io.BytesIO(raw))
    # Qwen3-Omni expects 16kHz mono float32
    if audio.ndim > 1:
        audio = audio.mean(axis=1)
    if sr != 16000:
        # Simple resample via linear interpolation
        duration = len(audio) / sr
        target_len = int(duration * 16000)
        indices = np.linspace(0, len(audio) - 1, target_len)
        audio = np.interp(indices, np.arange(len(audio)), audio)
    return audio.astype(np.float32)

def build_conversation(messages: list[ChatMessage]) -> tuple[list[dict], list[np.ndarray]]:
    """Convert OpenAI-format messages to Qwen processor format, extracting audio."""
    conversation = []
    audios = []

    for msg in messages:
        if isinstance(msg.content, str):
            conversation.append({"role": msg.role, "content": msg.content})
        else:
            # Multimodal content parts
            parts = []
            for part in msg.content:
                if part.type == "text" and part.text:
                    parts.append({"type": "text", "text": part.text})
                elif part.type == "input_audio" and part.input_audio:
                    audio_np = decode_audio_b64(
                        part.input_audio.data, part.input_audio.format
                    )
                    audios.append(audio_np)
                    parts.append({"type": "audio", "audio": f"audio_{len(audios) - 1}"})
            content = parts if parts else ""
            conversation.append({"role": msg.role, "content": content})

    return conversation, audios

# ── Endpoints ──────────────────────────────────────────────────

@app.get("/health")
async def health():
    return {"status": "ok", "model_loaded": model is not None}

@app.get("/v1/models")
async def list_models():
    return ModelList(data=[ModelInfo(id=served_model_name)])

@app.post("/v1/chat/completions")
async def chat_completions(request: ChatCompletionRequest):
    if model is None or processor is None:
        raise HTTPException(status_code=503, detail="model not loaded")

    if request.stream:
        raise HTTPException(status_code=400, detail="streaming not supported")

    t0 = time.time()

    try:
        conversation, audios = build_conversation(request.messages)

        # Use processor to build model inputs
        text = processor.apply_chat_template(
            conversation, add_generation_prompt=True, tokenize=False
        )

        if audios:
            inputs = processor(
                text=text,
                audios=audios,
                return_tensors="pt",
                padding=True,
            )
        else:
            inputs = processor(
                text=text,
                return_tensors="pt",
                padding=True,
            )

        # Move inputs to XPU
        inputs = {k: v.to("xpu") if hasattr(v, "to") else v for k, v in inputs.items()}

        with torch.no_grad():
            output_ids = model.generate(
                **inputs,
                max_new_tokens=request.max_tokens,
                temperature=request.temperature,
                do_sample=request.temperature > 0,
            )

        # Decode only the new tokens (skip prompt)
        input_len = inputs["input_ids"].shape[1]
        new_tokens = output_ids[0][input_len:]
        response_text = processor.decode(new_tokens, skip_special_tokens=True)

        elapsed = time.time() - t0
        log.info(
            "generated %d tokens in %.1fs (%.1f tok/s)",
            len(new_tokens), elapsed, len(new_tokens) / elapsed if elapsed > 0 else 0,
        )

        return ChatCompletionResponse(
            id=f"chatcmpl-{uuid.uuid4().hex[:12]}",
            created=int(time.time()),
            model=served_model_name,
            choices=[Choice(message=ChoiceMessage(content=response_text))],
            usage=Usage(
                prompt_tokens=input_len,
                completion_tokens=len(new_tokens),
                total_tokens=input_len + len(new_tokens),
            ),
        )
    except Exception as e:
        log.exception("generation failed")
        raise HTTPException(status_code=500, detail=str(e))

# ── Model loading ──────────────────────────────────────────────

def load_model(model_path: str, low_bit_path: str):
    """Load model with IPEX-LLM 4-bit quantization."""
    global model, processor
    from ipex_llm.transformers import AutoModelForCausalLM
    from transformers import AutoProcessor

    low_bit = Path(low_bit_path)

    if low_bit.exists() and (low_bit / "config.json").exists():
        log.info("loading pre-quantized 4-bit model from %s", low_bit_path)
        model = AutoModelForCausalLM.load_low_bit(
            low_bit_path,
            trust_remote_code=True,
        ).to("xpu")
    else:
        log.info("loading full model from %s with 4-bit quantization", model_path)
        model = AutoModelForCausalLM.from_pretrained(
            model_path,
            load_in_4bit=True,
            optimize_model=True,
            trust_remote_code=True,
        ).to("xpu")

        log.info("saving 4-bit checkpoint to %s", low_bit_path)
        low_bit.mkdir(parents=True, exist_ok=True)
        model.save_low_bit(low_bit_path)
        log.info("4-bit checkpoint saved")

    processor = AutoProcessor.from_pretrained(
        model_path, trust_remote_code=True
    )

    # Warmup
    log.info("running warmup inference")
    with torch.no_grad():
        dummy = processor("Hello", return_tensors="pt")
        dummy = {k: v.to("xpu") if hasattr(v, "to") else v for k, v in dummy.items()}
        model.generate(**dummy, max_new_tokens=1)
    log.info("model ready")

# ── Main ───────────────────────────────────────────────────────

def main():
    global served_model_name

    parser = argparse.ArgumentParser(description="Qwen3-Omni IPEX-LLM Server")
    parser.add_argument("--model", default="/opt/models/Qwen3-Omni-30B-A3B-Instruct")
    parser.add_argument("--low-bit-path", default="/opt/models/qwen3-omni-4bit")
    parser.add_argument("--served-model-name", default="Qwen3-Omni")
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=8000)
    args = parser.parse_args()

    served_model_name = args.served_model_name

    log.info("starting omni server: model=%s port=%d", args.model, args.port)
    load_model(args.model, args.low_bit_path)

    uvicorn.run(app, host=args.host, port=args.port, log_level="info")

if __name__ == "__main__":
    main()
