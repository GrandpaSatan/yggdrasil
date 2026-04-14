#!/usr/bin/env python3
"""Sprint 064 P9 — review the last 7 days of `dream_self_improvement` engrams.

Queries Mimir for engrams tagged `[self_improvement, pending]` from the
last `--days` days, classifies each into actionable / duplicate /
hallucinated / trivial via cheap text heuristics, and prints a tuning
recommendation for the nemotron rank-step threshold used in the dream flow.

This is a manual review aid — not an automated tuner. The recommendation
is meant to guide the operator's decision to raise/lower the threshold,
not replace it.

Usage:
    python3 scripts/ops/review-self-improvement.py
    python3 scripts/ops/review-self-improvement.py --days 14 --json
    python3 scripts/ops/review-self-improvement.py --mimir http://10.0.65.8:9090

Required when MIMIR_VAULT_CLIENT_TOKEN is set on the server:
    --token <client-token>   OR  set MIMIR_VAULT_CLIENT_TOKEN env locally
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request
from collections import Counter
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from typing import Any

DEFAULT_MIMIR_URL = "http://10.0.65.8:9090"
DEFAULT_DAYS = 7
TIMELINE_ENDPOINT = "/api/v1/timeline"

# Heuristic categorisation thresholds.
TRIVIAL_MAX_CHARS = 60          # very short suggestions are usually noise
DUPLICATE_PREFIX_CHARS = 40     # group by leading 40 chars of effect
HALLUCINATED_KEYWORDS = (
    "perhaps",
    "maybe consider",
    "might want to",
    "could potentially",
    "in theory",
)
ACTIONABLE_HINTS = (
    "rename ",
    "extract ",
    "delete ",
    "add a test",
    "add tests",
    "fix ",
    "refactor ",
    "deduplicate",
    "remove the",
    "consolidate",
    "split ",
    "inline ",
)


@dataclass
class Engram:
    id: str
    cause: str
    effect: str
    tags: list[str]
    created_at: str
    rank_score: float | None = None  # extracted from tags or effect prefix


@dataclass
class Categorised:
    actionable: list[Engram] = field(default_factory=list)
    duplicate: list[Engram] = field(default_factory=list)
    hallucinated: list[Engram] = field(default_factory=list)
    trivial: list[Engram] = field(default_factory=list)

    def total(self) -> int:
        return (
            len(self.actionable)
            + len(self.duplicate)
            + len(self.hallucinated)
            + len(self.trivial)
        )


def fetch_engrams(mimir_url: str, days: int, token: str | None) -> list[Engram]:
    """POST to Mimir's /api/v1/timeline asking for self_improvement+pending engrams."""
    after = (datetime.now(timezone.utc) - timedelta(days=days)).isoformat()
    body = {
        "tags": ["self_improvement", "pending"],
        "after": after,
        "limit": 200,
    }
    payload = json.dumps(body).encode()

    url = mimir_url.rstrip("/") + TIMELINE_ENDPOINT
    req = urllib.request.Request(
        url,
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    if token:
        req.add_header("Authorization", f"Bearer {token}")

    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            raw = json.loads(resp.read().decode())
    except urllib.error.HTTPError as exc:
        body_text = exc.read().decode(errors="replace")
        sys.exit(f"ERROR: HTTP {exc.code} from {url}: {body_text[:200]}")
    except (urllib.error.URLError, OSError) as exc:
        sys.exit(f"ERROR: cannot reach {url}: {exc}")

    raw_list = raw.get("engrams", []) or raw.get("results", []) or []
    out: list[Engram] = []
    for r in raw_list:
        tags = r.get("tags") or []
        out.append(
            Engram(
                id=str(r.get("id", "")),
                cause=str(r.get("cause", "")),
                effect=str(r.get("effect", "")),
                tags=[str(t) for t in tags],
                created_at=str(r.get("created_at", "")),
                rank_score=_extract_rank(r),
            )
        )
    return out


def _extract_rank(raw: dict[str, Any]) -> float | None:
    """Pull a rank score out of `tags` (e.g. `priority:7`) or the effect prefix."""
    for t in raw.get("tags", []) or []:
        if isinstance(t, str) and t.startswith("priority:"):
            try:
                return float(t.split(":", 1)[1])
            except (ValueError, IndexError):
                pass
    effect = raw.get("effect", "") or ""
    # Some emitters prefix the effect with "[rank=N.NN]"
    if effect.startswith("[rank="):
        try:
            return float(effect[6 : effect.index("]")])
        except (ValueError, IndexError):
            pass
    return None


def categorise(engrams: list[Engram]) -> Categorised:
    out = Categorised()
    seen_prefixes: dict[str, int] = {}

    for e in engrams:
        text = e.effect.strip()

        if len(text) < TRIVIAL_MAX_CHARS:
            out.trivial.append(e)
            continue

        prefix = text[:DUPLICATE_PREFIX_CHARS].lower()
        seen_prefixes[prefix] = seen_prefixes.get(prefix, 0) + 1
        if seen_prefixes[prefix] > 1:
            out.duplicate.append(e)
            continue

        lower = text.lower()
        if any(kw in lower for kw in HALLUCINATED_KEYWORDS) and not any(
            hint in lower for hint in ACTIONABLE_HINTS
        ):
            out.hallucinated.append(e)
            continue

        if any(hint in lower for hint in ACTIONABLE_HINTS):
            out.actionable.append(e)
            continue

        # Default: treat ambiguous mid-length suggestions as trivial.
        out.trivial.append(e)

    return out


def recommend_threshold(cats: Categorised, current_threshold: float) -> dict[str, Any]:
    """Suggest a new rank-step threshold based on the actionable ratio.

    Heuristic:
      - actionable_ratio > 0.6 → quality is high, lower threshold to surface more.
      - actionable_ratio < 0.2 → quality is low, raise threshold to filter noise.
      - otherwise → keep current.
    """
    total = cats.total()
    if total == 0:
        return {
            "current": current_threshold,
            "suggested": current_threshold,
            "reason": "no engrams in window — cannot recommend",
        }
    actionable_ratio = len(cats.actionable) / total
    duplicate_ratio = len(cats.duplicate) / total
    hallu_ratio = len(cats.hallucinated) / total

    suggested = current_threshold
    reasons = []

    if actionable_ratio > 0.6:
        suggested = max(0.0, current_threshold - 0.05)
        reasons.append(
            f"actionable ratio high ({actionable_ratio:.0%}); lower threshold to surface more"
        )
    elif actionable_ratio < 0.2:
        suggested = min(1.0, current_threshold + 0.05)
        reasons.append(
            f"actionable ratio low ({actionable_ratio:.0%}); raise threshold to filter"
        )

    if hallu_ratio > 0.3 and suggested <= current_threshold:
        suggested = min(1.0, suggested + 0.05)
        reasons.append(f"hallucinated ratio high ({hallu_ratio:.0%}); raise threshold")

    if duplicate_ratio > 0.4:
        reasons.append(
            f"duplicate ratio high ({duplicate_ratio:.0%}); consider tightening dedup, not threshold"
        )

    return {
        "current": current_threshold,
        "suggested": round(suggested, 3),
        "actionable_ratio": round(actionable_ratio, 3),
        "duplicate_ratio": round(duplicate_ratio, 3),
        "hallucinated_ratio": round(hallu_ratio, 3),
        "reason": "; ".join(reasons) if reasons else "ratios within healthy band — keep current",
    }


def render_text(cats: Categorised, rec: dict[str, Any], days: int) -> str:
    lines = [
        f"=== dream_self_improvement review — last {days} days ===",
        f"Total engrams: {cats.total()}",
        f"  actionable:    {len(cats.actionable):>4}",
        f"  duplicate:     {len(cats.duplicate):>4}",
        f"  hallucinated:  {len(cats.hallucinated):>4}",
        f"  trivial:       {len(cats.trivial):>4}",
        "",
        f"Threshold recommendation:",
        f"  current:   {rec['current']}",
        f"  suggested: {rec['suggested']}",
        f"  reason:    {rec['reason']}",
    ]
    if cats.actionable:
        lines.extend(["", "Top actionable suggestions:"])
        for e in cats.actionable[:5]:
            preview = e.effect.strip().splitlines()[0][:120]
            lines.append(f"  [{e.id[:8]}] {preview}")
    return "\n".join(lines)


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--mimir", default=DEFAULT_MIMIR_URL)
    p.add_argument("--days", type=int, default=DEFAULT_DAYS)
    p.add_argument(
        "--token",
        default=os.environ.get("MIMIR_VAULT_CLIENT_TOKEN"),
        help="bearer token for Mimir vault auth (defaults to env MIMIR_VAULT_CLIENT_TOKEN)",
    )
    p.add_argument(
        "--current-threshold",
        type=float,
        default=0.7,
        help="current nemotron rank-step threshold (default 0.7)",
    )
    p.add_argument("--json", action="store_true", help="output JSON instead of text")
    args = p.parse_args()

    engrams = fetch_engrams(args.mimir, args.days, args.token)
    cats = categorise(engrams)
    rec = recommend_threshold(cats, args.current_threshold)

    if args.json:
        out = {
            "days": args.days,
            "counts": {
                "total": cats.total(),
                "actionable": len(cats.actionable),
                "duplicate": len(cats.duplicate),
                "hallucinated": len(cats.hallucinated),
                "trivial": len(cats.trivial),
            },
            "recommendation": rec,
            "actionable_ids": [e.id for e in cats.actionable],
        }
        print(json.dumps(out, indent=2))
    else:
        print(render_text(cats, rec, args.days))

    return 0


if __name__ == "__main__":
    sys.exit(main())
