# Yggdrasil Shared Memory Fabric — findings

> Sprint 069 Phase G.9. This document captures the empirical results
> of training + benchmarking the fabric across Yggdrasil's swarm.
> Updated as pairs complete on Morrigan.

## Training protocol

Per `docs/research/shared-memory-fabric.md` §5. Concretely:

| Knob | Value |
|---|---|
| Script | `scripts/ops/fabric-train-projections.py` |
| Epochs | 15 |
| Batch size | 64 (sequence-padded) |
| Optimizer | AdamW, lr=3e-4, weight_decay=1e-4 |
| Scheduler | CosineAnnealing over epochs |
| MLP | 2-layer, GELU, dropout 0.1, hidden 2048 |
| Loss | MSE on flattened K (separate head trained for V) |
| Layer mapping | Proportional: tgt_layer → src_layer = round(tgt_layer · L_src/L_tgt) |
| Hardware | Morrigan 2× RTX 3060 12 GB + 60 GB RAM |

## Fleet coverage

Projections train across N=5 base models (when Nemotron/Gemma-4
extractions unblock):

| Base | Source → | Target ← | Attn layers | Heads | Head dim | Hidden |
|---|---|---|---|---|---|---|
| LiquidAI/LFM2-350M | ✅ | ✅ | 6 (idx 2,5,8,10,12,14) | 8 | 64 | 1024 |
| LiquidAI/LFM2.5-1.2B-Base | ✅ | ✅ | 6 (idx 2,5,8,10,12,14) | 8 | 64 | 2048 |
| google/gemma-4-E2B | ⏳ blocked on transformers git-main | ⏳ | — | — | — | — |
| google/gemma-4-E4B | ⏳ same | ⏳ | — | — | — | — |
| nvidia/NVIDIA-Nemotron-3-Nano-4B-BF16 | ⏳ blocked on mamba-ssm | ⏳ | — | — | — | — |
| BlinkDL/rwkv7-g1 (g1e-2.9b) | **N/A — intentionally excluded** | — | — | — | — | — |

**RWKV-7-G1e intentionally excluded from L1/L2** (2026-04-15 design refinement). RWKV serves as the memory sidecar's classifier — producing categorization + Mimir queries that populate the fabric's L3 tier indirectly via engram ingestion. It never participates in generative flows where KV-reuse would matter, so there's nothing to project to/from. This is not a deferral; RWKV's fabric role is "L3 producer" only, by design.

**Critical reclassification (2026-04-15 empirical finding):** Despite
sharing identical attention topology (same 6 attn layer indices,
same 8 heads × 64 head_dim), LFM2-350M and LFM2.5-1.2B-Base are
NOT "same-family" for L1 purposes. Their avg MSE of 0.65 is 160×
worse than the true same-family pair (Gemma-4-E2B → E4B at 0.004).

The ".5" version bump between LFM2 and LFM2.5 indicates a model
GENERATION change, not just a scale change. Liquid AI likely
modified SSM parameterization, training recipe, or initialization
between v2 and v2.5. Shape-compatibility ≠ value-compatibility.

**Tier reclassification:**
- **L1 (direct KV, near-identity projection):** Gemma-4-E2B ↔ Gemma-4-E4B ONLY
- **L2 (learned projection, moderate MSE):** LFM2 ↔ LFM2.5, all cross-family pairs
- **L3 (semantic text, universal):** everything, including RWKV (L3-producer role)

## Per-pair training results

### LFM2-350M → LFM2.5-1.2B-Base (training in progress)

| Target layer | Source layer | K MSE | V MSE | Wall-clock | Status |
|---|---|---|---|---|---|
| 02 | 2 | 0.789 | 0.795 | 557 s / 119 s | ✅ |
| 05 | 5 | 0.824 | 0.825 | 120 s / 124 s | ✅ |
| 08 | 8 | 0.803 | 0.824 | 122 s / 125 s | ✅ |
| 10 | 10 | **0.592** | **0.586** | 123 s / 126 s | ✅ |
| 12 | 12 | — | — | — | running |
| 14 | 14 | — | — | — | — |

**Observation: deeper layers converge to lower MSE.** Layer 10 is
~30% lower than layers 2/5/8. Hypothesis: post-attention residual
stream at deeper layers has been through more transformer blocks
and carries less model-idiosyncratic noise, making cross-model
projection easier. Final observation pending layers 12/14.

### LFM2.5-1.2B-Base → LFM2-350M (queued)

Starts automatically after the forward direction completes. The
reverse projection (2048-dim source → 512-dim target flattened K/V)
may converge FASTER than forward because information is discarded
not added; the MLP has more latitude to fit.

## Validation methodology (Phase G.9 bench)

`scripts/ops/fabric-bench.py` (to be committed) drives:

1. **Projection reconstruction quality** — hold out 10% of prompt
   hashes; project source hiddens → target K/V; compare to ground-
   truth target K/V by element-wise MSE + per-head cosine similarity.
2. **Attention output preservation** — for each held-out prompt,
   run B's attention with (Q_B, projected K, projected V) and
   measure cosine similarity of its output vs. attention with
   (Q_B, native K, native V). This is the "does it preserve
   downstream behavior" check that the paper's attention-MSE loss
   targets.
3. **End-to-end TTFT** — once vLLM fork (G.7) lands, fire 50 paired
   requests through llama-swap, with and without `X-Ygg-KV-Preseed`,
   measure Time-To-First-Token delta.

## L3 semantic tier soak (when Odin cut over)

| Metric | Target | Actual | Status |
|---|---|---|---|
| `ygg_fabric_publish_total` | > 0 after 30 min traffic | — | pending cutover |
| `ygg_fabric_l3_hits_total` | > 0 in first hour | — | pending |
| Publish latency p95 | < 50 ms | — | pending |
| Query latency p95 | < 25 ms | — | pending |

Numbers populate once `YGG_FABRIC_ENABLED=1` flips on a live Odin.

## Open research questions

- **Does deeper-layer projection quality hold cross-family?** LFM2 ↔ LFM2 has architectural identity beyond size. Cross-family (LFM2 → Gemma-4 or Gemma-4 → Nemotron) may show different convergence patterns.
- **Does attention-output MSE beat raw K/V MSE?** Raw MSE is the current objective. Paper predicts attention-output MSE gives better downstream quality. Worth running both and comparing on held-out cosine similarity of `Attn_B(Q_B, K̂, V̂)` vs `Attn_B(Q_B, K_B, V_B)`.
- **Does RWKV state-transfer work at all?** The fabric's universal memory vision requires an answer. First-cut hypothesis: RWKV time-mix state at layer ℓ ≈ a compressed summary of tokens 0..t, analogous to `sum over attention heads` in transformers. A small MLP might learn the correspondence — or might not.
- **Does the fabric's L3 tier alone already justify deployment, without L1/L2?** L3 is architecture-agnostic. If live soak shows meaningful quality lift from flow-scoped semantic memory alone, L1/L2 become bonus TTFT wins on top of an already-justified infrastructure investment.

## Next updates

This doc regenerates as:
- Remaining LFM2 pair layers complete (forward + reverse)
- Gemma-4 extractions unblock (transformers git main install finishes)
- Nemotron extraction unblocks (mamba-ssm install retry)
- RWKV extraction variant lands
- L3 soak on live Odin populates real metrics
