# Sprint 069 Phase F — Track B cutover status (2026-04-15)

## What's verified working

- Docker 29.4.0 installed on Hugin and configured with `jhernandez` in the `docker` group.
- **Real vLLM ROCm image** pulled and runs on Hugin:
  `rocm/vllm-dev:rocm7.2.1_navi_ubuntu24.04_py3.12_pytorch_2.9_vllm_0.16.0`
  (vLLM `0.16.1.dev0+g89a77b108`, PyTorch 2.9, ROCm 7.2.1, Python 3.12.)
- **gfx1150 (Hugin 890M iGPU) is supported** by this image with no special args.
  `HSA_OVERRIDE_GFX_VERSION=11.5.0` + `VLLM_USE_TRITON_FLASH_ATTN=0` envs are
  the right combination for the HX 370 iGPU.
- ROCm 7.5 + ROCM-SMI 3.0.0 + PyTorch present on the host (the in-image
  ROCm 7.2.1 stack runs fine on top of the host's 7.5).
- Hugin host disk: 1.5 TB free on `/`. Plenty of headroom for model staging.

## What's blocked — and why

The Ollama-sourced GGUF blobs we have on Hugin are **architecturally
incompatible with vLLM 0.16.1's transformers GGUF parser** for three of the
five model families we tested:

| GGUF blob | vLLM error |
|---|---|
| `gemma4:e4b`, `gemma4:e2b` | `ValueError: GGUF model with architecture gemma4 is not supported yet.` |
| `nemotron-3-nano:4b` | `ValueError: GGUF model with architecture nemotron_h is not supported yet.` |
| `lfm-1.2b`, `lfm25-tools`, `LFM2.5-1.2B-Instruct-GGUF:Q4_K_M` (same blob) | `AttributeError: 'Lfm2Config' object has no attribute 'conv_dim'` (transformers/vLLM interop bug on LFM2 GGUF) |
| `code-cleaner-350m`, `saga-350m` (LFM2-based) | likely same `conv_dim` issue |

The vLLM container itself loads cleanly — every error is at the model-config
parsing layer, NOT at the GPU/ROCm boundary. **The gfx1150 path works.**

## Path forward — two real options

### Option A: switch to HuggingFace safetensors format (recommended)

vLLM serves HF model directories natively without going through the
GGUF-parser path that's failing. Three of the four custom distilled models
on Morrigan are already in HF safetensors format
(`/home/jhernandez/fine-tuning/merged-models/lfm-saga-v3`,
`lfm-review-v2`, etc.) — those load directly. For the others we'd need
to download safetensors from HuggingFace (gemma-3, nemotron, etc.).

**Cost:** ~30–80 GB of additional downloads, mostly one-time.
**Benefit:** decoupled from GGUF-parser-of-the-month. Model config bugs
(like the LFM2 `conv_dim` one) are fixable upstream against
mainline `transformers`.

### Option B: build vLLM from source against newer transformers

Take the `Dockerfile.rocm` from vllm-project/vllm `main`, build with
`PYTORCH_ROCM_ARCH="gfx1150;gfx1200"`, pin a transformers commit that
has gemma3 / nemotron_h GGUF support landed. Estimated build time:
40–60 minutes on Hugin. Risk: each new transformers release can re-break
config compatibility.

## Authored artifacts (committed in this sprint)

- `deploy/hugin/llama-swap/config.yaml` — full 11-model llama-swap config
  using the verified vLLM image. Listen port `:11440` for soak; flips to
  `:11434` post-cutover. Mixes GGUF (for the working models) and HF
  safetensors (for Morrigan distilled).
- `deploy/munin/tei/docker-compose.yml` — TEI on Munin :11438 (soak port;
  moves to :11435 once `ollama-b.service` is shut down at cutover).
- `scripts/ops/stage-vllm-models.sh` — idempotent staging script: symlinks
  Ollama blobs + rsyncs Morrigan safetensors + downloads GLM-4.7-Flash GGUF.
- `scripts/ops/vllm-dual-serve-soak.py` — 24h dual-serve soak harness:
  fires identical prompts at Ollama and vLLM, hashes responses, logs
  divergence to `target/soak/dual-serve-<ts>.jsonl`.

## What an operator needs to do to finish Phase F

1. Decide A or B above.
2. (If A) Run `huggingface-cli download` for the safetensors-form of
   gemma-3, nemotron-3-nano, and any other Ollama-only models we want.
3. Update `config.yaml` to point those models at the safetensors directories.
4. Run `scripts/ops/stage-vllm-models.sh` on Hugin to populate
   `/opt/yggdrasil/models/`.
5. Install + start `yggdrasil-llama-swap.service` on Hugin (systemd unit
   already in `deploy/hugin/llama-swap/`).
6. `docker compose up -d` for `deploy/munin/tei/`.
7. Kick off `vllm-dual-serve-soak.py` for 24h.
8. Review divergence log; if low → flip Odin config URL from `:11434` to
   `:11440`, then update llama-swap config to listen on `:11434`,
   `systemctl disable --now ollama` on Hugin, restart llama-swap.
9. Free Munin :11435 by `systemctl disable --now ollama-b`, update
   TEI compose to publish on :11435, restart.

Until those steps run, the production fleet keeps using Ollama (no
behaviour change). Phases G/H/I/J author code that targets the vLLM
engine API; they can be implemented and unit-tested before the runtime
flip happens.
