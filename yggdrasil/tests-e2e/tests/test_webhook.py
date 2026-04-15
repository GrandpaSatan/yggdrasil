"""HA webhook — VULN-007 HMAC signature verification.

Today the webhook accepts any JSON with no signature check. These xfail tests
become the acceptance gate for VULN-007 remediation.
"""

from __future__ import annotations

import pytest
import requests

from helpers.services import service_urls


@pytest.mark.required_services("odin")
def test_webhook_rejects_unsigned_payload() -> None:
    url = service_urls()["odin"].rstrip("/")
    resp = requests.post(
        f"{url}/api/v1/webhook",
        json={"event": "motion_detected", "source": "unauthenticated"},
        timeout=5,
    )
    assert resp.status_code in (401, 403), (
        f"unsigned webhook must be rejected, got {resp.status_code}"
    )


@pytest.mark.required_services("odin")
def test_webhook_rejects_bad_signature() -> None:
    url = service_urls()["odin"].rstrip("/")
    resp = requests.post(
        f"{url}/api/v1/webhook",
        json={"event": "motion_detected"},
        headers={"X-Yggdrasil-Signature": "sha256=deadbeef"},
        timeout=5,
    )
    assert resp.status_code in (401, 403), (
        f"webhook with bad HMAC must be rejected, got {resp.status_code}"
    )
