"""Gaming orchestrator — list (read-only) + launch (destructive, gated).

Request schema matches ``GamingRequest`` in odin/src/handlers.rs::2490 —
POST /api/v1/gaming with ``{"action": "status"|"launch"|..., "vm_name": "..."}``.
Status response: ``SystemStatus { hosts: [HostStatus { name, online, vms, containers }] }``.
Launch response: externally-tagged ``LaunchResult`` enum
(e.g. ``{"Started": {"vm_name", "host", "gpu_name", "ip"}}`` or ``{"AlreadyRunning": {...}}``).
"""

from __future__ import annotations

import os

import pytest
import requests

from helpers.services import service_urls


@pytest.mark.required_services("odin")
def test_gaming_status_list_readonly() -> None:
    """POST /api/v1/gaming with {"action": "status"} — read-only status dump."""
    url = service_urls()["odin"].rstrip("/")
    resp = requests.post(
        f"{url}/api/v1/gaming",
        json={"action": "status"},
        timeout=10,
    )
    if resp.status_code == 503:
        # Handler returns 503 when no gaming_config is loaded — genuine optional
        # deployment gap, skip with a precise marker (not a generic catchall).
        detail = resp.text[:200]
        pytest.skip(f"Odin built without gaming config: {detail}")
    assert resp.status_code == 200, (
        f"gaming status must return 200; got {resp.status_code}: {resp.text[:200]}"
    )
    body = resp.json()
    assert isinstance(body, dict), f"status must be a JSON object; got {type(body).__name__}"
    assert "error" not in body, f"status must not be an error payload; got {body}"
    hosts = body.get("hosts")
    assert isinstance(hosts, list), f"status.hosts must be a list; got {body!r}"
    assert len(hosts) >= 1, "at least one host must be registered in the gaming config"
    # Each host must carry the HostStatus shape: name, online, vms, containers.
    for host in hosts:
        assert isinstance(host, dict), f"host entry must be a dict; got {type(host).__name__}"
        missing = {"name", "online", "vms", "containers"} - host.keys()
        assert not missing, f"host entry missing required keys {missing}: {host!r}"
        assert isinstance(host["name"], str) and host["name"], f"host name must be non-empty; got {host!r}"
        assert isinstance(host["online"], bool), f"host.online must be bool; got {host!r}"
        assert isinstance(host["vms"], list), f"host.vms must be a list; got {host!r}"
        assert isinstance(host["containers"], list), f"host.containers must be a list; got {host!r}"


@pytest.mark.destructive
@pytest.mark.required_services("odin")
def test_gaming_launch_returns_vm_running(require_destructive) -> None:
    """REAL VM launch via Proxmox — only when all three gates open.

    This spends real energy and ties up a GPU. The target VM name must come
    from the live gaming config; override via ``E2E_GAMING_VM`` if the default
    isn't present in the deployment.
    """
    url = service_urls()["odin"].rstrip("/")
    # Require explicit VM selection — hardcoding ``gaming-thor`` coupled the
    # test to one specific fleet topology. Operators running the test on a
    # different fleet would launch a non-existent VM and get a confusing 404.
    vm_name = os.environ.get("E2E_GAMING_VM", "").strip()
    if not vm_name:
        pytest.skip(
            "E2E_GAMING_VM not set — point this at a VM name that exists in "
            "your live gaming config (e.g. 'gaming-thor' on the Yggdrasil fleet)"
        )
    resp = requests.post(
        f"{url}/api/v1/gaming",
        json={"action": "launch", "vm_name": vm_name},
        timeout=120,
    )
    if resp.status_code == 503:
        pytest.skip(f"Odin built without gaming config: {resp.text[:200]}")
    assert resp.status_code == 200, (
        f"launch must return 200; got {resp.status_code}: {resp.text[:200]}"
    )
    body = resp.json()
    assert isinstance(body, dict), f"LaunchResult must be a JSON object; got {type(body).__name__}"
    assert "error" not in body, f"launch returned an error payload: {body}"
    # LaunchResult is externally-tagged — exactly one variant key.
    variants = {"Started", "AlreadyRunning", "ServerOffline", "NoGpuAvailable"}
    present = variants & body.keys()
    assert len(present) == 1, (
        f"LaunchResult must carry exactly one variant key from {variants}; got {list(body)}"
    )
    variant = next(iter(present))
    # Only Started / AlreadyRunning are success states. The others are explicit
    # failures that must surface as test failures (not silent accepts).
    assert variant in ("Started", "AlreadyRunning"), (
        f"launch did not bring the VM up: got {variant} variant with payload {body[variant]!r}"
    )
    inner = body[variant]
    assert isinstance(inner, dict) and inner.get("vm_name") == vm_name, (
        f"launch response must echo the requested vm_name; got {body!r}"
    )
    if variant == "Started":
        gpu_name = inner.get("gpu_name")
        assert isinstance(gpu_name, str) and gpu_name, (
            f"Started launch must report a non-empty gpu_name; got {inner!r}"
        )
