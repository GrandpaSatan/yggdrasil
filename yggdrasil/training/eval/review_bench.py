#!/usr/bin/env python3
"""Yggdrasil Code Review Benchmark.

Tests whether a model can identify issues in Rust code — the review agent role.
This tests PATTERN RECOGNITION, not generation.  LFM models may excel here
because recognizing violations is easier than generating correct code.

Each task provides code with (or without) an intentional issue and scores:
  - Correct identification (did it catch the bug?)
  - False positive rate (does it flag correct code?)
  - Explanation quality (is the feedback actionable?)

Usage:
    python review_bench.py --model LFM2-2.6B-Exp --ollama-url http://localhost:11434
"""

import argparse
import json
import re
import time
from dataclasses import dataclass, field, asdict
from pathlib import Path

import requests

SYSTEM_PROMPT = """You are a Rust code reviewer for the Yggdrasil AI homelab project.
Review the code for: bugs, convention violations, security issues, hardcoded values.
Respond with a JSON object: {"issues": [{"severity": "high|medium|low", "description": "..."}], "verdict": "LGTM|NEEDS_FIXES"}
If the code is correct, return {"issues": [], "verdict": "LGTM"}.
Respond ONLY with valid JSON. No explanations outside the JSON."""

TASKS = [
    {
        "id": "missing_error_variant",
        "name": "Missing Error Variant",
        "has_issue": True,
        "expected_detection": "missing error handling or variant for timeout",
        "code": """// Review this Odin handler code:
use axum::{extract::State, Json};

pub async fn health_check(State(state): State<AppState>) -> Json<serde_json::Value> {
    let pg_ok = sqlx::query("SELECT 1")
        .fetch_one(&state.pool)
        .await
        .is_ok();

    // BUG: If Qdrant check fails, this unwrap panics the handler
    let qdrant_ok = state.qdrant_client
        .collection_info("engrams")
        .await
        .unwrap()
        .status == CollectionStatus::Green;

    Json(serde_json::json!({"postgres": pg_ok, "qdrant": qdrant_ok}))
}""",
        "detection_patterns": [r"unwrap|panic|error.handl|could.crash|fail|should.handle"],
    },
    {
        "id": "hardcoded_ip",
        "name": "Hardcoded IP Address",
        "has_issue": True,
        "expected_detection": "hardcoded IP instead of env var or config",
        "code": """// Review this deployment config:
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployConfig {
    pub target_nodes: Vec<NodeConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    pub name: String,
    pub ip: String,
    pub port: u16,
}

impl Default for DeployConfig {
    fn default() -> Self {
        Self {
            target_nodes: vec![
                NodeConfig { name: "munin".into(), ip: "192.168.1.100".into(), port: 8080 },
                NodeConfig { name: "hugin".into(), ip: "192.168.1.101".into(), port: 8080 },
            ],
        }
    }
}""",
        "detection_patterns": [r"hardcod|ip.*address|10\.0\.65|env.var|config|should.not.*default|literal"],
    },
    {
        "id": "naming_convention",
        "name": "Naming Convention Violation",
        "has_issue": True,
        "expected_detection": "function name doesn't follow Rust snake_case convention",
        "code": """// Review this Yggdrasil utility module:
use tracing::info;

pub struct SessionManager {
    sessions: std::collections::HashMap<String, Session>,
    max_sessions: usize,
}

impl SessionManager {
    pub fn new(max_sessions: usize) -> Self {
        Self { sessions: std::collections::HashMap::new(), max_sessions }
    }

    // Should be snake_case per Rust conventions
    pub fn getActiveSessionCount(&self) -> usize {
        self.sessions.values().filter(|s| s.is_active()).count()
    }

    pub fn removeExpired(&mut self) {
        self.sessions.retain(|_, s| !s.is_expired());
        info!(remaining = self.sessions.len(), "cleaned expired sessions");
    }
}""",
        "detection_patterns": [r"snake.case|camelCase|naming|convention|getActive|removeExpired|should.be"],
    },
    {
        "id": "handler_pattern_violation",
        "name": "Handler Pattern Violation",
        "has_issue": True,
        "expected_detection": "handler directly calls Ollama instead of going through proxy module",
        "code": """// Review this Odin handler:
// Boundary rule: Only `proxy` communicates with Ollama.
use axum::{extract::State, Json};
use reqwest::Client;

pub async fn generate_summary(
    State(state): State<AppState>,
    Json(req): Json<SummaryRequest>,
) -> Result<Json<SummaryResponse>, StatusCode> {
    // Direct Ollama call violates the proxy boundary rule
    let resp = state.http_client
        .post("http://localhost:11434/api/chat")
        .json(&serde_json::json!({
            "model": "qwen3-coder:30b",
            "messages": [{"role": "user", "content": req.text}],
            "stream": false,
        }))
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    let body: serde_json::Value = resp.json().await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(SummaryResponse {
        summary: body["message"]["content"].as_str().unwrap_or("").to_string(),
    }))
}""",
        "detection_patterns": [r"direct|proxy|boundary|should.use|module|violat|ollama.*handler|separation"],
    },
    {
        "id": "correct_code",
        "name": "Correct Code (False Positive Test)",
        "has_issue": False,
        "expected_detection": "none — code is correct",
        "code": """// Review this Yggdrasil config struct:
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    /// Whether to expose Prometheus metrics endpoint.
    #[serde(default)]
    pub enabled: bool,
    /// Port for the metrics HTTP server.
    #[serde(default = "default_metrics_port")]
    pub port: u16,
    /// Histogram buckets for request duration in seconds.
    #[serde(default = "default_duration_buckets")]
    pub duration_buckets: Vec<f64>,
}

fn default_metrics_port() -> u16 { 9099 }

fn default_duration_buckets() -> Vec<f64> {
    vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]
}

impl MetricsConfig {
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn listen_addr(&self) -> String {
        format!("0.0.0.0:{}", self.port)
    }
}""",
        "detection_patterns": [],  # Should NOT find issues
    },
]


@dataclass
class ReviewResult:
    task_id: str
    task_name: str
    model: str
    has_issue: bool
    detected_issue: bool = False
    correct: bool = False
    valid_json: bool = False
    verdict: str = ""
    issues_found: int = 0
    latency_ms: int = 0
    tok_per_sec: float = 0.0
    response: str = ""
    error: str = ""


def query_model(url: str, model: str, code: str, backend: str = "ollama",
                timeout: int = 60) -> tuple[str, int, float]:
    """Query model for code review. Returns (text, tokens, latency_ms)."""
    prompt = f"Review the following Rust code for bugs, convention violations, and security issues:\n\n{code}"
    start = time.monotonic()

    try:
        if backend == "openai":
            resp = requests.post(
                f"{url}/v1/chat/completions",
                json={"model": model, "messages": [
                    {"role": "system", "content": SYSTEM_PROMPT},
                    {"role": "user", "content": prompt},
                ], "temperature": 0.1, "max_tokens": 512},
                timeout=timeout,
            )
        else:
            resp = requests.post(
                f"{url}/api/chat",
                json={"model": model, "messages": [
                    {"role": "system", "content": SYSTEM_PROMPT},
                    {"role": "user", "content": prompt},
                ], "stream": False, "think": False,
                "options": {"temperature": 0.1, "num_predict": 512}},
                timeout=timeout,
            )

        latency = (time.monotonic() - start) * 1000
        if resp.status_code != 200:
            return f"HTTP {resp.status_code}", 0, latency

        data = resp.json()
        if backend == "openai":
            msg = data["choices"][0]["message"]
            text = msg.get("content") or msg.get("reasoning_content") or ""
            tokens = data.get("usage", {}).get("completion_tokens", 0)
        else:
            msg = data.get("message", {})
            text = msg.get("content") or msg.get("thinking") or ""
            tokens = data.get("eval_count", 0)
        return text, tokens, latency
    except requests.RequestException as e:
        return f"Error: {e}", 0, (time.monotonic() - start) * 1000


def parse_review_response(text: str) -> tuple[bool, str, list, bool]:
    """Parse JSON review response. Returns (valid_json, verdict, issues, detected_issue)."""
    # Strip markdown fences
    text = re.sub(r"^```\w*\n?", "", text.strip(), flags=re.MULTILINE)
    text = re.sub(r"\n?```\s*$", "", text, flags=re.MULTILINE)

    # Try to extract JSON object
    match = re.search(r'\{[\s\S]*\}', text)
    if not match:
        return False, "", [], False

    try:
        data = json.loads(match.group())
        verdict = data.get("verdict", "")
        issues = data.get("issues", [])
        detected = verdict == "NEEDS_FIXES" or len(issues) > 0
        return True, verdict, issues, detected
    except json.JSONDecodeError:
        return False, "", [], False


def run_benchmark(model: str, url: str, backend: str = "ollama") -> list[ReviewResult]:
    results = []

    for task in TASKS:
        print(f"  [{task['id']}] {task['name']}...", end=" ", flush=True)

        text, tokens, latency = query_model(url, model, task["code"], backend)

        if text.startswith("Error:") or text.startswith("HTTP "):
            results.append(ReviewResult(
                task_id=task["id"], task_name=task["name"], model=model,
                has_issue=task["has_issue"], error=text, latency_ms=int(latency),
            ))
            print("ERROR")
            continue

        valid_json, verdict, issues, detected = parse_review_response(text)
        tok_per_sec = (tokens / (latency / 1000)) if latency > 0 and tokens > 0 else 0

        # For issues: check if detection matches expected patterns
        if task["has_issue"]:
            # Check if model found the specific issue
            response_lower = text.lower()
            pattern_match = any(
                re.search(p, response_lower)
                for p in task["detection_patterns"]
            )
            correct = detected and pattern_match
        else:
            # No issue — correct if model said LGTM
            correct = not detected

        result = ReviewResult(
            task_id=task["id"],
            task_name=task["name"],
            model=model,
            has_issue=task["has_issue"],
            detected_issue=detected,
            correct=correct,
            valid_json=valid_json,
            verdict=verdict,
            issues_found=len(issues),
            latency_ms=int(latency),
            tok_per_sec=round(tok_per_sec, 1),
            response=text[:1500],
        )

        status = "CORRECT" if correct else "WRONG"
        detail = f"detected={detected}" if task["has_issue"] else f"false_positive={detected}"
        print(f"{status} ({detail}, json={valid_json}, {tok_per_sec:.1f}tok/s)")
        results.append(result)

    return results


def print_summary(all_results: dict[str, list[ReviewResult]]):
    print(f"\n{'=' * 80}")
    print("CODE REVIEW BENCHMARK RESULTS")
    print(f"{'=' * 80}")

    models = list(all_results.keys())

    header = f"{'Task':<30} {'Issue?':<8}"
    for m in models:
        short = m.split(":")[0][-15:]
        header += f" {short:>15}"
    print(header)
    print("-" * len(header))

    for task in TASKS:
        row = f"{task['name']:<30} {'YES' if task['has_issue'] else 'NO':<8}"
        for m in models:
            results = all_results[m]
            match = next((r for r in results if r.task_id == task["id"]), None)
            if match:
                symbol = "CORRECT" if match.correct else ("MISSED" if task["has_issue"] else "FP")
                if match.error:
                    symbol = "ERR"
            else:
                symbol = "N/A"
            row += f" {symbol:>15}"
        print(row)

    print("-" * len(header))
    for m in models:
        results = all_results[m]
        correct = sum(1 for r in results if r.correct and not r.error)
        total = sum(1 for r in results if not r.error)
        tp = sum(1 for r in results if r.has_issue and r.detected_issue and not r.error)
        fn = sum(1 for r in results if r.has_issue and not r.detected_issue and not r.error)
        fp = sum(1 for r in results if not r.has_issue and r.detected_issue and not r.error)
        tn = sum(1 for r in results if not r.has_issue and not r.detected_issue and not r.error)

        short = m.split(":")[0][-25:]
        print(f"\n  {short}:")
        print(f"    Accuracy: {correct}/{total} ({correct/total*100:.0f}%)" if total else "    No results")
        print(f"    True Pos: {tp}  False Neg: {fn}  False Pos: {fp}  True Neg: {tn}")
        json_valid = sum(1 for r in results if r.valid_json and not r.error)
        print(f"    JSON valid: {json_valid}/{total}")
    print(f"{'=' * 80}")


def main():
    parser = argparse.ArgumentParser(description="Yggdrasil Code Review Benchmark")
    parser.add_argument("--model", required=True, help="Model name")
    parser.add_argument("--url", default="http://localhost:11434", help="API base URL")
    parser.add_argument("--backend", default="ollama", choices=["ollama", "openai"])
    parser.add_argument("--output", default="review_bench_results.json")
    args = parser.parse_args()

    print(f"\nModel: {args.model}")
    print(f"URL:   {args.url} ({args.backend})")
    print(f"{'─' * 60}")

    results = run_benchmark(args.model, args.url, args.backend)
    all_results = {args.model: results}

    output_path = Path(args.output)
    with open(output_path, "w") as f:
        json.dump({m: [asdict(r) for r in rs] for m, rs in all_results.items()}, f, indent=2)
    print(f"\nResults saved to {output_path}")

    print_summary(all_results)


if __name__ == "__main__":
    main()
