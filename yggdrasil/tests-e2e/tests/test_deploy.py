"""Build check + deploy — non-destructive probe + destructive push (gated)."""

from __future__ import annotations

import pytest
import requests

from helpers.services import service_urls


@pytest.mark.required_services("odin")
def test_build_check_returns_diagnostics_shape() -> None:
    """POST /api/v1/build_check must return the cargo output envelope.

    Request schema matches ``BuildCheckRequest`` in odin/src/handlers.rs::2822:
    ``{"mode": "check"|"build"|"clippy"|"test", "package": "<optional>"}``.
    Response on 200 is ``{"success": bool, "exit_code": int, "stdout": str, "stderr": str}``.
    A 404 on the route or a schema mismatch (422/400) is a real regression, not a skip.
    The only legitimate skip is a host without cargo on PATH — we match that narrowly.
    """
    url = service_urls()["odin"].rstrip("/")
    payload = {"mode": "check", "package": "ygg-domain"}
    resp = requests.post(f"{url}/api/v1/build_check", json=payload, timeout=180)
    # Narrow environmental skip: the handler returns 500 with a specific cargo
    # launch failure when the host lacks cargo on PATH (documented Munin gap).
    if resp.status_code == 500:
        text = resp.text.lower()
        if '"failed to run cargo' in text and ("no such file" in text or "not found" in text):
            pytest.skip(
                "cargo not on PATH for Odin host (Munin) — known gap, run from dev workstation instead"
            )
    # 504 is the handler's own timeout wrapper (120s cargo budget); still a real failure.
    assert resp.status_code == 200, (
        f"build_check must return 200; got {resp.status_code}: {resp.text[:300]}"
    )
    body = resp.json()
    assert isinstance(body, dict), f"build_check response must be a JSON object; got {type(body).__name__}"
    # The handler returns {success, exit_code, stdout, stderr} on OK path. Verify
    # each field by type, not just presence — an {"error": "..."} payload would
    # otherwise sneak past a presence-only check.
    assert isinstance(body.get("success"), bool), (
        f"build_check.success must be a bool; got {body!r}"
    )
    assert isinstance(body.get("exit_code"), int) or body.get("exit_code") is None, (
        f"build_check.exit_code must be an int or null; got {body!r}"
    )
    for field in ("stdout", "stderr"):
        assert isinstance(body.get(field), str), (
            f"build_check.{field} must be a string; got {body!r}"
        )
    # `cargo check -p ygg-domain` with a known-good workspace should succeed.
    # If the workspace has real compile errors, surface them in the failure
    # message so the operator sees the actual cargo output instead of a vague skip.
    if not body["success"]:
        tail = body["stderr"][-600:] if body["stderr"] else "(no stderr)"
        pytest.fail(
            f"cargo check failed (exit_code={body.get('exit_code')}). stderr tail:\n{tail}"
        )


@pytest.mark.destructive
@pytest.mark.required_services("odin")
def test_deploy_dry_run_returns_artifact_path(require_destructive) -> None:
    """Deploy with dry_run=true — should compute the artifact path without pushing."""
    url = service_urls()["odin"].rstrip("/")
    payload = {"binary": "ygg-node", "target": "munin", "dry_run": True}
    resp = requests.post(f"{url}/api/v1/deploy", json=payload, timeout=120)
    if resp.status_code == 404:
        pytest.skip("/api/v1/deploy not exposed")
    assert resp.status_code in (200, 202)
    body = resp.json()
    # Must return a path the server COMPUTED — not an echo of the request.
    # The old ``body.get("binary")`` fallback passed on a handler that just
    # parroted the input payload back ("binary": "ygg-node") without doing any
    # real build work.
    artifact = body.get("artifact_path") or body.get("path")
    assert isinstance(artifact, str) and "/" in artifact, (
        f"dry-run must return a computed artifact path containing '/'; got {body!r}"
    )
