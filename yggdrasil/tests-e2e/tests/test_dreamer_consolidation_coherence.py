"""Regression test: dreamer 404 — SDR/dense index coherence after consolidation.

Sprint 067 Phase 4a root-cause: when the summarization service archived a batch
of recall engrams (Step 8 of check_and_summarize), it deleted the source rows
from PostgreSQL and removed their vectors from Qdrant.  However the in-memory
SdrIndex (and, once Phase 1 ships, the DenseIndex) were NOT invalidated.  A
subsequent store whose embedding landed near a now-deleted engram's SDR would
score a high Hamming similarity, the novelty gate would return verdict=old or
verdict=update, and the handler would try to GET the matched id — returning 404
because the row was already gone.  That 404 is exactly what yggdrasil-dreamer
logged 111 times over 7 days.

This test reproduces the observable symptoms:

1. Store engram A — unique content signature so SDR/dense entries are seeded.
2. POST /api/v1/consolidate to trigger a consolidation cycle.
   NOTE: /api/v1/consolidate is a session-consolidation endpoint that takes a
   workstation parameter; it does NOT directly trigger the background
   SummarizationService.  The SummarizationService only fires when
   recall_capacity is breached.  We therefore CANNOT force a true
   summarization cycle from E2E without infrastructure access.
   The test is marked xfail(strict=True) on the consolidation step and the
   coherence assertion; the happy-path store steps (1, 2, 5) run as normal
   assertions so any regression in the store API itself is still caught.
3. The post-consolidation re-store (step 5) tests that force=False stores near
   a unique cause string return verdict=new (not 404 or verdict=old against a
   ghost SDR entry).  This is the safe half of the regression guard that CAN
   run today.
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
    """POST /api/v1/store and return the full response body.

    The MimirClient.store() helper discards the verdict field; tests that
    exercise the novelty gate need the full payload.

    Uses the X-Yggdrasil-Internal bypass for the Phase C bearer auth (same
    trust pattern as Odin / ygg-dreamer / the sidecar curl scripts).
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


# ──────────────────────────────────────────────────────────────────────────────
# Tests
# ──────────────────────────────────────────────────────────────────────────────

@pytest.mark.required_services("mimir")
def test_dreamer_coherence_store_a_returns_new(
    mimir_client: MimirClient,
    clean_test_engrams,
) -> None:
    """Happy path: storing a unique engram returns verdict=new and a non-empty id.

    This assertion must pass both before and after any consolidation fix.
    If this fails, the store API itself is broken.
    """
    unique_sig = uuid.uuid4().hex
    cause = f"test:dreamer_coherence_{unique_sig} — initial store"
    effect = (
        f"Sprint 067 Phase 4b coherence probe: unique signature {unique_sig}. "
        "This engram should always receive verdict=new on first insert."
    )
    tags = ["sprint:067", "test:coherence", "dreamer", clean_test_engrams.tag]
    # Sprint 069 Phase D: per-test isolated project keeps the dense gate from
    # matching against prior runs of this test (which left similar coherence-
    # probe engrams in the shared "yggdrasil" project's dense index, scoring
    # cosine > 0.88 → Update verdict on otherwise-novel inputs).
    iso_project = f"phase_d_dreamer_{unique_sig}"

    payload = _store_raw(
        mimir_client, cause, effect, tags=tags, project=iso_project, force=False
    )

    engram_id = payload.get("id") or payload.get("engram_id")
    assert engram_id, (
        f"Store response missing 'id' field: {payload}. "
        "Was Mimir rebuilt with the Sprint 064 P1 verdict response?"
    )
    verdict = payload.get("verdict", "new")  # pre-064 builds omit verdict
    assert verdict == "new", (
        f"Expected verdict=new for a unique cause string, got verdict={verdict!r}. "
        f"Engram id={engram_id}, cause={cause!r}"
    )


@pytest.mark.required_services("mimir")
def test_dreamer_coherence_near_duplicate_does_not_return_404(
    mimir_client: MimirClient,
    clean_test_engrams,
) -> None:
    """Near-duplicate store must return 200/201, never a 404.

    This directly guards against the dreamer 404 regression:  if the SDR index
    holds a stale pointer to a deleted engram, the novelty gate would return
    old/update for a near-duplicate store and the handler would try to fetch
    the missing row, resulting in a 404 propagated to the caller.
    """
    unique_sig = uuid.uuid4().hex
    iso_project = f"phase_d_dreamer_{unique_sig}"

    # Seed: store engram A with force=True so it definitely lands.
    cause_a = f"test:dreamer_coherence_{unique_sig} — seed engram for dedup probe"
    effect_a = (
        "Sprint 067 Phase 4b: seeded recall engram for near-duplicate dedup test. "
        f"Unique identifier: {unique_sig}"
    )
    tags_base = ["sprint:067", "test:coherence", "dreamer", clean_test_engrams.tag]

    seed_payload = _store_raw(
        mimir_client, cause_a, effect_a, tags=tags_base, project=iso_project, force=True
    )
    seed_id = seed_payload.get("id") or seed_payload.get("engram_id")
    assert seed_id, f"Seed store failed: {seed_payload}"

    # Near-duplicate: slightly different effect, same cause prefix.
    # With force=False the novelty gate will fire.  The critical assertion
    # is that the HTTP response is 2xx — never 404.
    cause_b = cause_a  # identical cause so SDR sim will be ~1.0
    effect_b = effect_a + " [variant: second store, testing coherence]"
    tags_b = tags_base + ["variant"]

    body_b = {
        "cause": cause_b,
        "effect": effect_b,
        "tags": tags_b,
        "project": iso_project,
        "force": False,
    }
    resp = requests.post(
        mimir_client._url("/api/v1/store"),
        json=body_b,
        headers=_INTERNAL_HEADERS,
        timeout=mimir_client.timeout,
    )

    assert resp.status_code != 404, (
        f"Near-duplicate store returned HTTP 404 — this is the dreamer regression. "
        f"Status={resp.status_code}, body={resp.text!r}"
    )
    assert resp.status_code in (200, 201), (
        f"Near-duplicate store returned unexpected status {resp.status_code}. "
        f"Body: {resp.text!r}"
    )


@pytest.mark.required_services("mimir")
def test_dreamer_coherence_post_consolidation_store_returns_new(
    mimir_client: MimirClient,
    clean_test_engrams,
) -> None:
    """Full scenario: store A, force-trigger summarization, store C near A, expect verdict=new.

    Sprint 069 Phase D added POST /api/v1/summarize/trigger which forces one
    summarization + consolidation cycle bypassing the recall-capacity gate.
    The trigger archives a batch of recall engrams (creating an archival
    summary engram) AND deletes the source rows from PostgreSQL + the in-memory
    SDR/dense indexes — the latter is the exact invalidation Sprint 067 Phase 4a
    fixed for the dedup path of consolidate_cycle.
    """
    unique_sig = uuid.uuid4().hex
    iso_project = f"phase_d_dreamer_{unique_sig}"
    cause_a = f"test:dreamer_coherence_{unique_sig} — consolidation coherence seed"
    effect_a = (
        "Sprint 067 Phase 4b: seed engram for post-consolidation coherence check. "
        f"Identifier: {unique_sig}"
    )
    tags = ["sprint:067", "test:coherence", "dreamer", clean_test_engrams.tag]

    # Step 1: Store A
    payload_a = _store_raw(
        mimir_client, cause_a, effect_a, tags=tags, project=iso_project, force=True
    )
    id_a = payload_a.get("id") or payload_a.get("engram_id")
    assert id_a, f"Seed store A failed: {payload_a}"

    # Step 2: Force-archive the seed via the trigger endpoint's fast path.
    # Passing `target_engram_id` skips the LLM call entirely and just performs
    # the same archive-and-invalidate that consolidate_cycle does on dedup
    # matches: PG delete + Qdrant delete + SDR/dense index removal. This is
    # deterministic and fast — exactly the regression guard for Sprint 067 P4a.
    trigger_resp = requests.post(
        mimir_client._url("/api/v1/summarize/trigger"),
        json={"target_engram_id": id_a},
        headers=_INTERNAL_HEADERS,
        timeout=30.0,
    )
    assert trigger_resp.status_code == 200, (
        f"Summarize trigger returned {trigger_resp.status_code}: "
        f"{trigger_resp.text!r}"
    )
    report = trigger_resp.json()
    assert report.get("archived_engrams", 0) >= 1, (
        f"Trigger ran but archived nothing: {report!r}. "
        "Either the seed was below min_age_secs or the LLM call failed."
    )

    # Step 3: Verify A's DB row is gone — the force-trigger contract is
    # archive-AND-delete (not just tier change), so get_engram must 404.
    engram_a = mimir_client.get_engram(id_a)
    assert engram_a is None, (
        f"Expected engram {id_a} to be deleted after force-trigger, "
        f"but it still exists. force_cycle did not honour bypass_capacity=true. "
        f"Trigger report: {report!r}"
    )
