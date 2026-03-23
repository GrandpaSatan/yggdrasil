#!/usr/bin/env python3
"""Augment Saga training data with synthetic variations via Munin's qwen3.5:4b.

Generates:
1. Paraphrased classify examples (positive + negative)
2. Additional distill pairs with varied phrasing
3. More negative examples covering edge cases
"""

import json
import random
import sys
import requests
import time

BARN = "/data/saga/data"
OLLAMA_URL = "http://<hugin-ip>:11434"  # Hugin's Ollama
MODEL = "qwen3-coder:30b-a3b-q4_K_M"
SYSTEM_PROMPT = "You are Saga, Yggdrasil's memory engine. Respond ONLY in valid JSON."

CATEGORIES = ["bug_fix", "architecture_decision", "sprint_lifecycle",
              "user_feedback", "deployment_change", "gotcha"]


def ollama_generate(prompt, max_retries=2):
    """Call Ollama generate API with retry."""
    for attempt in range(max_retries):
        try:
            resp = requests.post(
                f"{OLLAMA_URL}/api/generate",
                json={
                    "model": MODEL,
                    "prompt": prompt,
                    "stream": False,
                    "options": {"temperature": 0.8, "num_predict": 512},
                },
                timeout=120,
            )
            if resp.status_code == 200:
                return resp.json().get("response", "")
        except Exception as e:
            if attempt < max_retries - 1:
                time.sleep(2)
            else:
                print(f"  Ollama failed: {e}", file=sys.stderr)
    return None


def make_message(user, assistant):
    return {
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": user},
            {"role": "assistant", "content": assistant},
        ]
    }


def generate_synthetic_negatives(count=200):
    """Use LLM to generate realistic but NOT memory-worthy code snippets."""
    pairs = []
    batch_prompt = """Generate {n} short code snippets or shell commands that are routine, mundane, and NOT worth remembering. These are things like:
- Simple variable declarations
- Import statements
- Formatting changes
- Version bumps
- Trivial refactors (rename variable)
- Reading files or listing directories
- Running tests (just the command)
- Adding comments or docstrings

Return as a JSON array of strings. Each string should be 1-3 lines.
Example: ["let x = 42;", "use std::io;", "cargo fmt -- --check"]
Return ONLY the JSON array, nothing else."""

    print(f"Generating {count} synthetic negatives via LLM...")
    generated = 0
    tools = ["Edit", "Write", "Bash"]
    files = [
        "src/main.rs", "src/lib.rs", "Cargo.toml", "README.md",
        "tests/integration.rs", ".gitignore", "config.json",
    ]

    for batch in range(count // 20 + 1):
        if generated >= count:
            break
        result = ollama_generate(batch_prompt.format(n=20))
        if not result:
            continue
        try:
            # Try to extract JSON array from response
            # Handle potential markdown wrapping
            cleaned = result.strip()
            if cleaned.startswith("```"):
                cleaned = cleaned.split("\n", 1)[1].rsplit("```", 1)[0]
            snippets = json.loads(cleaned)
            if not isinstance(snippets, list):
                continue
            for snippet in snippets:
                if not isinstance(snippet, str) or len(snippet) < 3:
                    continue
                tool = random.choice(tools)
                user_text = f"CLASSIFY\ntool: {tool}\nfile: {random.choice(files)}\ncontent: {snippet[:300]}"
                assistant_text = json.dumps({
                    "category": "none",
                    "should_store": False,
                    "confidence": round(random.uniform(0.82, 0.98), 2),
                })
                pairs.append(make_message(user_text, assistant_text))
                generated += 1
                if generated >= count:
                    break
        except json.JSONDecodeError:
            continue
        print(f"  batch {batch+1}: {generated}/{count} negatives")

    return pairs


def generate_synthetic_positives(engrams, count=200):
    """Generate paraphrased versions of real engrams for more training variety."""
    pairs = []
    sample = random.sample(engrams, min(count, len(engrams)))
    tools = ["Edit", "Write", "Bash"]
    files = [
        "crates/mimir/src/handlers.rs", "crates/odin/src/router.rs",
        "crates/ygg-domain/src/engram.rs", "deploy/docker-compose.yml",
        "crates/ygg-ha/src/notify.rs", "crates/huginn/src/watcher.rs",
    ]

    print(f"Generating {count} synthetic positive variations via LLM...")
    generated = 0

    for eng in sample:
        if generated >= count:
            break
        cause = eng["cause"][:200]
        prompt = f"""Rephrase this software engineering event in a different way, as if describing what a developer just did. Keep it concise (1-3 sentences). Return ONLY the rephrased text, nothing else.

Original: {cause}"""

        result = ollama_generate(prompt)
        if not result or len(result) < 10:
            continue

        # Determine category from tags
        tags = [t.lower() for t in (eng.get("tags") or [])]
        category = "bug_fix"  # default
        for tag in tags:
            tag_map = {
                "bugfix": "bug_fix", "architecture": "architecture_decision",
                "sprint": "sprint_lifecycle", "deployment": "deployment_change",
                "gotcha": "gotcha", "feedback": "user_feedback",
            }
            if tag in tag_map:
                category = tag_map[tag]
                break

        tool = random.choice(tools)
        user_text = f"CLASSIFY\ntool: {tool}\nfile: {random.choice(files)}\ncontent: {result.strip()[:300]}"
        assistant_text = json.dumps({
            "category": category,
            "should_store": True,
            "confidence": round(random.uniform(0.78, 0.96), 2),
        })
        pairs.append(make_message(user_text, assistant_text))
        generated += 1
        if generated % 20 == 0:
            print(f"  {generated}/{count} positive variations")

    return pairs


def generate_distill_variations(engrams, count=150):
    """Generate varied distill pairs using LLM to rephrase cause/effect."""
    pairs = []
    sample = random.sample(engrams, min(count, len(engrams)))
    tools = ["Edit", "Write", "Bash"]
    files = [
        "crates/mimir/src/handlers.rs", "crates/odin/src/memory_router.rs",
        "crates/ygg-mesh/src/proxy.rs", "crates/ygg-sentinel/src/main.rs",
    ]

    print(f"Generating {count} distill variations via LLM...")
    generated = 0

    for eng in sample:
        if generated >= count:
            break
        cause = eng["cause"][:300]
        effect = eng["effect"][:500]
        tags = [t for t in (eng.get("tags") or []) if not t.startswith("project:")][:3]

        prompt = f"""Given this software event, write a shorter, cleaner summary as a JSON object with "cause" (what happened, 1 sentence), "effect" (the outcome, 1-2 sentences), and "tags" (2-3 relevant keywords as a list).

Event: {cause}
Outcome: {effect}

Return ONLY valid JSON like: {{"cause": "...", "effect": "...", "tags": ["...", "..."]}}"""

        result = ollama_generate(prompt)
        if not result:
            continue
        try:
            cleaned = result.strip()
            if cleaned.startswith("```"):
                cleaned = cleaned.split("\n", 1)[1].rsplit("```", 1)[0]
            parsed = json.loads(cleaned)
            if "cause" not in parsed or "effect" not in parsed:
                continue

            tool = random.choice(tools)
            content = f"{cause}\n{effect}"[:2000]
            user_text = f"DISTILL\ntool: {tool}\nfile: {random.choice(files)}\ncontent: {content}"
            assistant_text = json.dumps(parsed, ensure_ascii=False)
            pairs.append(make_message(user_text, assistant_text))
            generated += 1
            if generated % 20 == 0:
                print(f"  {generated}/{count} distill variations")
        except json.JSONDecodeError:
            continue

    return pairs


def main():
    random.seed(42)

    # Load existing engrams for paraphrasing
    engrams = []
    with open(f"{BARN}/engrams_raw.jsonl") as f:
        for line in f:
            engrams.append(json.loads(line))

    # Check Ollama availability
    try:
        resp = requests.get(f"{OLLAMA_URL}/api/tags", timeout=5)
        models = [m["name"] for m in resp.json().get("models", [])]
        if MODEL not in models and f"{MODEL}:latest" not in models:
            print(f"Warning: {MODEL} not loaded on Munin. Available: {models}")
            print("Attempting to pull...")
            requests.post(f"{OLLAMA_URL}/api/pull", json={"name": MODEL}, timeout=300)
    except Exception as e:
        print(f"Error: Cannot reach Munin Ollama at {OLLAMA_URL}: {e}")
        sys.exit(1)

    all_augmented = []

    # Generate synthetic negatives
    neg_pairs = generate_synthetic_negatives(200)
    print(f"  Generated {len(neg_pairs)} synthetic negatives")
    all_augmented.extend(neg_pairs)

    # Generate positive variations
    pos_pairs = generate_synthetic_positives(engrams, 200)
    print(f"  Generated {len(pos_pairs)} positive variations")
    all_augmented.extend(pos_pairs)

    # Generate distill variations
    distill_pairs = generate_distill_variations(engrams, 150)
    print(f"  Generated {len(distill_pairs)} distill variations")
    all_augmented.extend(distill_pairs)

    # Write augmented data
    out_path = f"{BARN}/saga_augmented.jsonl"
    with open(out_path, "w") as f:
        for pair in all_augmented:
            f.write(json.dumps(pair, ensure_ascii=False) + "\n")

    print(f"\n=== Augmentation Summary ===")
    print(f"Total augmented pairs: {len(all_augmented)}")
    print(f"Output: {out_path}")


if __name__ == "__main__":
    main()
