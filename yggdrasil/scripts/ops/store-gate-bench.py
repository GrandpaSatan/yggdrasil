#!/usr/bin/env python3
"""Sprint 065 A·P3 — store-gate precision benchmark.

Replays a corpus of cause/effect pairs through Mimir's /api/v1/store endpoint
and measures the false-New / false-Update rates. The corpus pairs each entry
with an expected verdict (new / update / old) derived from ground truth —
e.g. sprint-archive pairs (expected=new because they describe distinct
sprints) vs legitimate near-duplicates (expected=old).

Usage:
    python3 scripts/ops/store-gate-bench.py \\
        --corpus scripts/ops/store-gate-corpus.jsonl \\
        --mimir http://10.0.65.8:9090 \\
        [--out store-gate-bench.csv]

Exit codes:
    0 = bench completed
    1 = mimir unreachable OR false-Update rate > 20% (regression)
"""
from __future__ import annotations

import argparse
import json
import sys
import time
from collections import Counter
from pathlib import Path
from typing import Any
from urllib import error as urllib_error
from urllib import request as urllib_request


def _post_json(url: str, payload: dict[str, Any], timeout: float = 10.0) -> dict[str, Any]:
    body = json.dumps(payload).encode()
    req = urllib_request.Request(
        url,
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib_request.urlopen(req, timeout=timeout) as resp:  # noqa: S310 (internal URL)
        return json.loads(resp.read())


def run_bench(corpus_path: Path, mimir_url: str, out_csv: Path | None) -> int:
    with corpus_path.open() as f:
        entries = [json.loads(line) for line in f if line.strip() and not line.startswith("//")]

    if not entries:
        print(f"FAIL: corpus {corpus_path} is empty", file=sys.stderr)
        return 1

    print(f"Loaded {len(entries)} entries from {corpus_path}")
    print(f"Mimir: {mimir_url}")
    print()

    results: list[dict[str, Any]] = []
    verdict_counts: Counter[str] = Counter()
    accuracy_counts: Counter[str] = Counter()
    false_update_count = 0
    false_new_count = 0

    for idx, entry in enumerate(entries, start=1):
        cause = entry["cause"]
        effect = entry["effect"]
        expected = entry["expected_verdict"]
        tags = entry.get("tags", [])
        project = entry.get("project", "yggdrasil")

        payload = {
            "cause": cause,
            "effect": effect,
            "tags": tags,
            "project": project,
            "force": False,
        }

        t0 = time.monotonic()
        try:
            response = _post_json(f"{mimir_url}/api/v1/store", payload, timeout=15.0)
        except urllib_error.URLError as e:
            print(f"FAIL: mimir unreachable ({e})", file=sys.stderr)
            return 1
        latency_ms = (time.monotonic() - t0) * 1000.0

        actual_verdict = response.get("verdict", "unknown")
        similarity = response.get("similarity")

        verdict_counts[actual_verdict] += 1
        match = actual_verdict == expected
        accuracy_counts["correct" if match else "incorrect"] += 1

        # False-positive bookkeeping for the specific collision scenario we care about.
        if expected == "new" and actual_verdict == "update":
            false_update_count += 1
        if expected == "update" and actual_verdict == "new":
            false_new_count += 1

        status = "OK" if match else "MISS"
        print(
            f"[{idx:2}/{len(entries)}] {status:4} expected={expected:6} "
            f"actual={actual_verdict:6} sim={similarity if similarity is not None else '-':<6} "
            f"{latency_ms:.0f}ms — {cause[:60]}"
        )

        results.append(
            {
                "idx": idx,
                "cause": cause,
                "expected": expected,
                "actual": actual_verdict,
                "similarity": similarity,
                "latency_ms": round(latency_ms, 1),
                "tags": ",".join(tags),
            }
        )

    total = len(entries)
    correct = accuracy_counts["correct"]
    false_update_rate = false_update_count / total * 100
    false_new_rate = false_new_count / total * 100

    print()
    print("─" * 60)
    print(f"Total:         {total}")
    print(f"Correct:       {correct} ({correct / total * 100:.1f}%)")
    print(f"False Update:  {false_update_count} ({false_update_rate:.1f}%)  "
          f"[sprint collisions — target <5%]")
    print(f"False New:     {false_new_count} ({false_new_rate:.1f}%)  "
          f"[duplicate-miss — target <10%]")
    print(f"Verdicts:      {dict(verdict_counts)}")

    if out_csv:
        import csv

        with out_csv.open("w") as f:
            writer = csv.DictWriter(f, fieldnames=list(results[0].keys()))
            writer.writeheader()
            writer.writerows(results)
        print(f"Wrote CSV:     {out_csv}")

    # Regression gate.
    if false_update_rate > 20.0:
        print(f"FAIL: false-Update rate {false_update_rate:.1f}% > 20% regression threshold",
              file=sys.stderr)
        return 1

    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--corpus", type=Path, required=True, help="Path to corpus JSONL")
    parser.add_argument("--mimir", default="http://10.0.65.8:9090", help="Mimir base URL")
    parser.add_argument("--out", type=Path, help="Optional CSV output path")
    args = parser.parse_args()

    if not args.corpus.exists():
        print(f"FAIL: corpus {args.corpus} does not exist", file=sys.stderr)
        return 1

    return run_bench(args.corpus, args.mimir, args.out)


if __name__ == "__main__":
    sys.exit(main())
