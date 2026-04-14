"""Context offload — store a large blob and round-trip by handle."""

from __future__ import annotations

import pytest
import requests

from helpers.services import service_urls


@pytest.mark.required_services("mimir")
def test_context_offload_retrieve_roundtrip(run_scope) -> None:
    url = service_urls()["mimir"].rstrip("/")
    # Use a run-unique payload so the byte-equality check below doesn't false-match
    # against residue from earlier runs that happened to contain the same content.
    blob = f"probe-{run_scope.run_id}-" + ("a" * 2048)
    handle: str | None = None
    try:
        push = requests.post(
            f"{url}/api/v1/context",
            json={"content": blob, "label": f"e2e-offload-{run_scope.run_id[:8]}"},
            timeout=10,
        )
        if push.status_code == 404:
            pytest.skip("context offload endpoint not present on this Mimir build")
        assert push.status_code in (200, 201), f"context push got {push.status_code}"
        handle = push.json().get("handle") or push.json().get("id")
        assert handle, "offload must return a handle"

        pull = requests.get(f"{url}/api/v1/context/{handle}", timeout=10)
        assert pull.status_code == 200
        # Require STRUCTURED content — the ``or pull.text`` fallback previously
        # checked against the raw JSON body, which just tested serialization of
        # the blob into the response envelope (trivial). And ``blob in content``
        # allowed partial matches; a server returning only a prefix would pass.
        payload = pull.json()
        assert isinstance(payload, dict) and "content" in payload, (
            f"context GET must return a dict with a 'content' key; got keys {list(payload) if isinstance(payload, dict) else type(payload).__name__}"
        )
        assert payload["content"] == blob, (
            f"retrieved content must EXACTLY match stored blob; got "
            f"{len(payload['content'])} chars vs expected {len(blob)}"
        )
    finally:
        # L-4: explicit cleanup — context blobs would otherwise accumulate on
        # the live fleet across runs. 404/405 are acceptable (already gone or
        # endpoint doesn't support delete).
        if handle:
            try:
                requests.delete(f"{url}/api/v1/context/{handle}", timeout=5)
            except requests.RequestException:
                pass
