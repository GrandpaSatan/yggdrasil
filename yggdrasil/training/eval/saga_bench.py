#!/usr/bin/env python3
"""Yggdrasil Memory Pipeline (Saga) Benchmark.

Tests the Mimir smart-ingest model on its primary tasks:
  1. CLASSIFY — categorize content (bug_fix, architecture_decision, etc.)
  2. DISTILL — extract cause/effect/tags
  3. STORE/SKIP — correctly decide what's worth persisting
  4. False positives — routine code/comments should be SKIP

This benchmarks the current LFM2.5-1.2B-Instruct baseline and any grokked
lfm-saga variant.  Uses the same prompt format as Mimir's smart-ingest
handler (handlers.rs:1838).

Usage:
    python saga_bench.py --model hf.co/LiquidAI/LFM2.5-1.2B-Instruct-GGUF:Q4_K_M
    python saga_bench.py --model lfm-saga  # test grokked model
"""

import argparse
import json
import re
import time
from dataclasses import dataclass, asdict
from pathlib import Path

import requests

SYSTEM_PROMPT = "You are Saga, Yggdrasil's memory engine. Respond ONLY in valid JSON."

# ─────────────────────────────────────────────────────────────────
# Test cases — inline for self-contained benchmark
# ─────────────────────────────────────────────────────────────────

CLASSIFY_EXAMPLES = [
    {
        "input": "CLASSIFY\ntool: Edit\nfile: odin/src/handlers.rs\ncontent: Fixed session timeout bug — sessions were expiring after 60s instead of 3600s due to wrong default value",
        "expected": {"category": "bug_fix", "should_store": True},
    },
    {
        "input": "CLASSIFY\ntool: Edit\nfile: ygg-domain/src/config.rs\ncontent: Added BackendConfig struct with url, models, max_concurrent fields for multi-backend LLM routing",
        "expected": {"category": "architecture_decision", "should_store": True},
    },
    {
        "input": "CLASSIFY\ntool: Edit\nfile: sprints/sprint-054.md\ncontent: Sprint 054 started — LLM Fleet Optimization with grokked specialist models",
        "expected": {"category": "sprint_lifecycle", "should_store": True},
    },
    {
        "input": "CLASSIFY\ntool: Edit\nfile: src/main.rs\ncontent: Added missing semicolon",
        "expected": {"category": "bug_fix", "should_store": False},
    },
    {
        "input": "CLASSIFY\ntool: Bash\ncommand: cargo check\ncontent: Compiling yggdrasil v1.0.0\n    Finished dev [unoptimized + debuginfo]",
        "expected": {"category": "routine", "should_store": False},
    },
    {
        "input": "CLASSIFY\ntool: Edit\nfile: deploy/munin/odin.service\ncontent: Changed OLLAMA_MAX_LOADED_MODELS from 2 to 6 in systemd unit — Munin eGPU can handle parallel model loading",
        "expected": {"category": "deployment_change", "should_store": True},
    },
    {
        "input": "CLASSIFY\ntool: Read\nfile: README.md\ncontent: # Yggdrasil\\nAI homelab orchestrator",
        "expected": {"category": "routine", "should_store": False},
    },
    {
        "input": "CLASSIFY\ntool: Edit\nfile: odin/src/proxy.rs\ncontent: GOTCHA: Ollama returns newline-delimited JSON for streaming, not SSE. Had to switch from eventsource to line-by-line parsing.",
        "expected": {"category": "gotcha", "should_store": True},
    },
]

DISTILL_EXAMPLES = [
    {
        "input": "DISTILL\ncontent: Sprint 053 found that Munin's Ollama uses AMD RX 9060 XT eGPU via ROCm (gfx1200), not the Intel iGPU as previously assumed. LFM2-24B-A2B MoE works correctly on ROCm without the hallucination bug seen on IPEX.",
        "expected_fields": ["cause", "effect", "tags"],
        "expected_tag_contains": ["munin", "ollama"],
    },
    {
        "input": "DISTILL\ncontent: Discovered that HA automation generator hardcodes qwen3:30b-a3b model name in ygg-ha/src/automation.rs line 86. This should be configurable via HaConfig to support model swapping.",
        "expected_fields": ["cause", "effect", "tags"],
        "expected_tag_contains": ["ha", "config"],
    },
]

STORE_SKIP_EXAMPLES = [
    # Should STORE
    {"input": "CLASSIFY\ncontent: Migrated all nodes from VLAN 25 to VLAN 65 — new IP range 10.0.65.x. Updated all config files and .env.", "expect_store": True},
    {"input": "CLASSIFY\ncontent: qwen3.5:4b removed from fleet — empty response bug when thinking enabled. Replaced by LFM2.5-1.2B-Instruct.", "expect_store": True},
    # Should SKIP
    {"input": "CLASSIFY\ncontent: Updated import order in handlers.rs", "expect_store": False},
    {"input": "CLASSIFY\ncontent: cargo fmt -- all files", "expect_store": False},
    {"input": "CLASSIFY\ncontent: git status\nM odin/src/main.rs", "expect_store": False},
    {"input": "CLASSIFY\ncontent: ls -la /opt/yggdrasil/bin/", "expect_store": False},
]


@dataclass
class SagaResult:
    task_type: str
    model: str
    total: int = 0
    valid_json: int = 0
    correct: int = 0
    accuracy: float = 0.0
    json_rate: float = 0.0
    latency_avg_ms: float = 0.0
    details: list = None

    def __post_init__(self):
        if self.details is None:
            self.details = []


def query_saga(url: str, model: str, prompt: str, backend: str = "ollama",
               timeout: int = 15) -> tuple[dict | None, float]:
    """Query model in Saga format. Returns (parsed_json, latency_ms)."""
    start = time.monotonic()
    try:
        if backend == "openai":
            resp = requests.post(
                f"{url}/v1/chat/completions",
                json={"model": model, "messages": [
                    {"role": "system", "content": SYSTEM_PROMPT},
                    {"role": "user", "content": prompt},
                ], "temperature": 0.1, "max_tokens": 256},
                timeout=timeout,
            )
            latency = (time.monotonic() - start) * 1000
            if resp.status_code != 200:
                return None, latency
            text = resp.json()["choices"][0]["message"]["content"]
        else:
            resp = requests.post(
                f"{url}/api/chat",
                json={"model": model, "messages": [
                    {"role": "system", "content": SYSTEM_PROMPT},
                    {"role": "user", "content": prompt},
                ], "stream": False, "options": {"temperature": 0.1, "num_predict": 256}},
                timeout=timeout,
            )
            latency = (time.monotonic() - start) * 1000
            if resp.status_code != 200:
                return None, latency
            text = resp.json().get("message", {}).get("content", "")

        # Strip thinking tags and extract JSON
        text = re.sub(r'<think>.*?</think>', '', text, flags=re.DOTALL).strip()
        match = re.search(r'\{[^{}]*\}', text)
        if match:
            return json.loads(match.group()), latency
        return None, latency
    except (requests.RequestException, json.JSONDecodeError, KeyError):
        return None, (time.monotonic() - start) * 1000


def eval_classify(url: str, model: str, backend: str) -> SagaResult:
    """Evaluate CLASSIFY task."""
    result = SagaResult(task_type="CLASSIFY", model=model, total=len(CLASSIFY_EXAMPLES))
    latencies = []

    for ex in CLASSIFY_EXAMPLES:
        parsed, lat = query_saga(url, model, ex["input"], backend)
        latencies.append(lat)

        if parsed is None:
            result.details.append({"input": ex["input"][:80], "status": "invalid_json"})
            continue

        result.valid_json += 1
        expected = ex["expected"]
        cat_ok = parsed.get("category") == expected.get("category")
        store_ok = parsed.get("should_store") == expected.get("should_store")
        correct = cat_ok and store_ok

        if correct:
            result.correct += 1

        result.details.append({
            "input": ex["input"][:80],
            "expected_cat": expected.get("category"),
            "got_cat": parsed.get("category"),
            "expected_store": expected.get("should_store"),
            "got_store": parsed.get("should_store"),
            "correct": correct,
        })

    result.accuracy = result.correct / result.total if result.total else 0
    result.json_rate = result.valid_json / result.total if result.total else 0
    result.latency_avg_ms = sum(latencies) / len(latencies) if latencies else 0
    return result


def eval_distill(url: str, model: str, backend: str) -> SagaResult:
    """Evaluate DISTILL task."""
    result = SagaResult(task_type="DISTILL", model=model, total=len(DISTILL_EXAMPLES))
    latencies = []

    for ex in DISTILL_EXAMPLES:
        parsed, lat = query_saga(url, model, ex["input"], backend)
        latencies.append(lat)

        if parsed is None:
            result.valid_json += 0
            continue

        result.valid_json += 1
        has_fields = all(k in parsed for k in ex["expected_fields"])
        tags = parsed.get("tags", [])
        if isinstance(tags, str):
            tags = [t.strip() for t in tags.split(",")]

        tags_lower = [t.lower() for t in tags] if tags else []
        has_tags = all(
            any(exp in t for t in tags_lower)
            for exp in ex.get("expected_tag_contains", [])
        )

        correct = has_fields and has_tags
        if correct:
            result.correct += 1

        result.details.append({
            "input": ex["input"][:80],
            "has_fields": has_fields,
            "has_expected_tags": has_tags,
            "got_tags": tags[:5],
            "correct": correct,
        })

    result.accuracy = result.correct / result.total if result.total else 0
    result.json_rate = result.valid_json / result.total if result.total else 0
    result.latency_avg_ms = sum(latencies) / len(latencies) if latencies else 0
    return result


def eval_store_skip(url: str, model: str, backend: str) -> SagaResult:
    """Evaluate STORE vs SKIP decision accuracy."""
    result = SagaResult(task_type="STORE_SKIP", model=model, total=len(STORE_SKIP_EXAMPLES))
    latencies = []

    for ex in STORE_SKIP_EXAMPLES:
        parsed, lat = query_saga(url, model, ex["input"], backend)
        latencies.append(lat)

        if parsed is None:
            continue

        result.valid_json += 1
        got_store = parsed.get("should_store", None)

        # Handle string "true"/"false"
        if isinstance(got_store, str):
            got_store = got_store.lower() == "true"

        correct = got_store == ex["expect_store"]
        if correct:
            result.correct += 1

        result.details.append({
            "input": ex["input"][:80],
            "expected": ex["expect_store"],
            "got": got_store,
            "correct": correct,
        })

    result.accuracy = result.correct / result.total if result.total else 0
    result.json_rate = result.valid_json / result.total if result.total else 0
    result.latency_avg_ms = sum(latencies) / len(latencies) if latencies else 0
    return result


def print_summary(results: list[SagaResult]):
    print(f"\n{'=' * 60}")
    print(f"SAGA MEMORY PIPELINE BENCHMARK — {results[0].model}")
    print(f"{'=' * 60}")

    for r in results:
        print(f"\n  {r.task_type}:")
        print(f"    Accuracy:  {r.correct}/{r.total} ({r.accuracy:.0%})")
        print(f"    JSON rate: {r.valid_json}/{r.total} ({r.json_rate:.0%})")
        print(f"    Avg latency: {r.latency_avg_ms:.0f}ms")

    print(f"\n{'─' * 60}")
    print("ACCEPTANCE CRITERIA:")
    classify = next((r for r in results if r.task_type == "CLASSIFY"), None)
    distill = next((r for r in results if r.task_type == "DISTILL"), None)
    store_skip = next((r for r in results if r.task_type == "STORE_SKIP"), None)

    checks = []
    if classify:
        ok = classify.accuracy >= 0.75
        checks.append(ok)
        print(f"  CLASSIFY accuracy >= 75%: {'PASS' if ok else 'FAIL'} ({classify.accuracy:.0%})")
    if distill:
        ok = distill.accuracy >= 0.75
        checks.append(ok)
        print(f"  DISTILL completeness >= 75%: {'PASS' if ok else 'FAIL'} ({distill.accuracy:.0%})")
    if store_skip:
        ok = store_skip.accuracy >= 0.80
        checks.append(ok)
        print(f"  STORE/SKIP accuracy >= 80%: {'PASS' if ok else 'FAIL'} ({store_skip.accuracy:.0%})")

    json_ok = all(r.json_rate >= 0.80 for r in results)
    checks.append(json_ok)
    print(f"  JSON validity >= 80% all tasks: {'PASS' if json_ok else 'FAIL'}")

    overall = all(checks)
    print(f"\n  OVERALL: {'PASS' if overall else 'FAIL'}")
    print(f"{'=' * 60}")
    return overall


def main():
    parser = argparse.ArgumentParser(description="Yggdrasil Saga Memory Benchmark")
    parser.add_argument("--model", required=True)
    parser.add_argument("--url", default="http://localhost:11434")
    parser.add_argument("--backend", default="ollama", choices=["ollama", "openai"])
    parser.add_argument("--output", default="saga_bench_results.json")
    args = parser.parse_args()

    print(f"\nModel: {args.model}")
    print(f"URL:   {args.url} ({args.backend})")
    print(f"{'─' * 60}")

    print("\nRunning CLASSIFY...")
    classify = eval_classify(args.url, args.model, args.backend)

    print("\nRunning DISTILL...")
    distill = eval_distill(args.url, args.model, args.backend)

    print("\nRunning STORE/SKIP...")
    store_skip = eval_store_skip(args.url, args.model, args.backend)

    results = [classify, distill, store_skip]

    with open(args.output, "w") as f:
        json.dump([asdict(r) for r in results], f, indent=2)
    print(f"\nResults saved to {args.output}")

    print_summary(results)


if __name__ == "__main__":
    main()
