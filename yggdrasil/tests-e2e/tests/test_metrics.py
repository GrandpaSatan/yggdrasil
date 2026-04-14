"""Prometheus /metrics endpoints — reachable, parseable, expected counters present."""

from __future__ import annotations

import pytest
import requests

from helpers import OdinClient
from helpers.services import service_urls


@pytest.mark.required_services("odin")
def test_odin_metrics_text_format(odin_client: OdinClient) -> None:
    text = odin_client.metrics_text()
    assert text, "/metrics must return a non-empty payload"
    # Prometheus text exposition format starts each counter/gauge with # HELP / # TYPE
    # or the metric line itself. At minimum we expect *some* HELP lines.
    assert "# HELP" in text or "# TYPE" in text, (
        "metrics output does not look like Prometheus text format"
    )


@pytest.mark.required_services("odin")
def test_odin_e2e_hits_counter_increments(odin_client: OdinClient) -> None:
    """POST /api/v1/e2e/hit must strictly increment odin_e2e_hits_total.

    This is the same ping emitted by e2e-cron-wrapper.sh (Sprint 064 P8) — it
    lets the daily timer register activity in Prometheus. The endpoint and
    counter both ship in current Odin builds, so a missing route or counter
    is a real deployment regression — not an environmental quirk to skip past.
    Strictness: we read the counter *before* and *after* the hit and require
    after > before, so a counter that's stuck (or that resets on every scrape)
    fails the test instead of passing on a stale value.
    """
    before = _counter_value(odin_client.metrics_text(), "odin_e2e_hits_total") or 0.0

    status = odin_client.e2e_hit()
    assert status in (200, 202, 204), (
        f"/api/v1/e2e/hit must succeed (200/202/204); got {status}. "
        "404 = endpoint missing (Sprint 064 P8 deployment regression); "
        "5xx = handler broken — both must fail loudly."
    )

    after = _counter_value(odin_client.metrics_text(), "odin_e2e_hits_total")
    assert after is not None, (
        "odin_e2e_hits_total counter is missing from /metrics output. "
        "The ping was accepted but no counter was registered — that's a "
        "metrics-pipeline regression in this Odin build."
    )
    assert after > before, (
        f"counter did not increment after a hit: before={before}, after={after}. "
        "Either the handler is not incrementing or scrapes are reading a stale value."
    )


@pytest.mark.required_services("mimir")
def test_mimir_metrics_reachable() -> None:
    url = service_urls()["mimir"]
    resp = requests.get(f"{url.rstrip('/')}/metrics", timeout=5)
    assert resp.status_code == 200, f"mimir /metrics must be 200, got {resp.status_code}"


def _counter_value(text: str, name: str) -> float | None:
    """Minimal Prometheus parser — find ``name{...} VALUE [TIMESTAMP]``.

    Handles four shapes:
      ``name VALUE``                      — no labels, no timestamp
      ``name VALUE TS``                   — no labels, trailing timestamp
      ``name{a="b"} VALUE``               — labels, no timestamp
      ``name{a="b"} VALUE TS``            — labels + trailing timestamp

    Prometheus allows a unix-millisecond timestamp after the value. The
    previous implementation used ``rsplit(" ", 1)[-1]`` and mis-read the
    timestamp as the value. We isolate the metric name (stripping any label
    suffix) and then take the FIRST numeric token after it.
    """
    for line in text.splitlines():
        if line.startswith("#") or not line.strip():
            continue
        tokens = line.split()
        if not tokens:
            continue
        # First token is ``name`` or ``name{labels}`` — strip the label suffix
        # for matching. The Prometheus text format forbids unquoted spaces
        # inside labels, so ``tokens[0]`` always contains the full name{labels}
        # portion and the value is at ``tokens[1]``.
        first = tokens[0]
        name_part = first.split("{", 1)[0]
        if name_part != name:
            continue
        tail_tokens = tokens[1:]
        if not tail_tokens:
            continue
        try:
            return float(tail_tokens[0])
        except ValueError:
            continue
    return None
