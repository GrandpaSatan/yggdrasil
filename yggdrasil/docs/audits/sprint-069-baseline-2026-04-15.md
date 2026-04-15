# Sprint 069 — Phase A Baseline Audit (2026-04-15)

Locked starting state before any Sprint 069 code lands. Every item in the
plan's Project Atlas was checked against the running fleet. Deltas below.

## Fleet health (all UP)

| Node | IP | Service | Port | Health |
|---|---|---|---|---|
| Munin | 10.0.65.8 | odin | 8080 | 200 |
| Munin | 10.0.65.8 | mimir | 9090 | 200 |
| Munin | 10.0.65.8 | ygg-dreamer | 9097 | 200 |
| Munin | 10.0.65.8 | ollama (a+b) | 11434, 11435 | 200 |
| Munin | 10.0.65.8 | mcp-remote | 9093 | 406 (expected — MCP StreamableHTTP rejects GET) |
| Hugin | 10.0.65.9 | muninn | 9091 | 200 |
| Hugin | 10.0.65.9 | huginn | 9092 | 200 |
| Hugin | 10.0.65.9 | ollama | 11434 | 200 |
| Hugin | 10.0.65.9 | vision (LFM2.5-VL) | 9096 | 200 |
| Hugin | 10.0.65.9 | llama-omni2 (voice) | 9098 | (svc active) |

## Resource state (post-cleanup)

| Node | RAM used | RAM free | Disk used | Notes |
|---|---|---|---|---|
| Munin | 41 / 46 GiB (89%) | 5.2 GiB | 73G / 937G (9%) | **Memory-pressured.** 8 Ollama runners + 1 serve process keeping 9 models warm. Phase F cutover moves inference off Munin → projected 40 GiB recovery. |
| Hugin | 28 / 60 GiB (47%) | 31 GiB | 225G / 1.8T (13%) | Healthy. Room for vLLM + TEI + ai00 + LMCache NVMe tier. |

Background busy counter (`GET /api/backends/busy`) idle across all 4
backends (morrigan, hugin-ollama, munin-ollama, munin-ollama-b) at audit time.

## Cleanup executed this phase

| Item | Action | Node |
|---|---|---|
| `yggdrasil-ollama-warm.service` (disabled legacy) | Unit file removed, `daemon-reload` | Munin + Hugin |
| `/opt/yggdrasil/bin/ygg-sentinel` (orphan, no active service) | Archived to `/opt/yggdrasil/archive/ygg-sentinel.20260415` | Munin |
| journald vacuum to 7d | Freed 73.0 M + 72.7 M = **~146 M reclaimed** | Munin + Hugin |
| Stale pytest lock `/home/.../tests-e2e/.e2e.lock` | Removed after killing stranded background pytest | Workstation |

**Kept intentionally** (not orphans, future-use):
- `/opt/yggdrasil/bin/ygg-node` on both nodes — mesh architecture is future
  (VULN-006 handshake work in Phase C).
- `yggdrasil-ollama-warmup-{a,b}.service` (Munin) + `yggdrasil-ollama-warmup.service`
  (Hugin) — current production warmup; superseded by llama-swap post-Phase-F.

## Non-yggdrasil applications

Both servers audited for rogue processes. Only system daemons found:
`systemd`, `systemd-resolved`, `systemd-networkd`, `systemd-timesyncd`,
`rsyslogd`, `dbus-daemon`, `polkitd`, `fwupd`, `sshd`. No stale llama.cpp
binaries, no orphan Python scripts, no forgotten docker containers. Both
`docker ps -a` returned empty.

## Cargo workspace

`cargo check --workspace` — **clean (exit 0)**, no warnings.

## E2E suite baseline (focused xfail run, 27 tests in 33s)

Command:
```
pytest tests-e2e/tests/test_security.py tests-e2e/tests/test_webhook.py \
       tests-e2e/tests/test_mesh.py tests-e2e/tests/test_memory.py \
       tests-e2e/tests/test_task_queue.py tests-e2e/tests/test_dense_cosine_gate_tiers.py \
       tests-e2e/tests/test_dreamer_consolidation_coherence.py
```

**Result: 9 passed, 16 xfailed, 1 skipped, 1 failed.**

### All 16 xfails confirmed still outstanding (matches plan's atlas)

- VULN-001 Odin/Mimir/Muninn auth middleware
- VULN-002 McpServerConfig.deploy_sudo_password plaintext
- VULN-004 ProxmoxClient TLS validation disabled
- VULN-005 HaClient::call_service domain allowlist
- VULN-006 mesh handshake no PSK
- VULN-007 webhook no HMAC verification (×2 — unsigned + bad-signature)
- VULN-008 Core tier write without admin token **(fixed, marker removal only)**
- VULN-013 Odin session store no TTL/LRU eviction
- FLAW-008 secret values sent to LLM in plaintext
- FLAW-009 `GateConfig::default()` returns Allow
- task_queue `label` column SQL bug (×1)
- Dense cosine gate Phase 2 (×3: Tier 1 duplicate, Tier 2 ambiguous, metric shape)
- Dreamer consolidation coherence (needs `POST /api/v1/summarize/trigger`)

### NEW finding (not in the plan's atlas — added to Phase B scope)

**`test_mesh_hello_accepts_valid_handshake` FAILED with HTTP 404.** The
test POSTs to `/mesh/hello` on the configured node URL and expects 200/202;
Odin returns 404. This is orthogonal to VULN-006 (PSK handshake) — the
endpoint doesn't appear to exist at all. Could be:
(a) ygg-node was supposed to own `/mesh/hello` and it's unrouted, OR
(b) The test was written against a design that never landed.

**Action:** add to Phase C alongside VULN-006 — route `/mesh/hello` on
ygg-node or Odin, enforce PSK, make test pass.

### Skip: `DELETE /api/v1/engrams/{id}` returns 405

`test_memory.py:156` skips because the current Mimir build returns
Method Not Allowed on DELETE. Audit previously listed the endpoint as
supported. **Action:** add engram-delete route + handler to Mimir in Phase E.

## TODO / `not yet` inventory (source-level)

8 meaningful markers, all cosmetic except one:

| File:line | Excerpt |
|---|---|
| `crates/odin/src/handlers.rs:1289` | `sdr_intent: None, // TODO: pass through from hybrid_classify` — **real work for Phase E** |
| `crates/odin/src/flow.rs:170,612` | `"loop iteration (not yet converged)"` — user-facing status string, not a bug |
| `crates/odin/src/proxy.rs:694` | `vLLM does not yet support audio output` — comment, accurate |
| `crates/ygg-mcp/src/resources.rs:71,114,120` | Prefetch-not-yet-populated status strings — not bugs |
| `crates/ygg-dreamer/src/main.rs:38` | `0 when the daemon has not yet fired anything` — field docstring |

No `FIXME`, no `DEFERRED`, no `unimplemented!()`, no `todo!()` in Rust code.

## MEMORY.md Known Issues cross-check

| Entry | Reality |
|---|---|
| store_gate cross-sprint precision | RESOLVED Sprint 065 A·P1 — keep as resolved |
| Dreamer → Odin engram-store 404 | **CONFIRMED still intermittent.** Phase E scope. |
| Thor WoL not working | Hardware — out of scope per plan |
| SDR novelty gate | RESOLVED Sprint 044 — keep |
| Huginn tree-sitter bug | RESOLVED Sprint 051 — keep |
| build_check_tool no cargo on Munin | Still accurate — use local `cargo check` |
| store_memory auto-ingest hook | Needs smoke test — Phase E scope |
| Ollama CPU instead of iGPU | RESOLVED — keep |
| Hades Postgres pool timeout | Separate from Sprint 069 scope |
| Odin config munin-cpu port wrong | RESOLVED Sprint 053 — keep |

## Atlas delta vs. plan (2026-04-15 → reality)

Items where the plan's atlas needs adjustment before Phase B starts:

1. **Phase B scope grows by 1:** `test_mesh_hello_accepts_valid_handshake`
   needs an endpoint, not just marker removal. Pair this with VULN-006 work
   in Phase C so both `/mesh/hello` routing + PSK handshake land together.
2. **Phase E scope grows by 1:** `DELETE /api/v1/engrams/{id}` route + handler
   in Mimir.
3. **Track B pre-flight green:** Hugin already runs Ubuntu 24.04, glibc 2.39
   (≥ 2.35 required), Python 3.12.3, kernel 6.17.0-20. Proceed with ROCm 7.2
   install when Phase F starts — no blocking prerequisites.
4. **Munin RAM at 89%** — acceptable but tight. Phase F is highest priority
   for reclaiming headroom.

## Phase A acceptance

✅ Full cargo workspace clean.
✅ 16/16 expected xfails still xfailing (all plan assumptions hold).
✅ 2 new findings (mesh/hello route, engram DELETE) folded into Phase B/E.
✅ System cleanup executed — no failed units, no rogue apps, ~146 MB
   journal reclaimed, 1 disabled unit file removed, 1 orphan binary archived.
✅ Fleet health: 10/10 services up.

Phase B may begin.
