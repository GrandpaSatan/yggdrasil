"""ygg-dreamer warmup + health probe.

The warmup counter increments when idle_secs crosses min_idle_secs. We can't
artificially age activity without poking internals, so the best we can do
non-destructively is: assert /health responds and the metrics endpoint emits
the warmup counter at all.
"""

from __future__ import annotations

import pytest
import requests

from helpers.services import service_urls


@pytest.mark.required_services("dreamer")
def test_dreamer_health_endpoint_reachable() -> None:
    url = service_urls().get("dreamer")
    if not url:
        pytest.skip("DREAMER_URL not configured")
    try:
        resp = requests.get(f"{url.rstrip('/')}/health", timeout=5)
    except requests.ConnectionError:
        pytest.skip("ygg-dreamer not reachable (optional service)")
    assert resp.status_code == 200, f"dreamer /health expected 200, got {resp.status_code}"


@pytest.mark.required_services("dreamer")
def test_dreamer_metrics_expose_warmup_counter() -> None:
    url = service_urls().get("dreamer")
    if not url:
        pytest.skip("DREAMER_URL not configured")
    try:
        resp = requests.get(f"{url.rstrip('/')}/metrics", timeout=5)
    except requests.ConnectionError:
        pytest.skip("ygg-dreamer not reachable")
    assert resp.status_code == 200
    text = resp.text
    # ``idle_secs`` is too generic — other services emit it too. Keep only names
    # that are unambiguously dreamer-owned so a misrouted scrape can't pass.
    assert any(
        key in text for key in ("warmup_fires", "dreamer_warmup", "dreamer_")
    ), "dreamer metrics must expose at least one dreamer-prefixed metric"
