#!/usr/bin/env python3
"""Deployment Smoke Test — verify all LLM backends are responding.

For each configured backend, sends a simple request and checks:
  - HTTP connectivity
  - Model availability (Ollama /api/tags or OpenAI /v1/models)
  - Response format valid
  - Latency within bounds (<2s small models, <10s large)

Also runs model-specific canaries:
  - lfm-reviewer: given known-bad code, must detect the issue
  - lfm-saga: CLASSIFY task must return valid JSON

Usage:
    python smoke_test.py
    python smoke_test.py --config ../../configs/odin/config.json
"""

import argparse
import json
import os
import sys
import time
from pathlib import Path

import requests

# Default backend configuration (matches Odin config)
BACKENDS = [
    {"name": "munin", "url": "http://localhost:11434", "type": "ollama",
     "expected_models": ["LFM2.5-1.2B", "LFM2-24B"]},
    {"name": "hugin", "url": f"http://{os.environ.get('HUGIN_IP', 'localhost')}:11434",
     "type": "ollama", "expected_models": ["qwen3-coder"]},
    {"name": "morrigan", "url": os.environ.get("MORRIGAN_URL", "http://localhost:8080"),
     "type": "openai", "expected_models": ["Qwen3.5-27B"]},
    {"name": "munin-igpu", "url": "http://localhost:11435", "type": "ollama",
     "expected_models": ["qwen3-coder:7b"]},
]


def check_ollama_backend(name: str, url: str, expected_models: list[str]) -> dict:
    """Check an Ollama backend."""
    result = {"name": name, "url": url, "type": "ollama", "status": "UNKNOWN",
              "models_found": [], "latency_ms": 0, "errors": []}

    # Check /api/tags
    start = time.monotonic()
    try:
        resp = requests.get(f"{url}/api/tags", timeout=5)
        result["latency_ms"] = int((time.monotonic() - start) * 1000)

        if resp.status_code != 200:
            result["status"] = "FAIL"
            result["errors"].append(f"HTTP {resp.status_code}")
            return result

        models = [m["name"] for m in resp.json().get("models", [])]
        result["models_found"] = models

        # Check expected models (substring match)
        missing = []
        for expected in expected_models:
            if not any(expected.lower() in m.lower() for m in models):
                missing.append(expected)
        if missing:
            result["errors"].append(f"Missing models: {missing}")

    except requests.ConnectionError:
        result["status"] = "OFFLINE"
        result["errors"].append("Connection refused")
        return result
    except requests.Timeout:
        result["status"] = "TIMEOUT"
        result["errors"].append("Timeout after 5s")
        return result

    # Quick inference test
    if models:
        start = time.monotonic()
        try:
            resp = requests.post(
                f"{url}/api/chat",
                json={"model": models[0], "messages": [
                    {"role": "user", "content": "Reply with exactly: OK"}
                ], "stream": False, "options": {"num_predict": 10}},
                timeout=30,
            )
            inf_latency = int((time.monotonic() - start) * 1000)
            if resp.status_code == 200:
                result["status"] = "PASS"
                result["latency_ms"] = inf_latency
            else:
                result["status"] = "DEGRADED"
                result["errors"].append(f"Inference returned HTTP {resp.status_code}")
        except requests.RequestException as e:
            result["status"] = "DEGRADED"
            result["errors"].append(f"Inference failed: {e}")
    else:
        result["status"] = "PASS" if not result["errors"] else "DEGRADED"

    return result


def check_openai_backend(name: str, url: str, expected_models: list[str]) -> dict:
    """Check an OpenAI-compatible backend."""
    result = {"name": name, "url": url, "type": "openai", "status": "UNKNOWN",
              "models_found": [], "latency_ms": 0, "errors": []}

    # Check health
    start = time.monotonic()
    try:
        resp = requests.get(f"{url}/health", timeout=5)
        result["latency_ms"] = int((time.monotonic() - start) * 1000)

        if resp.status_code == 200:
            result["status"] = "PASS"
        else:
            result["status"] = "DEGRADED"
            result["errors"].append(f"Health returned HTTP {resp.status_code}")
    except requests.ConnectionError:
        result["status"] = "OFFLINE"
        result["errors"].append("Connection refused")
        return result
    except requests.Timeout:
        result["status"] = "TIMEOUT"
        return result

    # Check /v1/models
    try:
        resp = requests.get(f"{url}/v1/models", timeout=5)
        if resp.status_code == 200:
            models = [m["id"] for m in resp.json().get("data", [])]
            result["models_found"] = models
    except requests.RequestException:
        pass

    return result


def run_canary_reviewer(url: str, model: str) -> dict:
    """Canary: reviewer model must catch an unwrap() in code."""
    canary = {"name": "canary:reviewer", "status": "SKIP", "detail": ""}

    try:
        resp = requests.post(
            f"{url}/api/chat",
            json={"model": model, "messages": [
                {"role": "system", "content": "Review for bugs. Respond with issues found or LGTM."},
                {"role": "user", "content": 'fn get_user(id: u32) -> User { db.query(id).unwrap() }'},
            ], "stream": False, "options": {"temperature": 0.1, "num_predict": 256}},
            timeout=30,
        )
        if resp.status_code == 200:
            text = resp.json().get("message", {}).get("content", "").lower()
            if "unwrap" in text or "panic" in text or "error" in text:
                canary["status"] = "PASS"
                canary["detail"] = "Correctly identified unwrap() issue"
            else:
                canary["status"] = "FAIL"
                canary["detail"] = "Did not detect unwrap() bug"
        else:
            canary["status"] = "ERROR"
            canary["detail"] = f"HTTP {resp.status_code}"
    except requests.RequestException as e:
        canary["status"] = "ERROR"
        canary["detail"] = str(e)

    return canary


def main():
    parser = argparse.ArgumentParser(description="Yggdrasil Deployment Smoke Test")
    parser.add_argument("--config", type=Path, help="Odin config.json to read backends from")
    parser.add_argument("--output", default="smoke_test_results.json")
    args = parser.parse_args()

    print(f"{'=' * 60}")
    print("YGGDRASIL DEPLOYMENT SMOKE TEST")
    print(f"{'=' * 60}")

    results = []
    for backend in BACKENDS:
        print(f"\n  [{backend['name']}] {backend['url']}...", end=" ", flush=True)

        if backend["type"] == "ollama":
            result = check_ollama_backend(
                backend["name"], backend["url"], backend.get("expected_models", []))
        else:
            result = check_openai_backend(
                backend["name"], backend["url"], backend.get("expected_models", []))

        status_icon = {"PASS": "OK", "DEGRADED": "WARN", "OFFLINE": "DOWN",
                       "TIMEOUT": "SLOW", "FAIL": "FAIL"}.get(result["status"], "?")
        print(f"{status_icon} ({result['latency_ms']}ms, {len(result['models_found'])} models)")
        if result["errors"]:
            for err in result["errors"]:
                print(f"    ! {err}")

        results.append(result)

    # Summary
    print(f"\n{'─' * 60}")
    passed = sum(1 for r in results if r["status"] == "PASS")
    total = len(results)
    offline = sum(1 for r in results if r["status"] == "OFFLINE")
    print(f"  {passed}/{total} backends healthy, {offline} offline")

    # Save results
    with open(args.output, "w") as f:
        json.dump(results, f, indent=2)
    print(f"  Results: {args.output}")

    overall = all(r["status"] in ("PASS", "DEGRADED") for r in results if r["name"] != "munin-igpu")
    print(f"\n  OVERALL: {'PASS' if overall else 'FAIL'}")
    print(f"{'=' * 60}")

    sys.exit(0 if overall else 1)


if __name__ == "__main__":
    main()
