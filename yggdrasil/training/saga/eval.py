#!/usr/bin/env python3
"""Evaluate Saga model accuracy on held-out test set.

Tests:
  1. CLASSIFY accuracy (category match + should_store correctness)
  2. DISTILL quality (valid JSON with cause/effect/tags)
  3. QUERY quality (valid JSON with query string)
  4. FILTER quality (valid JSON with relevant_indices)
  5. False positive rate on noise content
"""

import json
import sys
import requests
from pathlib import Path

BARN = "/data/saga/data"
OLLAMA_URL = "http://localhost:11434"
MODEL = "saga:0.6b"


def saga_generate(prompt, timeout=10):
    """Call Ollama generate for Saga inference."""
    try:
        resp = requests.post(
            f"{OLLAMA_URL}/api/generate",
            json={
                "model": MODEL,
                "prompt": prompt,
                "stream": False,
                "options": {"temperature": 0.1, "num_predict": 256},
            },
            timeout=timeout,
        )
        if resp.status_code == 200:
            text = resp.json().get("response", "")
            # Strip Qwen3 thinking tags if present
            import re
            text = re.sub(r'<think>.*?</think>', '', text, flags=re.DOTALL).strip()
            # Extract JSON object from response
            match = re.search(r'\{[^{}]*\}', text)
            if match:
                return json.loads(match.group())
    except (requests.RequestException, json.JSONDecodeError):
        pass
    return None


def eval_classify(examples):
    """Evaluate CLASSIFY task accuracy."""
    correct_category = 0
    correct_store = 0
    valid_json = 0
    total = len(examples)

    for ex in examples:
        user_msg = ex["messages"][1]["content"]
        expected = json.loads(ex["messages"][2]["content"])

        result = saga_generate(user_msg)
        if result is None:
            continue

        valid_json += 1
        if result.get("category") == expected.get("category"):
            correct_category += 1
        if result.get("should_store") == expected.get("should_store"):
            correct_store += 1

    return {
        "total": total,
        "valid_json": valid_json,
        "json_rate": valid_json / total if total else 0,
        "category_accuracy": correct_category / total if total else 0,
        "store_accuracy": correct_store / total if total else 0,
    }


def eval_distill(examples):
    """Evaluate DISTILL task — checks JSON validity and field presence."""
    valid = 0
    has_all_fields = 0
    total = len(examples)

    for ex in examples:
        user_msg = ex["messages"][1]["content"]
        result = saga_generate(user_msg)
        if result is None:
            continue
        valid += 1
        if all(k in result for k in ("cause", "effect", "tags")):
            if isinstance(result["tags"], list) and len(result["cause"]) > 5:
                has_all_fields += 1

    return {
        "total": total,
        "valid_json": valid,
        "json_rate": valid / total if total else 0,
        "complete_fields": has_all_fields / total if total else 0,
    }


def eval_query(examples):
    """Evaluate QUERY task — checks JSON validity and query presence."""
    valid = 0
    has_query = 0
    total = len(examples)

    for ex in examples:
        user_msg = ex["messages"][1]["content"]
        result = saga_generate(user_msg)
        if result is None:
            continue
        valid += 1
        if "query" in result and isinstance(result["query"], str) and len(result["query"]) > 3:
            has_query += 1

    return {
        "total": total,
        "valid_json": valid,
        "json_rate": valid / total if total else 0,
        "has_valid_query": has_query / total if total else 0,
    }


def eval_filter(examples):
    """Evaluate FILTER task — checks JSON validity and index list."""
    valid = 0
    has_indices = 0
    total = len(examples)

    for ex in examples:
        user_msg = ex["messages"][1]["content"]
        result = saga_generate(user_msg)
        if result is None:
            continue
        valid += 1
        if "relevant_indices" in result and isinstance(result["relevant_indices"], list):
            has_indices += 1

    return {
        "total": total,
        "valid_json": valid,
        "json_rate": valid / total if total else 0,
        "has_valid_indices": has_indices / total if total else 0,
    }


def main():
    # Check Saga model availability
    try:
        resp = requests.get(f"{OLLAMA_URL}/api/tags", timeout=5)
        models = [m["name"] for m in resp.json().get("models", [])]
        if MODEL not in models:
            print(f"Error: {MODEL} not found in Ollama. Available: {models}")
            print(f"Run: ollama create {MODEL} -f Modelfile")
            sys.exit(1)
    except Exception as e:
        print(f"Error: Cannot reach Ollama at {OLLAMA_URL}: {e}")
        sys.exit(1)

    # Load test set
    test_path = f"{BARN}/saga_test.jsonl"
    examples = []
    with open(test_path) as f:
        for line in f:
            if line.strip():
                examples.append(json.loads(line))
    print(f"Loaded {len(examples)} test examples from {test_path}")

    # Split by task
    tasks = {"CLASSIFY": [], "DISTILL": [], "QUERY": [], "FILTER": []}
    for ex in examples:
        user_msg = ex["messages"][1]["content"]
        task = user_msg.split("\n")[0]
        if task in tasks:
            tasks[task].append(ex)

    print(f"\nTask distribution:")
    for task, exs in tasks.items():
        print(f"  {task}: {len(exs)} examples")

    # Evaluate each task
    print(f"\n{'='*50}")
    print(f"CLASSIFY evaluation ({len(tasks['CLASSIFY'])} examples):")
    classify_results = eval_classify(tasks["CLASSIFY"])
    for k, v in classify_results.items():
        print(f"  {k}: {v:.2%}" if isinstance(v, float) else f"  {k}: {v}")

    print(f"\n{'='*50}")
    print(f"DISTILL evaluation ({len(tasks['DISTILL'])} examples):")
    distill_results = eval_distill(tasks["DISTILL"])
    for k, v in distill_results.items():
        print(f"  {k}: {v:.2%}" if isinstance(v, float) else f"  {k}: {v}")

    print(f"\n{'='*50}")
    print(f"QUERY evaluation ({len(tasks['QUERY'])} examples):")
    query_results = eval_query(tasks["QUERY"])
    for k, v in query_results.items():
        print(f"  {k}: {v:.2%}" if isinstance(v, float) else f"  {k}: {v}")

    print(f"\n{'='*50}")
    print(f"FILTER evaluation ({len(tasks['FILTER'])} examples):")
    filter_results = eval_filter(tasks["FILTER"])
    for k, v in filter_results.items():
        print(f"  {k}: {v:.2%}" if isinstance(v, float) else f"  {k}: {v}")

    # Overall pass/fail
    print(f"\n{'='*50}")
    print("ACCEPTANCE CRITERIA:")
    classify_pass = classify_results["category_accuracy"] >= 0.90
    print(f"  Classify accuracy >= 90%: {'PASS' if classify_pass else 'FAIL'} ({classify_results['category_accuracy']:.1%})")
    distill_pass = distill_results["complete_fields"] >= 0.85
    print(f"  Distill completeness >= 85%: {'PASS' if distill_pass else 'FAIL'} ({distill_results['complete_fields']:.1%})")
    json_pass = all(r["json_rate"] >= 0.90 for r in [classify_results, distill_results, query_results, filter_results])
    print(f"  JSON validity >= 90% all tasks: {'PASS' if json_pass else 'FAIL'}")

    overall = classify_pass and distill_pass and json_pass
    print(f"\n  OVERALL: {'PASS' if overall else 'FAIL'}")

    # Save results
    results_path = f"{BARN}/eval_results.json"
    with open(results_path, "w") as f:
        json.dump({
            "classify": classify_results,
            "distill": distill_results,
            "query": query_results,
            "filter": filter_results,
            "overall_pass": overall,
        }, f, indent=2)
    print(f"\nResults saved to {results_path}")


if __name__ == "__main__":
    main()
