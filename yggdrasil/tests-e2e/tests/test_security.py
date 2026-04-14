"""Security audit gate harness — one xfail per VULN/FLAW finding.

Every test here is marked ``@pytest.mark.xfail(reason="VULN-NNN", strict=True)``
so that:
  - Today (vulnerability present) the test fails its assertion → XFAIL → pass.
  - After remediation the test succeeds → XPASS strict → loud failure that
    forces the maintainer to remove the xfail marker.

This is the executable form of the audit's remediation roadmap. As Phase 1, 2,
3, 4 of the roadmap land, the corresponding xfails flip and become regular
tests guarding against regression.

Findings already covered in topical test files (not duplicated here):
  - VULN-006: tests/test_mesh.py::test_mesh_forged_handshake_rejected
  - VULN-007: tests/test_webhook.py
  - VULN-008: tests/test_memory.py::test_core_tier_write_requires_admin_token
  - VULN-010/011: tests/test_memory.py::test_store_recall_unicode_roundtrip
  - FLAW-003: tests/test_memory.py::test_cross_sprint_store_isolation (now passes)

Findings that are pure algorithm / no HTTP boundary, kept as in-crate unit tests:
  - FLAW-001 (SDR drift), FLAW-002 (consolidation), FLAW-004 (token estimation),
    FLAW-007 (novelty threshold) — all under crates/mimir/src/{novelty,sdr*,store_gate}.rs

Findings outside the scope of E2E (require log scraping or destructive setup):
  - VULN-012 (RwLock poison panics): captured by stress harness in test_concurrency.py
  - VULN-015 (token logged in debug): operational concern; reviewed via tracing config
  - VULN-022 (energy_manager O(n²)): performance-tier, not a correctness bug
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest
import requests

from helpers.paths import repo_root
from helpers.services import probe, service_urls


def _require_service_or_fail(name: str) -> None:
    """Audit gates must NOT silently skip when a service is down.

    ``@pytest.mark.required_services`` skips the test, erasing security
    coverage from CI with no signal. This helper converts an unreachable
    service into a loud ``pytest.fail`` so a service outage can never pass CI
    without running the security check. Call at the TOP of each xfail(strict)
    test that depends on a live service.
    """
    url = service_urls().get(name)
    if url is None:
        pytest.fail(f"audit gate for {name!r} has no configured URL")
    health = probe(name, url)
    if not health.ok:
        pytest.fail(
            f"audit gate could not be evaluated: {name} unreachable ({health.detail}). "
            "A silent skip here would let CI pass without running this security check — "
            "bring the service up or remove the gate."
        )


def _crates_dir() -> Path:
    """Location of the Rust crates workspace — asserted to exist."""
    crates = repo_root() / "yggdrasil" / "crates"
    assert crates.is_dir(), f"crates dir missing from repo root: {crates}"
    return crates


def _extract_rust_signature(text: str, fn_name: str) -> list[tuple[int, str]]:
    """Return ``(offset, full_signature_text)`` for every ``pub (async) fn <name>(...)``.

    Unlike a regex, this scans forward from the opening ``(`` and tracks paren
    depth so it captures signatures containing nested parens (``Vec<(A, B)>``,
    tuple params, etc.) and multi-line parameter lists.
    """
    pattern = re.compile(rf"pub\s+(?:async\s+)?fn\s+{re.escape(fn_name)}\s*\(", re.MULTILINE)
    out: list[tuple[int, str]] = []
    for m in pattern.finditer(text):
        start = m.start()
        # Walk from the opening paren forward, balancing depth.
        i = m.end() - 1  # index of the '('
        depth = 0
        while i < len(text):
            c = text[i]
            if c == "(":
                depth += 1
            elif c == ")":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        if depth != 0:
            continue  # unbalanced — malformed source, skip
        out.append((start, text[start:i + 1]))
    return out


def _has_nontrivial_fn_body(text: str, fn_name: str, min_body_chars: int = 60) -> bool:
    """Return True if ``fn <fn_name>`` has a body with ≥ ``min_body_chars`` between braces.

    Regular expressions provably can't match arbitrarily-nested braces, so the
    prior template-format regex failed on any function whose body contained an
    ``if`` / ``match`` / ``for`` block (i.e., every realistic scrubber). This
    scanner walks forward from the fn-keyword, picks the opening body brace
    (stopping at a ``;`` that would signal a trait signature with no body),
    then tracks brace depth to find the true closing brace and measures the
    body length.

    Returns on the FIRST non-trivial definition found — callers typically want
    "at least one real implementation exists", not a count.
    """
    pattern = re.compile(rf"\bfn\s+{re.escape(fn_name)}\b")
    n = len(text)
    for m in pattern.finditer(text):
        # Step 1: locate the body's opening brace. A ';' first means this is
        # a trait signature or extern decl — no body, skip this match.
        i = m.end()
        brace_pos = -1
        while i < n:
            c = text[i]
            if c == "{":
                brace_pos = i
                break
            if c == ";":
                break
            i += 1
        if brace_pos == -1:
            continue
        # Step 2: walk the body tracking depth. At depth 0 we've found the
        # matching close brace for the function itself.
        depth = 0
        j = brace_pos
        while j < n:
            c = text[j]
            if c == "{":
                depth += 1
            elif c == "}":
                depth -= 1
                if depth == 0:
                    body_len = j - brace_pos - 1  # exclude the braces themselves
                    if body_len >= min_body_chars:
                        return True
                    break  # this match was trivial; try the next one
            j += 1
    return False


# ──────────────────────── VULN-001: Zero auth on core services ────────────

# Endpoints that intentionally allow unauthenticated reads (health/metrics).
# Consulted as a live safety filter in the sweep below — if a maintainer ever
# adds /health or /metrics to PROBE_MATRIX the probe skips them rather than
# flagging "public endpoint returns 200" as a false-positive auth failure.
PUBLIC_PATHS = {"/health", "/metrics"}

# Service → list of (method, path, sample_body) to probe.
# Muninn endpoints added so a partial auth rollout (Odin + Mimir only) can't
# flip this gate to XPASS while the code-search surface stays unauthenticated.
PROBE_MATRIX = [
    ("odin", "POST", "/v1/chat/completions", {"messages": [{"role": "user", "content": "ping"}], "stream": False}),
    ("odin", "GET", "/v1/models", None),
    ("odin", "GET", "/api/flows", None),
    ("odin", "POST", "/api/v1/webhook", {"event": "noop"}),
    ("mimir", "POST", "/api/v1/store", {"cause": "x", "effect": "y"}),
    ("mimir", "POST", "/api/v1/recall", {"text": "x", "limit": 1}),
    ("mimir", "POST", "/api/v1/timeline", {"limit": 1}),
    ("mimir", "GET", "/api/v1/stats", None),
    ("muninn", "POST", "/api/v1/search", {"query": "x", "limit": 1}),
    ("muninn", "POST", "/api/v1/assemble", {"query": "x", "limit": 1}),
]


@pytest.mark.xfail(
    reason="VULN-001: Odin/Mimir/Muninn have no auth middleware (vault is the only authed endpoint)",
    strict=True,
)
def test_all_core_endpoints_reject_unauthenticated_requests() -> None:
    """Sweep every documented endpoint and assert it rejects unauthenticated calls.

    This single test is the acceptance gate for Phase 1 of the audit roadmap.
    When VULN-001 is fixed, every entry in PROBE_MATRIX must return 401/403,
    flipping this test from XFAIL to XPASS (which is a strict failure → remove
    the xfail marker, this test is now your live regression guard).
    """
    _require_service_or_fail("odin")
    _require_service_or_fail("mimir")
    _require_service_or_fail("muninn")
    urls = service_urls()
    failures: list[str] = []
    for service_name, method, path, body in PROBE_MATRIX:
        if path in PUBLIC_PATHS:
            continue  # intentionally unauthenticated — see PUBLIC_PATHS above
        base = urls.get(service_name, "").rstrip("/")
        if not base:
            continue
        try:
            if method == "GET":
                resp = requests.get(f"{base}{path}", timeout=5)
            else:
                resp = requests.post(f"{base}{path}", json=body, timeout=10)
        except requests.RequestException as exc:
            failures.append(f"{service_name} {method} {path} unreachable: {exc}")
            continue
        if resp.status_code not in (401, 403):
            failures.append(f"{service_name} {method} {path} returned {resp.status_code}, expected 401/403")
    assert not failures, "endpoints accepted unauthenticated requests:\n  " + "\n  ".join(failures)


# ──────────────────────── VULN-002: Plaintext sudo password ───────────────

@pytest.mark.xfail(
    reason="VULN-002: McpServerConfig.deploy_sudo_password is a plaintext String field",
    strict=True,
)
def test_deploy_sudo_password_is_vault_reference_type() -> None:
    """The field type itself must be a vault reference, not a raw String.

    Today (vulnerable): ``pub deploy_sudo_password: Option<String>``
    After fix: ``pub deploy_sudo_password: Option<VaultRef>`` or removed
    in favor of ``{{secret:DEPLOY_SUDO_PASSWORD}}`` substitution at load time.

    Grep swept across the entire ``crates/`` tree rather than two hard-coded
    files so the finding stays tracked if the field moves.
    """
    root = repo_root()
    matches: list[str] = []
    for rs in _crates_dir().rglob("*.rs"):
        for line_no, line in enumerate(rs.read_text().splitlines(), start=1):
            if "deploy_sudo_password" not in line:
                continue
            stripped = line.strip()
            if stripped.startswith("//"):
                continue
            # Vulnerable forms: ``Option<String>`` or a bare ``: String``.
            # Acceptable forms reference a wrapper type (VaultRef/SecretRef/etc).
            if re.search(r"Option\s*<\s*String\s*>", stripped) or re.search(r":\s*String\b", stripped):
                matches.append(f"{rs.relative_to(root)}:{line_no} → {stripped[:120]}")
    assert not matches, (
        "deploy_sudo_password is still typed as plaintext String:\n  "
        + "\n  ".join(matches)
    )


# ──────────────────────── VULN-004: Proxmox TLS validation disabled ──────

@pytest.mark.xfail(
    reason="VULN-004: ProxmoxClient uses .danger_accept_invalid_certs(true)",
    strict=True,
)
def test_proxmox_client_does_not_disable_tls_validation() -> None:
    """grep the entire crates/ tree for ``danger_accept_invalid_certs(true)``.

    Sweeping the whole tree (rather than one hard-coded path) keeps this gate
    firing if the call moves to a different crate. Passes only when the call
    is gone or replaced by ``add_root_certificate``.
    """
    root = repo_root()
    matches: list[str] = []
    for rs in _crates_dir().rglob("*.rs"):
        text = rs.read_text()
        if "danger_accept_invalid_certs(true)" not in text:
            continue
        # Report line numbers for maintainer diagnostics.
        for line_no, line in enumerate(text.splitlines(), start=1):
            if "danger_accept_invalid_certs(true)" in line and not line.lstrip().startswith("//"):
                matches.append(f"{rs.relative_to(root)}:{line_no}")
    assert not matches, (
        "ProxmoxClient (or other crate) still disables TLS validation "
        "(VULN-004):\n  " + "\n  ".join(matches)
    )


# ──────────────────────── VULN-005: HA call_service domain bypass ─────────

@pytest.mark.xfail(
    reason="VULN-005: HaClient::call_service accepts any (domain, service) pair",
    strict=True,
)
def test_ha_client_call_service_validates_domain_allowlist() -> None:
    """The HaClient::call_service signature must take an allowlist parameter.

    Uses a brace-balanced extractor (not a flat regex) so signatures containing
    nested parens — ``Vec<(String, u32)>``, tuple params — are captured in full
    across multi-line parameter lists. A plain ``[^)]*`` regex would stop at
    the first inner ``)`` and miss the allowlist param entirely.
    """
    root = repo_root()
    signatures: list[tuple[str, str]] = []
    for rs in _crates_dir().rglob("*.rs"):
        for _, sig in _extract_rust_signature(rs.read_text(), "call_service"):
            signatures.append((str(rs.relative_to(root)), sig))
    assert signatures, "could not locate any `fn call_service` in crates/"
    offenders = [
        f"{path} → {sig[:300]}"
        for path, sig in signatures
        if not any(marker in sig.lower() for marker in ("alloweddomains", "allowlist", "allow_list"))
    ]
    assert not offenders, (
        "call_service signature(s) lack an allowlist parameter:\n  " + "\n  ".join(offenders)
    )


# ──────────────────────── VULN-013: CONTEXT_STORE unbounded growth ───────

@pytest.mark.xfail(
    reason="VULN-013: Odin session store has no TTL/LRU eviction",
    strict=True,
)
def test_odin_session_store_eviction_metric_present() -> None:
    """After eviction lands, /metrics must expose at least one ``session_evictions_total``-style counter."""
    _require_service_or_fail("odin")
    base = service_urls()["odin"].rstrip("/")
    text = requests.get(f"{base}/metrics", timeout=5).text
    assert any(
        key in text for key in ("session_evictions_total", "context_store_evictions", "sessions_evicted")
    ), "no session-eviction counter exposed (VULN-013 unremediated?)"


# ──────────────────────── FLAW-009: Mesh gate default-allow ─────────────

@pytest.mark.xfail(
    reason="FLAW-009: GateConfig::default() returns GatePolicy::Allow",
    strict=True,
)
def test_mesh_gate_default_policy_is_deny() -> None:
    """Source-level check: the default policy must be Deny (fail-closed).

    Sweeps crates/ for the Default impl — if gate.rs moves, the finding
    stays tracked.
    """
    pattern = re.compile(
        r"impl\s+Default\s+for\s+GateConfig\s*\{[^}]*GatePolicy::(\w+)",
        re.DOTALL,
    )
    hits: list[tuple[str, str]] = []
    for rs in _crates_dir().rglob("*.rs"):
        m = pattern.search(rs.read_text())
        if m:
            hits.append((str(rs.relative_to(repo_root())), m.group(1)))
    assert hits, "could not locate `impl Default for GateConfig` anywhere in crates/"
    offenders = [f"{path} → GatePolicy::{policy}" for path, policy in hits if policy != "Deny"]
    assert not offenders, (
        "GateConfig default policy must be Deny (fail-closed, FLAW-009):\n  "
        + "\n  ".join(offenders)
    )


# ──────────────────────── FLAW-008: Flow secrets in LLM prompt ──────────

@pytest.mark.xfail(
    reason="FLAW-008: resolved secret values are sent to the LLM in plaintext",
    strict=True,
)
def test_flow_secrets_scrubbed_from_response() -> None:
    """After remediation, an LLM response must never echo a resolved secret value.

    Requires TWO signals, not one:
      (1) A ``fn <name>(...) { ... }`` definition with a non-trivial body
          (more than 60 chars between the braces — filters out ``{}`` stubs
          and ``{ todo!() }`` placeholders).
      (2) A call site for that function inside an Odin handler module — proves
          the scrubber is actually wired into a response path, not just
          defined.
    A plain substring check would flip from XFAIL to XPASS the moment someone
    committed an empty stub with the right name.
    """
    names = ("scrub_response", "redact_response", "scrub_secrets_from")
    # Real definitions: use a brace-balanced scanner (``_has_nontrivial_fn_body``)
    # that handles arbitrary nesting depth. The previous flat regex could not
    # match a function whose body contained an ``if``/``match`` block — which
    # every realistic scrubber has — so FLAW-008 would never have flipped to
    # XPASS even after proper remediation.
    non_trivial_defns: list[str] = []
    call_sites: list[str] = []
    for rs in _crates_dir().rglob("*.rs"):
        text = rs.read_text()
        rel = str(rs.relative_to(repo_root()))
        for name in names:
            if _has_nontrivial_fn_body(text, name, min_body_chars=60):
                non_trivial_defns.append(f"{rel} (defines {name})")
            # Call-site heuristic: `NAME(` not preceded by `fn ` or `//`. Limit
            # to Odin/Mimir handler crates — a scrubber referenced only in tests
            # doesn't close the handler-path gap.
            if any(seg in rel for seg in ("odin/src/", "mimir/src/")):
                for line in text.splitlines():
                    if re.search(rf"(^|[^a-zA-Z_])({re.escape(name)})\s*\(", line) \
                            and "fn " + name not in line \
                            and not line.strip().startswith("//"):
                        call_sites.append(f"{rel}: {line.strip()[:120]}")
                        break
    assert non_trivial_defns, (
        f"no non-trivial definition of any {names} found in crates/; FLAW-008 unremediated"
    )
    assert call_sites, (
        f"scrubber(s) defined ({non_trivial_defns!r}) but NOT called in any "
        "Odin/Mimir handler path — FLAW-008 only partially remediated"
    )
