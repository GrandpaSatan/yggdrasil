"""Mesh handshake + proxy — positive path and VULN-006 negative regression."""

from __future__ import annotations

import os

import pytest
import requests

from helpers.services import service_urls


def _mesh_target_url() -> str:
    """Resolve the URL serving /api/v1/mesh/hello.

    The handler lives on ygg-node (crates/ygg-node/src/handlers.rs:30), not on
    Odin. If YGG_NODE_URL is set we use it directly. Otherwise we fall back to
    ODIN_URL and *require* the deployment to expose the route there — anything
    else (404 in particular) is a real failure, not an environmental quirk.
    """
    explicit = os.environ.get("YGG_NODE_URL", "").strip()
    if explicit:
        return explicit.rstrip("/")
    return service_urls()["odin"].rstrip("/")


@pytest.mark.required_services("odin")
def test_mesh_hello_accepts_valid_handshake() -> None:
    """Hello with minimal valid payload — must return our MeshHello echo."""
    payload = {
        "node": {
            "name": "e2e-test-node",
            "address": "127.0.0.1",
            "port": 0,
            "version": "0.66.0",
        },
        "services": [],
    }
    resp = requests.post(
        f"{_mesh_target_url()}/api/v1/mesh/hello",
        json=payload,
        timeout=5,
    )
    assert resp.status_code in (200, 202), (
        f"mesh hello must return 200/202 against the configured node URL "
        f"(YGG_NODE_URL or ODIN_URL); got {resp.status_code}: {resp.text[:200]}"
    )
    body = resp.json()
    assert isinstance(body, dict), f"hello echo must be a JSON object; got {type(body).__name__}"
    assert "error" not in body, f"hello echo must not be an error payload; got {body}"
    # The handler returns our local MeshHello (registry.local_hello). At minimum
    # we expect a node identity field — name lives at body["node"]["name"].
    node = body.get("node")
    assert isinstance(node, dict) and node.get("name"), (
        f"hello echo must carry our node identity; got {body}"
    )


@pytest.mark.xfail(
    reason="VULN-006: mesh handshake accepts any node (no pre-shared key)",
    strict=True,
)
@pytest.mark.required_services("odin")
def test_mesh_forged_handshake_rejected() -> None:
    """Once VULN-006 is fixed, a handshake without a pre-shared key must 401."""
    payload = {
        "node": {"name": "forged", "address": "evil.example", "port": 0, "version": "0.0.0"},
        "services": [],
    }
    resp = requests.post(
        f"{_mesh_target_url()}/api/v1/mesh/hello",
        json=payload,
        timeout=5,
    )
    assert resp.status_code in (401, 403), (
        f"forged mesh handshake must be rejected, got {resp.status_code}"
    )
