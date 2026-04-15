#!/usr/bin/env python3
"""
Yggdrasil Shared Memory Fabric — Hidden-State Extraction.
Sprint 069 Phase G.5.

For each model in the fleet, runs a forward pass on every prompt in a
shared corpus and captures:
- Per-layer K tensors (post-RoPE)
- Per-layer V tensors
- Final layer hidden state
- Input token ids

Output: one .pt file per (model, prompt) pair.

Runs on Morrigan (2x RTX 3060, CUDA). Designed to be resumable —
existing .pt files are skipped on restart.

Usage:
    python3 fabric-extract-hiddens.py \
        --prompts ~/fine-tuning/fabric-data/prompts.jsonl \
        --output-dir ~/fine-tuning/fabric-data/hiddens \
        --limit 300 \
        --max-seq 256
"""

import argparse
import hashlib
import json
import os
import sys
import time
from pathlib import Path

import torch
from transformers import AutoModelForCausalLM, AutoTokenizer

# Fleet roster — BASE models only. Yggdrasil production fleet is
# LFM2/LFM2.5 + Gemma + Nemotron + GLM + RWKV. NO Qwen. See
# memory/project_model_fleet.md and memory/feedback_never_qwen.md.
#
# Loaded from Morrigan's HuggingFace cache (~/.cache/huggingface/hub/).
MODELS = {
    "lfm2-350m":        "LiquidAI/LFM2-350M",
    "lfm2.5-1.2b-base": "LiquidAI/LFM2.5-1.2B-Base",
    "gemma-4-e2b":      "google/gemma-4-E2B",            # GATED — needs HF access grant
    "gemma-4-e4b":      "google/gemma-4-E4B",            # GATED — needs HF access grant
    "nemotron-nano-4b": "nvidia/Nemotron-H-4B-Base-8K",  # downloaded 2026-04-15
    "rwkv-7-world":     "BlinkDL/rwkv-7-world",          # RWKV needs special extraction (no K/V)
    # glm-4.7-flash:   "THUDM/GLM-4.7-Flash"             # 30B MoE — Hugin-only, needs 4-bit quant
}


def load_prompts(path: Path, limit: int):
    prompts = []
    with path.open() as f:
        for line in f:
            prompts.append(json.loads(line))
            if len(prompts) >= limit:
                break
    return prompts


def extract_for_model(model_name, model_path, prompts, output_dir, device, max_seq):
    out_dir = Path(output_dir) / model_name
    out_dir.mkdir(parents=True, exist_ok=True)

    # Skip model entirely if all prompts already extracted
    existing = {p.stem for p in out_dir.glob("*.pt")}
    to_process = [p for p in prompts
                  if hashlib.sha256(p["prompt"].encode()).hexdigest()[:16] not in existing]
    if not to_process:
        print(f"[{model_name}] all {len(prompts)} prompts already extracted, skipping")
        return
    print(f"[{model_name}] {len(to_process)}/{len(prompts)} remaining")

    print(f"[{model_name}] loading {model_path}")
    t0 = time.time()
    tokenizer = AutoTokenizer.from_pretrained(model_path, trust_remote_code=True)
    model = AutoModelForCausalLM.from_pretrained(
        model_path,
        torch_dtype=torch.float16,
        trust_remote_code=True,
        device_map={"": device},
    )
    model.eval()
    print(f"[{model_name}] loaded in {time.time()-t0:.1f}s")

    start = time.time()
    for i, record in enumerate(to_process):
        prompt_text = record["prompt"]
        prompt_hash = hashlib.sha256(prompt_text.encode()).hexdigest()[:16]
        out_file = out_dir / f"{prompt_hash}.pt"
        if out_file.exists():
            continue

        inputs = tokenizer(prompt_text, return_tensors="pt",
                           truncation=True, max_length=max_seq).to(device)

        with torch.no_grad():
            outputs = model(
                **inputs,
                use_cache=True,
                output_hidden_states=True,
                return_dict=True,
            )

        pkv = outputs.past_key_values
        layer_types = getattr(model.config, "layer_types", None)

        # Capture attention K/V for attention layers; capture conv state for
        # conv layers (LFM2 only). Pure-attention models (Qwen2.5) hit only
        # the K/V path.
        attn_layers = []   # list of (layer_idx, K, V)
        conv_layers = []   # list of (layer_idx, conv_state)

        def _cpu16(t):
            return t.detach().cpu().to(torch.float16)

        # Attribute-based access (works for Lfm2HybridConvCache, DynamicCache, etc)
        key_cache = getattr(pkv, "key_cache", None)
        value_cache = getattr(pkv, "value_cache", None)
        conv_cache = getattr(pkv, "conv_cache", None)

        n_layers = model.config.num_hidden_layers
        for li in range(n_layers):
            ltype = layer_types[li] if layer_types else "full_attention"
            if ltype == "full_attention":
                if key_cache is not None and li < len(key_cache) and key_cache[li].numel() > 0:
                    attn_layers.append((li, _cpu16(key_cache[li]), _cpu16(value_cache[li])))
            elif ltype == "conv":
                if conv_cache is not None and li < len(conv_cache) and conv_cache[li].numel() > 0:
                    conv_layers.append((li, _cpu16(conv_cache[li])))

        payload = {
            "prompt_hash": prompt_hash,
            "prompt": prompt_text,
            "source": record.get("source", "unknown"),
            "model_name": model_name,
            "model_path": model_path,
            "input_ids": inputs["input_ids"].cpu(),
            "attention_mask": inputs["attention_mask"].cpu(),
            "seq_len": inputs["input_ids"].shape[1],
            "layer_types": layer_types,  # None for pure-attention models
            "hidden_states": [_cpu16(h) for h in outputs.hidden_states],
            "attn_kv": attn_layers,      # [(layer_idx, K, V), ...]
            "conv_state": conv_layers,   # [(layer_idx, state), ...] or empty
        }
        torch.save(payload, out_file)

        if (i + 1) % 25 == 0 or (i + 1) == len(to_process):
            elapsed = time.time() - start
            rate = (i + 1) / max(elapsed, 0.001)
            remaining = (len(to_process) - i - 1) / max(rate, 0.001)
            print(f"[{model_name}] {i+1}/{len(to_process)} | {rate:.2f}/s | ETA {remaining/60:.1f}m | seq={payload['seq_len']}")

    # Unload to free VRAM for the next model
    del model
    torch.cuda.empty_cache()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--prompts", type=Path, required=True)
    ap.add_argument("--output-dir", type=Path, required=True)
    ap.add_argument("--models", nargs="+", default=list(MODELS.keys()),
                    help="Subset of models to extract")
    ap.add_argument("--limit", type=int, default=3000)
    ap.add_argument("--max-seq", type=int, default=256)
    ap.add_argument("--device", default="cuda:0")
    args = ap.parse_args()

    prompts = load_prompts(args.prompts, args.limit)
    print(f"Loaded {len(prompts)} prompts from {args.prompts}")
    print(f"Output dir: {args.output_dir}")
    print(f"Max seq len: {args.max_seq}")
    print(f"Device: {args.device}")
    print(f"Models: {args.models}")
    print("")

    for model_name in args.models:
        if model_name not in MODELS:
            print(f"!! unknown model {model_name}, skipping")
            continue
        model_path = MODELS[model_name]
        try:
            extract_for_model(model_name, model_path, prompts,
                              args.output_dir, args.device, args.max_seq)
        except Exception as e:
            print(f"!! [{model_name}] FAILED: {type(e).__name__}: {e}")
            import traceback
            traceback.print_exc()

    print("\nDONE")


if __name__ == "__main__":
    main()
