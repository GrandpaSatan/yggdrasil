Ready for review
Select text to add comments on the plan
Sprint 024: Housekeeping — Cleanup, Tags, Backup, Observability
Context
After Sprints 015–023 built and stabilized the CALM-inspired SDR memory system, several known issues and deferred tasks have accumulated. This sprint addresses them as a batch:

~10GB of unused Ollama models sitting on both nodes since Sprint 021 switched to ONNX
Backup cron flagged since Sprint 010, still not installed on Munin
NetworkHardware.md has stale model references and missing deployment state
Odin compiler warning from dead ContentPart.type field
MCP store_memory_tool silently drops tags that callers provide (Mimir fully supports them)
No observability dashboard for the Prometheus metrics all services already export
Changes
1. Remove stale qwen3-embedding Ollama model (~5GB each node)
Type: Ops (SSH commands, no code changes)

Both Munin and Hugin still have qwen3-embedding:latest installed. Sprint 021 switched all embedding to ONNX in-process all-MiniLM-L6-v2. The models are unused dead weight.

# Munin (REDACTED_MUNIN_IP) — IPEX container, use docker exec:
docker exec <ipex-container> ollama rm qwen3-embedding:latest

# Hugin (REDACTED_HUGIN_IP) — native Ollama:
ssh jhernandez@REDACTED_HUGIN_IP 'ollama rm qwen3-embedding:latest'
Verify with ollama list on each node afterward.

2. Install backup cron on Munin
Type: Ops (deploy script, systemd or crontab)

Script already exists: deploy/backup-hades.sh

Dumps PostgreSQL yggdrasil schema via pg_dump (localhost:5432)
Triggers Qdrant snapshots for engrams + code_chunks collections (REDACTED_HADES_IP:6333)
Stores dumps in /mnt/raven/yggdrasil-backups/ with 7-day retention
Requires postgresql-client package on Munin
Steps:

SSH to Munin, verify pg_dump is installed (apt install postgresql-client if not)
Copy script to /opt/yggdrasil/deploy/backup-hades.sh
Verify /mnt/raven is mounted (RAVEN pool on Hades, NFS or iSCSI)
Add crontab entry: 0 3 * * * /opt/yggdrasil/deploy/backup-hades.sh >> /var/log/yggdrasil-backup.log 2>&1
Run once manually to confirm it works
Risk: /mnt/raven may not be mounted on Munin. If not, need to set up NFS mount from Hades first, or change BACKUP_DIR to a local path.

3. Update NetworkHardware.md
Type: Docs

Fix stale content in the formatted markdown section at the top:

Update Munin Runs: line — remove "qwen3 14b" (model is qwen3-coder:30b-a3b-q4_K_M), remove "Whisper" if no longer active, add Mimir + Odin
Update PostgreSQL connection string — PG runs on Munin localhost:5432 (NOT Hades)
Add Hugin deployment details (Muninn service, Ollama, qwen3-coder)
Add Plume section with Nightjar (Grafana), Chirp (HA), Gitea, Peckhole
Remove duplicate raw-text inventory at bottom (lines 54-199), or reconcile with the formatted section
4. Fix Odin ContentPart.type compiler warning
File: crates/odin/src/openai.rs (lines 60-66)

The ContentPart struct has a dead r#type: Option<String> field (line 63) that is never read — only text is used (line 73). This causes a compiler warning on every build.

Fix: Remove the r#type field entirely. Serde's #[serde(default)] + missing fields = ignored by default, so removing it doesn't break deserialization of JSON payloads that include "type".

// Before:
#[derive(Deserialize)]
struct ContentPart {
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

// After:
#[derive(Deserialize)]
struct ContentPart {
    #[serde(default)]
    text: Option<String>,
}
Also need to add #[allow(dead_code)] or #[serde(deny_unkdefault)] + missing fields = ignored by default, so removing it doesn't break deserialization onown_fields)] is NOT used, so the missing field is just ignored. Clean one-line delete.

5. Wire engram tags through MCP and Odin layers
Files:

crates/ygg-mcp/src/tools.rs (lines 388-399)
crates/odin/src/handlers.rs (lines 162-183)
Problem: Two separate breaks in the tag pipeline:

Break 1 — MCP layer (tools.rs:390-394): The Req struct only serializes cause + effect, dropping params.tags. Fix: add tags to Req and populate from params.tags.

// Before:
#[derive(Serialize)]
struct Req<'a> {
    cause: &'a str,
    effect: &'a str,
}

// After:
#[derive(Serialize)]
struct Req<'a> {
    cause: &'a str,
    effect: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
}

let body = Req {
    cause: &params.cause,
    effect: &params.effect,
    tags: params.tags.unwrap_or_default(),
};
Also update the stale comment on line 388-389 — tags ARE supported by Mimir since Sprint 015.

Break 2 — Odin's spawn_engram_store (handlers.rs:162-183): Sends json!({ "cause": cause, "effect": effect }) with no tags. This is the fire-and-forget path from chat completions.

Fix: Add tags: Vec<String> parameter and include in the JSON body. Since Odin's chat handler doesn't have user-specified tags, pass an empty vec for now. The important fix is the MCP path (Break 1) where callers explicitly provide tags.

Note: Odin's proxy_store endpoint (handlers.rs:795-801) is a transparent byte proxy — it already forwards tags if the client sends them. No change needed there. The MCP tool routes through proxy_store, so fixing the MCP Req struct is sufficient for the MCP→Odin→Mimir path.

Downstream: NewEngram in ygg-domain/src/engram.rs:57 already has tags: Vec<String> with #[serde(default)]. Mimir's store_engram handler already reads body.tags and passes to insert_engram_sdr. No downstream changes needed.

6. Deploy Grafana on Nightjar for Prometheus observability
Type: Infra (Docker Compose on Nightjar)

Target: Nightjar (VM 101 on Plume, REDACTED_NIGHTJAR_IP, Debian, 4 cores, ARC A380, 16GB RAM)

Nightjar already runs Jellyfin, SearXNG, OpenWebUI, and other services. Grafana is lightweight and fits easily.

Stack:

Grafana (OSS, Apache 2.0): Dashboard and alerting
Prometheus: Scrapes metrics from Odin (REDACTED_MUNIN_IP:8080/metrics), Mimir (REDACTED_MUNIN_IP:9090/metrics), Muninn (REDACTED_HUGIN_IP:9091/metrics), Huginn (REDACTED_HUGIN_IP:9092/metrics)
Deployment:

Create deploy/nightjar/docker-compose.grafana.yml:

services:
  prometheus:
    image: prom/prometheus:latest
    ports: ["9092:9090"]  # avoid conflict with local services
    volumes:
      - ./prometheus.yml:/etc/prometheus/prometheus.yml
      - prometheus_data:/prometheus
    restart: unless-stopped
  grafana:
    image: grafana/grafana-oss:latest
    ports: ["3000:3000"]
    volumes:
      - grafana_data:/var/lib/grafana
    environment:
      GF_SECURITY_ADMIN_PASSWORD: "${GRAFANA_ADMIN_PASSWORD:-admin}"
    restart: unless-stopped
volumes:
  prometheus_data:
  grafana_data:
Create deploy/nightjar/prometheus.yml:

global:
  scrape_interval: 15s
scrape_configs:
  - job_name: odin
    static_configs:
      - targets: ['REDACTED_MUNIN_IP:8080']
  - job_name: mimir
    static_configs:
      - targets: ['REDACTED_MUNIN_IP:9090']
  - job_name: muninn
    static_configs:
      - targets: ['REDACTED_HUGIN_IP:9091']
  - job_name: huginn
    static_configs:
      - targets: ['REDACTED_HUGIN_IP:9092']
SSH to Nightjar, copy files, docker compose up -d

Access Grafana at http://REDACTED_NIGHTJAR_IP:3000, add Prometheus data source (http://prometheus:9090)

Import or create dashboards for request latency, engram counts, embedding times, SDR recall performance

Port note: Prometheus default 9090 conflicts with Mimir's port on Munin. Use 9092 on Nightjar (no conflict — Huginn's health port 9092 is on Hugin, not Nightjar). Alternatively just use a different host port mapping.

Files Modified
File	Changes
crates/odin/src/openai.rs	Remove dead r#type field from ContentPart
crates/ygg-mcp/src/tools.rs	Add tags to store_memory Req, update stale comment
crates/odin/src/handlers.rs	Add tags param to spawn_engram_store
docs/NetworkHardware.md	Fix stale model refs, PG location, add deployment state
deploy/nightjar/docker-compose.grafana.yml	New — Grafana + Prometheus stack
deploy/nightjar/prometheus.yml	New — scrape config for all 4 services
Verification
cargo build --release --bin odin --bin mimir — no warnings (ContentPart.type gone)
cargo build --release --bin ygg-mcp-server — clean compile with tag wiring
MCP store_memory_tool with tags → verify tags appear in SELECT tags FROM yggdrasil.engrams WHERE ...
MCP query_memory_tool → still works (no regression)
ollama list on both nodes — qwen3-embedding removed
Manual backup run on Munin — pg_dump succeeds, Qdrant snapshots trigger
curl http://REDACTED_NIGHTJAR_IP:3000 — Grafana login page accessible
Prometheus targets page (http://REDACTED_NIGHTJAR_IP:9092/targets) — all 4 services UP
NetworkHardware.md — review for accuracy
Add Comment
Import or create dashboards for request latency, engram counts, embedding times, SDR recall performance
