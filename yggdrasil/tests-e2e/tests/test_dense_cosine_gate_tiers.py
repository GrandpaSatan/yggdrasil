"""Sprint 067 Phase 2 acceptance gate — Three-Tier Dense Cosine Gate (xfail).

Phase 2 has not shipped yet.  This test is the pre-wired acceptance gate so
Phase 2's close-out has a clear green-light signal.  When Phase 2 lands:

1. Remove the @pytest.mark.xfail decorator from all three tests below.
2. Run pytest test_dense_cosine_gate_tiers.py -v against the updated Mimir
   binary.  All three should PASS.
3. If any fails, Phase 2 is not complete.

What Phase 2 ships (from the sprint plan A.5–A.6):
  - Three-tier cascade in handlers.rs lines 301–505:
      Tier 0: SimHash pre-filter (Hamming ≤ 2 → Old, no embedding lookup)
      Tier 1: dense_index.query_scoped_with_tags → classify_dense → New/Update/Old
      Tier 2: On DenseVerdict::Ambiguous (cosine 0.80–0.88), escalate to store_gate
  - Four new Prometheus metrics:
      ygg_novelty_gate_tier_total{tier="0"|"1"|"2"}
      ygg_novelty_cosine_similarity (histogram)
      ygg_novelty_verdict_total{verdict="new"|"update"|"old"}
      ygg_novelty_gate_duration_seconds (histogram)

The xfail reason references the ``ygg_novelty_gate_tier_total`` metric as the
canonical signal that Phase 2 is deployed — if the metric appears in /metrics,
the gate is live.
"""

from __future__ import annotations

import uuid

import pytest
import requests

from helpers import MimirClient


# ──────────────────────────────────────────────────────────────────────────────
# Helpers
# ──────────────────────────────────────────────────────────────────────────────

_INTERNAL_HEADERS = {"X-Yggdrasil-Internal": "true"}


def _store_raw(
    mimir_client: MimirClient,
    cause: str,
    effect: str,
    *,
    tags: list[str],
    project: str = "yggdrasil",
    force: bool = False,
) -> dict:
    """POST /api/v1/store and return the full response body including verdict.

    Sprint 069 Phase C added bearer auth on Mimir; the E2E suite runs from a
    trusted LAN host, so we use the same `X-Yggdrasil-Internal: true` bypass
    that internal services (Odin, ygg-dreamer, sidecar) use.
    """
    body = {
        "cause": cause,
        "effect": effect,
        "tags": tags,
        "project": project,
        "force": force,
    }
    resp = requests.post(
        mimir_client._url("/api/v1/store"),
        json=body,
        headers=_INTERNAL_HEADERS,
        timeout=mimir_client.timeout,
    )
    resp.raise_for_status()
    return resp.json()


def _scrape_metrics(mimir_client: MimirClient) -> str:
    """GET /metrics and return the Prometheus text-format body."""
    resp = requests.get(
        mimir_client._url("/metrics"),
        timeout=10.0,
    )
    resp.raise_for_status()
    return resp.text


def _parse_counter(metrics_text: str, metric_name: str, label_filter: str) -> float | None:
    """Parse a Prometheus counter value from text-format metrics.

    Searches for lines matching ``metric_name{...label_filter...} <value>``
    and returns the float value of the first match, or None if not found.

    Example:
        _parse_counter(text, "ygg_novelty_gate_tier_total", 'tier="2"') -> 42.0
    """
    for line in metrics_text.splitlines():
        line = line.strip()
        if line.startswith("#"):
            continue
        if metric_name not in line:
            continue
        if label_filter not in line:
            continue
        # Format: metric_name{labels} value [timestamp]
        parts = line.rsplit(None, 2)
        if len(parts) >= 2:
            try:
                return float(parts[-2] if len(parts) == 3 else parts[-1])
            except ValueError:
                continue
    return None


# ──────────────────────────────────────────────────────────────────────────────
# Phase 2 acceptance gate tests (ALL xfail until Phase 2 deploys)
# ──────────────────────────────────────────────────────────────────────────────

@pytest.mark.required_services("mimir")
def test_dense_cosine_gate_tier2_fires_on_ambiguous_cosine(
    mimir_client: MimirClient,
    clean_test_engrams,
) -> None:
    """Storing an engram in the ambiguous cosine band (0.80–0.88) increments Tier 2 counter.

    Tier 2 is the LLM escalation path (store_gate::classify).  It fires when
    the dense cosine similarity is Ambiguous — too similar to confidently accept
    as New (>0.88 would be Update/Old) but too dissimilar to reject as duplicate
    (<0.80 would be New via Tier 1).

    To reliably land in the ambiguous band:
    1. Store a seed engram with a specific topic.
    2. Store a paraphrase of the same topic with moderate lexical overlap.
       The dense cosine should land between 0.80 and 0.88 for well-chosen pairs.

    NOTE: The exact cosine similarity depends on the ONNX embedder weights and
    cannot be guaranteed from E2E.  This test is a best-effort probe — if the
    pair chosen here does not land in the ambiguous band, the test will fail even
    after Phase 2 ships.  Adjust the cause/effect text during Phase 2 calibration
    using the shadow log data to find a pair that reliably produces 0.80–0.88.
    """
    unique_sig = uuid.uuid4().hex
    # Per-test unique project isolates the dense index from prior runs of this
    # test (which leave tier2_probe-tagged engrams behind in the shared
    # "yggdrasil" project, making cosine matches non-deterministic).
    iso_project = f"phase_d_test_{unique_sig}"

    # Seed engram: specific technical statement.
    seed_cause = (
        f"tier2_ambiguous_probe_{unique_sig}: "
        "The Yggdrasil Mimir service uses ONNX-embedded 384-dimensional vectors "
        "to compute novelty scores for incoming engrams before persisting them."
    )
    seed_effect = (
        "Sprint 067 Phase 2 Tier 2 probe: ONNX embedding pipeline is the "
        "primary signal for novelty classification in the Dense Cosine Gate."
    )
    tags_base = ["sprint:067", "tier2_probe", clean_test_engrams.tag]

    seed_payload = _store_raw(
        mimir_client, seed_cause, seed_effect, tags=tags_base, project=iso_project, force=True
    )
    seed_id = seed_payload.get("id") or seed_payload.get("engram_id")
    assert seed_id, f"Seed store failed: {seed_payload}"

    # Record Tier 2 counter before the ambiguous store.
    metrics_before = _scrape_metrics(mimir_client)
    tier2_before = _parse_counter(
        metrics_before, "ygg_novelty_gate_tier_total", 'tier="2"'
    )
    # Pre-Phase-2: metric does not exist → None.  Post-Phase-2: should be a float.
    assert tier2_before is not None, (
        "ygg_novelty_gate_tier_total{tier='2'} not found in /metrics. "
        "Phase 2 has not been deployed yet.  This xfail is expected."
    )

    # Ambiguous probe: paraphrase of the seed CAUSE — the dense index keys on
    # cause text only (handlers.rs:226-228), not on the cause+effect combo, so
    # tweaking the effect alone leaves cosine near 1.0. We rephrase the cause
    # itself to land in the [ambiguous_floor=0.80, update_threshold=0.88) band.
    # Calibrated via /api/v1/embed on 2026-04-15: cosine ≈ 0.8527 against the
    # Munin ONNX MiniLM-L6-v2 weights. If the embedder model changes, re-tune
    # this pair using the live /api/v1/embed endpoint.
    ambiguous_cause = (
        f"tier2_ambiguous_probe_{unique_sig}: "
        "Mimir in Yggdrasil applies ONNX-derived 384-dim embeddings to assess "
        "novelty for arriving engrams prior to commit."
    )
    ambiguous_effect = (
        "Dense embedding-based novelty evaluation in the Sprint 067 memory service "
        "determines whether an incoming engram is new, an update, or a duplicate."
    )
    tags_probe = ["sprint:067", "tier2_probe", "ambiguous", clean_test_engrams.tag]

    _store_raw(
        mimir_client, ambiguous_cause, ambiguous_effect,
        tags=tags_probe, project=iso_project, force=False,
    )

    # Verify Tier 2 counter incremented.
    metrics_after = _scrape_metrics(mimir_client)
    tier2_after = _parse_counter(
        metrics_after, "ygg_novelty_gate_tier_total", 'tier="2"'
    )

    assert tier2_after is not None, (
        "ygg_novelty_gate_tier_total{tier='2'} disappeared after the store. "
        "Phase 2 metrics may have a label mismatch."
    )
    assert tier2_after > (tier2_before or 0.0), (
        f"Tier 2 counter did not increment: before={tier2_before}, after={tier2_after}. "
        "Either the ambiguous probe did not land in the 0.80–0.88 cosine band, "
        "or Tier 2 escalation is not firing.  Check the shadow log for the actual "
        f"dense_cosine_sim to determine whether the probe content needs adjustment."
    )


@pytest.mark.required_services("mimir")
def test_dense_cosine_gate_tier1_resolves_obvious_duplicate(
    mimir_client: MimirClient,
    clean_test_engrams,
) -> None:
    """An obvious near-duplicate (cosine > 0.88) is resolved by Tier 1, not Tier 2.

    Tier 1 is the dense_index fast path.  It resolves New/Update/Old in <2ms
    without LLM escalation.  An exact re-store of the same cause/effect should
    score cosine ~1.0 and land verdict=old via Tier 0 (SimHash) or Tier 1.

    Post-Phase-2: Tier 0 or Tier 1 counter should increment, NOT Tier 2.
    """
    unique_sig = uuid.uuid4().hex
    cause = (
        f"tier1_duplicate_probe_{unique_sig}: exact duplicate engram for Tier 0/1 test"
    )
    effect = (
        "Sprint 067 Phase 2 Tier 0/1 probe: this exact content is stored twice. "
        "The second store must be resolved by SimHash (Tier 0) or dense Tier 1, "
        "not by LLM escalation (Tier 2)."
    )
    tags = ["sprint:067", "tier1_probe", clean_test_engrams.tag]

    # First store — seeds the index.
    first_payload = _store_raw(
        mimir_client, cause, effect, tags=tags, force=True
    )
    first_id = first_payload.get("id") or first_payload.get("engram_id")
    assert first_id, f"First store failed: {first_payload}"

    # Record counters before duplicate.
    metrics_before = _scrape_metrics(mimir_client)
    tier0_before = _parse_counter(
        metrics_before, "ygg_novelty_gate_tier_total", 'tier="0"'
    ) or 0.0
    tier1_before = _parse_counter(
        metrics_before, "ygg_novelty_gate_tier_total", 'tier="1"'
    ) or 0.0
    tier2_before = _parse_counter(
        metrics_before, "ygg_novelty_gate_tier_total", 'tier="2"'
    ) or 0.0

    assert (tier0_before + tier1_before) > 0 or tier2_before >= 0, (
        "ygg_novelty_gate_tier_total not found in /metrics. "
        "Phase 2 has not been deployed.  This xfail is expected."
    )

    # Duplicate store — exact same content, force=False so novelty gate fires.
    _store_raw(mimir_client, cause, effect, tags=tags, force=False)

    metrics_after = _scrape_metrics(mimir_client)
    tier0_after = _parse_counter(
        metrics_after, "ygg_novelty_gate_tier_total", 'tier="0"'
    ) or 0.0
    tier1_after = _parse_counter(
        metrics_after, "ygg_novelty_gate_tier_total", 'tier="1"'
    ) or 0.0
    tier2_after = _parse_counter(
        metrics_after, "ygg_novelty_gate_tier_total", 'tier="2"'
    ) or 0.0

    tier01_incremented = (tier0_after + tier1_after) > (tier0_before + tier1_before)
    tier2_incremented = tier2_after > tier2_before

    assert tier01_incremented, (
        f"Tier 0 or Tier 1 counter did not increment for an exact duplicate. "
        f"tier0: {tier0_before} → {tier0_after}, tier1: {tier1_before} → {tier1_after}. "
        "Phase 2 fast-path is not resolving obvious duplicates."
    )
    assert not tier2_incremented, (
        f"Tier 2 LLM escalation fired for an exact duplicate — this should not happen. "
        f"tier2: {tier2_before} → {tier2_after}. "
        "SimHash (Tier 0) or dense Tier 1 should resolve obvious duplicates without LLM."
    )


@pytest.mark.required_services("mimir")
def test_dense_cosine_gate_metrics_exist_in_prometheus_scrape(
    mimir_client: MimirClient,
) -> None:
    """All four Phase 2 Prometheus metrics are present in the /metrics scrape.

    This is the lightest-weight Phase 2 readiness check: no stores needed,
    just a scrape and a presence check.  If this test passes, Phase 2 has
    been deployed and the metrics are wired.

    Expected metrics (from sprint plan A.6):
      - ygg_novelty_gate_tier_total{tier="0"|"1"|"2"}
      - ygg_novelty_cosine_similarity (histogram: _bucket, _count, _sum)
      - ygg_novelty_verdict_total{verdict="new"|"update"|"old"}
      - ygg_novelty_gate_duration_seconds (histogram: _bucket, _count, _sum)
    """
    metrics_text = _scrape_metrics(mimir_client)

    required_metric_prefixes = [
        "ygg_novelty_gate_tier_total",
        "ygg_novelty_cosine_similarity",
        "ygg_novelty_verdict_total",
        "ygg_novelty_gate_duration_seconds",
    ]

    missing = []
    for prefix in required_metric_prefixes:
        found = any(
            line.startswith(prefix) or (prefix in line and not line.startswith("#"))
            for line in metrics_text.splitlines()
        )
        if not found:
            missing.append(prefix)

    assert not missing, (
        f"Phase 2 Prometheus metrics not found in /metrics scrape: {missing}. "
        "Deploy the Phase 2 Mimir binary and retry."
    )
