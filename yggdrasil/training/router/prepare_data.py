#!/usr/bin/env python3
"""Convert Odin JSONL request logs into Unsloth SFT training data.

Input:  /var/lib/yggdrasil/odin-request-log.jsonl (from Sprint 052)
Output: training_data.jsonl (messages format for Unsloth SFT)

Filtering:
  - Only entries with router_method "LlmConfirmed" or "SdrOnly" (high confidence)
  - Skip entries with empty user_message
  - Optionally join with feedback entries for accuracy_rating >= threshold

Usage:
  python prepare_data.py --input /var/lib/yggdrasil/odin-request-log.jsonl --output training_data.jsonl
  python prepare_data.py --synthetic  # Generate synthetic examples from keyword lists
"""

import argparse
import json
import sys
from pathlib import Path

SYSTEM_PROMPT = (
    "You are an intent classifier. Classify the user's message into exactly one intent: "
    "coding, reasoning, home_automation, gaming, default. "
    "Respond with JSON only: {\"intent\": \"<intent>\", \"confidence\": <0.0-1.0>}"
)

# Synthetic keyword examples (from Odin's router.rs keyword lists)
SYNTHETIC_EXAMPLES = {
    "coding": [
        "Write a Rust function to sort a vector",
        "Debug this segfault in my code",
        "How do I implement a trait in Rust?",
        "Fix the compilation error in main.rs",
        "Refactor this function to use iterators",
        "Add error handling to the HTTP client",
        "What does this regex do?",
        "Write unit tests for the parser module",
        "Implement a binary search tree",
        "Review this pull request diff",
    ],
    "reasoning": [
        "Analyze the tradeoffs between microservices and monoliths",
        "Explain the CAP theorem with examples",
        "Compare PostgreSQL and SQLite for embedded use",
        "What are the implications of this architecture decision?",
        "Summarize the key points from this document",
        "Help me think through this design problem",
        "What are the pros and cons of using gRPC vs REST?",
        "Evaluate whether we should use async or threads here",
    ],
    "home_automation": [
        "Turn on the living room lights",
        "Set the thermostat to 72 degrees",
        "What's the temperature in the bedroom?",
        "Turn off all lights in the house",
        "Is the garage door open?",
        "Dim the kitchen lights to 50%",
        "Lock the front door",
        "What devices are currently on?",
    ],
    "gaming": [
        "Launch the gaming VM on Thor",
        "Start Moonlight streaming",
        "Check GPU availability on Thor",
        "Stop the Harpy VM",
        "What games are installed?",
        "Pair my controller with Sunshine",
    ],
    "default": [
        "Hello, how are you?",
        "What time is it?",
        "Tell me a joke",
        "What's the weather like?",
        "Remember that I prefer dark mode",
        "What did we talk about yesterday?",
    ],
}


def format_training_example(user_message: str, intent: str, confidence: float = 0.95) -> dict:
    """Format a single training example in messages format for Unsloth SFT."""
    return {
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": user_message},
            {
                "role": "assistant",
                "content": json.dumps({"intent": intent, "confidence": confidence}),
            },
        ]
    }


def generate_synthetic(output_path: Path):
    """Generate synthetic training data from keyword examples."""
    examples = []
    for intent, messages in SYNTHETIC_EXAMPLES.items():
        for msg in messages:
            examples.append(format_training_example(msg, intent))

    with open(output_path, "w") as f:
        for ex in examples:
            f.write(json.dumps(ex) + "\n")

    print(f"Generated {len(examples)} synthetic examples -> {output_path}")


def convert_logs(input_path: Path, output_path: Path, min_confidence: float = 0.6):
    """Convert JSONL request logs to training data."""
    high_confidence_methods = {"LlmConfirmed", "SdrOnly"}

    # Load feedback map
    feedback_map = {}
    log_entries = []

    with open(input_path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                entry = json.loads(line)
            except json.JSONDecodeError:
                continue

            # Feedback entries have accuracy_rating
            if "accuracy_rating" in entry:
                feedback_map[entry["request_id"]] = entry
            elif "final_intent" in entry:
                log_entries.append(entry)

    examples = []
    for entry in log_entries:
        method = entry.get("router_method", "")
        user_msg = entry.get("user_message", "")

        if not user_msg or method not in high_confidence_methods:
            continue

        confidence = entry.get("llm_confidence") or entry.get("sdr_confidence") or 0.8

        # If we have feedback, filter by quality
        fb = feedback_map.get(entry.get("request_id", ""))
        if fb and (fb.get("accuracy_rating", 0) < min_confidence or fb.get("redo_requested")):
            continue

        examples.append(
            format_training_example(user_msg, entry["final_intent"], confidence)
        )

    with open(output_path, "w") as f:
        for ex in examples:
            f.write(json.dumps(ex) + "\n")

    print(f"Converted {len(examples)} log entries -> {output_path}")


def main():
    parser = argparse.ArgumentParser(description="Prepare training data for LFM2.5 router fine-tuning")
    parser.add_argument("--input", type=Path, help="JSONL request log path")
    parser.add_argument("--output", type=Path, default=Path("training_data.jsonl"))
    parser.add_argument("--synthetic", action="store_true", help="Generate synthetic examples")
    parser.add_argument("--min-confidence", type=float, default=0.6)
    args = parser.parse_args()

    if args.synthetic:
        generate_synthetic(args.output)
    elif args.input:
        if not args.input.exists():
            print(f"Error: {args.input} not found", file=sys.stderr)
            sys.exit(1)
        convert_logs(args.input, args.output, args.min_confidence)
    else:
        parser.print_help()
        sys.exit(1)


if __name__ == "__main__":
    main()
