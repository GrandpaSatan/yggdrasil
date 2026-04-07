#!/usr/bin/env python3
"""Standalone math evaluation script.

Evaluates exact-match accuracy on a fixed math problem set. Works with:
  1. Local HuggingFace model directory (full fine-tuned)
  2. Remote llama-server endpoint (for GGUF exports)

Usage:
    # Evaluate a local model
    python3 eval_math.py --model ./output-forced-grok/forced-wd04/model --gpu 0

    # Evaluate via llama-server (GGUF)
    python3 eval_math.py --endpoint http://localhost:8080/v1/chat/completions

    # Custom eval set size
    python3 eval_math.py --model ./model --n-problems 1000 --gpu 0

    # Only OOD problems (4-5 digit, harder than training)
    python3 eval_math.py --model ./model --ood-only --gpu 0
"""

import argparse
import json
import sys
import time
from pathlib import Path
from typing import Optional

import requests

sys.path.insert(0, str(Path(__file__).resolve().parent))
from math_data import generate_eval_set


def eval_local(model_path: str, problems: list[dict], gpu: int = 0) -> list[dict]:
    """Evaluate using a local HuggingFace model."""
    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer

    print(f"Loading model from {model_path}...")
    model = AutoModelForCausalLM.from_pretrained(
        model_path,
        torch_dtype=torch.float16,
        device_map={"": gpu},
        trust_remote_code=True,
    )
    tokenizer = AutoTokenizer.from_pretrained(model_path, trust_remote_code=True)
    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token

    model.eval()
    results = []

    for i, problem in enumerate(problems):
        # Build prompt (system + user, no assistant response)
        text = problem["text"]
        marker = "<|im_start|>assistant\n"
        idx = text.rfind(marker)
        prompt = text[:idx + len(marker)] if idx >= 0 else text

        inputs = tokenizer(
            prompt, return_tensors="pt", truncation=True, max_length=256
        ).to(model.device)

        with torch.no_grad():
            outputs = model.generate(
                **inputs,
                max_new_tokens=32,
                do_sample=False,
                pad_token_id=tokenizer.pad_token_id,
            )

        generated = tokenizer.decode(
            outputs[0][inputs["input_ids"].shape[1]:],
            skip_special_tokens=True,
        ).strip()
        gen_clean = generated.split("<|im_end|>")[0].strip()

        expected = problem["answer"]
        match = gen_clean == expected

        results.append({
            "question": problem.get("question", ""),
            "expected": expected,
            "generated": gen_clean,
            "correct": match,
            "operation": problem.get("metadata", {}).get("operation", "unknown"),
            "difficulty": problem.get("metadata", {}).get("difficulty", 0),
            "ood": problem.get("ood", False),
        })

        if (i + 1) % 50 == 0:
            acc_so_far = sum(r["correct"] for r in results) / len(results) * 100
            print(f"  [{i+1}/{len(problems)}] running accuracy: {acc_so_far:.1f}%")

    del model
    return results


def eval_endpoint(endpoint: str, problems: list[dict], model_name: str = "") -> list[dict]:
    """Evaluate using a remote OpenAI-compatible endpoint (llama-server, Ollama)."""
    results = []

    for i, problem in enumerate(problems):
        msgs = problem.get("messages", [])
        # Send only system + user (no assistant)
        chat_msgs = [m for m in msgs if m["role"] != "assistant"]

        payload = {
            "messages": chat_msgs,
            "max_tokens": 32,
            "temperature": 0,
            "stream": False,
        }
        if model_name:
            payload["model"] = model_name

        try:
            resp = requests.post(endpoint, json=payload, timeout=30)
            resp.raise_for_status()
            data = resp.json()
            generated = data["choices"][0]["message"]["content"].strip()
        except Exception as e:
            generated = f"ERROR: {e}"

        expected = problem["answer"]
        match = generated == expected

        results.append({
            "question": problem.get("question", ""),
            "expected": expected,
            "generated": generated,
            "correct": match,
            "operation": problem.get("metadata", {}).get("operation", "unknown"),
            "difficulty": problem.get("metadata", {}).get("difficulty", 0),
            "ood": problem.get("ood", False),
        })

        if (i + 1) % 50 == 0:
            acc_so_far = sum(r["correct"] for r in results) / len(results) * 100
            print(f"  [{i+1}/{len(problems)}] running accuracy: {acc_so_far:.1f}%")

    return results


def print_report(results: list[dict], name: str):
    """Print a detailed accuracy report."""
    total = len(results)
    correct = sum(r["correct"] for r in results)

    # By operation
    by_op = {}
    for r in results:
        op = r["operation"]
        if op not in by_op:
            by_op[op] = {"correct": 0, "total": 0}
        by_op[op]["total"] += 1
        if r["correct"]:
            by_op[op]["correct"] += 1

    # By difficulty
    by_diff = {}
    for r in results:
        d = r["difficulty"]
        if d not in by_diff:
            by_diff[d] = {"correct": 0, "total": 0}
        by_diff[d]["total"] += 1
        if r["correct"]:
            by_diff[d]["correct"] += 1

    # ID vs OOD
    id_results = [r for r in results if not r["ood"]]
    ood_results = [r for r in results if r["ood"]]
    id_correct = sum(r["correct"] for r in id_results)
    ood_correct = sum(r["correct"] for r in ood_results)

    print(f"\n{'='*70}")
    print(f"  MATH EVAL REPORT: {name}")
    print(f"{'='*70}")
    print(f"  Overall:          {correct}/{total} = {correct/total*100:.1f}%")
    if id_results:
        print(f"  In-distribution:  {id_correct}/{len(id_results)} = "
              f"{id_correct/len(id_results)*100:.1f}%")
    if ood_results:
        print(f"  Out-of-dist:      {ood_correct}/{len(ood_results)} = "
              f"{ood_correct/len(ood_results)*100:.1f}%")

    print(f"\n  By Operation:")
    for op, d in sorted(by_op.items()):
        pct = d["correct"] / d["total"] * 100 if d["total"] else 0
        bar = "█" * int(pct / 5) + "░" * (20 - int(pct / 5))
        print(f"    {op:<14} {d['correct']:>3}/{d['total']:<3} {pct:>5.1f}% {bar}")

    print(f"\n  By Difficulty:")
    for d, counts in sorted(by_diff.items()):
        pct = counts["correct"] / counts["total"] * 100 if counts["total"] else 0
        label = {1: "easy (2-digit)", 2: "medium (2-3 digit)",
                 3: "hard (3-4 digit)", 4: "OOD (4-5 digit)"}.get(d, f"d={d}")
        print(f"    {label:<20} {counts['correct']:>3}/{counts['total']:<3} {pct:>5.1f}%")

    # Show some errors
    errors = [r for r in results if not r["correct"]]
    if errors:
        print(f"\n  Sample errors ({len(errors)} total):")
        for err in errors[:5]:
            print(f"    [{err['operation']}] Q: {err['question'][:50]}")
            print(f"      Expected: {err['expected']}  Got: {err['generated'][:50]}")
    print(f"{'='*70}\n")


def main():
    parser = argparse.ArgumentParser(description="Math Evaluation Script")
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--model", type=str, help="Local model directory")
    group.add_argument("--endpoint", type=str, help="Remote endpoint URL")
    parser.add_argument("--model-name", type=str, default="",
                        help="Model name for endpoint (if required)")
    parser.add_argument("--gpu", type=int, default=0)
    parser.add_argument("--n-problems", type=int, default=500)
    parser.add_argument("--seed", type=int, default=9999)
    parser.add_argument("--ood-only", action="store_true",
                        help="Only evaluate OOD problems")
    parser.add_argument("--output", type=str, default=None,
                        help="Save results to JSON file")
    args = parser.parse_args()

    print("Generating eval set...")
    problems = generate_eval_set(
        n=args.n_problems, seed=args.seed, include_ood=True
    )
    if args.ood_only:
        problems = [p for p in problems if p.get("ood")]
        print(f"  OOD only: {len(problems)} problems")
    else:
        n_ood = sum(1 for p in problems if p.get("ood"))
        print(f"  {len(problems)} problems ({len(problems) - n_ood} ID, {n_ood} OOD)")

    start = time.time()
    if args.model:
        results = eval_local(args.model, problems, args.gpu)
        name = Path(args.model).name
    else:
        results = eval_endpoint(args.endpoint, problems, args.model_name)
        name = args.endpoint.split("/")[-2] if "/" in args.endpoint else args.endpoint
    elapsed = time.time() - start

    print_report(results, name)
    print(f"  Evaluated {len(problems)} problems in {elapsed:.1f}s "
          f"({len(problems)/elapsed:.1f} problems/sec)")

    if args.output:
        with open(args.output, "w") as f:
            json.dump(results, f, indent=2)
        print(f"  Results saved to {args.output}")


if __name__ == "__main__":
    main()
