"""
OpenVINO GenAI voice LLM server — OpenAI-compatible chat completions.

Runs a small model on Intel Arc iGPU (or CPU fallback) via OpenVINO GenAI.
Exposes /v1/chat/completions for Odin voice pipeline integration.

Usage:
    python server.py --model-dir /opt/models/openvino/qwen2.5-3b-int4 --device GPU --port 11435
"""
import argparse
import json
import logging
import time
import uuid
from typing import Optional

import uvicorn
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field

logging.basicConfig(
    level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s"
)
log = logging.getLogger("voice-llm")

app = FastAPI(title="OpenVINO Voice LLM")

_pipe = None
_model_name = "gemma-3-4b-it-int4"
_device = "GPU.0"
_is_vlm = False


# ── Request/Response models (OpenAI-compatible subset) ──────────

class ChatMessage(BaseModel):
    role: str
    content: str

class ToolFunction(BaseModel):
    name: str
    description: str = ""
    parameters: dict = {}

class ToolDef(BaseModel):
    type: str = "function"
    function: ToolFunction

class ChatRequest(BaseModel):
    model: Optional[str] = None
    messages: list[ChatMessage]
    max_tokens: Optional[int] = 256
    temperature: Optional[float] = 0.7
    stream: Optional[bool] = False
    tools: Optional[list[ToolDef]] = None
    tool_choice: Optional[str] = None
    # Odin extras
    session_id: Optional[str] = None

class ChatChoice(BaseModel):
    index: int = 0
    message: ChatMessage
    finish_reason: str = "stop"

class Usage(BaseModel):
    prompt_tokens: int = 0
    completion_tokens: int = 0
    total_tokens: int = 0

class ChatResponse(BaseModel):
    id: str
    object: str = "chat.completion"
    created: int
    model: str
    choices: list[ChatChoice]
    usage: Usage = Usage()


# ── Endpoints ───────────────────────────────────────────────────

@app.get("/health")
async def health():
    return {
        "status": "ok" if _pipe is not None else "loading",
        "model": _model_name,
        "device": _device,
    }

@app.get("/api/version")
async def version():
    """Ollama-compatible version endpoint."""
    return {"version": "openvino-genai"}

@app.get("/v1/models")
@app.get("/api/tags")
async def list_models():
    """OpenAI/Ollama-compatible model listing."""
    return {
        "object": "list",
        "data": [{"id": _model_name, "object": "model"}],
        "models": [{"name": _model_name}],
    }

@app.post("/v1/chat/completions")
@app.post("/api/chat")
async def chat_completions(req: ChatRequest):
    if _pipe is None:
        raise HTTPException(503, "Model not loaded yet")

    import openvino_genai as ov_genai

    # Build prompt from messages (ChatML format for Qwen)
    prompt_parts = []
    for msg in req.messages:
        if msg.role == "system":
            prompt_parts.append(f"<|im_start|>system\n{msg.content}<|im_end|>")
        elif msg.role == "user":
            prompt_parts.append(f"<|im_start|>user\n{msg.content}<|im_end|>")
        elif msg.role == "assistant":
            prompt_parts.append(f"<|im_start|>assistant\n{msg.content}<|im_end|>")
        elif msg.role == "tool":
            prompt_parts.append(f"<|im_start|>tool\n{msg.content}<|im_end|>")

    # Inject tool definitions into system prompt if provided
    if req.tools:
        tool_block = json.dumps(
            [{"type": t.type, "function": {"name": t.function.name, "description": t.function.description, "parameters": t.function.parameters}} for t in req.tools],
            indent=2,
        )
        # Prepend to first system message or add one
        tool_system = f"\n\nAvailable tools:\n{tool_block}\n\nTo call a tool, respond with: <tool_call>{{\"name\":\"tool_name\",\"args\":{{...}}}}</tool_call>"
        if prompt_parts and prompt_parts[0].startswith("<|im_start|>system"):
            # Inject before closing tag
            prompt_parts[0] = prompt_parts[0].replace("<|im_end|>", f"{tool_system}<|im_end|>", 1)
        else:
            prompt_parts.insert(0, f"<|im_start|>system\n{tool_system}<|im_end|>")

    prompt_parts.append("<|im_start|>assistant\n")
    prompt = "\n".join(prompt_parts)

    config = ov_genai.GenerationConfig()
    config.max_new_tokens = req.max_tokens or 45
    config.temperature = req.temperature or 0.7
    config.do_sample = (req.temperature or 0.7) > 0

    t0 = time.time()
    try:
        if _is_vlm:
            result_obj = _pipe.generate(prompt, generation_config=config)
            result = result_obj.texts[0] if hasattr(result_obj, "texts") else str(result_obj)
        else:
            result = _pipe.generate(prompt, config)
    except Exception as e:
        log.error("Generation failed: %s", e)
        raise HTTPException(500, f"Generation failed: {e}")
    gen_time = time.time() - t0

    # Clean up trailing special tokens (Gemma, Qwen, etc.)
    text = result.strip()
    for stop in ["<|im_end|>", "<end_of_turn>", "</s>", "<|end|>"]:
        if stop in text:
            text = text.split(stop)[0].strip()

    completion_tokens = len(text.split())  # Approximate
    log.info(
        "Generated %d tokens in %.2fs (%.1f tok/s)",
        completion_tokens, gen_time, completion_tokens / max(gen_time, 0.001),
    )

    return ChatResponse(
        id=f"chatcmpl-{uuid.uuid4().hex[:12]}",
        created=int(time.time()),
        model=_model_name,
        choices=[ChatChoice(message=ChatMessage(role="assistant", content=text))],
        usage=Usage(
            prompt_tokens=len(prompt.split()),
            completion_tokens=completion_tokens,
            total_tokens=len(prompt.split()) + completion_tokens,
        ),
    )


# ── Startup ─────────────────────────────────────────────────────

def load_model(model_dir: str, device: str):
    global _pipe, _model_name, _device, _is_vlm
    import openvino_genai as ov_genai
    import os

    _device = device
    # Auto-detect VLM (has vision embeddings model)
    _is_vlm = os.path.exists(os.path.join(model_dir, "openvino_vision_embeddings_model.xml"))

    pipeline_cls = ov_genai.VLMPipeline if _is_vlm else ov_genai.LLMPipeline
    log.info("Loading model from %s on %s (%s)...", model_dir, device,
             "VLM" if _is_vlm else "LLM")
    t0 = time.time()
    _pipe = pipeline_cls(model_dir, device)
    load_time = time.time() - t0
    log.info("Model loaded in %.1fs on %s", load_time, device)

    # Warm up
    config = ov_genai.GenerationConfig()
    config.max_new_tokens = 5
    if _is_vlm:
        _pipe.generate("hi", generation_config=config)
    else:
        _pipe.generate("hi", config)
    log.info("Warmup complete")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="OpenVINO Voice LLM Server")
    parser.add_argument("--model-dir", required=True, help="Path to OpenVINO model")
    parser.add_argument("--device", default="GPU", help="GPU, CPU, or NPU")
    parser.add_argument("--port", type=int, default=11435)
    parser.add_argument("--host", default="0.0.0.0")
    args = parser.parse_args()

    _model_name = args.model_dir.rstrip("/").split("/")[-1]
    load_model(args.model_dir, args.device)
    uvicorn.run(app, host=args.host, port=args.port, log_level="info")
