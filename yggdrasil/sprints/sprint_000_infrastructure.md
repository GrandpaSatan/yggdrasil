# Sprint 000 — Infrastructure Prerequisites
**Type:** Manual Infrastructure (no code changes)
**Status:** IN PROGRESS
**Owner:** jhernandez
**Hardware source of truth:** `/home/jesus/Documents/HardwareSetup/NetworkHardware.md`
**Date:** 2026-03-09

---

## Objective

Provision all remote services that the Yggdrasil Rust codebase depends on before any binary
can connect. This sprint is a gate — Sprint 001 (Rust bring-up) cannot start until every
acceptance criterion below is checked off.

No files in this repository are modified during this sprint. All work is performed manually on
remote hosts via SSH or TrueNAS UI.

---

## Scope

| # | System | Host | What Must Exist |
|---|--------|------|-----------------|
| 1 | PostgreSQL (cerberus container) | REDACTED_HADES_IP | `vector` extension, `yggdrasil` schema, migrations 001 + 002 applied |
| 2 | Qdrant (qdrant container) | REDACTED_HADES_IP | Container running, collections `engrams` + `code_chunks` created |
| 3 | Ollama on munin | REDACTED_MUNIN_IP | `qwen3-coder:30b-a3b` + embedding model pulled and answering |
| 4 | Ollama on hugin | REDACTED_HUGIN_IP | Ollama installed, `qwq:32b` + embedding model pulled, listening on 0.0.0.0 |
| 5 | Network verification | from munin | All four services reachable cross-host |

---

## Hardware Reference

### hades (REDACTED_HADES_IP) — TrueNAS Scale 25.04.2.6

| Attribute | Value |
|-----------|-------|
| CPU | Intel N150 |
| RAM | 32 GB DDR5 SODIMM |
| Pool for DB/Qdrant | Merlin — 444.32 GiB available (3 SATA SSD) |
| PostgreSQL container | `postgre` on cerberus (TrueNAS) |
| Qdrant container | `qdrant` on cerberus (TrueNAS) |
| DB user | jhernandez |
| DB pass | K6m4B129CF9u |

### munin (REDACTED_MUNIN_IP) — Ubuntu Server 25.10

| Attribute | Value |
|-----------|-------|
| CPU | Intel Core Ultra 9 185H |
| RAM | 48 GB DDR5 |
| GPU | Arc iGPU |
| Network | 2x 5 GbE |
| SSH | jhernandez / 723559 |
| Already running | Ollama with qwen3:14b, Whisper |

### hugin (REDACTED_HUGIN_IP) — Ubuntu Server 25.10

| Attribute | Value |
|-----------|-------|
| CPU | AMD Ryzen 7 255 |
| RAM | 64 GB DDR5 |
| GPU | iGPU |
| SSH | jhernandez / 723559 |
| Status | Experimental — Ollama state unknown [UNVERIFIED] |

---

## Port Allocation (services this sprint depends on)

| Port | Protocol | Host | Service | Direction |
|------|----------|------|---------|-----------|
| 5432 | TCP | REDACTED_HADES_IP | PostgreSQL | inbound from yggdrasil clients |
| 6333 | HTTP | REDACTED_HADES_IP | Qdrant REST API | inbound from yggdrasil clients |
| 6334 | gRPC | REDACTED_HADES_IP | Qdrant gRPC API | inbound from yggdrasil clients |
| 11434 | HTTP | REDACTED_MUNIN_IP | Ollama (munin) | inbound from yggdrasil clients |
| 11434 | HTTP | REDACTED_HUGIN_IP | Ollama (hugin) | inbound from yggdrasil clients |

---

## Task 1 — PostgreSQL on hades (cerberus container)

**Access method:** SSH into hades, then exec into the `postgre` container, or use the TrueNAS
Shell tab to reach the container directly.

```
ssh jhernandez@REDACTED_HADES_IP
# Then find the container shell via TrueNAS UI → Apps → postgre → Shell
# or: docker exec -it postgre psql -U jhernandez
```

### Checklist

- [ ] **1.1** Confirm the `postgre` container is running in TrueNAS UI (Apps or Containers section).
- [ ] **1.2** Open a `psql` session as `jhernandez`:
  ```sql
  -- From inside the container or via:
  psql "postgres://jhernandez:K6m4B129CF9u@REDACTED_HADES_IP:5432/postgres"
  ```
- [ ] **1.3** Install pgvector extension (safe to re-run):
  ```sql
  CREATE EXTENSION IF NOT EXISTS vector;
  ```
- [ ] **1.4** Create the yggdrasil schema (safe to re-run):
  ```sql
  CREATE SCHEMA IF NOT EXISTS yggdrasil;
  ```
- [ ] **1.5** Run migration 001 — engram schema. Copy and paste the contents of
  `migrations/001_engram_schema.up.sql` into the psql session, or pipe it in:
  ```bash
  psql "postgres://jhernandez:K6m4B129CF9u@REDACTED_HADES_IP:5432/postgres" \
    -f /path/to/yggdrasil/migrations/001_engram_schema.up.sql
  ```
  This creates: `yggdrasil.engrams`, `yggdrasil.lsh_buckets`, and their indexes.
  The IVFFlat index on `cause_embedding` requires `vector(1024)` — pgvector must already be
  installed (step 1.3) or this will fail.
- [ ] **1.6** Run migration 002 — index metadata:
  ```bash
  psql "postgres://jhernandez:K6m4B129CF9u@REDACTED_HADES_IP:5432/postgres" \
    -f /path/to/yggdrasil/migrations/002_index_metadata.up.sql
  ```
  This creates: `yggdrasil.indexed_files`, `yggdrasil.code_chunks`, and their indexes
  (including the GIN index on the generated `search_vec` tsvector column).
- [ ] **1.7** Verify migrations applied cleanly — expected result: empty set, no error:
  ```sql
  SELECT * FROM yggdrasil.engrams LIMIT 1;
  SELECT * FROM yggdrasil.indexed_files LIMIT 1;
  SELECT * FROM yggdrasil.code_chunks LIMIT 1;
  ```
- [ ] **1.8** Verify the pgvector extension is active:
  ```sql
  SELECT * FROM pg_extension WHERE extname = 'vector';
  -- Should return one row.
  ```

**Expected final state:** Three tables exist under the `yggdrasil` schema, all indexes are
present, and `SELECT 1` connects without error from an external host.

---

## Task 2 — Qdrant on hades (qdrant container)

**Note from NetworkHardware.md:** "I have never used this before, no idea if it's configured
yet." Treat this task as a bring-up from unknown state.

**Context:** Qdrant replaces pgvector for vector search per the note in NetworkHardware.md.
The Rust codebase uses `qdrant-client = "1"`. Collections require 1024-dimensional Cosine
vectors to match the embedding model output.

### Checklist

- [ ] **2.1** Open TrueNAS UI → Apps (or Containers). Confirm whether a container named
  `qdrant` exists.
  - If it does not exist: deploy the official Qdrant image (`qdrant/qdrant`) via TrueNAS
    with the following port bindings: `6333:6333` (REST) and `6334:6334` (gRPC). Map
    a persistent storage path on the Merlin pool for `/qdrant/storage` inside the container.
  - If it exists but is stopped: start it.
- [ ] **2.2** Verify Qdrant REST API is reachable from hades itself:
  ```bash
  curl -s http://REDACTED_HADES_IP:6333/collections | python3 -m json.tool
  # Expected: {"result":{"collections":[]},"status":"ok","time":...}
  ```
- [ ] **2.3** Create the `engrams` collection (1024-dim, Cosine distance):
  ```bash
  curl -s -X PUT http://REDACTED_HADES_IP:6333/collections/engrams \
    -H 'Content-Type: application/json' \
    -d '{"vectors":{"size":1024,"distance":"Cosine"}}' \
    | python3 -m json.tool
  # Expected: {"result":true,"status":"ok","time":...}
  ```
- [ ] **2.4** Create the `code_chunks` collection (1024-dim, Cosine distance):
  ```bash
  curl -s -X PUT http://REDACTED_HADES_IP:6333/collections/code_chunks \
    -H 'Content-Type: application/json' \
    -d '{"vectors":{"size":1024,"distance":"Cosine"}}' \
    | python3 -m json.tool
  # Expected: {"result":true,"status":"ok","time":...}
  ```
- [ ] **2.5** Verify both collections exist:
  ```bash
  curl -s http://REDACTED_HADES_IP:6333/collections | python3 -m json.tool
  # "collections" array should list both "engrams" and "code_chunks"
  ```
- [ ] **2.6** Confirm gRPC port is open (the Rust qdrant-client uses gRPC by default):
  ```bash
  nc -zv REDACTED_HADES_IP 6334
  # Expected: Connection to REDACTED_HADES_IP 6334 port [tcp/*] succeeded!
  ```

**Expected final state:** Qdrant container running, both collections present with 1024-dim
Cosine config, REST and gRPC ports reachable from the local network.

---

## Task 3 — Ollama on munin (REDACTED_MUNIN_IP)

Munin already runs Ollama with `qwen3:14b`. This task adds the coding and embedding models.

### Checklist

- [ ] **3.1** SSH into munin:
  ```bash
  ssh jhernandez@REDACTED_MUNIN_IP
  # password: 723559
  ```
- [ ] **3.2** Verify Ollama service is running:
  ```bash
  systemctl status ollama
  ollama list
  ```
- [ ] **3.3** Pull the coding model. Try in order — use whichever tag resolves:
  ```bash
  ollama pull qwen3-coder:30b-a3b
  # If the above tag is not found, check available tags:
  # curl -s https://ollama.com/api/tags | python3 -m json.tool | grep qwen3-coder
  # Then pull the closest 30B-A3B variant shown.
  ```
  Note: `30b-a3b` denotes 30B total params with 3B active (MoE). This is a large pull —
  plan for significant download time and ~18 GB of disk space.
- [ ] **3.4** Pull the embedding model. Try in order:
  ```bash
  ollama pull qwen3-embedding
  # Fallback if not found:
  ollama pull bge-m3
  # bge-m3 outputs 1024-dim embeddings — matches the vector(1024) schema.
  # qwen3-embedding output dimension must be confirmed before using it.
  ```
  **IMPORTANT:** Whichever model is used, verify its output dimension is exactly 1024:
  ```bash
  curl -s http://localhost:11434/api/embeddings \
    -d '{"model":"<model-name>","prompt":"test"}' \
    | python3 -c "import sys,json; v=json.load(sys.stdin)['embedding']; print(len(v))"
  # Must print: 1024
  ```
  If the dimension is not 1024, the model cannot be used with the current schema without
  a migration. Flag this as a blocker before proceeding.
- [ ] **3.5** Verify both models appear in `ollama list`.
- [ ] **3.6** Test embedding endpoint responds:
  ```bash
  curl -s http://localhost:11434/api/embeddings \
    -d '{"model":"<embedding-model-name>","prompt":"test"}' \
    | python3 -m json.tool
  # "embedding" field should be an array of 1024 floats.
  ```
- [ ] **3.7** Confirm Ollama is listening on all interfaces (needed so hugin and other hosts
  can call munin):
  ```bash
  ss -tlnp | grep 11434
  # Should show 0.0.0.0:11434 or :::11434, NOT 127.0.0.1:11434
  ```
  If it is bound to localhost only, add `Environment="OLLAMA_HOST=0.0.0.0"` to the
  systemd unit (see Task 4 step 4.6 for the procedure — same applies here).

**Expected final state:** `ollama list` on munin shows qwen3-coder (30B-A3B variant) and
the embedding model. Embedding test returns a 1024-element array. Port 11434 is reachable
from the local network.

---

## Task 4 — Ollama on hugin (REDACTED_HUGIN_IP)

Hugin's Ollama state is unknown [UNVERIFIED]. Treat as a fresh install.

### Checklist

- [ ] **4.1** SSH into hugin:
  ```bash
  ssh jhernandez@REDACTED_HUGIN_IP
  # password: 723559
  ```
- [ ] **4.2** Check if Ollama is already installed:
  ```bash
  which ollama && ollama --version
  systemctl status ollama 2>/dev/null || echo "No systemd unit found"
  ```
- [ ] **4.3** If Ollama is NOT installed, install it:
  ```bash
  curl -fsSL https://ollama.com/install.sh | sh
  # The installer creates a systemd unit and starts the service automatically.
  ```
- [ ] **4.4** Pull the reasoning model:
  ```bash
  ollama pull qwq:32b
  # Large model — plan for significant download time and ~20 GB disk space.
  ```
- [ ] **4.5** Pull the embedding model (same choice as munin — must be consistent):
  ```bash
  ollama pull qwen3-embedding
  # Fallback:
  ollama pull bge-m3
  ```
  Verify the dimension matches munin's embedding model (must be 1024):
  ```bash
  curl -s http://localhost:11434/api/embeddings \
    -d '{"model":"<model-name>","prompt":"test"}' \
    | python3 -c "import sys,json; v=json.load(sys.stdin)['embedding']; print(len(v))"
  # Must print: 1024
  ```
- [ ] **4.6** Configure Ollama to listen on all interfaces. Edit the systemd unit:
  ```bash
  sudo systemctl edit ollama
  ```
  In the override file that opens, add:
  ```ini
  [Service]
  Environment="OLLAMA_HOST=0.0.0.0"
  ```
  Save, then reload and restart:
  ```bash
  sudo systemctl daemon-reload
  sudo systemctl restart ollama
  ```
  Verify:
  ```bash
  ss -tlnp | grep 11434
  # Must show 0.0.0.0:11434, not 127.0.0.1:11434
  ```
- [ ] **4.7** Verify both models appear in `ollama list`.

**Expected final state:** Ollama installed on hugin, `qwq:32b` and the embedding model
present, service bound to `0.0.0.0:11434`, reachable from the local network.

---

## Task 5 — Network Verification

Run these checks from munin (REDACTED_MUNIN_IP) to confirm all services are reachable cross-host.
This simulates what the Rust binaries will do at startup.

### Checklist

- [ ] **5.1** Qdrant reachable from munin:
  ```bash
  curl -s http://REDACTED_HADES_IP:6333/collections | python3 -m json.tool
  # Must list "engrams" and "code_chunks" collections.
  ```
- [ ] **5.2** PostgreSQL reachable from munin:
  ```bash
  psql "postgres://jhernandez:K6m4B129CF9u@REDACTED_HADES_IP:5432/postgres" -c "SELECT 1"
  # Must return: ?column? = 1
  # If psql is not installed: sudo apt install postgresql-client
  ```
- [ ] **5.3** Verify yggdrasil schema accessible:
  ```bash
  psql "postgres://jhernandez:K6m4B129CF9u@REDACTED_HADES_IP:5432/postgres" \
    -c "SELECT table_name FROM information_schema.tables WHERE table_schema = 'yggdrasil';"
  # Must list: engrams, lsh_buckets, indexed_files, code_chunks
  ```
- [ ] **5.4** Hugin Ollama reachable from munin:
  ```bash
  curl -s http://REDACTED_HUGIN_IP:11434/api/tags | python3 -m json.tool
  # Must list qwq:32b and the embedding model.
  ```
- [ ] **5.5** Munin Ollama self-check (from munin):
  ```bash
  curl -s http://REDACTED_MUNIN_IP:11434/api/tags | python3 -m json.tool
  # Must list qwen3-coder (30B-A3B) and the embedding model.
  ```
- [ ] **5.6** Qdrant gRPC port reachable from munin (Rust client uses gRPC):
  ```bash
  nc -zv REDACTED_HADES_IP 6334
  ```

---

## Acceptance Criteria

All of the following must be true before Sprint 001 begins:

| Criterion | Verified By |
|-----------|-------------|
| `SELECT * FROM yggdrasil.engrams LIMIT 1` returns empty set, no error | Task 1.7 |
| `SELECT * FROM yggdrasil.code_chunks LIMIT 1` returns empty set, no error | Task 1.7 |
| `pg_extension` row exists for `vector` | Task 1.8 |
| `GET /collections` on Qdrant returns both `engrams` and `code_chunks` | Task 2.5 |
| Qdrant gRPC port 6334 is open | Task 2.6 |
| `ollama list` on munin shows coding model + embedding model | Task 3.5 |
| Embedding model on munin outputs exactly 1024 dimensions | Task 3.4 |
| `ollama list` on hugin shows `qwq:32b` + embedding model | Task 4.7 |
| Ollama on hugin bound to `0.0.0.0:11434` | Task 4.6 |
| All 5 network checks pass from munin | Task 5 |

---

## Blockers / Assumptions

| Item | Status | Notes |
|------|--------|-------|
| Qdrant container existence on hades | [UNVERIFIED] | NetworkHardware.md states "never used before". Task 2.1 covers the not-found case. |
| Hugin Ollama installation state | [UNVERIFIED] | NetworkHardware.md shows "Experimental AI" with no Ollama listed. Task 4.2 checks before installing. |
| Embedding model output dimension | [MUST VERIFY] | Schema uses `vector(1024)`. If the chosen embedding model does not output 1024-dim vectors, the migration must be updated before Sprint 001. This is a hard blocker. |
| qwen3-coder:30b-a3b tag availability | [UNVERIFIED] | Tag name must be confirmed against `ollama list` / Ollama registry before pulling. |
| Disk space on hades Merlin pool | Available — 444 GiB | Sufficient for Qdrant storage. |
| Disk space on munin for model blobs | [UNVERIFIED] | qwen3-coder 30B-A3B + bge-m3 ≈ 22 GB. Verify free space before pulling. |
| Disk space on hugin for model blobs | [UNVERIFIED] | qwq:32b ≈ 20 GB. Verify free space before pulling. |

---

## Notes for Sprint 001

Once all acceptance criteria are checked:

1. The Rust workspace config (`configs/`) must be updated with the verified embedding model
   name (whichever of `qwen3-embedding` or `bge-m3` was actually used).
2. The Qdrant client in `crates/ygg-store` will connect via gRPC to `REDACTED_HADES_IP:6334`.
3. The Ollama embedding client will target `http://REDACTED_MUNIN_IP:11434` (munin) by default.
4. The reasoning client (`crates/huginn`) will target `http://REDACTED_HUGIN_IP:11434` (hugin).
5. The coding client (`crates/muninn`) will target `http://REDACTED_MUNIN_IP:11434` (munin).
