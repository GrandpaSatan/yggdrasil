# Yggdrasil Operations Runbook

## Service Architecture Overview

| Service | Binary | Node | Port | Role |
|---------|--------|------|------|------|
| Mimir | `mimir` | Munin (REDACTED_MUNIN_IP) | 9090 | Engram memory service (PostgreSQL + Qdrant + LSH) |
| Odin | `odin` | Munin (REDACTED_MUNIN_IP) | 8080 | LLM orchestrator, semantic router, RAG pipeline |
| ygg-mcp-server | `ygg-mcp-server` | Munin (REDACTED_MUNIN_IP) | stdio | MCP server for IDE clients (stdio transport) |
| Muninn | `muninn` | Hugin (REDACTED_HUGIN_IP) | 9091 | Code retrieval engine (hybrid search) |
| Huginn | `huginn` | Hugin (REDACTED_HUGIN_IP) | 9092 | Code indexer + file watcher |
| PostgreSQL | — | Hades (REDACTED_HADES_IP) | 5432 | Primary database (yggdrasil schema) |
| Qdrant | — | Hades (REDACTED_HADES_IP) | 6333/6334 | Vector database (engrams + code_chunks collections) |

Startup ordering: Hades (PostgreSQL + Qdrant) must be reachable before any
Yggdrasil service starts. On Munin, `yggdrasil-mimir.service` starts first
and `yggdrasil-odin.service` waits for Mimir's `/health` to return 200 before
its own process starts (`ExecStartPre`).

---

## Starting and Stopping Services

### Start all services on Munin

```bash
ssh jhernandez@munin
sudo systemctl start yggdrasil-mimir
sudo systemctl start yggdrasil-odin
```

### Start all services on Hugin

```bash
ssh jhernandez@hugin
sudo systemctl start yggdrasil-muninn
sudo systemctl start yggdrasil-huginn
```98,181
22
Last updated:
13 days ago
￼
￼
Staff Pick



### Stop a service

```bash
sudo systemctl stop yggdrasil-<service>
```

### Check service status98,181
22
Last updated:
13 days ago
￼
￼
Staff Pick



```bash
sudo systemctl status yggdrasil-mimir
sudo systemctl status yggdrasil-odin
sudo systemctl status yggdrasil-huginn
sudo systemctl status yggdrasil-muninn
```

### Reload systemd after unit file changes

```bash
sudo systemctl daemon-reload
```

### Enable a service to start on boot

```bash
sudo systemctl enable yggdrasil-<service>
```

---

## Common journalctl Queries

### Follow live logs for a service

```bash
journalctl -u yggdrasil-odin -f
journalctl -u yggdrasil-mimir -f
journalctl -u yggdrasil-huginn -f
journalctl -u yggdrasil-muninn -f
```

### View errors from the last hour98,181
22
Last updated:
13 days ago
￼
￼
Staff Pick



```bash
journalctl -u yggdrasil-mimir --since "1 hour ago" -p err
journalctl -u yggdrasil-odin --since "1 hour ago" -p err
```

### View today's indexing activity (Huginn)

```bash
journalctl -u yggdrasil-huginn --grep "indexed" --since today
```

### View all Yggdrasil services since last boot

```bash
journalctl -u "yggdrasil-*" -b
```

### View watchdog notifications

```bash
journalctl -u yggdrasil-mimir --grep "watchdog" -b
```

### View the last 100 lines of a service log

```bash
journalctl -u yggdrasil-odin -n 100 --no-pager
```

---

## Health Check Endpoints

| Service | URL | Expected Response |
|---------|-----|-------------------|
| Odin | `GET http://munin:8080/health` | `200 OK` (JSON with backend status) |
| Mimir | `GET http://munin:9090/health` | `200 OK` |
| Muninn | `GET http://hugin:9091/health` | `200 OK` |
| Huginn | `GET http://hugin:9092/health` | `200 OK` (JSON with indexing stats) |

### Manual health checks

```bash
curl -s http://localhost:8080/health  # Odin
curl -s http://localhost:9090/health  # Mimir
curl -s http://localhost:9091/health  # Muninn
curl -s http://localhost:9092/health  # Huginn
```

---

## Prometheus Scrape Targets

All HTTP services expose `GET /metrics` in Prometheus text exposition format
(Content-Type: `text/plain; version=0.0.4`). No authentication required.

| Service | Scrape URL | Scrape Interval |
|---------|-----------|----------------|
| Mimir | `http://munin:9090/metrics` | 15s |
| Odin | `http://munin:8080/metrics` | 15s |
| Muninn | `http://hugin:9091/metrics` | 15s |
| Huginn | `http://hugin:9092/metrics` | 15s |

### Prometheus scrape config snippet

```yaml
scrape_configs:
  - job_name: yggdrasil
    static_configs:
      - targets:
          - munin:9090   # mimir
          - munin:8080   # odin
          - hugin:9091   # muninn
          - hugin:9092   # huginn
    scrape_interval: 15s
```

### Manual metrics check

```bash
curl -s http://localhost:9090/metrics | head -40
curl -s http://localhost:8080/metrics | wc -c  # should be < 50KB
```

---

## Key Metrics Reference

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `ygg_http_requests_total` | counter | service, endpoint, status | Total HTTP requests |
| `ygg_http_request_duration_seconds` | histogram | service, endpoint | Request duration |
| `ygg_routing_intent_total` | counter | intent | Odin routing decisions |
| `ygg_llm_generation_duration_seconds` | histogram | model | Ollama call duration |
| `ygg_backend_active_requests` | gauge | backend | Active Ollama requests |
| `ygg_engram_count` | gauge | tier | Engrams by tier (core/recall/archival) |
| `ygg_embedding_duration_seconds` | histogram | — | Embedding API call duration |
| `ygg_summarization_total` | counter | — | Summarization batches completed |
| `ygg_summarization_engrams_archived` | counter | — | Total engrams archived |
| `ygg_search_duration_seconds` | histogram | — | Muninn search pipeline duration |
| `ygg_search_results_count` | histogram | — | Results returned per search |
| `ygg_qdrant_duration_seconds` | histogram | operation | Qdrant operation duration |
| `ygg_indexed_files_total` | gauge | — | Files indexed by Huginn |
| `ygg_code_chunks_total` | gauge | — | Code chunks stored by Huginn |
| `ygg_watcher_events_total` | counter | event | File watcher events (modify/create/remove) |

---

## Backup and Restore

### Backup (run on Hades or a machine with network access to Hades)

The backup script is at `deploy/backup-hades.sh`.

```bash
# Manual backup
./deploy/backup-hades.sh

# Scheduled backup (add to crontab on Hades)
crontab -e
# Add: 0 3 * * * /opt/yggdrasil/deploy/backup-hades.sh >> /var/log/yggdrasil-backup.log 2>&1
```

Backup files are stored in `/mnt/raven/yggdrasil-backups/`:
- `pg_yggdrasil_YYYYMMDD_HHMMSS.dump` — PostgreSQL custom format dump
- `qdrant_<collection>_YYYYMMDD_HHMMSS.snapshot` — Qdrant snapshot trigger records

Retention: 7 days. Old files are automatically cleaned up.

Prerequisites on the backup executor:
```bash
apt install postgresql-client curl
```

### PostgreSQL Restore

```bash
# Restore from a custom-format dump
pg_restore -h REDACTED_HADES_IP -U jhernandez -d postgres \
  --schema=yggdrasil \
  /mnt/raven/yggdrasil-backups/pg_yggdrasil_YYYYMMDD_HHMMSS.dump
```

### Qdrant Restore

Qdrant snapshots are stored locally on the Qdrant server. To restore:

1. Stop the service that writes to the collection (Mimir or Huginn).
2. Use the Qdrant REST API to restore from snapshot:

```bash
# List available snapshots
curl -s http://REDACTED_HADES_IP:6333/collections/engrams/snapshots | jq .

# Restore from a snapshot (replace <snapshot_name> with the actual name)
curl -X PUT "http://REDACTED_HADES_IP:6333/collections/engrams/snapshots/recover" \
  -H "Content-Type: application/json" \
  -d '{"location": "/path/to/snapshot/<snapshot_name>"}'
```

If Qdrant data is lost entirely, the vector index can be rebuilt from
PostgreSQL by re-embedding all engrams (Mimir startup will detect missing
vectors and can re-index).

---

## Deployment (Install / Update / Rollback)

### First-time installation

```bash
# Install Odin and Mimir on Munin
./deploy/install.sh munin odin mimir

# Install Huginn and Muninn on Hugin
./deploy/install.sh hugin huginn muninn
```

### Rolling update (one service)

```bash
# Update Odin on Munin (stops service, replaces binary, restarts, health checks)
./deploy/update.sh munin odin

# Update Huginn on Hugin
./deploy/update.sh hugin huginn
```

### Rollback to previous binary

```bash
# Rollback Odin on Munin (replaces binary with .prev, restarts)
./deploy/rollback.sh munin odin
```

The `.prev` binary is preserved as `.failed` after rollback so you can
inspect what was deployed.

### SSH note

Scripts use `jhernandez@<node>` for SSH. SSH key-based authentication is
required. Ensure `~/.ssh/id_rsa.pub` (or equivalent) is in
`~/.ssh/authorized_keys` on Munin and Hugin.

---

## Graceful Degradation Matrix

| Component Down | Impact | Odin Behavior |
|----------------|--------|---------------|
| Mimir | No engram memory in responses | Proxy endpoints `/api/v1/query` and `/api/v1/store` return 503. Chat completions skip engram context injection and engram store-on-completion. Logs warning. Chat still works. |
| Muninn | No code search in responses | Chat completions skip code context injection. Logs warning. Chat still works. |
| Ollama on Munin | Coding model requests fail | Chat completions to coding model return 503. Reasoning model (Hugin) unaffected. |
| Ollama on Hugin | Reasoning model requests fail | Chat completions to reasoning model return 503. Coding requests unaffected. |
| Hades (PostgreSQL) | Memory and search unavailable | Mimir returns 503. Muninn returns 503. Huginn stops indexing. Chat without context still works via Odin → Ollama directly. |
| Hades (Qdrant) | Reduced search quality | Mimir falls back to PostgreSQL-only query (pgvector). Muninn search degrades. Functional but slower. |
| Huginn | Code search serves stale index | No new code indexing. Existing index in PostgreSQL/Qdrant still searchable. No user impact unless codebase changes frequently. |
| Home Assistant | HA tool calls fail | HA tools return error responses. Non-HA features unaffected. Implemented in Sprint 007. |

---

## Port Assignments

| Service | Port | Protocol | Node |
|---------|------|----------|------|
| Odin | 8080 | HTTP | Munin |
| Mimir | 9090 | HTTP | Munin |
| Muninn | 9091 | HTTP | Hugin |
| Huginn | 9092 | HTTP | Hugin |
| PostgreSQL | 5432 | TCP | Hades |
| Qdrant REST | 6333 | HTTP | Hades |
| Qdrant gRPC | 6334 | gRPC | Hades |

---

## Node Deployment Mapping

| Node | Hostname | IP | Services | Hardware |
|------|----------|-----|---------|----------|
| Munin | munin | REDACTED_MUNIN_IP | Odin, Mimir, ygg-mcp-server | Intel Core Ultra 185H, 48GB DDR5 |
| Hugin | hugin | REDACTED_HUGIN_IP | Huginn, Muninn | AMD Ryzen 7 255, 64GB DDR5 |
| Hades | hades | REDACTED_HADES_IP | PostgreSQL, Qdrant | Intel N150, 32GB DDR5, Merlin SSD pool (444 GiB), RAVEN SSD pool (2.63 TiB) |

---

## Common Troubleshooting

### Service fails to start

1. Check the unit status: `sudo systemctl status yggdrasil-<service>`
2. Check recent logs: `journalctl -u yggdrasil-<service> -n 50`
1. Check the unit status: `sudo systemctl status yggdrasil-<service>`
2. Check recent logs: `journalctl -u yggdrasil-<service> -n 50`
3. Verify the binary exists: `ls -la /opt/yggdrasil/bin/<service>`
4. Verify the config file exists: `ls -la /etc/yggdrasil/<service>/`
5. Test the config manually: `/opt/yggdrasil/bin/<service> --config /etc/yggdrasil/<service>/config.yaml`

### Odin fails to start (waits for Mimir)

Odin's `ExecStartPre` polls Mimir's `/health` for up to 30 seconds. If Mimir
is not healthy:
1. Check Mimir status: `sudo systemctl status yggdrasil-mimir`
2. Check Hades connectivity: `curl -s http://REDACTED_HADES_IP:5432` (should connect)
3. Check Mimir logs for database errors: `journalctl -u yggdrasil-mimir -n 100`

### Metrics endpoint not responding

```bash
curl -v http://localhost:<port>/metrics
```

Verify the service is running and the port is correct. The `/metrics` endpoint
is served by the same Axum process as all other endpoints — if `/health` works,
`/metrics` should too.

### Huginn health endpoint not reachable

Huginn's health server only runs in `watch` mode (not `index` mode). The
systemd unit uses `watch` subcommand. If running manually, specify:
```bash
/opt/yggdrasil/bin/huginn --config /etc/yggdrasil/huginn/config.yaml watch
```

### High memory usage

Each service uses < 2MB additional RSS for metrics counters. If memory is
higher than expected, check for embedding model loading (Mimir/Muninn use
Ollama via HTTP, not in-process) or large Qdrant result sets.

