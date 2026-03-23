#!/usr/bin/env python3
"""Merge all JSONL data files and split into train/val/test (80/10/10)."""

import json
import random
from pathlib import Path

BARN = "/data/saga/data"


def validate_pair(pair):
    """Validate a training pair has correct structure."""
    if not isinstance(pair, dict) or "messages" not in pair:
        return False
    msgs = pair["messages"]
    if len(msgs) != 3:
        return False
    if msgs[0]["role"] != "system" or msgs[1]["role"] != "user" or msgs[2]["role"] != "assistant":
        return False
    # Validate assistant response is valid JSON
    try:
        json.loads(msgs[2]["content"])
    except (json.JSONDecodeError, KeyError):
        return False
    return True


def main():
    random.seed(42)

    # Collect all JSONL files
    sources = ["saga_combined.jsonl", "saga_augmented.jsonl"]
    all_pairs = []
    for src in sources:
        path = Path(BARN) / src
        if not path.exists():
            print(f"  Skipping {src} (not found)")
            continue
        count = 0
        with open(path) as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    pair = json.loads(line)
                    if validate_pair(pair):
                        all_pairs.append(pair)
                        count += 1
                except json.JSONDecodeError:
                    continue
        print(f"  {src}: {count} valid pairs")

    print(f"\nTotal valid pairs: {len(all_pairs)}")

    # Stats
    task_counts = {}
    neg_count = 0
    for p in all_pairs:
        user_msg = p["messages"][1]["content"]
        task = user_msg.split("\n")[0]
        task_counts[task] = task_counts.get(task, 0) + 1
        try:
            asst = json.loads(p["messages"][2]["content"])
            if asst.get("should_store") is False:
                neg_count += 1
        except (json.JSONDecodeError, KeyError):
            pass

    print("\nTask distribution:")
    for task, count in sorted(task_counts.items()):
        print(f"  {task}: {count}")
    print(f"  Negatives: {neg_count}")

    # Shuffle and split
    random.shuffle(all_pairs)
    n = len(all_pairs)
    train_end = int(n * 0.8)
    val_end = int(n * 0.9)

    splits = {
        "saga_train.jsonl": all_pairs[:train_end],
        "saga_val.jsonl": all_pairs[train_end:val_end],
        "saga_test.jsonl": all_pairs[val_end:],
    }

    for filename, data in splits.items():
        path = Path(BARN) / filename
        with open(path, "w") as f:
            for pair in data:
                f.write(json.dumps(pair, ensure_ascii=False) + "\n")
        print(f"\n{filename}: {len(data)} pairs")

    print(f"\nAll files written to {BARN}/")


if __name__ == "__main__":
    main()
