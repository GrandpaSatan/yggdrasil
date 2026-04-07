#!/usr/bin/env python3
"""Infinite synthetic math data generator for forced-generalization grokking.

Two generation modes:
  1. Python-only (fallback): deterministic, fast, limited phrasing variety
  2. LLM-backed (primary): live LLM generates questions with natural language
     diversity, Python verifies every answer for correctness

The LLM mode uses a background thread to prefetch problems into a buffer,
ensuring smooth data flow even if the LLM is slower than the training loop.
Falls back to Python generation transparently when the buffer is empty.

Usage:
    from math_data import LLMMathStreamDataset, MathStreamDataset, generate_eval_set

    # LLM-backed (preferred)
    train_ds = LLMMathStreamDataset(
        endpoint="http://10.0.65.8:11434/v1/chat/completions",
        model="qwen3-coder:30b-a3b-q4_K_M",
    )

    # Python-only fallback
    train_ds = MathStreamDataset(difficulty=0, seed=42)

    # Fixed eval set (always Python-generated for reproducibility)
    eval_ds = generate_eval_set(n=500, seed=9999)

    # CLI
    python3 math_data.py --preview 20
    python3 math_data.py --preview-llm 10 --endpoint http://10.0.65.8:11434/v1/chat/completions
"""

import argparse
import json
import math
import os
import random
import re
import threading
import time
from collections import deque
from dataclasses import dataclass
from typing import Iterator, Optional

import requests
import torch
from torch.utils.data import IterableDataset


# ── Problem Generators (Python-only) ───────────────────────────

@dataclass
class MathProblem:
    question: str
    answer: str
    operation: str
    difficulty: int  # 1=easy, 2=medium, 3=hard


def _gcd(a: int, b: int) -> int:
    while b:
        a, b = b, a % b
    return a


def _lcm(a: int, b: int) -> int:
    return abs(a * b) // _gcd(a, b) if a and b else 0


_ASK_PREFIXES = [
    "What is", "Calculate", "Compute", "Find", "Evaluate",
    "Solve", "Determine", "Work out",
]

_EQUALS_SUFFIXES = ["", " =", "?", " = ?"]


def _phrase(rng: random.Random, expr: str) -> str:
    style = rng.randint(0, 2)
    if style == 0:
        return f"{rng.choice(_ASK_PREFIXES)} {expr}?"
    elif style == 1:
        return f"{expr}{rng.choice(_EQUALS_SUFFIXES)}"
    else:
        return f"{rng.choice(_ASK_PREFIXES)}: {expr}"


def _rand_operand(rng: random.Random, difficulty: int) -> int:
    ranges = {1: (2, 99), 2: (10, 999), 3: (100, 9999), 4: (1000, 99999)}
    lo, hi = ranges.get(difficulty, ranges[2])
    return rng.randint(lo, hi)


def gen_addition(rng, d):
    a, b = _rand_operand(rng, d), _rand_operand(rng, d)
    op = rng.choice(["+", "plus"])
    expr = f"{a} {op} {b}" if op == "+" else f"{a} plus {b}"
    return MathProblem(_phrase(rng, expr), str(a + b), "addition", d)


def gen_subtraction(rng, d):
    a, b = _rand_operand(rng, d), _rand_operand(rng, d)
    if rng.random() < 0.8:
        a, b = max(a, b), min(a, b)
    op = rng.choice(["-", "minus"])
    expr = f"{a} {op} {b}" if op == "-" else f"{a} minus {b}"
    return MathProblem(_phrase(rng, expr), str(a - b), "subtraction", d)


def gen_multiplication(rng, d):
    cap = min(d, 3)
    a, b = _rand_operand(rng, cap), _rand_operand(rng, max(1, cap - 1))
    op = rng.choice(["*", "×", "times"])
    expr = f"{a} times {b}" if op == "times" else f"{a} {op} {b}"
    return MathProblem(_phrase(rng, expr), str(a * b), "multiplication", d)


def gen_division(rng, d):
    b = _rand_operand(rng, max(1, d - 1))
    q = rng.randint(2, _rand_operand(rng, max(1, d - 1)))
    a = b * q
    op = rng.choice(["/", "÷", "divided by"])
    expr = f"{a} divided by {b}" if op == "divided by" else f"{a} {op} {b}"
    return MathProblem(_phrase(rng, expr), str(q), "division", d)


def gen_modular(rng, d):
    a = _rand_operand(rng, d)
    b = rng.randint(2, max(3, _rand_operand(rng, max(1, d - 1))))
    style = rng.choice(["mod", "remainder"])
    if style == "mod":
        expr = f"{a} mod {b}"
    else:
        expr = f"the remainder when {a} is divided by {b}"
    return MathProblem(_phrase(rng, expr), str(a % b), "modular", d)


def gen_gcd(rng, d):
    a, b = _rand_operand(rng, d), _rand_operand(rng, d)
    style = rng.choice(["GCD", "greatest common divisor"])
    return MathProblem(_phrase(rng, f"the {style} of {a} and {b}"),
                       str(_gcd(a, b)), "gcd", d)


def gen_lcm(rng, d):
    cap = min(d, 2)
    a, b = _rand_operand(rng, cap), _rand_operand(rng, cap)
    style = rng.choice(["LCM", "least common multiple"])
    return MathProblem(_phrase(rng, f"the {style} of {a} and {b}"),
                       str(_lcm(a, b)), "lcm", d)


def gen_exponent(rng, d):
    base = rng.randint(2, 12 + d * 3)
    exp = rng.randint(2, 4)
    style = rng.choice(["**", "^", "to the power of"])
    if style == "to the power of":
        expr = f"{base} to the power of {exp}"
    else:
        expr = f"{base}{style}{exp}"
    return MathProblem(_phrase(rng, expr), str(base ** exp), "exponent", d)


def gen_chained(rng, d):
    n_ops = rng.randint(2, 3)
    nums = [_rand_operand(rng, max(1, d - 1)) for _ in range(n_ops + 1)]
    ops = [rng.choice(["+", "-", "*"]) for _ in range(n_ops)]
    if n_ops == 2 and rng.random() < 0.5:
        inner = f"({nums[0]} {ops[0]} {nums[1]})"
        expr = f"{inner} {ops[1]} {nums[2]}"
        inner_val = eval(f"{nums[0]} {ops[0]} {nums[1]}")
        result = eval(f"{inner_val} {ops[1]} {nums[2]}")
    else:
        expr = f"{nums[0]}"
        for i in range(n_ops):
            expr += f" {ops[i]} {nums[i+1]}"
        result = eval(expr)
    return MathProblem(_phrase(rng, expr), str(int(result)), "chained", d)


def gen_comparison(rng, d):
    n = rng.randint(3, 6)
    nums = [_rand_operand(rng, d) for _ in range(n)]
    nums_str = ", ".join(str(x) for x in nums)
    op = rng.choice(["min", "max", "sum", "sorted"])
    if op == "min":
        expr, answer = f"the minimum of [{nums_str}]", str(min(nums))
    elif op == "max":
        expr, answer = f"the maximum of [{nums_str}]", str(max(nums))
    elif op == "sum":
        expr, answer = f"the sum of [{nums_str}]", str(sum(nums))
    else:
        expr, answer = f"[{nums_str}] sorted ascending", str(sorted(nums))
    return MathProblem(_phrase(rng, expr), answer, "comparison", d)


GENERATORS = [
    (gen_addition, 15), (gen_subtraction, 15), (gen_multiplication, 12),
    (gen_division, 10), (gen_modular, 10), (gen_gcd, 8), (gen_lcm, 8),
    (gen_exponent, 7), (gen_chained, 10), (gen_comparison, 5),
]
_GEN_FUNCS = [fn for fn, _ in GENERATORS]
_GEN_WEIGHTS = [w for _, w in GENERATORS]


def generate_problem(rng: random.Random, difficulty: int = 0) -> MathProblem:
    if difficulty == 0:
        difficulty = rng.choices([1, 2, 3], weights=[30, 50, 20])[0]
    fn = rng.choices(_GEN_FUNCS, weights=_GEN_WEIGHTS)[0]
    return fn(rng, difficulty)


# ── Chat Formatting ─────────────────────────────────────────────

SYSTEM_VARIANTS = [
    "You are a precise math calculator. Answer with ONLY the numerical result, nothing else.",
    "Solve the math problem. Respond with just the answer.",
    "You are a math engine. Output only the final number.",
    "Calculate and respond with the answer only. No explanation.",
]


def problem_to_chat(problem: MathProblem, rng: random.Random) -> dict:
    return {
        "messages": [
            {"role": "system", "content": rng.choice(SYSTEM_VARIANTS)},
            {"role": "user", "content": problem.question},
            {"role": "assistant", "content": problem.answer},
        ],
        "metadata": {
            "operation": problem.operation,
            "difficulty": problem.difficulty,
        },
    }


def format_chat_text(example: dict) -> dict:
    parts = [f"<|im_start|>{m['role']}\n{m['content']}<|im_end|>"
             for m in example["messages"]]
    return {"text": "\n".join(parts)}


# ── LLM-Backed Generation ──────────────────────────────────────

# Prompt that asks the LLM to generate math problems in structured format
LLM_GENERATION_PROMPT = """Generate {batch_size} unique math problems. Each problem MUST have:
1. A natural language question (varied phrasing — don't repeat the same format)
2. A computable mathematical expression that gives the exact answer
3. The correct numerical answer

Problem types to include (mix them):
- Addition, subtraction with 2-4 digit numbers
- Multiplication with 2-3 digit numbers
- Integer division (exact, no remainders)
- Modular arithmetic (remainder operations)
- GCD or LCM of two numbers
- Exponentiation (small bases and exponents)
- Chained operations with 2-3 steps (use parentheses)
- Finding min/max/sum of a list of numbers

Respond with ONLY a JSON array, no markdown fences, no explanation:
[
  {{"question": "If you add 347 and 892, what do you get?", "expression": "347 + 892", "answer": "1239", "operation": "addition"}},
  {{"question": "How many times does 16 go into 1024?", "expression": "1024 // 16", "answer": "64", "operation": "division"}},
  ...
]

IMPORTANT:
- Every "expression" must be valid Python that evaluates to an integer
- Every "answer" must be a string containing ONLY the number (no units, no text)
- Use diverse question phrasing — word problems, direct computation, "find the...", etc.
- Use numbers between {min_digits} and {max_digits} digits
- Generate EXACTLY {batch_size} problems"""


def _safe_eval(expr: str) -> Optional[int]:
    """Safely evaluate a math expression. Returns None on failure."""
    # Only allow safe math operations
    allowed = set("0123456789+-*/%() .")
    # Also allow // for integer division and ** for exponent
    clean = expr.replace("//", "÷÷").replace("**", "^^")
    if not all(c in allowed or c in "÷^" for c in clean):
        return None
    try:
        result = eval(expr, {"__builtins__": {}}, {})
        if isinstance(result, (int, float)):
            return int(result)
    except Exception:
        pass
    return None


def _parse_llm_response(text: str) -> list[dict]:
    """Parse LLM JSON response into problem dicts. Tolerant of formatting issues."""
    # Strip markdown fences if present
    text = text.strip()
    if text.startswith("```"):
        text = re.sub(r"^```\w*\n?", "", text)
        text = re.sub(r"\n?```$", "", text)
        text = text.strip()

    # Try to find JSON array
    start = text.find("[")
    end = text.rfind("]")
    if start < 0 or end < 0:
        return []

    try:
        items = json.loads(text[start:end + 1])
        if isinstance(items, list):
            return items
    except json.JSONDecodeError:
        pass

    # Fallback: try to parse line by line
    problems = []
    for line in text.split("\n"):
        line = line.strip().rstrip(",")
        if line.startswith("{"):
            try:
                problems.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    return problems


def _verify_problem(item: dict) -> Optional[MathProblem]:
    """Verify an LLM-generated problem by evaluating the expression."""
    question = item.get("question", "").strip()
    expression = item.get("expression", "").strip()
    answer_str = item.get("answer", "").strip()
    operation = item.get("operation", "llm_generated").strip()

    if not question or not answer_str:
        return None

    # If expression is provided, verify the answer
    if expression:
        computed = _safe_eval(expression)
        if computed is not None and str(computed) == answer_str:
            return MathProblem(question, answer_str, operation, 2)
        elif computed is not None:
            # LLM got the answer wrong — use computed answer instead
            return MathProblem(question, str(computed), operation, 2)

    # No expression or eval failed — try to verify answer directly
    # For simple questions, try to extract and compute
    try:
        answer_int = int(answer_str)
        # Accept it but mark as unverified
        return MathProblem(question, str(answer_int), operation, 2)
    except ValueError:
        return None


class LLMProblemGenerator:
    """Generates math problems via LLM with Python verification.

    Runs a background thread that continuously fetches problems from the LLM
    and fills a thread-safe buffer. The training loop pulls from the buffer.
    Falls back to Python generation when the buffer is empty.
    """

    def __init__(
        self,
        endpoint: str,
        model: str,
        batch_size: int = 20,
        buffer_size: int = 200,
        min_digits: int = 2,
        max_digits: int = 4,
        temperature: float = 0.9,
        timeout: int = 60,
    ):
        self.endpoint = endpoint
        self.model = model
        self.batch_size = batch_size
        self.min_digits = min_digits
        self.max_digits = max_digits
        self.temperature = temperature
        self.timeout = timeout

        self._buffer: deque[MathProblem] = deque(maxlen=buffer_size)
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._thread: Optional[threading.Thread] = None

        # Stats
        self.total_generated = 0
        self.total_verified = 0
        self.total_rejected = 0
        self.llm_errors = 0
        self.fallback_count = 0
        self._fallback_rng = random.Random(12345)

    def start(self):
        """Start the background generation thread."""
        self._stop.clear()
        self._thread = threading.Thread(target=self._generation_loop, daemon=True)
        self._thread.start()
        print(f"  [LLMGen] Started — endpoint={self.endpoint} model={self.model}")
        print(f"  [LLMGen] Batch={self.batch_size} buffer={self._buffer.maxlen}")

    def stop(self):
        """Stop the background generation thread."""
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=5)
        print(f"  [LLMGen] Stopped — generated={self.total_generated} "
              f"verified={self.total_verified} rejected={self.total_rejected} "
              f"errors={self.llm_errors} fallbacks={self.fallback_count}")

    def get_problem(self) -> MathProblem:
        """Get a verified problem. Falls back to Python if buffer empty."""
        with self._lock:
            if self._buffer:
                return self._buffer.popleft()

        # Buffer empty — fallback to Python generation
        self.fallback_count += 1
        return generate_problem(self._fallback_rng, difficulty=0)

    def buffer_size(self) -> int:
        with self._lock:
            return len(self._buffer)

    def _generation_loop(self):
        """Background loop that continuously generates problems."""
        while not self._stop.is_set():
            # Don't overfill the buffer
            if self.buffer_size() > self._buffer.maxlen * 0.8:
                self._stop.wait(0.5)
                continue

            try:
                problems = self._fetch_batch()
                with self._lock:
                    for p in problems:
                        self._buffer.append(p)
            except Exception as e:
                self.llm_errors += 1
                if self.llm_errors % 10 == 1:
                    print(f"  [LLMGen] Error #{self.llm_errors}: {e}")
                self._stop.wait(2)  # backoff on error

    def _fetch_batch(self) -> list[MathProblem]:
        """Fetch one batch of problems from the LLM."""
        prompt = LLM_GENERATION_PROMPT.format(
            batch_size=self.batch_size,
            min_digits=self.min_digits,
            max_digits=self.max_digits,
        )

        payload = {
            "model": self.model,
            "messages": [
                {"role": "system", "content": "You are a math problem generator. "
                 "Output ONLY valid JSON arrays. No markdown, no explanation."},
                {"role": "user", "content": prompt},
            ],
            "temperature": self.temperature,
            "max_tokens": 4096,
            "stream": False,
        }

        resp = requests.post(self.endpoint, json=payload, timeout=self.timeout)
        resp.raise_for_status()
        data = resp.json()
        text = data["choices"][0]["message"]["content"]

        raw_problems = _parse_llm_response(text)
        self.total_generated += len(raw_problems)

        verified = []
        for item in raw_problems:
            problem = _verify_problem(item)
            if problem:
                verified.append(problem)
                self.total_verified += 1
            else:
                self.total_rejected += 1

        return verified


# ── Streaming Datasets ──────────────────────────────────────────

class MathStreamDataset(IterableDataset):
    """Infinite streaming math dataset (Python-only). Never repeats."""

    def __init__(self, difficulty: int = 0, seed: int = 42):
        self.difficulty = difficulty
        self.base_seed = seed

    def __iter__(self) -> Iterator[dict]:
        worker_info = torch.utils.data.get_worker_info()
        seed = self.base_seed + (worker_info.id * 1_000_000 if worker_info else 0)
        rng = random.Random(seed)
        counter = 0

        while True:
            if counter % 100_000 == 0 and counter > 0:
                rng = random.Random(seed + counter)
            problem = generate_problem(rng, self.difficulty)
            chat = problem_to_chat(problem, rng)
            formatted = format_chat_text(chat)
            formatted["metadata"] = chat["metadata"]
            yield formatted
            counter += 1


class LLMMathStreamDataset(IterableDataset):
    """Infinite streaming dataset backed by live LLM generation.

    The LLM (e.g. qwen3-coder on Hugin) generates natural-language math
    problems in the background. Python verifies every answer. Falls back
    to Python-generated problems when the LLM buffer is empty.
    """

    def __init__(
        self,
        endpoint: str,
        model: str,
        batch_size: int = 20,
        buffer_size: int = 200,
        temperature: float = 0.9,
        seed: int = 42,
    ):
        self.generator = LLMProblemGenerator(
            endpoint=endpoint,
            model=model,
            batch_size=batch_size,
            buffer_size=buffer_size,
            temperature=temperature,
        )
        self.base_seed = seed
        self._started = False

    def __iter__(self) -> Iterator[dict]:
        # Start the background generator (once per worker)
        if not self._started:
            self.generator.start()
            self._started = True
            # Wait briefly for initial buffer fill
            time.sleep(3)

        worker_info = torch.utils.data.get_worker_info()
        seed = self.base_seed + (worker_info.id * 1_000_000 if worker_info else 0)
        rng = random.Random(seed)
        counter = 0

        while True:
            problem = self.generator.get_problem()
            chat = problem_to_chat(problem, rng)
            formatted = format_chat_text(chat)
            formatted["metadata"] = chat["metadata"]
            yield formatted
            counter += 1

            # Periodic stats
            if counter % 500 == 0:
                gen = self.generator
                buf = gen.buffer_size()
                pct = gen.total_verified / max(1, gen.total_generated) * 100
                print(f"  [LLMData] yielded={counter} buffer={buf} "
                      f"verified={gen.total_verified}/{gen.total_generated} "
                      f"({pct:.0f}%) fallbacks={gen.fallback_count}")

    def __del__(self):
        if self._started:
            self.generator.stop()


# ── Fixed Evaluation Set ────────────────────────────────────────

def generate_eval_set(
    n: int = 500,
    seed: int = 9999,
    difficulty: int = 0,
    include_ood: bool = True,
) -> list[dict]:
    """Generate a fixed eval set (always Python-generated for reproducibility)."""
    rng = random.Random(seed)
    problems = []

    n_ood = int(n * 0.2) if include_ood else 0
    n_id = n - n_ood

    for _ in range(n_id):
        problem = generate_problem(rng, difficulty)
        chat = problem_to_chat(problem, rng)
        formatted = format_chat_text(chat)
        formatted["metadata"] = chat["metadata"]
        formatted["answer"] = problem.answer
        formatted["question"] = problem.question
        formatted["ood"] = False
        formatted["messages"] = chat["messages"]
        problems.append(formatted)

    for _ in range(n_ood):
        problem = generate_problem(rng, difficulty=4)
        chat = problem_to_chat(problem, rng)
        formatted = format_chat_text(chat)
        formatted["metadata"] = chat["metadata"]
        formatted["answer"] = problem.answer
        formatted["question"] = problem.question
        formatted["ood"] = True
        formatted["messages"] = chat["messages"]
        problems.append(formatted)

    rng.shuffle(problems)
    return problems


# ── CLI ─────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="Synthetic Math Data Generator")
    parser.add_argument("--preview", type=int, default=0,
                        help="Print N sample problems (Python-generated)")
    parser.add_argument("--preview-llm", type=int, default=0,
                        help="Print N sample problems (LLM-generated)")
    parser.add_argument("--endpoint", type=str,
                        default="http://10.0.65.8:11434/v1/chat/completions",
                        help="LLM endpoint for generation")
    parser.add_argument("--model", type=str,
                        default="qwen3-coder:30b-a3b-q4_K_M",
                        help="LLM model name")
    parser.add_argument("--generate-eval", type=int, default=0,
                        help="Generate fixed eval set of N problems")
    parser.add_argument("--output", type=str, default="eval_math.jsonl")
    parser.add_argument("--difficulty", type=int, default=0)
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--stats", action="store_true",
                        help="Generate 10K problems and show distribution stats")
    args = parser.parse_args()

    if args.preview > 0:
        rng = random.Random(args.seed)
        print(f"=== {args.preview} Python-Generated Problems ===\n")
        for i in range(args.preview):
            p = generate_problem(rng, args.difficulty)
            print(f"  [{p.operation:>12} d={p.difficulty}] Q: {p.question}")
            print(f"  {' '*16}  A: {p.answer}\n")

    if args.preview_llm > 0:
        print(f"=== Fetching {args.preview_llm} LLM-Generated Problems ===")
        print(f"  Endpoint: {args.endpoint}")
        print(f"  Model: {args.model}\n")

        gen = LLMProblemGenerator(
            endpoint=args.endpoint,
            model=args.model,
            batch_size=args.preview_llm,
        )
        try:
            problems = gen._fetch_batch()
            for i, p in enumerate(problems):
                print(f"  [{p.operation:>14} d={p.difficulty}] Q: {p.question}")
                print(f"  {' '*18}  A: {p.answer}")
                print()
            print(f"  Generated: {gen.total_generated}, "
                  f"Verified: {gen.total_verified}, "
                  f"Rejected: {gen.total_rejected}")
        except Exception as e:
            print(f"  ERROR: {e}")

    if args.generate_eval > 0:
        eval_set = generate_eval_set(args.generate_eval, args.seed, args.difficulty)
        with open(args.output, "w") as f:
            for item in eval_set:
                f.write(json.dumps(item) + "\n")
        n_ood = sum(1 for x in eval_set if x.get("ood"))
        print(f"Wrote {len(eval_set)} eval problems to {args.output} "
              f"({n_ood} OOD, {len(eval_set) - n_ood} in-distribution)")

    if args.stats:
        rng = random.Random(args.seed)
        from collections import Counter
        ops = Counter()
        diffs = Counter()
        for _ in range(10_000):
            p = generate_problem(rng, args.difficulty)
            ops[p.operation] += 1
            diffs[p.difficulty] += 1
        print("\n=== Operation Distribution (10K samples) ===")
        for op, count in ops.most_common():
            print(f"  {op:>12}: {count:>5} ({count/100:.1f}%)")
        print("\n=== Difficulty Distribution ===")
        for d, count in sorted(diffs.items()):
            print(f"  difficulty {d}: {count:>5} ({count/100:.1f}%)")


if __name__ == "__main__":
    main()
