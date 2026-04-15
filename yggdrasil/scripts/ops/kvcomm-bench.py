#!/usr/bin/env python3
"""Sprint 069 Phase G — KVCOMM per-pair TTFT bench.

Compares time-to-first-token (TTFT) on a flow-pair query with KVCOMM
disabled vs. enabled. Records P50/P95 per pair and the anchor hit rate
scraped from llama-swap's Prometheus endpoint.

Targets the live llama-swap on Hugin (or a localhost test instance).

Usage:
    python3 scripts/ops/kvcomm-bench.py --pair coding_swarm --queries 50
    python3 scripts/ops/kvcomm-bench.py --all-pairs --queries 50

Output: target/kvcomm/<pair>-<timestamp>.json with full per-query results
plus a summary table printed to stdout.
"""
from __future__ import annotations

import argparse
import json
import statistics
import sys
import time
from dataclasses import dataclass, asdict
from pathlib import Path

import requests

# Mirrors deploy/hugin/llama-swap/kvcomm.yaml. Each pair's bench fires the
# producer FIRST to warm the anchor pool, then the consumer with the
# producer's output verbatim — that's the whole point of KVCOMM.
PAIRS: dict[str, tuple[str, str, str, str]] = {
    "coding_swarm": (
        "nemotron-3-nano:4b",
        "review-1.2b:latest",
        "Write a Rust function to compute the SHA-256 of a byte slice.",
        "Review the following Rust code for correctness and idiomatic style:\n",
    ),
    "memory_consolidate": (
        "saga-350m:latest",
        "review-1.2b:latest",
        "Summarise these three engrams about Sprint 069 Phase D into one paragraph: ...",
        "Fact-check the following summary against the originals:\n",
    ),
    "perceive": (
        "lfm-1.2b:latest",
        "gemma4:e4b",
        "Transcribe (mock): The user said 'show me the kitchen lights'.",
        "Plan the home-assistant call for this transcribed user request:\n",
    ),
    "research": (
        "gemma4:e4b",
        "nemotron-3-nano:4b",
        "Plan tools to use to find: 'latest TurboQuant paper'.",
        "Execute the following research plan step by step:\n",
    ),
    "dream_exploration": (
        "lfm25-tools:latest",
        "review-1.2b:latest",
        "Generate three exploratory query variants on 'KV cache compression'.",
        "Rank the following query variants by expected information gain:\n",
    ),
    "dream_self_improvement": (
        "review-1.2b:latest",
        "code-cleaner-350m:latest",
        "Critique this code: def f(x):return x+1 # missing space, no docstring",
        "Apply the following review feedback to clean up the code:\n",
    ),
}


@dataclass
class TtftResult:
    ok: bool
    ttft_ms: float
    total_ms: float
    status: int
    error: str | None = None


def measure_ttft(base_url: str, model: str, prompt: str, timeout: float) -> TtftResult:
    """Stream a chat completion and capture the time to first content token."""
    body = {
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": True,
        "max_tokens": 64,
        "temperature": 0.0,
    }
    t0 = time.time()
    first_token_ts: float | None = None
    try:
        r = requests.post(
            f"{base_url.rstrip('/')}/v1/chat/completions",
            json=body,
            headers={"X-Yggdrasil-Internal": "true"},
            stream=True,
            timeout=timeout,
        )
        if r.status_code != 200:
            return TtftResult(False, 0.0, 0.0, r.status_code, error=r.text[:300])
        for line in r.iter_lines():
            if not line or not line.startswith(b"data: "):
                continue
            payload = line[6:]
            if payload == b"[DONE]":
                break
            try:
                obj = json.loads(payload)
            except Exception:
                continue
            delta = obj.get("choices", [{}])[0].get("delta", {})
            if delta.get("content") and first_token_ts is None:
                first_token_ts = time.time()
        total = (time.time() - t0) * 1000
        ttft = ((first_token_ts - t0) * 1000) if first_token_ts else total
        return TtftResult(True, ttft, total, 200)
    except requests.RequestException as e:
        return TtftResult(False, 0.0, (time.time() - t0) * 1000, -1, error=f"{type(e).__name__}: {e}")


def fetch_anchor_hit_rate(metrics_url: str, pair: str) -> float | None:
    """Scrape llama-swap /metrics for the anchor hit rate."""
    try:
        r = requests.get(metrics_url, timeout=5)
        r.raise_for_status()
        for line in r.text.splitlines():
            if line.startswith("vllm_kvcomm_anchor_hit_rate") and f'pair="{pair}"' in line:
                parts = line.rsplit(None, 1)
                if len(parts) == 2:
                    return float(parts[1])
    except Exception:
        return None
    return None


def bench_pair(
    pair: str, base_url: str, metrics_url: str, queries: int, timeout: float
) -> dict:
    if pair not in PAIRS:
        raise SystemExit(f"unknown pair: {pair}; choose from {list(PAIRS)}")
    producer, consumer, producer_prompt, consumer_prefix = PAIRS[pair]
    print(f"\n=== pair={pair}  producer={producer}  consumer={consumer}  queries={queries} ===")

    consumer_results: list[TtftResult] = []
    for i in range(queries):
        prod = measure_ttft(base_url, producer, producer_prompt, timeout)
        if not prod.ok:
            print(f"  q{i:03d} producer FAILED: {prod.status} {prod.error or ''}")
            continue
        cons_prompt = consumer_prefix + (
            f"(producer output, {int(prod.total_ms)} ms total)" if i == 0 else ""
        )
        cons = measure_ttft(base_url, consumer, cons_prompt, timeout)
        consumer_results.append(cons)
        if (i + 1) % 10 == 0:
            ttfts = [r.ttft_ms for r in consumer_results if r.ok]
            if ttfts:
                print(f"  q{i+1:03d}  consumer TTFT median={statistics.median(ttfts):.0f}ms  ok={sum(r.ok for r in consumer_results)}/{len(consumer_results)}")

    ttfts_ok = [r.ttft_ms for r in consumer_results if r.ok]
    summary = {
        "pair": pair,
        "producer": producer,
        "consumer": consumer,
        "queries_attempted": queries,
        "queries_ok": len(ttfts_ok),
        "consumer_ttft_p50_ms": statistics.median(ttfts_ok) if ttfts_ok else None,
        "consumer_ttft_p95_ms": statistics.quantiles(ttfts_ok, n=20)[18] if len(ttfts_ok) >= 20 else None,
        "anchor_hit_rate": fetch_anchor_hit_rate(metrics_url, pair),
        "raw": [asdict(r) for r in consumer_results],
    }
    return summary


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base-url", default="http://10.0.65.9:11440",
                    help="llama-swap or vLLM base URL")
    ap.add_argument("--metrics-url", default="http://10.0.65.9:11440/metrics",
                    help="Prometheus metrics endpoint")
    ap.add_argument("--pair", help="single pair name; mutually exclusive with --all-pairs")
    ap.add_argument("--all-pairs", action="store_true")
    ap.add_argument("--queries", type=int, default=50)
    ap.add_argument("--timeout", type=float, default=120.0)
    ap.add_argument("--out-dir", default="target/kvcomm")
    args = ap.parse_args()

    if not args.pair and not args.all_pairs:
        raise SystemExit("must pass --pair <name> or --all-pairs")

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    ts = time.strftime("%Y%m%dT%H%M%S", time.gmtime())

    results: list[dict] = []
    pairs_to_run = list(PAIRS) if args.all_pairs else [args.pair]
    for p in pairs_to_run:
        summary = bench_pair(p, args.base_url, args.metrics_url, args.queries, args.timeout)
        out_path = out_dir / f"{p}-{ts}.json"
        out_path.write_text(json.dumps(summary, indent=2))
        results.append(summary)

    print("\n=== Summary ===")
    print(f"{'pair':<24} {'p50':>8} {'p95':>8} {'hit_rate':>10} {'ok/total':>10}")
    for s in results:
        p50 = f"{s['consumer_ttft_p50_ms']:.0f}" if s["consumer_ttft_p50_ms"] else "—"
        p95 = f"{s['consumer_ttft_p95_ms']:.0f}" if s["consumer_ttft_p95_ms"] else "—"
        hit = f"{s['anchor_hit_rate']:.3f}" if s["anchor_hit_rate"] is not None else "—"
        ok  = f"{s['queries_ok']}/{s['queries_attempted']}"
        print(f"{s['pair']:<24} {p50:>8} {p95:>8} {hit:>10} {ok:>10}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
