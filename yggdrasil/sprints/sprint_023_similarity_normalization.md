# Sprint 023: Mimir Similarity Normalization + Legacy Query Fix

**Date:** 2026-03-10
**Status:** In Progress

## Problem

1. **Qdrant System 2 scores are raw dot-product values (0–128 for ~50% density SDRs)**
   while System 1 (in-memory Hamming) returns properly normalized [0.0, 1.0] similarity.
   The merge step in `recall_engrams` takes `max(sys1, sys2)` per UUID — raw Qdrant
   scores always win, producing similarity values that break the memory_router thresholds
   (0.6 noise floor, 0.85 pattern override).

2. **Legacy `/api/v1/query` endpoint is broken** — calls `query_by_similarity()` which
   references the `cause_embedding` column dropped in Sprint 015. The MCP
   `query_memory_tool` returns HTTP 500.

3. **Dead code in `ygg-store/src/postgres/engrams.rs`** — `insert_engram`,
   `insert_archival_engram`, `query_by_similarity`, and `format_embedding` all reference
   the dropped `cause_embedding` column and are never called.

## Root Cause Analysis

### Qdrant normalization

The `engrams_sdr` collection uses `Distance::Dot` on 256-dim binary {0,1} vectors.
Dot product of two binary vectors = count of positions where both are 1.
For sign-thresholded SDRs with ~50% density (popcount ≈ 128):

- Identical SDRs: dot = 128, hamming_sim = 1.0
- Random SDRs: dot ≈ 64, hamming_sim ≈ 0.5
- Opposite SDRs: dot = 0, hamming_sim = 0.0

The relationship: `hamming_similarity ≈ dot / query_popcount` when densities are similar.
Normalizing Qdrant scores by `query_popcount` produces values comparable to System 1.

### Legacy query endpoint

Sprint 015 dropped `cause_embedding` and replaced it with `sdr_bits BYTEA` + SDR-based
recall. The `/api/v1/query` handler was never updated to the new path.

## Changes

### 1. Normalize Qdrant scores in recall handler
**File:** `crates/mimir/src/handlers.rs`

Compute `query_popcount = sdr::popcount(&query_sdr)` and normalize each System 2 score:
```rust
let normalized = (score as f64 / query_pop).clamp(0.0, 1.0);
```

### 2. Rewrite `/api/v1/query` to use SDR path
**File:** `crates/mimir/src/handlers.rs`

Replace the broken `query_by_similarity` call with:
1. ONNX embed → binarize → SDR
2. System 1 + System 2 merge (same as recall)
3. Fetch full Engram objects by ID from PostgreSQL
4. Return `Vec<Engram>` (backward-compatible response)

### 3. Add `fetch_engrams_by_ids` to store layer
**File:** `crates/ygg-store/src/postgres/engrams.rs`

New function that fetches full `Engram` structs by a list of UUIDs. Used by the
rewritten `/api/v1/query` handler.

### 4. Remove dead code (Trace & Destroy)
**File:** `crates/ygg-store/src/postgres/engrams.rs`

Delete:
- `insert_engram` (replaced by `insert_engram_sdr`)
- `insert_archival_engram` (summarization uses `insert_engram_sdr`)
- `query_by_similarity` (replaced by SDR recall)
- `format_embedding` (only used by deleted functions)

## Files Modified

| File | Changes |
|------|---------|
| `crates/mimir/src/handlers.rs` | Normalize Qdrant scores, rewrite query handler |
| `crates/ygg-store/src/postgres/engrams.rs` | Add `fetch_engrams_by_ids`, delete dead code |

## Verification

1. `cargo build --release --bin mimir` — clean compile
2. Deploy to Munin, restart yggdrasil-mimir
3. `curl -X POST localhost:9090/api/v1/recall -H 'Content-Type: application/json' -d '{"text":"test","limit":3}'` — similarity values in [0.0, 1.0]
4. `curl -X POST localhost:9090/api/v1/query -H 'Content-Type: application/json' -d '{"text":"test","limit":3}'` — returns Engram objects (no 500 error)
5. MCP `query_memory_tool` — works without `cause_embedding` error
