#!/usr/bin/env python3
"""Yggdrasil Code Generation Benchmark.

Tests LLM code generation quality on 6 Yggdrasil-specific Rust tasks.
Each task provides a prompt with context and scores the response on:
  - Structure (valid Rust syntax via rustfmt check)
  - Pattern adherence (regex checks for expected idioms)
  - Completeness (expected elements present)
  - Latency (tokens per second)

Usage:
    python code_bench.py --model qwen3-coder:30b-a3b --ollama-url http://localhost:11434
    python code_bench.py --model Qwen3.5-27B --backend openai --url http://${MORRIGAN_URL}
    python code_bench.py --all-models  # run full matrix on all configured backends

Output: JSON results file + console summary table.
"""

import argparse
import json
import re
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Optional

import requests

# ─────────────────────────────────────────────────────────────────
# Benchmark Tasks — Yggdrasil-specific code generation
# ─────────────────────────────────────────────────────────────────

SYSTEM_PROMPT = """You are a Rust code generator for the Yggdrasil AI homelab project.
Generate ONLY valid Rust code. No explanations, no markdown fences, no commentary.
Follow Yggdrasil conventions: axum handlers, serde structs, thiserror enums,
tokio::test, tracing instrumentation."""

TASKS = [
    {
        "id": "axum_handler",
        "name": "Axum Handler",
        "prompt": """Generate an axum handler function for a new GET /api/v1/system/uptime endpoint.

Context — existing Yggdrasil handler pattern:
```rust
use axum::{extract::State, http::StatusCode, Json};
use serde::Serialize;
use crate::state::AppState;

#[derive(Serialize)]
pub struct UptimeResponse {
    uptime_secs: u64,
    node_name: String,
}
```

The handler should:
1. Take State<AppState> as extractor
2. Compute uptime from state.start_time (a std::time::Instant)
3. Return Json<UptimeResponse> with the node name from state.config.node_name
4. Use #[instrument(skip(state))] attribute for tracing""",
        "checks": {
            "has_state_extractor": r"State\s*<\s*AppState\s*>",
            "has_json_return": r"Json\s*<\s*UptimeResponse\s*>",
            "has_instrument": r"#\[instrument",
            "has_serialize_derive": r"#\[derive\(.*Serialize",
            "has_elapsed": r"elapsed\(\)|duration_since",
        },
    },
    {
        "id": "config_struct",
        "name": "Config Struct",
        "prompt": """Generate a config struct for a new notification service in Yggdrasil.

Context — existing Yggdrasil config pattern:
```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
    #[serde(default = "default_session_ttl")]
    pub session_ttl_secs: u64,
}

fn default_max_sessions() -> usize { 256 }
fn default_session_ttl() -> u64 { 3600 }
```

Generate NotificationConfig with fields:
- enabled: bool (default false)
- webhook_url: Option<String>
- retry_count: u32 (default 3)
- timeout_secs: u64 (default 10)
- channels: Vec<String> (default empty vec)

Include all serde defaults and default functions.""",
        "checks": {
            "has_derive": r"#\[derive\(.*Debug.*Clone.*Serialize.*Deserialize",
            "has_serde_default": r'#\[serde\(default\s*=\s*"default_',
            "has_option_type": r"Option<String>",
            "has_vec_type": r"Vec<String>",
            "has_default_fns": r"fn default_\w+\(\)",
        },
    },
    {
        "id": "test_generation",
        "name": "Test Generation",
        "prompt": """Generate tests for this function using Yggdrasil's testing conventions.

Function to test:
```rust
pub fn expand_env_vars(input: &str) -> String {
    let re = regex::Regex::new(r"\\$\\{([A-Z_][A-Z0-9_]*)\\}").unwrap();
    re.replace_all(input, |caps: &regex::Captures| {
        std::env::var(&caps[1]).unwrap_or_else(|_| caps[0].to_string())
    }).to_string()
}
```

Yggdrasil test conventions:
- Use #[cfg(test)] mod tests { ... }
- Use #[test] (not #[tokio::test] — this is sync)
- Test: basic substitution, missing var leaves ${VAR} intact, multiple vars, no vars passthrough
- Use descriptive test names with snake_case""",
        "checks": {
            "has_cfg_test": r"#\[cfg\(test\)\]",
            "has_mod_tests": r"mod tests",
            "has_test_attr": r"#\[test\]",
            "has_assert": r"assert(_eq)?!",
            "min_test_count": r"fn test_\w+",  # at least 3 tests
        },
    },
    {
        "id": "error_variant",
        "name": "Error Variant",
        "prompt": """Add a new error variant to this thiserror enum and implement From.

Existing error enum:
```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OdinError {
    #[error("backend unavailable: {0}")]
    BackendUnavailable(String),
    #[error("request timeout after {0}s")]
    Timeout(u64),
    #[error("model not found: {0}")]
    ModelNotFound(String),
}
```

Add:
1. A ConfigError variant that wraps ygg_config::ConfigError
2. A RateLimited variant with retry_after_secs: u64
3. Implement From<ygg_config::ConfigError> for OdinError
4. Implement From<reqwest::Error> for OdinError (mapped to BackendUnavailable)""",
        "checks": {
            "has_config_error": r"ConfigError",
            "has_rate_limited": r"RateLimited",
            "has_from_impl": r"impl From<",
            "has_thiserror": r"#\[error\(",
            "has_retry_after": r"retry_after",
        },
    },
    {
        "id": "tool_registration",
        "name": "Tool Registration",
        "prompt": """Generate a tool specification for the Odin tool registry.

Context — existing Yggdrasil tool registry pattern:
```rust
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub tier: ToolTier,
    pub endpoint: ToolEndpoint,
    pub schema: serde_json::Value,
    pub keywords: &'static [&'static str],
}

pub enum ToolTier { Safe, Restricted, Blocked }
pub enum ToolEndpoint { Mimir, Muninn, OdinSelf, Ha(HaToolKind) }
```

Generate a ToolSpec for a new "deploy_status_tool" that:
- Checks deployment status across nodes (Safe tier)
- Takes parameters: node_name (string, required) and service_name (string, optional)
- Returns JSON with status, version, uptime
- Dispatches to OdinSelf endpoint
- Has keywords: ["deploy", "status", "service", "version", "node"]
- Include the JSON Schema for parameters""",
        "checks": {
            "has_tool_spec": r"ToolSpec",
            "has_safe_tier": r"ToolTier::Safe|Safe",
            "has_schema": r"serde_json::json!|\"type\":\s*\"object\"",
            "has_properties": r"properties|node_name|service_name",
            "has_keywords": r"deploy.*status|keywords",
        },
    },
    {
        "id": "yaml_extraction",
        "name": "Structured Output Parser",
        "prompt": """Generate a function that extracts YAML from an LLM response.

Context — existing Yggdrasil pattern from ygg-ha/src/automation.rs:
The LLM may return YAML inside a ```yaml code fence, or raw YAML without fences.

Generate a function with this signature:
```rust
pub fn extract_yaml(response: &str) -> Option<String>
```

Requirements:
1. Try to find ```yaml ... ``` fenced block first
2. If no fence, try to find content starting with a YAML key (line matching /^\\w+:/)
3. Trim whitespace from the extracted content
4. Return None if no YAML-like content found
5. Handle edge cases: multiple fences (take first), empty fence, no closing fence""",
        "checks": {
            "has_fn_signature": r"fn extract_yaml\s*\(\s*response:\s*&str\s*\)\s*->\s*Option<String>",
            "has_regex_or_find": r"find|Regex|contains|starts_with",
            "has_none_return": r"None",
            "has_some_return": r"Some\(",
            "has_trim": r"trim",
        },
    },
]


# ─────────────────────────────────────────────────────────────────
# Scoring
# ─────────────────────────────────────────────────────────────────

@dataclass
class TaskResult:
    task_id: str
    task_name: str
    model: str
    passed_checks: dict[str, bool] = field(default_factory=dict)
    syntax_valid: bool = False
    check_score: float = 0.0  # 0.0-1.0
    total_score: float = 0.0  # weighted final
    tokens_generated: int = 0
    latency_ms: int = 0
    tok_per_sec: float = 0.0
    response: str = ""
    error: str = ""


def check_rust_syntax(code: str) -> bool:
    """Check if code is valid Rust syntax via rustfmt."""
    # Strip markdown fences if present
    code = re.sub(r"^```\w*\n?", "", code, flags=re.MULTILINE)
    code = re.sub(r"\n?```\s*$", "", code, flags=re.MULTILINE)

    with tempfile.NamedTemporaryFile(suffix=".rs", mode="w", delete=False) as f:
        # Wrap in a module if it doesn't have fn main or mod
        if "fn main" not in code and "mod " not in code and "#[cfg(test)]" not in code:
            f.write("// Auto-wrapped for syntax check\n")
            f.write("#![allow(unused, dead_code)]\n")
            f.write("use serde::{Serialize, Deserialize};\n\n")
            f.write(code)
        else:
            f.write(code)
        f.flush()

        try:
            result = subprocess.run(
                ["rustfmt", "--check", f.name],
                capture_output=True, text=True, timeout=10,
            )
            # rustfmt --check returns 0 if formatted, 1 if needs formatting
            # Both mean the syntax is valid. Only real errors return 2+
            return result.returncode in (0, 1)
        except (subprocess.TimeoutExpired, FileNotFoundError):
            return False
        finally:
            Path(f.name).unlink(missing_ok=True)


def score_task(task: dict, response: str) -> tuple[dict[str, bool], float]:
    """Score a response against task checks. Returns (check_results, score)."""
    checks = task["checks"]
    results = {}

    for check_name, pattern in checks.items():
        if check_name == "min_test_count":
            # Special: count matches, need at least 3
            matches = re.findall(pattern, response)
            results[check_name] = len(matches) >= 3
        else:
            results[check_name] = bool(re.search(pattern, response, re.DOTALL))

    passed = sum(1 for v in results.values() if v)
    score = passed / len(results) if results else 0.0
    return results, score


# ─────────────────────────────────────────────────────────────────
# Model interaction
# ─────────────────────────────────────────────────────────────────

def query_ollama(url: str, model: str, prompt: str, timeout: int = 120) -> tuple[str, int, float]:
    """Query Ollama API. Returns (response_text, token_count, latency_ms)."""
    start = time.monotonic()
    try:
        resp = requests.post(
            f"{url}/api/chat",
            json={
                "model": model,
                "messages": [
                    {"role": "system", "content": SYSTEM_PROMPT},
                    {"role": "user", "content": prompt},
                ],
                "stream": False,
                "options": {"temperature": 0.1, "num_predict": 2048},
            },
            timeout=timeout,
        )
        latency = (time.monotonic() - start) * 1000
        if resp.status_code != 200:
            return f"HTTP {resp.status_code}: {resp.text[:200]}", 0, latency

        data = resp.json()
        text = data.get("message", {}).get("content", "")
        tokens = data.get("eval_count", len(text.split()))
        return text, tokens, latency
    except requests.RequestException as e:
        latency = (time.monotonic() - start) * 1000
        return f"Error: {e}", 0, latency


def query_openai(url: str, model: str, prompt: str, timeout: int = 120) -> tuple[str, int, float]:
    """Query OpenAI-compatible API. Returns (response_text, token_count, latency_ms)."""
    start = time.monotonic()
    try:
        resp = requests.post(
            f"{url}/v1/chat/completions",
            json={
                "model": model,
                "messages": [
                    {"role": "system", "content": SYSTEM_PROMPT},
                    {"role": "user", "content": prompt},
                ],
                "temperature": 0.1,
                "max_tokens": 2048,
            },
            timeout=timeout,
        )
        latency = (time.monotonic() - start) * 1000
        if resp.status_code != 200:
            return f"HTTP {resp.status_code}: {resp.text[:200]}", 0, latency

        data = resp.json()
        text = data["choices"][0]["message"]["content"]
        tokens = data.get("usage", {}).get("completion_tokens", len(text.split()))
        return text, tokens, latency
    except requests.RequestException as e:
        latency = (time.monotonic() - start) * 1000
        return f"Error: {e}", 0, latency


# ─────────────────────────────────────────────────────────────────
# Benchmark runner
# ─────────────────────────────────────────────────────────────────

def run_benchmark(
    model: str,
    url: str,
    backend_type: str = "ollama",
    tasks: list[dict] | None = None,
) -> list[TaskResult]:
    """Run all tasks against a model. Returns list of TaskResult."""
    tasks = tasks or TASKS
    query_fn = query_openai if backend_type == "openai" else query_ollama
    results = []

    for task in tasks:
        print(f"  [{task['id']}] {task['name']}...", end=" ", flush=True)

        response, tokens, latency = query_fn(url, model, task["prompt"])

        if response.startswith("Error:") or response.startswith("HTTP "):
            result = TaskResult(
                task_id=task["id"],
                task_name=task["name"],
                model=model,
                error=response,
                latency_ms=int(latency),
            )
            print("ERROR")
            results.append(result)
            continue

        check_results, check_score = score_task(task, response)
        syntax_ok = check_rust_syntax(response)
        tok_per_sec = (tokens / (latency / 1000)) if latency > 0 and tokens > 0 else 0

        # Weighted score: 40% check adherence + 30% syntax + 30% completeness
        syntax_score = 1.0 if syntax_ok else 0.0
        completeness = 1.0 if check_score >= 0.8 else check_score
        total = 0.40 * check_score + 0.30 * syntax_score + 0.30 * completeness

        result = TaskResult(
            task_id=task["id"],
            task_name=task["name"],
            model=model,
            passed_checks=check_results,
            syntax_valid=syntax_ok,
            check_score=round(check_score, 3),
            total_score=round(total, 3),
            tokens_generated=tokens,
            latency_ms=int(latency),
            tok_per_sec=round(tok_per_sec, 1),
            response=response[:2000],
        )

        status = "PASS" if total >= 0.7 else "PARTIAL" if total >= 0.4 else "FAIL"
        print(f"{status} (score={total:.2f}, syntax={'OK' if syntax_ok else 'BAD'}, "
              f"{tokens}tok, {tok_per_sec:.1f}tok/s)")
        results.append(result)

    return results


def print_summary(all_results: dict[str, list[TaskResult]]):
    """Print comparison table across models."""
    print(f"\n{'=' * 80}")
    print("CODE GENERATION BENCHMARK RESULTS")
    print(f"{'=' * 80}")

    models = list(all_results.keys())
    tasks = TASKS

    # Header
    header = f"{'Task':<25}"
    for m in models:
        short = m.split(":")[0][-20:]
        header += f" {short:>15}"
    print(header)
    print("-" * len(header))

    # Per-task scores
    for task in tasks:
        row = f"{task['name']:<25}"
        for m in models:
            results = all_results[m]
            match = next((r for r in results if r.task_id == task["id"]), None)
            if match:
                score = f"{match.total_score:.2f}"
                if match.error:
                    score = "ERR"
            else:
                score = "N/A"
            row += f" {score:>15}"
        print(row)

    # Average scores
    print("-" * len(header))
    row = f"{'AVERAGE':<25}"
    for m in models:
        results = all_results[m]
        scores = [r.total_score for r in results if not r.error]
        avg = sum(scores) / len(scores) if scores else 0
        row += f" {avg:>15.3f}"
    print(row)

    # Tok/s
    row = f"{'avg tok/s':<25}"
    for m in models:
        results = all_results[m]
        rates = [r.tok_per_sec for r in results if r.tok_per_sec > 0]
        avg = sum(rates) / len(rates) if rates else 0
        row += f" {avg:>15.1f}"
    print(row)
    print(f"{'=' * 80}")


# ─────────────────────────────────────────────────────────────────
# CLI
# ─────────────────────────────────────────────────────────────────

# Pre-configured model matrix for --all-models
MODEL_MATRIX = [
    {"model": "qwen3-coder:30b-a3b-q4_K_M", "url": "http://${HUGIN_IP}:11434", "backend": "ollama"},
    {"model": "qwen3-coder:7b", "url": "http://localhost:11434", "backend": "ollama"},
    {"model": "hf.co/LiquidAI/LFM2-8B-A1B-GGUF:Q4_K_M", "url": "http://localhost:11434", "backend": "ollama"},
    {"model": "hf.co/LiquidAI/LFM2-2.6B-Exp-GGUF:Q4_K_M", "url": "http://localhost:11434", "backend": "ollama"},
    {"model": "hf.co/LiquidAI/LFM2.5-1.2B-Instruct-GGUF:Q4_K_M", "url": "http://localhost:11434", "backend": "ollama"},
    {"model": "Qwen3.5-27B-Q4_K_M.gguf", "url": "http://${MORRIGAN_URL}", "backend": "openai"},
]


def main():
    parser = argparse.ArgumentParser(description="Yggdrasil Code Generation Benchmark")
    parser.add_argument("--model", help="Model name for Ollama/OpenAI API")
    parser.add_argument("--url", default="http://localhost:11434", help="API base URL")
    parser.add_argument("--backend", default="ollama", choices=["ollama", "openai"])
    parser.add_argument("--all-models", action="store_true", help="Run full model matrix")
    parser.add_argument("--output", default="code_bench_results.json", help="Output JSON file")
    parser.add_argument("--task", help="Run a single task by ID")
    args = parser.parse_args()

    if not args.model and not args.all_models:
        parser.error("Either --model or --all-models is required")

    all_results: dict[str, list[TaskResult]] = {}
    tasks = TASKS
    if args.task:
        tasks = [t for t in TASKS if t["id"] == args.task]
        if not tasks:
            parser.error(f"Unknown task: {args.task}. Available: {[t['id'] for t in TASKS]}")

    if args.all_models:
        import os
        for entry in MODEL_MATRIX:
            url = entry["url"].replace("${HUGIN_IP}", os.environ.get("HUGIN_IP", "localhost"))
            url = url.replace("${MORRIGAN_URL}", os.environ.get("MORRIGAN_URL", "localhost:8080"))
            model = entry["model"]
            print(f"\n{'─' * 60}")
            print(f"Model: {model}")
            print(f"URL:   {url} ({entry['backend']})")
            print(f"{'─' * 60}")
            results = run_benchmark(model, url, entry["backend"], tasks)
            all_results[model] = results
    else:
        print(f"\nModel: {args.model}")
        print(f"URL:   {args.url} ({args.backend})")
        print(f"{'─' * 60}")
        results = run_benchmark(args.model, args.url, args.backend, tasks)
        all_results[args.model] = results

    # Save results
    output_path = Path(args.output)
    serializable = {
        model: [asdict(r) for r in results]
        for model, results in all_results.items()
    }
    with open(output_path, "w") as f:
        json.dump(serializable, f, indent=2, default=str)
    print(f"\nResults saved to {output_path}")

    # Print summary
    print_summary(all_results)


if __name__ == "__main__":
    main()
