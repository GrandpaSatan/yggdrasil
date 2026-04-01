# Liquid AI LFM Documentation — Complete Technical Reference

> Compiled from docs.liquid.ai on 2026-03-31. All content extracted verbatim from official documentation.

---

## Table of Contents

1. [Overview & Architecture](#1-overview--architecture)
2. [Complete Model Library](#2-complete-model-library)
3. [Text Models](#3-text-models)
4. [Vision Language Models](#4-vision-language-models)
5. [Audio Models](#5-audio-models)
6. [Liquid Nanos (Task-Specific)](#6-liquid-nanos-task-specific)
7. [Chat Template & Special Tokens](#7-chat-template--special-tokens)
8. [Prompting Guide & Generation Parameters](#8-prompting-guide--generation-parameters)
9. [Tool Use / Function Calling](#9-tool-use--function-calling)
10. [Fine-Tuning with TRL](#10-fine-tuning-with-trl)
11. [Fine-Tuning with Unsloth](#11-fine-tuning-with-unsloth)
12. [Dataset Formats](#12-dataset-formats)
13. [Inference: Transformers (GPU)](#13-inference-transformers-gpu)
14. [Inference: vLLM (GPU)](#14-inference-vllm-gpu)
15. [Inference: SGLang (GPU)](#15-inference-sglang-gpu)
16. [Inference: llama.cpp (CPU/GPU)](#16-inference-llamacpp-cpugpu)
17. [Inference: Ollama](#17-inference-ollama)
18. [Inference: MLX (Apple Silicon)](#18-inference-mlx-apple-silicon)
19. [Inference: ONNX (Cross-Platform)](#19-inference-onnx-cross-platform)
20. [Troubleshooting & Known Issues](#20-troubleshooting--known-issues)
21. [Performance Benchmarks](#21-performance-benchmarks)

---

## 1. Overview & Architecture

LFM2 is a new generation of hybrid models from Liquid AI designed for edge AI and on-device deployment. The architecture is described as a **new hybrid architecture** enabling:

- **3x faster training** compared to standard approaches
- State-of-the-art quality on benchmarks relative to similarly-sized models
- Memory optimization for resource-constrained deployment scenarios
- Compatible with major inference frameworks and platforms

### Core Specifications (All Models)

| Property | Value |
|----------|-------|
| Context Length | **32,768 tokens (32k)** |
| Parameter Range | 350M to 8B (24B total for MoE) |
| Architecture Types | Dense and Mixture-of-Experts (MoE) |
| Modalities | Text, Vision (VL), Audio |

### Model Families

- **LFM2**: Base generation — dense models (350M, 700M, 1.2B, 2.6B) and MoE models (8B-A1B, 24B-A2B)
- **LFM2.5**: Extended pre-training + reinforcement learning on top of LFM2 — improved quality
- **LFM2-VL**: Vision Language Models combining LFM text backbones with dynamic SigLIP2 image encoders
- **LFM2.5-Audio**: Fully interleaved audio/text-in, audio/text-out models with complete reasoning backbone
- **Liquid Nanos**: Task-specific fine-tuned models for extraction, RAG, translation, math, etc.

### Deployment Formats

| Format | Purpose | Quantizations |
|--------|---------|---------------|
| **HuggingFace (HF)** | Standard PyTorch weights | bf16, fp32 |
| **GGUF** | CPU/GPU inference (llama.cpp, LM Studio, Ollama) | Q4_0, Q4_K_M, Q5_K_M, Q6_K, Q8_0, BF16, F16 |
| **MLX** | Apple Silicon (M1-M4) | 3bit, 4bit, 5bit, 6bit, 8bit, BF16 |
| **ONNX** | Production/edge/browser/NPU | FP32, FP16, Q4, Q8 (MoE: Q4F16) |

### Supported Inference Frameworks

Transformers, llama.cpp, vLLM, SGLang, MLX, Ollama, LEAP (mobile SDK)

### Supported Fine-Tuning Methods

SFT, DPO, GRPO via TRL and Unsloth frameworks

---

## 2. Complete Model Library

### Text Models

| Model | Parameters | Architecture | Formats | Trainable | Status |
|-------|-----------|--------------|---------|-----------|--------|
| LFM2.5-1.2B-Instruct | 1.2B | LFM2.5 (Dense) | HF, GGUF, MLX-8bit, ONNX | TRL | Current (recommended) |
| LFM2.5-1.2B-Thinking | 1.2B | LFM2.5 (Dense) | HF, GGUF, MLX-8bit, ONNX | TRL | Current |
| LFM2.5-1.2B-Base | 1.2B | LFM2.5 (Dense) | HF, GGUF, ONNX | TRL | Current |
| LFM2.5-1.2B-JP | 1.2B | LFM2.5 (Dense) | HF, GGUF, MLX-8bit, ONNX | TRL | Current |
| LFM2-24B-A2B | 24B total / 2B active | LFM2 (MoE) | HF | TRL | Current |
| LFM2-8B-A1B | 8B total / 1.5B active | LFM2 (MoE) | HF, GGUF, MLX-8bit, ONNX | TRL | Current |
| LFM2-2.6B | 2.6B | LFM2 (Dense) | HF, GGUF, MLX-8bit, ONNX | TRL | Current |
| LFM2-2.6B-Exp | 2.6B | LFM2 (Dense) | HF, GGUF | TRL | Current (RL post-trained) |
| LFM2-1.2B | 1.2B | LFM2 (Dense) | HF, GGUF, MLX-8bit, ONNX | TRL | **Deprecated** |
| LFM2-700M | 700M | LFM2 (Dense) | HF, GGUF, MLX-8bit, ONNX | TRL | Current |
| LFM2-350M | 350M | LFM2 (Dense) | HF, GGUF, MLX-8bit, ONNX | TRL | Current |

### Vision Language Models

| Model | Parameters | Formats | Trainable | Status |
|-------|-----------|---------|-----------|--------|
| LFM2.5-VL-1.6B | 1.6B | HF, GGUF, MLX-8bit, ONNX | TRL | Current (recommended) |
| LFM2-VL-3B | 3B | HF, GGUF, MLX-8bit, ONNX | TRL | Current |
| LFM2-VL-1.6B | 1.6B | HF, GGUF, MLX-8bit, ONNX | TRL | **Deprecated** |
| LFM2-VL-450M | 450M | HF, GGUF, MLX-8bit, ONNX | TRL | Current |

### Audio Models

| Model | Parameters | Formats | Trainable | Status |
|-------|-----------|---------|-----------|--------|
| LFM2.5-Audio-1.5B | 1.5B | HF, GGUF, ONNX | TRL | Current (recommended) |
| LFM2-Audio-1.5B | 1.5B | HF, GGUF | Not trainable | **Deprecated** |

### Liquid Nanos (Task-Specific)

| Model | Parameters | Task | Formats | Trainable |
|-------|-----------|------|---------|-----------|
| LFM2-1.2B-Extract | 1.2B | Structured JSON extraction | HF, GGUF, ONNX | TRL |
| LFM2-350M-Extract | 350M | Fastest edge extraction | HF, GGUF, ONNX | TRL |
| LFM2-350M-PII-Extract-JP | 350M | Japanese PII detection | HF, GGUF | TRL |
| LFM2-2.6B-Transcript | 2.6B | Meeting summarization | HF, GGUF, ONNX | TRL |
| LFM2-1.2B-RAG | 1.2B | Context-grounded QA | HF, GGUF, ONNX | TRL |
| LFM2-ColBERT-350M | 350M | Multi-language retrieval/reranking | HF | PyLate |
| LFM2-350M-ENJP-MT | 350M | Japanese/English translation | HF, GGUF, MLX-8bit, ONNX | TRL |
| LFM2-350M-Math | 350M | Math reasoning | HF, GGUF, ONNX | TRL |
| LFM2-1.2B-Tool | 1.2B | Tool calling | HF, GGUF, ONNX | TRL | **Deprecated** (use LFM2.5-1.2B-Instruct) |

---

## 3. Text Models

LFM text models range from 350M to 8B parameters, delivering ultra-low-latency generation with both dense and MoE variants.

**Capabilities:** Chat, text rewriting, tool calling, structured JSON output, classification tasks.

**LFM2.5 improvements over LFM2:** Extended pre-training and reinforcement learning.

### LFM2-700M Specifics
- Architecture: LFM2 (Dense)
- Parameters: 700M
- Context: 32K tokens
- Recommended sampling: temperature=0.3, min_p=0.15, repetition_penalty=1.05

### LFM2.5-1.2B-JP Specifics
- Architecture: LFM2.5 (Dense)
- Parameters: 1.2B
- Context: 32K tokens
- Fine-tuned for Japanese language tasks: text generation, translation, conversation
- Recommended sampling: temperature=0.3, min_p=0.15, repetition_penalty=1.05

### MoE Models
- **LFM2-24B-A2B**: 24B total parameters, 2B active per token
- **LFM2-8B-A1B**: 8B total parameters, 1.5B active per token

---

## 4. Vision Language Models

Vision models combine **lightweight LFM text backbones with dynamic SigLIP2 image encoders**.

### Use Cases
- Image captioning and visual summarization
- Optical character recognition (OCR) and document processing
- Scene comprehension and visual question-answering
- Real-time activity recognition on-device

### Image Tokenization Parameters

| Parameter | Purpose |
|-----------|---------|
| `min_image_tokens` | Minimum encoding tokens per image |
| `max_image_tokens` | Maximum encoding tokens per image |
| `do_image_splitting` | Enables 512x512 patch splitting for large images |

### Quality Configurations

| Quality | max_image_tokens | min_image_tokens |
|---------|-----------------|-----------------|
| High | 256 | 128 |
| Balanced | 128 | 64 |
| Fast | 64 | 32 |

### Recommended Vision Sampling
temperature=0.1, min_p=0.15, repetition_penalty=1.05, min_image_tokens=64, max_image_tokens=256, do_image_splitting=True

---

## 5. Audio Models

Liquid's LFM audio models are among the **smallest fully interleaved audio/text-in, audio/text-out models with a complete reasoning backbone**. The architecture integrates audio and text processing without requiring separate TTS/ASR encoders combined with standalone language models.

### Capabilities
- Text-to-Speech (TTS) synthesis
- Speech Recognition (multilingual ASR)
- Audio-to-Audio (interleaved voice chat with reasoning)
- Audio Function Calling (voice-driven tool use)

### LFM2.5-Audio Improvements
- Custom LFM-based audio detokenizer
- llama.cpp-compatible GGUFs for CPU inference
- Improved ASR and TTS performance
- WebGPU acceleration capability

---

## 6. Liquid Nanos (Task-Specific)

Low-latency, task-specific models fine-tuned on Liquid's multimodal LFM base models, designed for on-device or high-volume deployment.

| Model | Size | Task Description |
|-------|------|------------------|
| LFM2-1.2B-Extract | 1.2B | Extract structured JSON from unstructured documents |
| LFM2-350M-Extract | 350M | Fastest extraction model for edge deployment |
| LFM2-350M-PII-Extract-JP | 350M | Japanese PII detection into structured JSON |
| LFM2-2.6B-Transcript | 2.6B | Private, on-device meeting summarization from transcripts |
| LFM2-1.2B-RAG | 1.2B | Answer questions grounded in provided context documents |
| LFM2-ColBERT-350M | 350M | Multi-language document embeddings for retrieval and reranking |
| LFM2-350M-ENJP-MT | 350M | Near real-time bidirectional Japanese/English translation |
| LFM2-350M-Math | 350M | Tiny reasoning model for math problem solving |

---

## 7. Chat Template & Special Tokens

### Special Tokens
| Token | Purpose |
|-------|---------|
| `<\|startoftext\|>` | Initiates conversations |
| `<\|im_start\|>` | Marks message beginning, followed by role and line break |
| `<\|im_end\|>` | Terminates messages |

### Conversation Roles
- `system` — Optional, defines assistant behavior
- `user` — Questions/instructions
- `assistant` — Model responses
- `tool` — Function execution results

### Message Format
```python
messages = [
    {"role": "system", "content": "You are a helpful assistant."},
    {"role": "user", "content": "What is machine learning?"}
]
```

### Tokenizer Usage
```python
from transformers import AutoTokenizer
tokenizer = AutoTokenizer.from_pretrained("LiquidAI/LFM2.5-1.2B-Instruct")
formatted = tokenizer.apply_chat_template(messages, tokenize=False, add_generation_prompt=True)
```

### Resulting Wire Format
```
<|startoftext|><|im_start|>system
Your content<|im_end|>
<|im_start|>user
Your content<|im_end|>
<|im_start|>assistant
```

### Vision Model Chat Template
Content becomes a list for multimodal:
```python
{"type": "image", "image": image_object}
{"type": "text", "text": "prompt"}
```
Images render as `<image>` sentinel tokens, automatically replaced by the processor.

```python
from transformers import AutoProcessor
processor = AutoProcessor.from_pretrained("LiquidAI/LFM2.5-VL-1.6B")
```

The template definition resides in each model's `chat_template.jinja` file on Hugging Face.

---

## 8. Prompting Guide & Generation Parameters

### Prompt Roles
- **System**: Sets assistant behavior, context, instructions (personality, task context, format, constraints)
- **User**: Contains the actual question or request
- **Assistant**: Enables model continuation, multi-turn dialogue, few-shot examples, or output prefilling

### Prompting Strategies

**Few-shot Learning**: Provide example input-output pairs demonstrating desired patterns.

**Prefilling**: Begin assistant responses with partial content (e.g., JSON opening brace `{`) to guide structured output.

**Multi-turn Conversations**: Leverage conversation history across multiple exchanges.

### Generation Parameters

| Parameter | Range | Purpose |
|-----------|-------|---------|
| temperature | 0.0-2.0 | Randomness control (lower=deterministic, higher=creative) |
| top_p | 0.0-1.0 | Nucleus sampling (lower=focused, higher=diverse) |
| top_k | Integer | Limits highest-probability tokens |
| min_p | 0.0-1.0 | Filters tokens below threshold while maintaining quality |
| repetition_penalty | 1.0+ | Reduces repetitive output (1.0=no penalty) |
| max_tokens / max_new_tokens | Integer | Generation length limit |

### Model-Specific Recommended Settings

| Model | temperature | top_k | top_p | min_p | repetition_penalty |
|-------|-------------|-------|-------|-------|-------------------|
| **LFM2.5-1.2B-Instruct** | 0.1 | 50 | — | — | 1.05 |
| **LFM2.5-1.2B-Thinking** | 0.1 | 50 | 0.1 | — | 1.05 |
| **LFM2 Text Models** | 0.3 | — | — | 0.15 | 1.05 |
| **LFM2-VL / LFM2.5-VL** | 0.1 | — | — | 0.15 | 1.05 |

---

## 9. Tool Use / Function Calling

### Supported Models
- LFM2.5 and LFM2 (text models)
- LFM2-VL and LFM2.5-VL (vision models, text-only function calling)

### Workflow
1. Define tools as JSON
2. Generate model responses with function calls
3. Execute functions externally
4. Regenerate to interpret results

### Special Tokens

**LFM2.5**: Tool calls wrap in `<|tool_call_start|>` and `<|tool_call_end|>` tokens.

**LFM2**: Additionally wraps tool definitions in `<|tool_list_start|>`/`<|tool_list_end|>` and responses in `<|tool_response_start|>`/`<|tool_response_end|>`.

### Tool Definition Schema
```json
{
  "name": "function_identifier",
  "description": "what the tool does",
  "parameters": {
    "type": "object",
    "properties": {
      "param_name": {
        "type": "string",
        "description": "Parameter description"
      }
    },
    "required": ["param_name"]
  }
}
```

### Function Call Format

By default, models generate **Pythonic function calls**:
```
get_candidate_status(candidate_id="12345")
```

For JSON format output, add `"Output function calls as JSON"` to your system prompt.

### Implementation Options

**Option 1**: Include JSON tool definitions directly in system prompt.

**Option 2**: Use `.apply_chat_template()` with Python function definitions and `tools=` parameter.

### Vision Model Tool Use
```python
processor.tokenizer.apply_chat_template(messages, tools=tools)
```

### Limitations
Tool definitions consume context tokens since they're inserted as text. For large tool lists (100+ tools), this can use significant portions of the 32k context window. Include only relevant tools and use clear, concise descriptions.

---

## 10. Fine-Tuning with TRL

### Installation
```bash
pip install trl>=0.9.0 transformers>=4.55.0 torch>=2.6 peft accelerate
```

### Supervised Fine-Tuning (SFT)

**LoRA Configuration (Recommended):**
```python
from peft import LoraConfig

lora_config = LoraConfig(
    r=16,
    lora_alpha=32,
    lora_dropout=0.05,
    target_modules=["q_proj", "k_proj", "v_proj", "o_proj"],
    task_type="CAUSAL_LM"
)
```

**SFT Training Hyperparameters:**
| Parameter | Value |
|-----------|-------|
| Learning rate | 2e-4 |
| Batch size (per device) | 4 |
| Gradient accumulation | 4 steps |
| Epochs | 3 |
| dtype | bfloat16 |

**Full Fine-Tuning Alternative:**
| Parameter | Value |
|-----------|-------|
| Learning rate | 2e-5 (lower than LoRA) |
| Batch size | 4 |
| Gradient accumulation | 4 steps |
| Saves checkpoint per epoch | Yes |

### Vision Language Model Fine-Tuning (VLM-SFT)

**VLM LoRA Config:**
```python
lora_config = LoraConfig(
    r=8,
    lora_alpha=16,
    lora_dropout=0.05,
    target_modules=["q_proj", "v_proj", "fc1", "fc2", "linear", "gate_proj", "up_proj", "down_proj"]
)
```

**VLM Training Hyperparameters:**
| Parameter | Value |
|-----------|-------|
| Learning rate | 5e-4 |
| Batch size (per device) | 1 |
| Gradient accumulation | 16 steps |
| Max length | 512 tokens |
| Gradient checkpointing | enabled |
| Processor max image tokens | 256 |

**Full VLM Fine-Tuning:** Learning rate 2e-5, same batch/accumulation.

### Direct Preference Optimization (DPO)

**DPO LoRA Config:**
```python
lora_config = LoraConfig(
    r=16,
    lora_alpha=32,
    lora_dropout=0.05,
    target_modules=["q_proj", "k_proj", "v_proj", "o_proj"]
)
```

**DPO Training Hyperparameters:**
| Parameter | Value |
|-----------|-------|
| Learning rate | 5e-7 |
| Batch size (per device) | 2 |
| Gradient accumulation | 8 steps |
| Beta | 0.1 (controls reference model deviation) |
| Epochs | 3 |
| dtype | bfloat16 |

### Training Tips
- SFT typically uses higher learning rates (1e-5 to 5e-5) than DPO (1e-7 to 1e-6)
- Start with LoRA rank r=16; set lora_alpha to 2 * r
- Full DPO learning rate remains at 5e-7

---

## 11. Fine-Tuning with Unsloth

Unsloth makes fine-tuning LLMs **2-5x faster with 70% less memory** through optimized kernels and efficient memory management. LFM2.5 models are fully compatible.

### Key Configuration

**Loading:**
- Model: `LiquidAI/LFM2.5-1.2B-Instruct`
- Max sequence length: 2048 tokens
- Quantization: 4-bit QLoRA (`load_in_4bit=True`)

**LoRA Adapter Settings:**
```python
r = 16
alpha = 32
target_modules = ["q_proj", "k_proj", "v_proj", "o_proj", "gate_proj", "up_proj", "down_proj"]
```

**Training:**
- Epochs: 1
- dtype: bf16 (bfloat16)
- Gradient checkpointing: `"unsloth"` variant (2x faster than standard)

### Performance Optimizations
- `load_in_4bit=True` reduces memory approximately 4x with minimal quality degradation
- `use_gradient_checkpointing='unsloth'` achieves 2x faster performance vs standard
- `FastLanguageModel.for_inference()` switches to optimized inference mode

### Training Framework
Uses TRL's SFTTrainer with SFTConfig for supervised fine-tuning.

### Available Notebooks
- SFT (Supervised Fine-Tuning)
- GRPO (Group Relative Policy Optimization)
- Continued Pre-training

---

## 12. Dataset Formats

### File Format Support
JSONL (one JSON object per line), CSV (tabular), Parquet/Arrow (larger datasets).

### Instruction Datasets (SFT)
```json
{
  "messages": [
    {"role": "system", "content": "You are a helpful assistant."},
    {"role": "user", "content": "What is X?"},
    {"role": "assistant", "content": "X is..."}
  ]
}
```
Multi-turn conversations supported through alternating user/assistant messages.

### Preference Datasets (DPO)
```json
{
  "prompt": [{"role": "user", "content": "Query"}],
  "chosen": [{"role": "assistant", "content": "Preferred response"}],
  "rejected": [{"role": "assistant", "content": "Non-preferred response"}]
}
```

### Prompt-Only Datasets (GRPO)
```json
{
  "prompt": [
    {"role": "system", "content": "..."},
    {"role": "user", "content": "..."}
  ]
}
```
Completions are generated during training and evaluated by reward functions.

### Vision Datasets (VLM-SFT)
```json
{
  "messages": [
    {"role": "user", "content": [{"type": "image"}, {"type": "text", "text": "Describe this image"}]},
    {"role": "assistant", "content": "The image shows..."}
  ],
  "images": ["PIL Image object in RGB"]
}
```
Image loading: `Image.open(sample["image_path"]).convert("RGB")`

---

## 13. Inference: Transformers (GPU)

### Installation
```bash
pip install transformers>=5.0.0 torch accelerate
```

Fallback for Transformers v5 issues:
```bash
uv pip install git+https://github.com/huggingface/transformers.git@0c9a72e4576fe4c84077f066e585129c97bfd4e6 torch accelerate
```

### Model Loading Parameters
| Parameter | Options |
|-----------|---------|
| `model_id` | HF model ID or local path (e.g., `"LiquidAI/LFM2.5-1.2B-Instruct"`) |
| `device_map` | `"auto"` (distributed), `"cuda"` (single GPU), `"cpu"` |
| `dtype` | `"bfloat16"` (recommended), `"auto"`, `"float32"` |
| `attn_implementation` | `"flash_attention_2"` (optional, compatible GPUs only) |

### Generation Interfaces
- `generate()` — Fine-grained control and direct model access
- `pipeline()` — Simplified API with automatic chat template handling

### Generation Parameters
| Parameter | Default | Range |
|-----------|---------|-------|
| `do_sample` | — | bool (sampling vs greedy) |
| `temperature` | 1.0 | 0.0-2.0 |
| `top_p` | 1.0 | 0.1-1.0 |
| `top_k` | 50 | 1-100 |
| `min_p` | — | 0.01-0.2 |
| `max_new_tokens` | — | int |
| `repetition_penalty` | 1.0 | 1.0-1.5 |
| `stop_strings` | — | str or list |

### Streaming
```python
from transformers import TextStreamer
streamer = TextStreamer(tokenizer, skip_prompt=True, skip_special_tokens=True)
```

### Batch Processing
Use `padding=True` in `apply_chat_template()` for multiple prompts.

### Vision Models
```python
from transformers import AutoModelForImageTextToText, AutoProcessor
# Workaround required:
model.lm_head.weight = model.get_input_embeddings().weight
```

### Limitations
`device_map="auto"` does NOT apply tensor parallelism — uses one GPU at a time. For production high-throughput, use vLLM.

---

## 14. Inference: vLLM (GPU)

### Installation
```bash
uv pip install vllm==0.14
```
CUDA-compatible GPU required.

### Basic Usage
```python
from vllm import LLM, SamplingParams

llm = LLM(model="LiquidAI/LFM2.5-1.2B-Instruct")
sampling_params = SamplingParams(temperature=0.1, top_k=50, repetition_penalty=1.05, max_tokens=512)
output = llm.chat(messages, sampling_params)
```

### Server Mode
```bash
vllm serve LiquidAI/LFM2.5-1.2B-Instruct \
    --host 0.0.0.0 \
    --port 8000 \
    --dtype auto \
    --max-model-len L \
    --gpu-memory-utilization 0.9
```

### OpenAI-Compatible API
```python
from openai import OpenAI
client = OpenAI(base_url="http://localhost:8000/v1", api_key="dummy")
response = client.chat.completions.create(
    model="LiquidAI/LFM2.5-1.2B-Instruct",
    messages=[{"role": "user", "content": "prompt"}],
    temperature=0.1, max_tokens=512,
    extra_body={"top_k": 50, "repetition_penalty": 1.05}
)
```

### Vision Model Support
```bash
VLLM_PRECOMPILED_WHEEL_COMMIT=72506c98349d6bcd32b4e33eec7b5513453c1502 \
VLLM_USE_PRECOMPILED=1 uv pip install git+https://github.com/vllm-project/vllm.git
uv pip install "transformers>=5.0.0" pillow
```
```python
llm = LLM(model="LiquidAI/LFM2.5-VL-1.6B", max_model_len=1024)
```

### Key Features
- High-throughput via PagedAttention and continuous batching
- Automatic prompt batching
- OpenAI API compatibility
- Streaming support

---

## 15. Inference: SGLang (GPU)

### Installation
```bash
pip install uv
uv pip install "sglang>=0.5.8"
```

### Supported Models
| Model Type | Status |
|---|---|
| Dense text | Supported (LFM2-350M, LFM2.5-1.2B-Instruct, LFM2-2.6B) |
| MoE text | Coming in 0.5.9 (LFM2-8B-A1B) |
| Vision | Not supported (use Transformers) |

### Server Launch
```bash
python3 -m sglang.launch_server \
    --model-path LiquidAI/LFM2.5-1.2B-Instruct \
    --host 0.0.0.0 \
    --port 30000 \
    --tool-call-parser lfm2
```

Default dtype: bfloat16. For float16: `--dtype float16` and `export SGLANG_MAMBA_CONV_DTYPE=float16`.

### Docker
```bash
docker run --gpus all --shm-size 32g -p 30000:30000 \
    -v ~/.cache/huggingface:/root/.cache/huggingface \
    --env "HF_TOKEN=<secret>" --ipc=host \
    lmsysorg/sglang:dev \
    python3 -m sglang.launch_server \
        --model-path LiquidAI/LFM2.5-1.2B-Instruct \
        --host 0.0.0.0 --port 30000 --tool-call-parser lfm2
```

### Blackwell GPU Optimization (B300/GB300)
Key flags:
- `--enable-torch-compile`: Torch compilation for faster execution (recommended for concurrency < 256)
- `--chunked-prefill-size -1`: Disable chunked prefill for lower TTFT

**B300 Benchmark Results** (256 prompts, 1024 input tokens, 128 output tokens):
| Metric | Value |
|--------|-------|
| Mean TTFT | 8.79ms |
| Mean TPOT | 0.86ms |
| Output throughput | 1100.92 tok/s |

### System Requirements
- CUDA-compatible GPU required
- Docker: 32GB shared memory minimum

---

## 16. Inference: llama.cpp (CPU/GPU)

### Installation

**Homebrew:** `brew install llama.cpp`

**Build from source:**
```bash
git clone https://github.com/ggml-org/llama.cpp
cd llama.cpp
cmake -B build
cmake --build build --config Release -j 8
```

### Platform Binary Selection

| Platform | Binary |
|----------|--------|
| Windows CPU (Intel/AMD) | `llama-*-bin-win-avx2-x64.zip` |
| Windows NVIDIA GPU | `llama-*-bin-win-cu12-x64.zip` |
| macOS Intel | `llama-*-bin-macos-x64.zip` |
| macOS Apple Silicon | `llama-*-bin-macos-arm64.zip` |
| Linux | `llama-*-bin-linux-x64.zip` |

### GGUF Quantization Levels

| Level | Description |
|-------|-------------|
| Q4_0 | 4-bit, smallest size |
| Q4_K_M | 4-bit, **recommended balance** |
| Q5_K_M | 5-bit, improved quality |
| Q6_K | 6-bit, excellent quality |
| Q8_0 | 8-bit, near-original quality |
| F16 | 16-bit float, full precision |

### Model Download
```bash
uv pip install huggingface-hub
hf download LiquidAI/LFM2.5-1.2B-Instruct-GGUF lfm2.5-1.2b-instruct-q4_k_m.gguf --local-dir .
```

### Server (llama-server)
```bash
llama-server -hf LiquidAI/LFM2.5-1.2B-Instruct-GGUF -c 4096 --port 8080
```

Key parameters:
- `-hf`: HuggingFace model ID (auto-downloads)
- `-m`: Local GGUF file path
- `-c`: Context length (default 4096)
- `--port`: Server port (default 8080)
- `-ngl 99`: GPU layer offloading

### CLI (llama-cli)
```bash
llama-cli -hf LiquidAI/LFM2.5-1.2B-Instruct-GGUF -c 4096 --color -i \
    --temp 0.1 --top-k 50 --repeat-penalty 1.05
```

### Vision Model Support
```bash
llama-cli \
    -hf LiquidAI/LFM2.5-VL-1.6B-GGUF:Q4_0 \
    --image test_image.jpg \
    --image-max-tokens 64 \
    -p "What's in this image?" \
    -n 128 \
    --temp 0.1 --min-p 0.15 --repeat-penalty 1.05
```

Server with vision:
```bash
llama-server \
    -m LFM2-VL-1.6B-Q8_0.gguf \
    --mmproj mmproj-LFM2-VL-1.6B-Q8_0.gguf \
    -c 4096 --port 8080 -ngl 99
```

### Model Conversion
```bash
python convert_hf_to_gguf.py /path/to/your/model --outfile model.gguf --outtype q4_k_m
```

---

## 17. Inference: Ollama

### Installation

| Platform | Command |
|----------|---------|
| macOS/Windows | Download from ollama.com/download |
| Linux | `curl -fsSL https://ollama.com/install.sh \| sh` |
| Docker CPU | `docker run -d -v ollama:/root/.ollama -p 11434:11434 --name ollama ollama/ollama` |
| Docker GPU | `docker run -d --gpus=all -v ollama:/root/.ollama -p 11434:11434 --name ollama ollama/ollama` |

### CRITICAL: Version Requirement
Ollama v0.17.0 (latest stable) **fails** with `missing tensor 'output_norm.weight'` error on the `lfm2moe` architecture. Requires **v0.17.1-rc0 or later** for LFM MoE models.

### Model Execution
```bash
# Direct from HuggingFace
ollama run hf.co/LiquidAI/LFM2.5-1.2B-Instruct-GGUF

# Local GGUF file
ollama run /path/to/model.gguf

# Single prompt
ollama run hf.co/LiquidAI/LFM2.5-1.2B-Instruct-GGUF "What is machine learning?"
```

### Modelfile Configuration
```
FROM hf.co/LiquidAI/LFM2.5-1.2B-Instruct-GGUF
TEMPLATE ...
PARAMETER temperature 0.1
PARAMETER top_k 50
PARAMETER repeat_penalty 1.05
PARAMETER stop "<|im_end|>"
```
```bash
ollama create my-model -f Modelfile
```

### API Endpoints
- Server: `http://localhost:11434`
- Generate: `/api/generate`
- Chat: `/api/chat`
- OpenAI-compatible: `/v1` (base_url: `http://localhost:11434/v1`)

### Model Management
```bash
ollama list
ollama rm hf.co/LiquidAI/LFM2.5-1.2B-Instruct-GGUF
ollama show hf.co/LiquidAI/LFM2.5-1.2B-Instruct-GGUF
```

### Capabilities
GGUF format, GPU acceleration (CUDA, Metal, ROCm), vision models (LFM2-VL), base64 image encoding.

---

## 18. Inference: MLX (Apple Silicon)

### Installation
```bash
pip install mlx-lm
```

### Usage
```python
from mlx_lm import load, generate
model, tokenizer = load("mlx-community/LFM2-1.2B-8bit")
```

Leverages **unified memory architecture on Apple Silicon** for seamless CPU/GPU data sharing. Targets M1, M2, M3, M4 chips with Metal GPU acceleration.

### Streaming
```python
from mlx_lm import stream_generate
# Use stream_generate() for streaming output
```

### Server
```bash
mlx_lm.server --model mlx-community/LFM2-1.2B-8bit --port 8080
```
OpenAI-compatible API.

### Vision Models
```python
from mlx_vlm import load, generate
```

### Quantization
8-bit quantized models available (e.g., `LFM2-1.2B-8bit`). Also supports 3-bit through bf16.

---

## 19. Inference: ONNX (Cross-Platform)

### Installation

**LiquidONNX Tool:**
```bash
git clone https://github.com/Liquid4All/onnx-export.git
cd onnx-export
uv sync
uv sync --extra gpu  # For GPU inference
```

**Python:**
```bash
pip install onnxruntime transformers numpy huggingface_hub jinja2
pip install onnxruntime-gpu  # For GPU
```

**JavaScript/WebGPU:**
```bash
npm install @huggingface/transformers
```

### Quantization Support by Model Family

| Model Family | Quantizations |
|---|---|
| LFM2.5, LFM2 (text) | fp32, fp16, q4, q8 |
| LFM2.5-VL, LFM2-VL (vision) | fp32, fp16, q4, q8 |
| LFM2-MoE | fp32, fp16, q4, q4f16 |
| LFM2.5-Audio | fp32, fp16, q4, q8 |

### Quantization Guidance
- **Q4**: Recommended for most deployments; supports WebGPU, CPU, GPU
- **FP16**: Higher quality; WebGPU/GPU
- **Q8**: Balanced quality/size; CPU/GPU only (no WebGPU)
- **FP32**: Full precision baseline

### Export Commands
```bash
# Text
uv run lfm2-export LiquidAI/LFM2.5-1.2B-Instruct --precision q4

# Vision
uv run lfm2-vl-export LiquidAI/LFM2.5-VL-1.6B --precision q4

# MoE
uv run lfm2-moe-export LiquidAI/LFM2-8B-A1B --precision q4

# Audio
uv run lfm2-audio-export LiquidAI/LFM2.5-Audio-1.5B --precision q4
```

### Inference Commands
```bash
# Text chat
uv run lfm2-infer --model ./exports/LFM2.5-1.2B-Instruct-ONNX/onnx/model_q4.onnx

# Vision
uv run lfm2-vl-infer --model ./exports/LFM2.5-VL-1.6B-ONNX --images photo.jpg --prompt "Describe"

# Audio ASR
uv run lfm2-audio-infer LFM2.5-Audio-1.5B-ONNX --mode asr --audio input.wav --precision q4

# Audio TTS
uv run lfm2-audio-infer LFM2.5-Audio-1.5B-ONNX --mode tts --prompt "Hello" --output speech.wav --precision q4
```

### Platform Support
- CPU/GPU: All quantizations
- WebGPU/Browser: Q4 and FP16 only
- NPU: Supported via ONNX runtime
- Edge devices: Fully compatible

### WebGPU Browser Setup
Enable in Chrome/Edge: `chrome://flags/#enable-unsafe-webgpu`

---

## 20. Troubleshooting & Known Issues

### Installation Issues

**`ImportError: cannot import name 'LfmForCausalLM'`**
- Requires `transformers>=4.55.0`

**CUDA Out of Memory**
- Use smaller model (LFM2-350M)
- Enable 4-bit quantization: `load_in_4bit=True`
- Reduce batch/sequence sizes
- Enable `gradient_checkpointing_enable()`

**Model Download Failures**
- Check connectivity
- Run `huggingface-cli login`
- Set `HF_HUB_DOWNLOAD_TIMEOUT=600`
- Use `snapshot_download()` as fallback

### Inference Issues

**Repetitive/Low-Quality Output**
- Factual tasks: temperature 0.3-0.5
- Creative tasks: temperature 0.7-1.0
- Default top_p: 0.9
- Repetition penalty: 1.1-1.2

**Slow Inference**
- CPU: Use GGUF with llama.cpp
- Apple Silicon: Use MLX
- Enable `attn_implementation="flash_attention_2"`
- High-throughput: Deploy vLLM
- Q4 quantization for speed

**Incomplete Output**
- Increase `max_new_tokens` (default 512-1024)
- Note: 32k context, but long inputs reduce output space

### Fine-Tuning Issues

**Training Loss Stagnation**
- LoRA learning rate: 2e-4
- Full fine-tuning: 2e-5
- Verify chat template compatibility
- Check for train/eval data overlap

**Memory During Fine-Tuning**
- Use QLoRA instead of full
- `per_device_train_batch_size=1`, `gradient_accumulation_steps=8`
- Enable `gradient_checkpointing=True`

### Ollama MoE Issue
Ollama v0.17.0 fails with `missing tensor 'output_norm.weight'` on `lfm2moe` architecture. Requires v0.17.1-rc0+.

### Support
- Discord: discord.gg/DFU3WQeaYD
- GitHub Issues: github.com/Liquid4All/docs/issues

---

## 21. Performance Benchmarks

### llama.cpp Benchmarks (LFM2-1.2B-Q4_0)

| Device | Prefill (tok/s) | Decode (tok/s) |
|--------|-----------------|-----------------|
| AMD Ryzen AI Max+ 395 | 5,476 | 143 |
| AMD Ryzen AI 9 HX 370 | 2,680 | 113 |
| Apple Mac Mini (M4) | 1,427 | 122 |
| Snapdragon X1E-78-100 | 978 | 125 |
| Intel Core Ultra 9 185H | 1,310 | 58 |
| Intel Core Ultra 7 258V | 1,104 | 78 |

### SGLang Benchmarks (NVIDIA B300 GPU)

LFM2.5-1.2B-Instruct, 256 prompts, 1024 input tokens, 128 output tokens:

| Metric | Value |
|--------|-------|
| Mean TTFT | 8.79ms |
| Mean TPOT | 0.86ms |
| Output throughput | 1,100.92 tok/s |

---

## Appendix: Full Documentation URLs

### LFM Core
- Welcome: https://docs.liquid.ai/lfm/getting-started/welcome
- Chat Template: https://docs.liquid.ai/lfm/key-concepts/chat-template
- Prompting Guide: https://docs.liquid.ai/lfm/key-concepts/text-generation-and-prompting
- Tool Use: https://docs.liquid.ai/lfm/key-concepts/tool-use
- Text Models: https://docs.liquid.ai/lfm/models/text-models
- Vision Models: https://docs.liquid.ai/lfm/models/vision-models
- Audio Models: https://docs.liquid.ai/lfm/models/audio-models
- Complete Library: https://docs.liquid.ai/lfm/models/complete-library
- Liquid Nanos: https://docs.liquid.ai/lfm/models/liquid-nanos
- FAQs: https://docs.liquid.ai/lfm/help/faqs
- Troubleshooting: https://docs.liquid.ai/lfm/help/troubleshooting

### Deployment
- Transformers: https://docs.liquid.ai/deployment/gpu-inference/transformers
- vLLM: https://docs.liquid.ai/deployment/gpu-inference/vllm
- SGLang: https://docs.liquid.ai/deployment/gpu-inference/sglang
- llama.cpp: https://docs.liquid.ai/deployment/on-device/llama-cpp
- Ollama: https://docs.liquid.ai/deployment/on-device/ollama
- MLX: https://docs.liquid.ai/deployment/on-device/mlx
- ONNX: https://docs.liquid.ai/deployment/on-device/onnx
- LM Studio: https://docs.liquid.ai/deployment/on-device/lm-studio

### Customization
- TRL: https://docs.liquid.ai/customization/finetuning-frameworks/trl
- Unsloth: https://docs.liquid.ai/customization/finetuning-frameworks/unsloth
- Datasets: https://docs.liquid.ai/customization/finetuning-frameworks/datasets

### API
- OpenAPI Spec: https://docs.liquid.ai/api-reference/openapi.json
- Full docs index: https://docs.liquid.ai/llms.txt
