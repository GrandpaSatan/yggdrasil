# Yggdrasil Shared Memory Fabric — deploy artifacts (Sprint 069 Phase G)

Runs on **Hugin :11450**. Uses **Valkey on Munin :6479** as primary
storage (reached over USB4) with an in-memory DashMap fallback when
Valkey is unreachable. Uses **TEI on Munin :11438** (all-MiniLM-L6-v2)
for 384-dim embeddings.

## Deploy sequence

### 1. Valkey warm-remote tier on Munin

```bash
ssh jhernandez@10.0.65.8
cd /opt/yggdrasil/deploy/munin/valkey
docker compose pull
docker compose up -d
# verify
redis-cli -h 10.0.65.8 -p 6479 PING
```

### 2. Fabric service on Hugin

```bash
# build + deploy the binary
cargo build --release -p ygg-memory-fabric
scp target/release/ygg-memory-fabric jhernandez@10.0.65.9:/tmp/
ssh jhernandez@10.0.65.9 'sudo cp /tmp/ygg-memory-fabric /opt/yggdrasil/bin/ && sudo chown yggdrasil:yggdrasil /opt/yggdrasil/bin/ygg-memory-fabric && sudo chmod 755 /opt/yggdrasil/bin/ygg-memory-fabric'

# install the systemd unit
scp yggdrasil-memory-fabric.service jhernandez@10.0.65.9:/tmp/
ssh jhernandez@10.0.65.9 'sudo cp /tmp/yggdrasil-memory-fabric.service /etc/systemd/system/ && sudo systemctl daemon-reload && sudo systemctl enable --now yggdrasil-memory-fabric.service'

# verify
curl http://10.0.65.9:11450/health
curl http://10.0.65.9:11450/metrics | head
```

### 3. Smoke test

```bash
# publish a step
curl -X POST http://10.0.65.9:11450/fabric/publish \
  -H 'Content-Type: application/json' \
  -d '{"flow_id":"smoke-1","step_n":1,"model":"saga-350m","text":"The swarm stores its working memory in the fabric."}'

# retrieve it
curl -X POST http://10.0.65.9:11450/fabric/query \
  -H 'Content-Type: application/json' \
  -d '{"flow_id":"smoke-1","query_text":"where does the swarm keep its state?","top_k":3}'

# evict
curl -X POST http://10.0.65.9:11450/fabric/done \
  -H 'Content-Type: application/json' \
  -d '{"flow_id":"smoke-1"}'
```

### 4. Odin cutover (when ready)

Once the fabric service has soaked for at least an hour with
production traffic mirrored (via YGG_FABRIC_ENABLED=1 on a shadow
Odin), flip the live Odin:

```bash
# add to /opt/yggdrasil/.env on Munin:
# YGG_FABRIC_ENABLED=1
# YGG_FABRIC_URL=http://10.0.65.9:11450

sudo systemctl restart yggdrasil-odin.service
```

Odin auto-publishes every flow step's output and enriches every
step's prompt with the top-3 prior steps retrieved by cosine
similarity. Flows NEVER fail because of fabric issues — every call
degrades silently to no-op.

## Rollback

```bash
# on Munin:
# in /opt/yggdrasil/.env set YGG_FABRIC_ENABLED=0
sudo systemctl restart yggdrasil-odin.service
```

Odin returns to Sprint 068 behavior instantly. Fabric service +
Valkey stay up idempotent; they're just not called.

## Metrics

Fabric exports Prometheus metrics on `/metrics`:

| Metric | Labels | Meaning |
|---|---|---|
| `ygg_fabric_publish_total` | `model` | Total step-output publishes |
| `ygg_fabric_query_total` | `flow_id_bucket` | Total working-memory queries |
| `ygg_fabric_l3_hits_total` | `pair` | Non-empty query results (hit rate proxy) |
| `ygg_fabric_evictions_total` | `reason` | Explicit + TTL evictions |
| `ygg_fabric_bytes_stored` | `tier` | Approximate bytes per tier |
| `ygg_fabric_publish_latency_seconds` | `model` | Publish path histogram |
| `ygg_fabric_query_latency_seconds` | `cache_hit` | Query path histogram |

Scrape target: `http://10.0.65.9:11450/metrics`.
