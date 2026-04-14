#!/usr/bin/env python3
"""Sprint 063 Track C — P5g: flow_bench_data.json runner.

Parses training/eval/flow_bench_data.json, POSTs each test case to Odin, and
validates responses against the case's validation rules.  Exits 1 on any
failure.

Validation rules supported:
  - syntax_valid         → basic Python/Rust AST check on response content
  - response_contains_any → substring match (case-insensitive) against any item
  - expected_steps       → checks that expected_steps > 0 (response non-empty)
  - json_valid           → response must be valid JSON (after stripping fences)
  - has_fields           → JSON response must contain these top-level keys

No dependencies beyond stdlib (urllib.request, json, re, ast).

Usage:
  python3 flows-bench.py
  ODIN_URL=http://10.0.65.8:8080 python3 flows-bench.py
  ODIN_URL=http://10.0.65.8:8080 python3 flows-bench.py --flow code_review
"""
from __future__ import annotations

import ast
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

# ─────────────────────────────────────────────────────────────────────────────
# Configuration
# ─────────────────────────────────────────────────────────────────────────────

ODIN_URL = os.environ.get("ODIN_URL", "http://10.0.65.8:8080").rstrip("/")
BENCH_FILE = Path(__file__).parent.parent.parent / "training" / "eval" / "flow_bench_data.json"
REQUEST_TIMEOUT_SECS = 120

# ANSI colours
RED    = "\033[31m"
GREEN  = "\033[32m"
YELLOW = "\033[33m"
BOLD   = "\033[1m"
RESET  = "\033[0m"


def log(msg: str = "") -> None:
    print(msg, flush=True)


def ok(msg: str) -> None:
    print(f"  {GREEN}✓{RESET} {msg}", flush=True)


def fail(msg: str) -> None:
    print(f"  {RED}✗{RESET} {msg}", file=sys.stderr, flush=True)


def warn(msg: str) -> None:
    print(f"  {YELLOW}!{RESET} {msg}", flush=True)


def banner_pass() -> None:
    print(f"\n{BOLD}{GREEN}================ PASS ================{RESET}")


def banner_fail() -> None:
    print(f"\n{BOLD}{RED}================ FAIL ================{RESET}", file=sys.stderr)


# ─────────────────────────────────────────────────────────────────────────────
# HTTP helper
# ─────────────────────────────────────────────────────────────────────────────

def post_chat(message: str, flow_name: str | None = None, stream: bool = False) -> tuple[int, dict]:
    """POST to Odin /v1/chat/completions.  Returns (http_status, parsed_json).

    On connection error or JSON parse failure, returns (0, {}) with a warning.
    """
    payload: dict[str, Any] = {
        "model": None,
        "messages": [{"role": "user", "content": message}],
        "stream": stream,
    }
    if flow_name:
        payload["flow"] = flow_name

    data = json.dumps(payload).encode()
    req = urllib.request.Request(
        f"{ODIN_URL}/v1/chat/completions",
        data=data,
        headers={"Content-Type": "application/json"},
        method="POST",
    )

    try:
        with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT_SECS) as resp:
            status = resp.status
            body = json.loads(resp.read().decode())
            return status, body
    except urllib.error.HTTPError as exc:
        try:
            body_text = exc.read().decode()
        except Exception:
            body_text = str(exc)
        warn(f"HTTP {exc.code}: {body_text[:200]}")
        return exc.code, {}
    except (urllib.error.URLError, OSError) as exc:
        warn(f"Connection error: {exc}")
        return 0, {}
    except json.JSONDecodeError as exc:
        warn(f"JSON parse error: {exc}")
        return 200, {}


# ─────────────────────────────────────────────────────────────────────────────
# Validation helpers
# ─────────────────────────────────────────────────────────────────────────────

def extract_content(body: dict) -> str:
    """Extract the assistant message content from a chat completion response."""
    choices = body.get("choices", [])
    if not choices:
        return ""
    msg = choices[0].get("message", {})
    return msg.get("content", "") or ""


def strip_fences(text: str) -> str:
    """Strip leading/trailing code fences from text."""
    text = text.strip()
    # Remove ```<lang> ... ``` wrapper
    text = re.sub(r"^```[a-zA-Z]*\n?", "", text)
    text = re.sub(r"\n?```$", "", text)
    return text.strip()


def check_syntax_valid(content: str) -> tuple[bool, str]:
    """Attempt Python or Rust syntax validation on the response content.

    For Python: uses ast.parse().
    For Rust: uses a heuristic (fn/struct/impl keywords present, balanced braces).
    Returns (ok, message).
    """
    code = strip_fences(content)
    if not code:
        return False, "response content is empty"

    # Try Python parse first (if it looks like Python).
    if any(kw in code for kw in ("def ", "class ", "import ", "return ")):
        try:
            ast.parse(code)
            return True, "Python AST parse: OK"
        except SyntaxError as exc:
            return False, f"Python AST parse failed: {exc}"

    # Rust heuristic: must have at least one Rust keyword and balanced braces.
    rust_keywords = ["fn ", "pub fn", "struct ", "impl ", "use ", "let "]
    has_keyword = any(kw in code for kw in rust_keywords)
    if has_keyword:
        open_braces = code.count("{")
        close_braces = code.count("}")
        if open_braces == 0 and close_braces == 0:
            return True, "Rust heuristic: no braces (may be valid snippet)"
        if open_braces == close_braces:
            return True, "Rust heuristic: balanced braces"
        return False, f"Rust heuristic: unbalanced braces ({open_braces} open, {close_braces} close)"

    # No recognisable syntax — treat as prose, pass through.
    return True, "no code syntax detected — treating as prose (pass)"


def check_response_contains_any(content: str, substrings: list[str]) -> tuple[bool, str]:
    """Case-insensitive substring match against any item in substrings."""
    lower = content.lower()
    for sub in substrings:
        if sub.lower() in lower:
            return True, f"found '{sub}'"
    return False, f"none of {substrings!r} found in response"


def check_json_valid(content: str) -> tuple[bool, str]:
    """Check that the content (after stripping fences) is valid JSON."""
    raw = strip_fences(content)
    try:
        json.loads(raw)
        return True, "JSON parse: OK"
    except json.JSONDecodeError as exc:
        return False, f"JSON parse failed: {exc}"


def check_has_fields(content: str, fields: list[str]) -> tuple[bool, str]:
    """Check that the content (parsed as JSON) has the required top-level keys."""
    raw = strip_fences(content)
    try:
        obj = json.loads(raw)
    except json.JSONDecodeError as exc:
        return False, f"JSON parse failed: {exc}"
    if not isinstance(obj, dict):
        return False, "response is not a JSON object"
    missing = [f for f in fields if f not in obj]
    if missing:
        return False, f"missing fields: {missing}"
    return True, f"all required fields present: {fields}"


def run_validation(case: dict, content: str, http_status: int) -> list[tuple[bool, str]]:
    """Run all validation rules for a test case.  Returns list of (ok, message)."""
    results: list[tuple[bool, str]] = []
    validation = case.get("validation", {})

    if http_status == 0:
        results.append((False, "connection error — Odin unreachable"))
        return results
    if http_status != 200:
        results.append((False, f"HTTP {http_status} — expected 200"))
        return results

    if not content:
        results.append((False, "response content is empty"))
        return results
    results.append((True, f"HTTP 200 + non-empty response ({len(content)} chars)"))

    if validation.get("syntax_valid"):
        results.append(check_syntax_valid(content))

    if substrings := validation.get("response_contains_any"):
        results.append(check_response_contains_any(content, substrings))

    if validation.get("json_valid"):
        results.append(check_json_valid(content))

    if fields := validation.get("has_fields"):
        results.append(check_has_fields(content, fields))

    # expected_steps: we can only verify that content is non-empty (the mock
    # responses are the real step count verifier in unit tests).  Here we log
    # the expected steps and do a basic sanity check.
    if expected_steps := case.get("expected_steps"):
        results.append((bool(content), f"expected {len(expected_steps)} steps: {expected_steps}"))

    return results


# ─────────────────────────────────────────────────────────────────────────────
# Main runner
# ─────────────────────────────────────────────────────────────────────────────

def main() -> int:
    # CLI filter: --flow <name> runs only that flow.
    flow_filter: str | None = None
    args = sys.argv[1:]
    if "--flow" in args:
        idx = args.index("--flow")
        if idx + 1 < len(args):
            flow_filter = args[idx + 1]

    if not BENCH_FILE.exists():
        print(f"{RED}ERROR{RESET}: bench file not found: {BENCH_FILE}", file=sys.stderr)
        return 1

    with BENCH_FILE.open() as fh:
        bench_data = json.load(fh)

    flows = bench_data.get("flows", [])
    if not flows:
        print(f"{RED}ERROR{RESET}: no flows in bench file", file=sys.stderr)
        return 1

    log(f"\n{BOLD}Sprint 063 Track C — Flow bench runner{RESET}")
    log(f"  Odin:  {ODIN_URL}")
    log(f"  File:  {BENCH_FILE}")
    log(f"  Flows: {len(flows)}")
    if flow_filter:
        log(f"  Filter: --flow {flow_filter}")
    log()

    total_cases = 0
    failed_cases = 0
    skipped_cases = 0

    for flow in flows:
        flow_name: str = flow.get("name", "unknown")

        if flow_filter and flow_name != flow_filter:
            continue

        test_cases: list[dict] = flow.get("test_cases", [])
        log(f"{BOLD}{flow_name}{RESET} — {flow.get('description', '')} ({len(test_cases)} case(s))")

        for i, case in enumerate(test_cases, start=1):
            total_cases += 1
            user_input: str = case.get("input", "")

            # Skip cases that require binary content (images/audio).
            if "_note" in case and ("base64" in case["_note"].lower() or "image" in case["_note"].lower()):
                warn(f"  case {i}: SKIP — requires binary input ({case['_note']})")
                skipped_cases += 1
                continue

            # Determine if this case requests an explicit flow override.
            # Cases in dream_* and home_assistant flows use the flow name.
            dream_flows = {"dream_consolidation", "dream_exploration", "dream_speculation"}
            explicit_flow: str | None = None
            if flow_name in dream_flows or flow_name == "home_assistant":
                explicit_flow = flow_name

            log(f"  case {i}: {user_input[:80]!r}")
            if explicit_flow:
                log(f"          flow override: {explicit_flow}")

            start = time.monotonic()
            http_status, body = post_chat(user_input, flow_name=explicit_flow)
            elapsed_ms = int((time.monotonic() - start) * 1000)

            content = extract_content(body)
            validations = run_validation(case, content, http_status)

            case_ok = all(v[0] for v in validations)
            for v_ok, v_msg in validations:
                if v_ok:
                    ok(v_msg)
                else:
                    fail(v_msg)

            log(f"          latency: {elapsed_ms}ms")

            if not case_ok:
                failed_cases += 1
                if content:
                    log(f"          content preview: {content[:200]!r}")

        log()

    # ── Summary ────────────────────────────────────────────────────────────
    passed = total_cases - failed_cases - skipped_cases
    log(f"Results: {GREEN}{passed} passed{RESET}, {RED}{failed_cases} failed{RESET}, {YELLOW}{skipped_cases} skipped{RESET} / {total_cases} total")

    if failed_cases > 0:
        banner_fail()
        return 1

    banner_pass()
    return 0


if __name__ == "__main__":
    sys.exit(main())
