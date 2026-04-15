#!/usr/bin/env python3
"""Sprint 069 Phase F dual-serve soak.

Fires identical flow-probe prompts against both the legacy Ollama endpoint and
the new llama-swap/vLLM endpoint on Hugin, recording per-call latency and a
content hash so divergence is measurable after the 24h window. Output goes to
`target/soak/dual-serve-<timestamp>.jsonl` — one line per probe pair.

Usage:
    python3 scripts/ops/vllm-dual-serve-soak.py \\
        --ollama-url http://10.0.65.9:11434 \\
        --vllm-url   http://10.0.65.9:11440 \\
        --duration   86400 \\
        --interval   30

The 24h default exits cleanly when the wall-clock window closes; a SIGTERM
mid-run flushes the buffer and exits too.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import signal
import sys
import time
from pathlib import Path

import requests

# Flow probe prompts — picked to exercise the three dominant Odin routes
# (coding, chat, reasoning) without pulling in live memory/tools. Each prompt
# maps to a stable model the soak rotates through so every model in the
# llama-swap config gets at least 48 samples over 24h at the default cadence.
PROBES: list[tuple[str, str]] = [
    ("gemma4:e4b",                "Explain why CRDTs are useful for offline-first apps in 2 sentences."),
    ("gemma4:e2b",                "What is a Merkle tree? Two sentences."),
    ("glm-4.7-flash:latest",      "Summarize the novelty gate cascade in Yggdrasil Mimir."),
    ("nemotron-3-nano:4b",        "Write a Rust function that counts set bits in a u64."),
    ("lfm-1.2b:latest",           "Name three benefits of per-session SDR drift tracking."),
    ("lfm25-tools:latest",        "JSON tool call to search 'quantization'."),
    ("saga-350m:latest",          "One-line summary: vLLM vs Ollama."),
    ("review-1.2b:latest",        "Review this code: let x = 1; let y = x; println!(\"{y}\");"),
    ("fusion-v6:latest",          "Generate a simple extrusion sketch operation."),
    ("code-cleaner-350m:latest",  "Clean up: def f ( x,y ):return x+ y"),
    ("hf.co/LiquidAI/LFM2.5-1.2B-Instruct-GGUF:Q4_K_M",
                                   "State one difference between Triton and CUTLASS kernels."),
]

STOP = False


def _handle_sigterm(_signum, _frame):  # noqa: ANN001 — signal signature
    global STOP
    STOP = True


def one_probe(base_url: str, model: str, prompt: str, timeout: float) -> dict:
    """Fire one /v1/chat/completions request and return a result dict."""
    t0 = time.time()
    body = {
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": False,
        "max_tokens": 128,
        "temperature": 0.0,
    }
    try:
        r = requests.post(
            f"{base_url.rstrip('/')}/v1/chat/completions",
            json=body,
            headers={"X-Yggdrasil-Internal": "true"},
            timeout=timeout,
        )
        latency_ms = (time.time() - t0) * 1000
        if r.status_code != 200:
            return {
                "ok": False,
                "status": r.status_code,
                "latency_ms": round(latency_ms, 1),
                "error_body": r.text[:500],
                "content_hash": None,
            }
        data = r.json()
        content = (
            data.get("choices", [{}])[0].get("message", {}).get("content", "") or ""
        )
        return {
            "ok": True,
            "status": 200,
            "latency_ms": round(latency_ms, 1),
            "content_hash": hashlib.sha256(content.encode("utf-8", "replace")).hexdigest()[:16],
            "content_len": len(content),
        }
    except requests.RequestException as e:
        return {
            "ok": False,
            "status": -1,
            "latency_ms": round((time.time() - t0) * 1000, 1),
            "error_body": f"{type(e).__name__}: {e}",
            "content_hash": None,
        }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--ollama-url", default="http://10.0.65.9:11434")
    ap.add_argument("--vllm-url",   default="http://10.0.65.9:11440")
    ap.add_argument("--duration",   type=int, default=86400,
                    help="soak duration in seconds (default 24h)")
    ap.add_argument("--interval",   type=int, default=30,
                    help="seconds between probe pairs")
    ap.add_argument("--timeout",    type=float, default=60.0,
                    help="per-request timeout in seconds")
    ap.add_argument("--out-dir",    default="target/soak")
    args = ap.parse_args()

    signal.signal(signal.SIGTERM, _handle_sigterm)
    signal.signal(signal.SIGINT, _handle_sigterm)

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    ts = time.strftime("%Y%m%dT%H%M%S", time.gmtime())
    out_path = out_dir / f"dual-serve-{ts}.jsonl"
    print(f"soak starting → {out_path}")
    print(f"  ollama={args.ollama_url}")
    print(f"  vllm  ={args.vllm_url}")
    print(f"  duration={args.duration}s  interval={args.interval}s  timeout={args.timeout}s")

    started = time.time()
    probe_idx = 0
    total_pairs = 0
    divergences = 0

    with out_path.open("w") as fh:
        while not STOP and (time.time() - started) < args.duration:
            model, prompt = PROBES[probe_idx % len(PROBES)]
            probe_idx += 1

            ollama_result = one_probe(args.ollama_url, model, prompt, args.timeout)
            vllm_result = one_probe(args.vllm_url, model, prompt, args.timeout)

            diverged = (
                ollama_result.get("ok")
                and vllm_result.get("ok")
                and ollama_result.get("content_hash") != vllm_result.get("content_hash")
            )
            if diverged:
                divergences += 1

            pair = {
                "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "model": model,
                "prompt_prefix": prompt[:60],
                "ollama": ollama_result,
                "vllm":   vllm_result,
                "diverged": diverged,
            }
            fh.write(json.dumps(pair) + "\n")
            fh.flush()
            total_pairs += 1

            if total_pairs % 20 == 0:
                elapsed = int(time.time() - started)
                print(f"  +{elapsed}s  pairs={total_pairs}  divergences={divergences}"
                      f"  last_model={model}")

            # Sleep in 1s slices so SIGTERM responds within a second.
            slept = 0
            while slept < args.interval and not STOP:
                time.sleep(1)
                slept += 1

    print(f"soak complete. pairs={total_pairs}  divergences={divergences}  out={out_path}")
    return 0 if not STOP else 130


if __name__ == "__main__":
    sys.exit(main())
