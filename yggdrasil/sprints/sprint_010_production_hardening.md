# Sprint: 010 - Production Hardening
## Status: DONE

## Objective

Harden the Yggdrasil deployment for unattended operation by creating systemd service units for all five services (Odin, Mimir, Huginn, Muninn, ygg-mcp-server), adding Prometheus metrics instrumentation via the `metrics` and `metrics-exporter-prometheus` crates, establishing a log aggregation strategy via journald, implementing graceful degradation policies for partial service failures, creating a PostgreSQL + Qdrant backup strategy for Hades, and producing deployment scripts for rolling updates across Munin, Hugin, and Hades. After this sprint, the entire Yggdrasil system can be deployed, monitored, backed up, and updated with documented, repeatable procedures.

## Scope

### In Scope
- **Systemd service units** for all 5 services:
  - `yggdrasil-odin.service` (Munin)
  - `yggdrasil-mimir.service` (Munin)
  - `yggdrasil-huginn.service` (Hugin)
  - `yggdrasil-muninn.service` (Hugin)
  - `yggdrasil-mcp-server@.service` (Munin, template unit for per-user instances)
- Systemd dependency ordering: Mimir starts before Odin (Odin depends on Mimir health)
- Systemd watchdog integration via `Type=notify` and `sd_notify` (using `sd-notify` Rust crate)
- Configuration in `/etc/yggdrasil/` on each node (production paths instead of relative `configs/`)
- **Health check endpoints** (verify existing, add to Huginn):
  - Odin: `GET /health` (exists, Sprint 005)
  - Mimir: `GET /health` (exists, Sprint 002)
  - Muninn: `GET /health` (exists, Sprint 004)
  - Huginn: add `GET /health` via a lightweight HTTP listener (Huginn is a daemon, currently has no HTTP interface)
- **Graceful degradation policies** documented and implemented:
  - Mimir down: Odin proxies return 503, chat completions skip engram context, log warning
  - Muninn down: Odin skips code context in RAG, log warning
  - Ollama down: Odin returns 503 for chat completions, other endpoints continue
  - Hades (PostgreSQL/Qdrant) down: all store/query operations fail with 503, services stay running
  - Huginn down: no new indexing, Muninn still serves stale index, no user impact
  - Home Assistant down: HA tools return errors, non-HA features unaffected (already implemented Sprint 007)
- **Prometheus metrics** via `metrics` crate + `metrics-exporter-prometheus`:
  - Each HTTP service exposes `GET /metrics` endpoint (Prometheus scrape target)
  - Metrics: request latency histograms, request count by endpoint, error count by type, embedding throughput, engram counts by tier, code chunk count, indexing progress (Huginn), Qdrant operation latency, LLM generation latency, active backend connections
  - Huginn exposes metrics via the new health HTTP listener
- **Log aggregation strategy:**
  - All services log to stderr (captured by journald via systemd)
  - Structured JSON logging via `tracing-subscriber` JSON format (switchable via config)
  - `journalctl` commands documented for common debugging scenarios
  - Optional: forward journald to a central syslog/Loki instance (documented, not implemented)
- **Backup strategy for Hades:**
  - PostgreSQL: `pg_dump` cron job for `yggdrasil` schema, daily, 7-day retention
  - Qdrant: snapshot API (`POST /collections/{name}/snapshots`), daily, 7-day retention
  - Backup destination: Hades RAVEN pool (2.63 TiB SSD, high-speed scratch)
  - Restore procedures documented
- **Deployment scripts:**
  - `deploy/install.sh`: build release binaries, copy to target nodes via `rsync`, install systemd units
  - `deploy/update.sh`: rolling update -- stop service, replace binary, start service, verify health
  - `deploy/rollback.sh`: revert to previous binary (kept as `.prev` alongside current)
  - Cross-compilation: build on any machine, deploy to Munin (x86_64) and Hugin (x86_64)

### Out of Scope
- Kubernetes or container orchestration (bare-metal systemd is the deployment model)
- Docker images (services run as native binaries)
- Grafana dashboards (Prometheus exposition is sufficient; dashboards are a future enhancement)
- Alerting rules (Prometheus alertmanager configuration; can be added later)
- Centralized log aggregation server setup (Loki/Elasticsearch; journald is sufficient for now)
- TLS/HTTPS for inter-service communication (private LAN, no auth)
- Secret management (HA token in config file, not Vault/SOPS)
- CI/CD pipeline (builds and deploys are manual/scripted)
- Load balancing or horizontal scaling (single instance per service)
- Database replication or failover for Hades

## Hardware Constraints & Utilization Strategy

- **Workload Classification:** I/O-bound (metrics collection is negligible CPU overhead; deployment scripts are short-lived).
- **Target Hardware:**
  - Munin (REDACTED_MUNIN_IP): Odin, Mimir, ygg-mcp-server -- Intel Core Ultra 185H, 48GB DDR5
  - Hugin (REDACTED_HUGIN_IP): Huginn, Muninn -- AMD Ryzen 7 255, 64GB DDR5
  - Hades (REDACTED_HADES_IP): PostgreSQL, Qdrant -- Intel N150, 32GB DDR5, Merlin SSD pool (444 GiB)
- **Utilization Plan:**
  - Prometheus metrics: each service adds a `/metrics` handler that returns text exposition format. The handler is stateless and computes values from in-memory counters and histograms. Overhead: < 1ms per scrape, < 2MB additional RSS per service for counter/histogram storage.
  - systemd watchdog: each service sends `WATCHDOG=1` to systemd every `WatchdogSec/2` interval. This is a single syscall per interval (negligible CPU). `WatchdogSec=30` means one notify every 15 seconds.
  - Backup scripts: `pg_dump` of the `yggdrasil` schema takes ~1s for < 10k engrams + < 100k chunks. Qdrant snapshot takes ~5s per collection. Total backup time < 30s. Run during low-activity hours (3 AM).
  - Deployment scripts: binary `rsync` is < 30MB per service. Over 5Gb LAN, transfer takes < 1s. Rolling update with health check takes ~10s per service.
- **Fallback Strategy:**
  - If `sd-notify` is not available (non-systemd environments): services run normally without watchdog. The `sd-notify` crate gracefully no-ops when `NOTIFY_SOCKET` is not set.
  - If Prometheus scraping is too frequent or expensive: adjust scrape interval (default 15s is fine for 5 services).
  - If backup scripts fail: cron job logs to journald, manual investigation. Backup failure does not affect service operation.

## Performance Targets

| Metric | Target | Measurement Method |
|--------|--------|--------------------|
| Metrics endpoint P95 | < 10ms | `tracing` span on `/metrics` handler |
| Metrics endpoint response size | < 50KB | `curl -s localhost:PORT/metrics \| wc -c` |
| RSS overhead per service (metrics) | < 2MB additional | Compare RSS with and without metrics feature |
| Watchdog notify interval | every 15s (WatchdogSec=30) | `journalctl -u yggdrasil-*.service` watchdog logs |
| Service startup time (to healthy) | < 10s (Odin, Mimir, Muninn), < 30s (Huginn with backfill) | Time from `systemctl start` to `GET /health` returning 200 |
| Rolling update: single service downtime | < 10s | Time between `systemctl stop` and new instance passing health check |
| Backup: total wall clock time | < 60s | Cron job log |
| Backup: disk usage per day | < 500MB | `du -sh /backup/yggdrasil/` |

## Data Schemas

### Prometheus Metrics (text exposition format)

All services expose the following common metrics:

```prometheus
# HELP ygg_http_requests_total Total HTTP requests by endpoint and status
# TYPE ygg_http_requests_total counter
ygg_http_requests_total{service="mimir",endpoint="/api/v1/query",status="200"} 1542
ygg_http_requests_total{service="mimir",endpoint="/api/v1/store",status="201"} 834
ygg_http_requests_total{service="mimir",endpoint="/api/v1/query",status="400"} 12

# HELP ygg_http_request_duration_seconds HTTP request duration histogram
# TYPE ygg_http_request_duration_seconds histogram
ygg_http_request_duration_seconds_bucket{service="mimir",endpoint="/api/v1/query",le="0.01"} 890
ygg_http_request_duration_seconds_bucket{service="mimir",endpoint="/api/v1/query",le="0.05"} 1490
ygg_http_request_duration_seconds_bucket{service="mimir",endpoint="/api/v1/query",le="0.1"} 1530
ygg_http_request_duration_seconds_bucket{service="mimir",endpoint="/api/v1/query",le="0.5"} 1542
ygg_http_request_duration_seconds_bucket{service="mimir",endpoint="/api/v1/query",le="+Inf"} 1542

# HELP ygg_http_request_duration_seconds_sum
ygg_http_request_duration_seconds_sum{service="mimir",endpoint="/api/v1/query"} 42.8
# HELP ygg_http_request_duration_seconds_count
ygg_http_request_duration_seconds_count{service="mimir",endpoint="/api/v1/query"} 1542
```

Service-specific metrics:

**Mimir:**
```prometheus
# HELP ygg_engram_count Current engram count by tier
# TYPE ygg_engram_count gauge
ygg_engram_count{tier="core"} 5
ygg_engram_count{tier="recall"} 847
ygg_engram_count{tier="archival"} 215

# HELP ygg_embedding_duration_seconds Embedding call duration
# TYPE ygg_embedding_duration_seconds histogram
ygg_embedding_duration_seconds_bucket{le="0.005"} 120
ygg_embedding_duration_seconds_bucket{le="0.01"} 580
ygg_embedding_duration_seconds_bucket{le="0.05"} 834

# HELP ygg_summarization_total Summarization batches completed
# TYPE ygg_summarization_total counter
ygg_summarization_total 12

# HELP ygg_summarization_engrams_archived Total engrams archived by summarization
# TYPE ygg_summarization_engrams_archived counter
ygg_summarization_engrams_archived 1200
```

**Odin:**
```prometheus
# HELP ygg_llm_generation_duration_seconds LLM generation duration (Ollama call)
# TYPE ygg_llm_generation_duration_seconds histogram
ygg_llm_generation_duration_seconds_bucket{model="qwen3-coder-30b-a3b",le="1"} 10
ygg_llm_generation_duration_seconds_bucket{model="qwen3-coder-30b-a3b",le="5"} 85
ygg_llm_generation_duration_seconds_bucket{model="qwen3-coder-30b-a3b",le="30"} 142

# HELP ygg_backend_active_requests Current active requests per backend
# TYPE ygg_backend_active_requests gauge
ygg_backend_active_requests{backend="munin"} 1
ygg_backend_active_requests{backend="hugin"} 0

# HELP ygg_routing_intent_total Routing decisions by intent
# TYPE ygg_routing_intent_total counter
ygg_routing_intent_total{intent="coding"} 120
ygg_routing_intent_total{intent="reasoning"} 35
ygg_routing_intent_total{intent="home_automation"} 8
```

**Huginn:**
```prometheus
# HELP ygg_indexed_files_total Total files indexed
# TYPE ygg_indexed_files_total gauge
ygg_indexed_files_total 1542

# HELP ygg_code_chunks_total Total code chunks stored
# TYPE ygg_code_chunks_total gauge
ygg_code_chunks_total 12847

# HELP ygg_indexing_duration_seconds File indexing duration (per file)
# TYPE ygg_indexing_duration_seconds histogram
ygg_indexing_duration_seconds_bucket{language="rust",le="0.1"} 890
ygg_indexing_duration_seconds_bucket{language="rust",le="0.5"} 1400
ygg_indexing_duration_seconds_bucket{language="rust",le="1"} 1520

# HELP ygg_watcher_events_total File watcher events received
# TYPE ygg_watcher_events_total counter
ygg_watcher_events_total{event="modify"} 2341
ygg_watcher_events_total{event="create"} 145
ygg_watcher_events_total{event="remove"} 32
```

**Muninn:**
```prometheus
# HELP ygg_search_duration_seconds Search operation duration
# TYPE ygg_search_duration_seconds histogram

# HELP ygg_search_results_count Number of results returned per search
# TYPE ygg_search_results_count histogram

# HELP ygg_qdrant_duration_seconds Qdrant operation duration
# TYPE ygg_qdrant_duration_seconds histogram
ygg_qdrant_duration_seconds_bucket{operation="search",le="0.01"} 450
ygg_qdrant_duration_seconds_bucket{operation="search",le="0.05"} 780
```

### Systemd Unit: `yggdrasil-mimir.service` (IMPLEMENTED)

```ini
[Unit]
Description=Yggdrasil Mimir - Engram Memory Service
After=network-online.target docker.service
Wants=network-online.target
Requires=docker.service
StartLimitIntervalSec=300
StartLimitBurst=5

[Service]
Type=notify
User=yggdrasil
Group=yggdrasil
WorkingDirectory=/opt/yggdrasil
ExecStart=/opt/yggdrasil/bin/mimir --config /etc/yggdrasil/mimir/config.yaml
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
RestartSec=5
TimeoutStartSec=30
TimeoutStopSec=15
WatchdogSec=30
Environment="RUST_LOG=info"
Environment="MIMIR_DATABASE_URL=postgres://jhernandez:K6m4B129CF9u@localhost:5432/yggdrasil"
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

**Note:** The original draft had `After=network-online.target` and PG at `REDACTED_HADES_IP:5432/postgres` (Hades). Corrected to depend on `docker.service` (pgvector container) and target `localhost:5432/yggdrasil`.

### Systemd Unit: `yggdrasil-odin.service` (IMPLEMENTED)

```ini
[Unit]
Description=Yggdrasil Odin - LLM Orchestrator
After=network-online.target yggdrasil-mimir.service
Wants=network-online.target
Requires=yggdrasil-mimir.service
StartLimitIntervalSec=300
StartLimitBurst=5

[Service]
Type=notify
User=yggdrasil
Group=yggdrasil
WorkingDirectory=/opt/yggdrasil
ExecStart=/opt/yggdrasil/bin/odin --config /etc/yggdrasil/odin/node.yaml
ExecStartPre=/opt/yggdrasil/bin/wait-for-health.sh http://localhost:9090/health 30
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
RestartSec=5
TimeoutStartSec=60
TimeoutStopSec=15
WatchdogSec=30
Environment="RUST_LOG=info"
Environment="HA_TOKEN="
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

### Systemd Unit: `yggdrasil-huginn.service` (IMPLEMENTED)

```ini
[Unit]
Description=Yggdrasil Huginn - Code Indexer
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=5

[Service]
Type=notify
User=yggdrasil
Group=yggdrasil
WorkingDirectory=/opt/yggdrasil
ExecStart=/opt/yggdrasil/bin/huginn --config /etc/yggdrasil/huginn/config.yaml watch
Restart=on-failure
RestartSec=10
TimeoutStartSec=120
TimeoutStopSec=15
WatchdogSec=30
Environment="RUST_LOG=info"
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

**Note:** The original draft had `WatchdogSec=60`. Standardized to `WatchdogSec=30` across all daemon units. Huginn ExecStart includes the `watch` subcommand.

### Systemd Unit: `yggdrasil-muninn.service` (IMPLEMENTED)

```ini
[Unit]
Description=Yggdrasil Muninn - Code Retrieval Engine
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=5

[Service]
Type=notify
User=yggdrasil
Group=yggdrasil
WorkingDirectory=/opt/yggdrasil
ExecStart=/opt/yggdrasil/bin/muninn --config /etc/yggdrasil/muninn/config.yaml
Restart=on-failure
RestartSec=5
TimeoutStartSec=30
TimeoutStopSec=15
WatchdogSec=30
Environment="RUST_LOG=info"
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

### Systemd Unit: `yggdrasil-mcp-server@.service` (template)

```ini
[Unit]
Description=Yggdrasil MCP Server for user %i
After=yggdrasil-odin.service

[Service]
Type=simple
User=%i
ExecStart=/opt/yggdrasil/bin/ygg-mcp-server --config /etc/yggdrasil/mcp-server/config.yaml
Restart=on-failure
RestartSec=5
Environment="RUST_LOG=info"

[Install]
WantedBy=multi-user.target
```

Note: MCP server is stdio-based, so `Type=simple` (not `notify`). It is launched by IDE clients directly, not by systemd in normal operation. The systemd unit is provided for testing and manual invocation.

### Backup script: `deploy/backup-hades.sh` (IMPLEMENTED)

```bash
#!/usr/bin/env bash
# Database backup script for Hades (PostgreSQL + Qdrant).
# Run as a cron job at 3 AM daily:
#   0 3 * * * /opt/yggdrasil/deploy/backup-hades.sh >> /var/log/yggdrasil-backup.log 2>&1
#
# Prerequisites: postgresql-client must be installed on the backup executor.
#   apt install postgresql-client
set -euo pipefail

BACKUP_DIR="/mnt/raven/yggdrasil-backups"
DATE=$(date +%Y%m%d_%H%M%S)
RETENTION_DAYS=7

mkdir -p "${BACKUP_DIR}"
echo "[${DATE}] Starting Yggdrasil backup"

# PostgreSQL dump (yggdrasil schema only).
# PostgreSQL runs as a Docker container on Munin (localhost:5432).
pg_dump -h 127.0.0.1 -U jhernandez -d yggdrasil \
  --schema=yggdrasil --format=custom \
  -f "${BACKUP_DIR}/pg_yggdrasil_${DATE}.dump"

echo "[${DATE}] PostgreSQL dump complete: pg_yggdrasil_${DATE}.dump"

# Qdrant snapshots (trigger on Hades, store metadata locally).
for collection in engrams code_chunks; do
    response=$(curl -s -X POST "http://REDACTED_HADES_IP:6333/collections/${collection}/snapshots")
    echo "[${DATE}] Qdrant snapshot triggered for ${collection}: ${response}"
    echo "${response}" > "${BACKUP_DIR}/qdrant_${collection}_${DATE}.snapshot"
done

# Cleanup old backups (older than RETENTION_DAYS days).
find "${BACKUP_DIR}" -name "pg_yggdrasil_*.dump" -mtime "+${RETENTION_DAYS}" -delete
find "${BACKUP_DIR}" -name "qdrant_*.snapshot" -mtime "+${RETENTION_DAYS}" -delete

echo "[${DATE}] Backup completed. Backup directory: ${BACKUP_DIR}"
echo "[${DATE}] Disk usage: $(du -sh "${BACKUP_DIR}" | cut -f1)"
```

**Note:** The original draft had `pg_dump -h REDACTED_HADES_IP -d postgres` (targeting Hades). This was incorrect -- PostgreSQL runs on Munin (localhost). Fixed to `-h 127.0.0.1 -d yggdrasil`.

### Deployment directory structure

```
deploy/
  install.sh          # First-time setup: create user, dirs, install systemd units
  update.sh           # Rolling update: stop, replace binary, start, health check
  rollback.sh         # Revert to previous binary
  backup-hades.sh     # Database backup script
  wait-for-health.sh  # Health check polling script (used by systemd ExecStartPre)
  systemd/            # Systemd unit files
    yggdrasil-odin.service
    yggdrasil-mimir.service
    yggdrasil-huginn.service
    yggdrasil-muninn.service
    yggdrasil-mcp-server@.service
```

### `deploy/wait-for-health.sh`

```bash
#!/usr/bin/env bash
# Usage: wait-for-health <url> <timeout_seconds>
set -euo pipefail
URL=$1
TIMEOUT=${2:-30}
ELAPSED=0

while [ $ELAPSED -lt $TIMEOUT ]; do
  if curl -sf "$URL" > /dev/null 2>&1; then
    exit 0
  fi
  sleep 1
  ELAPSED=$((ELAPSED + 1))
done

echo "Health check failed: $URL did not respond within ${TIMEOUT}s"
exit 1
```

### Graceful Degradation Config (in Odin)

No new config fields. Degradation is implemented in the existing RAG pipeline and proxy handlers. The behavior is:

| Service Down | Odin Behavior | User-Visible Effect |
|-------------|--------------|---------------------|
| Mimir | Proxy endpoints (`/api/v1/query`, `/api/v1/store`) return 503. Chat completions skip engram context injection. Engram store-on-completion is skipped. | No engram memory in responses. Chat still works. |
| Muninn | Chat completions skip code context injection. | No code search results in context. Chat still works. |
| Ollama (Munin) | Chat completions to coding model return 503. Reasoning model (Hugin) still works. | Coding requests fail. Reasoning/HA requests unaffected. |
| Ollama (Hugin) | Chat completions to reasoning model return 503. Coding model (Munin) still works. | Reasoning/HA requests fail. Coding requests unaffected. |
| Hades (PostgreSQL) | All database operations fail. Mimir returns 503. Huginn stops indexing. Muninn returns 503. | Memory and search unavailable. Chat without context still works via Odin -> Ollama directly. |
| Hades (Qdrant) | Vector search fails. Mimir falls back to PostgreSQL-only query (pgvector). Muninn search degrades. | Reduced search quality but functional. |
| Huginn | No new code indexing. Existing index in PostgreSQL/Qdrant is stale but still searchable. | Code search returns stale results. No impact unless codebase changes frequently. |
| Home Assistant | HA tools return errors. Non-HA features unaffected. | HA control unavailable. Already implemented in Sprint 007. |

## API Contracts

### New endpoint: `GET /metrics` (all HTTP services)

Returns Prometheus text exposition format.

```
Content-Type: text/plain; version=0.0.4; charset=utf-8
```

No authentication. Scrape target for Prometheus.

### New endpoint: `GET /health` (Huginn only -- new)

Huginn currently has no HTTP listener. Add a lightweight Axum listener on a configurable port (default 9092) with a single endpoint.

Request: `GET /health`
Response: `200 OK`
```json
{
  "status": "ok",
  "watching": 3,
  "indexed_files": 1542,
  "last_index_at": "2026-03-09T14:30:00Z"
}
```

Huginn's health listener also exposes `GET /metrics` for Prometheus scraping.

### Existing health endpoints (verify, no changes)

| Service | Endpoint | Response |
|---------|----------|----------|
| Odin | `GET /health` | `200 OK` with backend status |
| Mimir | `GET /health` | `200 OK` |
| Muninn | `GET /health` | `200 OK` |

### Huginn health configuration

New field in `HuginnConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HuginnConfig {
    pub watch_paths: Vec<String>,
    pub database_url: String,
    pub qdrant_url: String,
    pub embed: EmbedConfig,
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    /// Address for health/metrics HTTP listener (default "0.0.0.0:9092").
    #[serde(default = "default_huginn_listen_addr")]
    pub listen_addr: String,
}

fn default_huginn_listen_addr() -> String {
    "0.0.0.0:9092".to_string()
}
```

## Interface Boundaries

| Module | Owns | Exposes | Depends On |
|--------|------|---------|------------|
| `odin::metrics` (new) | Metrics registration, middleware, `/metrics` endpoint | `metrics_middleware()` Axum layer, `metrics_handler()` | `metrics`, `metrics-exporter-prometheus` |
| `mimir::metrics` (new) | Mimir-specific metric gauges/counters, `/metrics` endpoint | `metrics_handler()`, `record_*()` helper functions | `metrics`, `metrics-exporter-prometheus` |
| `huginn::health` (new) | Health HTTP listener lifecycle, `/health` and `/metrics` endpoints, indexing stats | `start_health_server()` | `axum`, `metrics`, `metrics-exporter-prometheus` |
| `muninn::metrics` (new) | Muninn-specific metric gauges/counters, `/metrics` endpoint | `metrics_handler()` | `metrics`, `metrics-exporter-prometheus` |
| `deploy/` (new directory) | Deployment scripts, systemd units, backup scripts | Shell scripts | SSH access, `rsync`, `systemctl` |
| `docs/OPERATIONS.md` (new) | Operational runbook: common journalctl queries, backup/restore procedures, rollback | Human-readable reference | N/A |

**Ownership rules:**
- Only the `deploy/` directory contains deployment and operational scripts. No deployment logic in Rust code.
- Each binary crate owns its own metrics module. The metrics module registers metrics at startup and provides recording functions called from handlers.
- The `metrics` crate provides the global recorder. Each service installs a `PrometheusBuilder` recorder at startup. The `/metrics` handler calls `PrometheusHandle::render()` to produce the exposition text.
- Huginn's health HTTP listener is a minimal Axum server running alongside the file watcher and indexer. It does not share state with the indexer beyond read-only access to counters.
- Backup scripts are executed by the `infra-devops` agent or cron. They are not invoked by any Yggdrasil service.

## File-Level Implementation Plan

### New directory: `deploy/`

**`deploy/systemd/yggdrasil-odin.service`** -- See systemd unit in Data Schemas section.

**`deploy/systemd/yggdrasil-mimir.service`** -- See systemd unit in Data Schemas section.

**`deploy/systemd/yggdrasil-huginn.service`** -- See systemd unit in Data Schemas section.

**`deploy/systemd/yggdrasil-muninn.service`** -- See systemd unit in Data Schemas section.

**`deploy/systemd/yggdrasil-mcp-server@.service`** -- See systemd unit in Data Schemas section.

**`deploy/install.sh`:**
```bash
#!/usr/bin/env bash
# First-time installation on a target node.
# Usage: ./install.sh <node> <services...>
# Example: ./install.sh munin odin mimir
# Example: ./install.sh hugin huginn muninn
set -euo pipefail

NODE=$1
shift
SERVICES=("$@")
REMOTE="jhernandez@${NODE}"
INSTALL_DIR="/opt/yggdrasil"
CONFIG_DIR="/etc/yggdrasil"

# 1. Create yggdrasil user and directories
ssh "$REMOTE" "sudo useradd -r -s /sbin/nologin yggdrasil 2>/dev/null || true"
ssh "$REMOTE" "sudo mkdir -p ${INSTALL_DIR}/bin ${CONFIG_DIR}"
ssh "$REMOTE" "sudo chown yggdrasil:yggdrasil ${INSTALL_DIR}"

# 2. Build release binaries
cargo build --release --bin "${SERVICES[@]}"

# 3. Copy binaries
for svc in "${SERVICES[@]}"; do
    rsync -avz "target/release/${svc}" "${REMOTE}:${INSTALL_DIR}/bin/"
done

# 4. Copy config files
for svc in "${SERVICES[@]}"; do
    config_dir="configs/${svc}"
    if [ -d "$config_dir" ]; then
        ssh "$REMOTE" "sudo mkdir -p ${CONFIG_DIR}/${svc}"
        rsync -avz "${config_dir}/" "${REMOTE}:${CONFIG_DIR}/${svc}/"
    fi
done

# 5. Install systemd units
for svc in "${SERVICES[@]}"; do
    rsync -avz "deploy/systemd/yggdrasil-${svc}.service" \
        "${REMOTE}:/etc/systemd/system/"
done
rsync -avz "deploy/wait-for-health.sh" "${REMOTE}:${INSTALL_DIR}/bin/"
ssh "$REMOTE" "sudo chmod +x ${INSTALL_DIR}/bin/wait-for-health.sh"

# 6. Reload systemd and enable services
ssh "$REMOTE" "sudo systemctl daemon-reload"
for svc in "${SERVICES[@]}"; do
    ssh "$REMOTE" "sudo systemctl enable yggdrasil-${svc}.service"
done

echo "Installation complete on ${NODE}. Start services with:"
for svc in "${SERVICES[@]}"; do
    echo "  sudo systemctl start yggdrasil-${svc}"
done
```

**`deploy/update.sh`:**
```bash
#!/usr/bin/env bash
# Rolling update of a service on a target node.
# Usage: ./update.sh <node> <service>
# Example: ./update.sh munin odin
set -euo pipefail

NODE=$1
SERVICE=$2
REMOTE="jhernandez@${NODE}"
INSTALL_DIR="/opt/yggdrasil"
BIN="${INSTALL_DIR}/bin/${SERVICE}"

# 1. Build release binary
cargo build --release --bin "$SERVICE"

# 2. Copy new binary (keep previous as .prev for rollback)
ssh "$REMOTE" "sudo cp ${BIN} ${BIN}.prev 2>/dev/null || true"
rsync -avz "target/release/${SERVICE}" "${REMOTE}:${BIN}.new"
ssh "$REMOTE" "sudo mv ${BIN}.new ${BIN}"

# 3. Restart service
ssh "$REMOTE" "sudo systemctl restart yggdrasil-${SERVICE}"

# 4. Wait for health
sleep 2
HEALTH_PORT=$(ssh "$REMOTE" "systemctl show yggdrasil-${SERVICE} -p ExecStart" | grep -oP ':\d+' | head -1 | tr -d ':')
if [ -n "$HEALTH_PORT" ]; then
    ssh "$REMOTE" "${INSTALL_DIR}/bin/wait-for-health.sh http://localhost:${HEALTH_PORT}/health 30"
fi

echo "Update complete: ${SERVICE} on ${NODE}"
ssh "$REMOTE" "sudo systemctl status yggdrasil-${SERVICE} --no-pager"
```

**`deploy/rollback.sh`:**
```bash
#!/usr/bin/env bash
# Rollback a service to the previous binary.
# Usage: ./rollback.sh <node> <service>
set -euo pipefail

NODE=$1
SERVICE=$2
REMOTE="jhernandez@${NODE}"
BIN="/opt/yggdrasil/bin/${SERVICE}"

ssh "$REMOTE" "sudo cp ${BIN} ${BIN}.failed && sudo cp ${BIN}.prev ${BIN}"
ssh "$REMOTE" "sudo systemctl restart yggdrasil-${SERVICE}"
echo "Rolled back ${SERVICE} on ${NODE}. Previous binary preserved as ${BIN}.failed"
```

**`deploy/backup-hades.sh`** -- See backup script in Data Schemas section.

**`deploy/wait-for-health.sh`** -- See health check script in Data Schemas section.

### New workspace dependencies (Cargo.toml)

```toml
# Metrics
metrics = "0.24"
metrics-exporter-prometheus = "0.16"

# Systemd notification
sd-notify = "0.4"
```

### `crates/odin/src/metrics.rs` (NEW)

```rust
use axum::{extract::Request, middleware::Next, response::Response};
use metrics::{counter, histogram};
use std::time::Instant;

/// Axum middleware layer that records request count and duration.
pub async fn metrics_middleware(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().to_string();
    let start = Instant::now();

    let response = next.run(req).await;

    let duration = start.elapsed().as_secs_f64();
    let status = response.status().as_u16().to_string();

    counter!("ygg_http_requests_total",
        "service" => "odin", "endpoint" => path.clone(), "status" => status)
        .increment(1);
    histogram!("ygg_http_request_duration_seconds",
        "service" => "odin", "endpoint" => path)
        .record(duration);

    response
}
```

Install in Odin's router:
```rust
use axum::middleware;
use metrics_exporter_prometheus::PrometheusBuilder;

// In main(), before building router:
let prometheus_handle = PrometheusBuilder::new()
    .install_recorder()
    .expect("failed to install prometheus recorder");

let router = Router::new()
    // ... existing routes ...
    .route("/metrics", get(move || async move {
        prometheus_handle.render()
    }))
    .layer(middleware::from_fn(metrics::metrics_middleware))
    // ... existing layers ...
```

### `crates/mimir/src/metrics.rs` (NEW)

Same pattern as Odin. Additional Mimir-specific metrics:
- `ygg_engram_count` gauge: updated on store/promote/summarization
- `ygg_embedding_duration_seconds` histogram: recorded in `store_engram` and `query_engrams`
- `ygg_summarization_total` counter: incremented by summarization service

### `crates/huginn/src/health.rs` (NEW)

Minimal Axum HTTP server for health and metrics:

```rust
use std::sync::Arc;
use axum::{Router, routing::get, extract::State, Json, http::StatusCode};
use metrics_exporter_prometheus::PrometheusHandle;

pub struct HealthState {
    pub prometheus: PrometheusHandle,
    pub watch_count: std::sync::atomic::AtomicU64,
    pub indexed_files: std::sync::atomic::AtomicU64,
    pub last_index_at: tokio::sync::RwLock<Option<chrono::DateTime<chrono::Utc>>>,
}

pub async fn start_health_server(
    listen_addr: &str,
    state: Arc<HealthState>,
) -> anyhow::Result<()> {
    let router = Router::new()
        .route("/health", get(health_handler))
        .route("/metrics", get(metrics_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    tracing::info!("huginn health server listening on {listen_addr}");
    axum::serve(listener, router).await?;
    Ok(())
}
```

### `crates/huginn/Cargo.toml` (MODIFY)

Add dependencies:
```toml
axum = { workspace = true }
metrics = { workspace = true }
metrics-exporter-prometheus = { workspace = true }
sd-notify = { workspace = true }
```

### `crates/muninn/src/metrics.rs` (NEW)

Same pattern as Odin/Mimir. Muninn-specific metrics:
- `ygg_search_duration_seconds` histogram
- `ygg_search_results_count` histogram
- `ygg_qdrant_duration_seconds` histogram by operation

### All binary crates: systemd watchdog integration

Add to each `main.rs` after successful startup:

```rust
// Signal systemd that the service is ready.
sd_notify::notify(true, &[sd_notify::NotifyState::Ready])?;

// Spawn watchdog task.
if let Ok(interval) = sd_notify::watchdog_enabled(false) {
    if let Some(interval) = interval {
        let half = interval / 2;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(half);
            loop {
                tick.tick().await;
                let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Watchdog]);
            }
        });
    }
}
```

Add `sd-notify` dependency to all binary crate Cargo.toml files:
- `crates/odin/Cargo.toml`
- `crates/mimir/Cargo.toml`
- `crates/huginn/Cargo.toml`
- `crates/muninn/Cargo.toml`

### `crates/ygg-domain/src/config.rs` (MODIFY)

Add `listen_addr` to `HuginnConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HuginnConfig {
    pub watch_paths: Vec<String>,
    pub database_url: String,
    pub qdrant_url: String,
    pub embed: EmbedConfig,
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    #[serde(default = "default_huginn_listen_addr")]
    pub listen_addr: String,
}

fn default_huginn_listen_addr() -> String {
    "0.0.0.0:9092".to_string()
}
```

### `configs/huginn/config.yaml` (MODIFY)

Add health listener address:

```yaml
watch_paths:
  - "/home/jesus/Documents/Rust"
  - "/home/jesus/Documents/HardwareSetup"
database_url: "postgres://jhernandez:K6m4B129CF9u@REDACTED_HADES_IP:5432/postgres"
qdrant_url: "http://REDACTED_HADES_IP:6334"
embed:
  ollama_url: "http://localhost:11434"
  model: "qwen3-embedding"
debounce_ms: 500
listen_addr: "0.0.0.0:9092"
```

### `docs/OPERATIONS.md` (NEW)

Operational runbook containing:
- Service architecture overview (which services run where)
- Starting/stopping services
- Common `journalctl` queries:
  - `journalctl -u yggdrasil-odin -f` (follow logs)
  - `journalctl -u yggdrasil-mimir --since "1 hour ago" -p err` (errors in last hour)
  - `journalctl -u yggdrasil-huginn --grep "indexed" --since today` (today's indexing)
- Prometheus scrape targets table
- Backup and restore procedures
- Rollback procedure
- Graceful degradation behavior matrix
- Port assignments table
- Node deployment mapping

## Acceptance Criteria

### Systemd
- [ ] All 5 systemd service units are syntactically valid (`systemd-analyze verify`)
- [ ] `yggdrasil-mimir.service` starts and passes health check on Munin
- [ ] `yggdrasil-odin.service` waits for Mimir health before starting (`ExecStartPre`)
- [ ] `yggdrasil-huginn.service` starts and exposes health endpoint on port 9092 on Hugin
- [ ] `yggdrasil-muninn.service` starts and passes health check on Hugin
- [ ] All services restart automatically on crash (`Restart=on-failure`)
- [ ] All services use `Type=notify` and send `READY=1` on startup
- [ ] All services send `WATCHDOG=1` at the configured interval
- [ ] `systemctl status yggdrasil-*.service` shows all services as active and healthy

### Health Checks
- [ ] Huginn exposes `GET /health` on port 9092 returning JSON with `status`, `watching`, `indexed_files`, `last_index_at`
- [ ] Existing health endpoints on Odin (8080), Mimir (9090), Muninn (9091) continue to work

### Metrics
- [ ] All HTTP services expose `GET /metrics` returning valid Prometheus exposition format
- [ ] Huginn exposes `GET /metrics` on its health listener port (9092)
- [ ] `ygg_http_requests_total` counter increments on each request
- [ ] `ygg_http_request_duration_seconds` histogram records request durations
- [ ] Mimir reports `ygg_engram_count` gauge with per-tier breakdown
- [ ] Odin reports `ygg_llm_generation_duration_seconds` histogram
- [ ] Odin reports `ygg_routing_intent_total` counter with per-intent breakdown
- [ ] Huginn reports `ygg_indexed_files_total` and `ygg_code_chunks_total` gauges
- [ ] Metrics endpoint response is < 50KB and responds in < 10ms

### Graceful Degradation
- [ ] With Mimir down: Odin chat completions work but without engram context (logged as warning)
- [ ] With Mimir down: Odin `/api/v1/query` and `/api/v1/store` return 503
- [ ] With Muninn down: Odin chat completions work but without code context (logged as warning)
- [ ] With Ollama down on Munin: coding model requests return 503, reasoning (Hugin) unaffected
- [ ] With Hades down: services stay running, all DB/Qdrant operations return 503

### Backup
- [ ] `deploy/backup-hades.sh` produces a PostgreSQL dump and Qdrant snapshots
- [ ] Backup files are named with timestamps and stored in the configured backup directory
- [ ] Old backups (> 7 days) are automatically cleaned up
- [ ] Restore procedure documented and tested: `pg_restore` from dump file, Qdrant snapshot restore

### Deployment
- [ ] `deploy/install.sh` installs binaries, configs, and systemd units on a target node
- [ ] `deploy/update.sh` performs rolling update with health check verification
- [ ] `deploy/rollback.sh` reverts to the previous binary
- [ ] Binary rollback works correctly (`.prev` file preserved)

### Documentation
- [ ] `docs/OPERATIONS.md` exists with complete operational runbook
- [ ] Port assignments documented: Odin 8080, Mimir 9090, Muninn 9091, Huginn 9092
- [ ] Graceful degradation matrix documented
- [ ] Prometheus scrape targets documented

## Post-Sprint Deploy Tasks

These items are deployment-only (not code changes) and are deferred to the `infra-devops` agent:

1. **Backup cron job installation on Munin.** The `deploy/backup-hades.sh` script is written and tested but the cron entry (`0 3 * * * /opt/yggdrasil/deploy/backup-hades.sh >> /var/log/yggdrasil-backup.log 2>&1`) has not been installed on Munin. The `infra-devops` agent must SSH to Munin and install the crontab entry for the `yggdrasil` user (or root, since pg_dump needs access to the Docker PG container).

2. **NetworkHardware.md stale model reference.** `NetworkHardware.md` still lists Munin as running "qwen3 14b". The actual deployed model is `qwen3-coder:30b-a3b-q4_K_M` via the IPEX-LLM container (Sprint 014). The `infra-devops` agent owns `NetworkHardware.md` and must update it.

## Bug Fixes Applied (Code Changes)

The following bugs were identified during Sprint 010 hardening review and fixed by `core-executor`:

1. **qwq-32b model references (Bug 1).** `ygg-ha/src/automation.rs` and `ygg-mcp-server/src/server.rs` contained hardcoded references to the `qwq-32b` model (deprecated since Sprint 013). All occurrences replaced with `qwen3:30b-a3b`. Verified: `cargo check --workspace` clean, zero `qwq` references remaining in the codebase.

2. **HA_TOKEN env var expansion (Bug 2).** `ygg-mcp-server/src/main.rs` now performs `${HA_TOKEN}` expansion in the loaded config's `ha.token` field at startup, since `serde_yaml` does not natively expand environment variables. This was a known discrepancy documented in the architecture memory.

3. **backup-hades.sh PG host (Sprint 010 Item 1).** The original draft targeted `pg_dump -h REDACTED_HADES_IP -d postgres` (Hades). PostgreSQL actually runs on Munin (localhost, pgvector Docker container). Fixed to `-h 127.0.0.1 -d yggdrasil`.

4. **WatchdogSec=30 re-enabled (Sprint 010 Item 2).** All 4 daemon systemd units (odin, mimir, huginn, muninn) now have `WatchdogSec=30`. This was previously removed due to sd-notify watchdog loop issues that have since been resolved in the Rust code. MCP server remains `Type=simple` (no watchdog) since its stdout is the JSON-RPC channel.

**Verification:** `cargo check --workspace` clean. `cargo test --workspace` -- 57 tests, all pass.

## Dependencies

| Dependency | Type | Status |
|------------|------|--------|
| Sprint 002 (Mimir MVP) | Must be implemented | Mimir service to add metrics and systemd |
| Sprint 003 (Huginn MVP) | Must be implemented | Huginn daemon to add health endpoint and metrics |
| Sprint 004 (Muninn MVP) | Must be implemented | Muninn service to add metrics and systemd |
| Sprint 005 (Odin) | Must be implemented | Odin orchestrator to add metrics and systemd |
| Sprint 008 (Mimir Advanced) | Should be implemented | Summarization metrics depend on Sprint 008 |
| `metrics` crate v0.24 | External dependency | Stable, widely used Rust metrics facade |
| `metrics-exporter-prometheus` crate v0.16 | External dependency | Prometheus exposition format exporter |
| `sd-notify` crate v0.4 | External dependency | systemd notification protocol |
| SSH access to Munin, Hugin | Infrastructure | For deployment scripts |
| Backup storage on Hades RAVEN pool | Infrastructure | 2.63 TiB SSD available |
| `yggdrasil` system user | Infrastructure | Created by `install.sh` |

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| `sd-notify` crate does not support the Ubuntu 25.10 systemd version | `sd-notify` implements the basic `sd_notify()` protocol which is stable across systemd versions. If issues arise, fall back to `Type=simple` without watchdog. |
| Prometheus metrics add memory overhead | The `metrics` crate uses lock-free atomics for counters and pre-allocated histogram buckets. Overhead is < 2MB per service. If memory is tight, reduce histogram bucket count. |
| Deployment scripts hardcode SSH credentials | Scripts use `jhernandez@<node>` which matches the SSH config in NetworkHardware.md. For production, SSH keys should be used instead of password authentication. Document this in `OPERATIONS.md`. |
| Backup script assumes `pg_dump` is available on the backup executor | The backup script runs on the machine that has network access to Hades. `pg_dump` must be installed. Document prerequisite: `apt install postgresql-client`. |
| Qdrant snapshot API is not available or returns errors | Qdrant snapshot API is available since Qdrant 1.0. If the API fails, log the error and skip Qdrant backup (PostgreSQL backup is the primary recovery path since Qdrant can be rebuilt from PostgreSQL embeddings). |
| Rolling update causes brief service unavailability | The update script stops the old instance, replaces the binary, and starts the new instance. Downtime is < 10s per service. For zero-downtime, a load balancer or socket activation would be needed (out of scope). |
| Huginn health listener conflicts with the file watcher async runtime | Huginn already uses Tokio. The health listener runs as a separate Tokio task on the same runtime. No conflict expected. |
| `metrics_middleware` adds latency to every request | The middleware records an `Instant::now()` and a `counter!` + `histogram!` call. Total overhead: < 1 microsecond per request. Negligible. |
| Multiple Prometheus scrapers hitting `/metrics` concurrently | `PrometheusHandle::render()` is thread-safe and lock-free. Multiple concurrent scrapes are handled correctly. |

## Decision Log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-03-09 | systemd `Type=notify` with `sd-notify` crate instead of `Type=simple` | `Type=notify` lets systemd know when the service is actually ready (after DB connections, migrations, LSH backfill). `Type=simple` assumes readiness on fork, which is incorrect for services with async startup. |
| 2026-03-09 | Odin `Requires=yggdrasil-mimir.service` with `ExecStartPre` health check | Odin depends on Mimir for engram proxy. Without Mimir, Odin's proxy endpoints return 503. Hard dependency ensures correct startup ordering. The health check script waits up to 30s for Mimir to be healthy before Odin starts. |
| 2026-03-09 | `metrics` crate (facade) + `metrics-exporter-prometheus` instead of `prometheus` crate | The `metrics` crate is the idiomatic Rust metrics facade (similar to `tracing` for logging). It decouples metric recording from export format. `prometheus` crate is the alternative but has a larger API surface and is more opinionated. |
| 2026-03-09 | Huginn health listener on port 9092 | Huginn is a daemon with no HTTP interface. Adding a minimal health listener enables systemd watchdog integration, Prometheus scraping, and deployment health checks. Port 9092 follows the existing port sequence (Mimir 9090, Muninn 9091). |
| 2026-03-09 | MCP server uses `Type=simple`, not `Type=notify` | MCP server is stdio-based and typically launched by IDE clients, not systemd. The systemd unit is for manual testing. `Type=simple` avoids needing `sd-notify` in a stdio process where stdout is the JSON-RPC channel (writing to stdout would corrupt the protocol). |
| 2026-03-09 | Backup to Hades RAVEN pool, not external storage | RAVEN is a 2.63 TiB SSD pool on the same machine as the databases. Backup is a local operation (fast, no network transfer). Off-site backup is a future enhancement. |
| 2026-03-09 | Shell scripts for deployment, not Ansible | The deployment is 5 services across 2 nodes. Ansible adds a dependency (Python, inventory files, playbook syntax) for a problem that is adequately solved by 3 shell scripts (< 50 lines each). If the deployment grows beyond 2 nodes or 10 services, migrate to Ansible. |
| 2026-03-09 | Graceful degradation is implemented in existing code paths, not a new module | Each service already handles downstream HTTP errors. The "degradation policy" is documenting and verifying existing behavior, plus ensuring Odin's RAG pipeline continues when Mimir/Muninn are unreachable (log warning, skip context, proceed). No new abstraction is needed. |
| 2026-03-09 | JSON structured logging as optional, not default | Default human-readable logs are easier to debug during development. JSON format is available via `RUST_LOG_FORMAT=json` env var for production journald aggregation. Both formats use the same `tracing-subscriber` infrastructure. |
| 2026-03-09 | Port 9092 for Huginn, completing the 9090-9092 sequence | Mimir: 9090, Muninn: 9091, Huginn: 9092. Consistent port numbering makes firewall rules and documentation simpler. Odin stays at 8080 as the external-facing gateway. |
| 2026-03-09 | WatchdogSec=30 re-enabled in all 4 daemon systemd units | Previously removed due to sd-notify watchdog loop not firing correctly. The Rust code now correctly spawns a tokio task that sends `WATCHDOG=1` every 15s (half of WatchdogSec=30). Standardized to 30s across all units (Huginn draft had 60s). MCP server excluded (Type=simple, stdout is JSON-RPC). |
| 2026-03-09 | backup-hades.sh: PG host changed from REDACTED_HADES_IP to 127.0.0.1, database from postgres to yggdrasil | Original draft incorrectly targeted Hades (REDACTED_HADES_IP) for PostgreSQL. Actual deployment: PG runs as a pgvector Docker container on Munin (localhost:5432), database name is `yggdrasil` not `postgres`. Script runs on Munin, so localhost is correct. |
| 2026-03-09 | qwq-32b references replaced with qwen3:30b-a3b in ygg-ha and ygg-mcp-server | Sprint 013 replaced QwQ-32B with qwen3:30b-a3b on Hugin, but AutomationGenerator and MCP server still hardcoded the old model name. Fixed to match actual deployed model. |
| 2026-03-09 | HA_TOKEN env var expansion added to ygg-mcp-server main.rs | serde_yaml does not natively expand `${VAR}` placeholders. Added explicit `std::env::var("HA_TOKEN")` expansion at startup in the MCP server binary, so the config file can use `${HA_TOKEN}` and the actual token is injected from the environment. |
