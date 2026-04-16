# Yggdrasil Shared Memory Fabric

> Sprint 069 Phase G design. This document locks the architecture, math, and
> training protocol BEFORE any Rust or Python lands, so that the downstream
> steps (crate scaffolding, hidden-state extraction, projection training,
> vLLM patch) are executing against a single source of truth.

## 1. Problem

The Yggdrasil swarm runs 4+ models with different architectures, hidden
dimensions, tokenizers, and attention shapes. Today each model's working
memory (KV cache + output text) is private to its own vLLM process. A
multi-model flow (e.g. `saga → review → lfm25-tools`) re-prefills the same
content at every step, burning GPU time and paying full TTFT on every
handoff.

The goal: **every model in the swarm has access to every other model's
relevant working memory, at the highest fidelity each architecture pair
supports.** Not "one model at a time." Not "same-family only." The
entire fleet, in a mesh.

## 2. Tiered architecture

Three tiers, from most-universal to highest-fidelity. The fabric tries
them in order; the first that hits wins.

```
 ┌─────────────────────── L3 Semantic (universal) ───────────────────────┐
 │  Embedding-indexed text memory. Any model → any model. Always available.│
 │  Backing: Valkey on Munin :6479, flow_id-scoped.                        │
 └────────────────────────────────────────────────────────────────────────┘
 ┌─────────── L2 Projected Activation (cross-architecture) ───────────────┐
 │  Per-layer K & V projection MLPs trained on paired hidden states.       │
 │  Any pair with a trained projection → KV-level reuse across archs.      │
 │  Deployed via vLLM fork (G.7) OR text-injection fallback (G.8).         │
 └────────────────────────────────────────────────────────────────────────┘
 ┌─────────────────────── L1 Direct KV (same-family) ─────────────────────┐
 │  Raw K/V tensors with trivial linear projection for size differences.   │
 │  Same-family pairs only: LFM2 internal, Qwen internal.                  │
 │  5–8× TTFT on hit. Zero training required.                              │
 └────────────────────────────────────────────────────────────────────────┘
```

## 3. L3 — Semantic tier

### Schema

Single Valkey instance on Munin (`10.0.65.8:6479`). Key namespacing:

```
fabric:flow:<flow_id>:step:<n>           HASH   model, text, embedding_bytes, ts
fabric:flow:<flow_id>:index              SORTED SET ordered by step
fabric:emb:<flow_id>                     VECTOR  (RediSearch; fall back to in-memory if absent)
```

Records TTL at 24 hours by default. Flows that conclude earlier emit a
`fabric.flow.done` event that triggers eviction.

### Endpoints (ygg-memory-fabric, :11450)

```
POST /fabric/publish
  { flow_id, step_n, model, text, embedding? }
  → fabric publishes; auto-computes embedding via TEI :11438 if absent

POST /fabric/query
  { flow_id, query_text|embedding, top_k }
  → [ { step_n, model, text, similarity } ]

POST /fabric/done
  { flow_id }
  → evicts all keys for flow_id

GET /fabric/flow/<flow_id>/history
  → full step sequence, for debugging
```

### Odin integration

`run_step_streaming` gains two hooks:

1. **Pre-step (enrich):** before constructing the step's prompt, POST
   `/fabric/query {flow_id, query_text: current_step.system_prompt, top_k: 3}`.
   If hits returned, prepend as:
   `<working_memory>\n[step_3 saga-350m]: ...\n[step_2 review-1.2b]: ...\n</working_memory>\n`
2. **Post-step (publish):** after the step streams to completion, POST
   `/fabric/publish {flow_id, step_n, model, text: full_output}`.

Zero cost to architectures — works for every model in the fleet,
including vendors that will never have projection MLPs (hypothetical
GPT-OSS endpoints, Anthropic, etc).

## 4. L1 — Direct KV (true same-architecture pairs only)

> **Reclassification (2026-04-15):** L1 requires models that are true
> scale variants of ONE architecture release — identical macro AND micro
> structure, differing only in width/depth. LFM2 ↔ LFM2.5 was
> originally placed here but empirical results proved it belongs in L2:
> their 0.65 avg MSE is 160× worse than the only true L1 pair
> (Gemma-4-E2B ↔ E4B at 0.004 MSE). The ".5" version bump indicates
> architectural changes, not just scale.

**Current L1 pair (the ONLY one in the fleet):**

Gemma-4-E2B ↔ Gemma-4-E4B — both released 2026-04-02 by Google as
simultaneous scale variants of Gemma 4. Same tokenizer, same
sliding/full attention pattern, same head_dim (256), same training.

| Model | hidden_dim | kv_heads | head_dim | cache layers |
|---|---|---|---|---|
| Gemma-4-E2B | 1536 | 1 (MQA) | 256 | 15 |
| Gemma-4-E4B | 2560 | 2 (GQA) | 256 | 24 |

Projection MSE: **0.004 avg** — near-identity. Direct KV injection
will preserve >99% of attention behavior. The only projection needed
is a learned width adaptation (1536→2560 hidden space); the K/V
geometry is already aligned.

**Why LFM2 ↔ LFM2.5 is NOT L1:**

Despite sharing the same macro layout (16 layers, 6 attention at
identical indices, 8 heads × 64 head_dim), LFM2 (v2, ~2025) and
LFM2.5 (v2.5, ~late 2025) are different MODEL GENERATIONS from
Liquid AI. The ".5" version bump signals:
- Different training data/recipe
- Potential SSM parameterization changes in the conv blocks
- Different weight initialization

Empirical evidence: LFM2-350M → LFM2.5-1.2B-Base averages **0.65 MSE**
(20% variance explained at early layers, 68% at deep layers). For
comparison, Gemma-4 same-family averages 0.004 (99.6%). The 160×
gap proves these are cross-architecture, not same-architecture.

LFM2 ↔ LFM2.5 pairs are served by **L2 (projected activation).**

**Implementation:** Python sidecar `ygg-kv-sidecar.py` on Hugin :11451.
Loads trained projection .pt files, exposes `POST /project` for KV
shape translation. For L1 pairs (Gemma only), the projection is
near-identity; sidecar overhead is negligible.

## 5. L2 — Projected Activation

### Math

Given two models A (source) and B (target) with hidden-state sequences
`h_A ∈ ℝ^{T × d_A}` and `h_B ∈ ℝ^{T × d_B}` when both process the same
text, we train **per-layer K and V projections:**

```
For each layer ℓ ∈ {0, …, L_B − 1}:
  P^K_ℓ,A→B : ℝ^{d_A,ℓ} → ℝ^{d_B,ℓ}     (a 2-layer MLP, width 2048)
  P^V_ℓ,A→B : ℝ^{d_A,ℓ} → ℝ^{d_B,ℓ}
```

Source-to-target layer mapping: proportional stretch when `L_A ≠ L_B`.
For saga (16 layers) → review (24 layers), source layer `j` maps to
target layer `round(j * 24/16)`.

Loss: **MSE on B's attention output**, not raw MSE on hidden states.
That is, given projected `K̂, V̂` at layer ℓ, we compute B's attention
as it would with its own K, V, and minimize:

```
L_KV = || Attn_B(Q_B, K̂, V̂) − Attn_B(Q_B, K_B, V_B) ||²
```

This is stronger than direct MSE because it optimizes for preserving
downstream behavior, not superficial representational similarity.

### Training data

Generated on Morrigan with `scripts/ops/fabric-extract-hiddens.py`:

- Corpus: **3,000 prompts** drawn from:
  - 1,000 code-swarm prompts from `tests-e2e/fixtures/flows/` and Huginn's indexed codebase
  - 1,000 memory-consolidation summaries synthesized from past Mimir engrams
  - 1,000 from a general corpus (HotpotQA + TriviaQA random subsample)
- Length: 256–1024 tokens each. Multi-turn where realistic.
- For each prompt × each of 4 models, capture:
  - Final output text
  - Per-layer K tensor (post-RoPE), shape `(T, n_heads, head_dim)`
  - Per-layer V tensor, same shape
  - Per-token final hidden state, shape `(T, d)`
- Storage: `/mnt/morrigan-data/fabric-hiddens/<model>/<prompt_hash>.pt`

Expected size: 3k prompts × 4 models × ~20 layers × ~(512 × 16 × 64 float16)
≈ **~30 GB total.** Manageable on Morrigan's 691 GB free disk.

### Training

`scripts/ops/fabric-train-projections.py` — launches ~1,152 training
jobs (12 directional pairs × ~24 layers × 2 for K+V, minus un-needed
same-family shortcuts). Each job:

- Model: 2-layer MLP, width 2048, GELU, dropout 0.1
- Optimizer: AdamW, lr=3e-4, warmup=100 steps
- Epochs: 20 (early-stop on validation loss plateau)
- Batch: 128 per GPU
- Validation: 10% held-out prompts, report KV-reconstruction loss +
  cosine-similarity of B's next-token distribution using projected vs.
  native K/V
- Export: ONNX via `torch.onnx.export`, then loaded by ONNX Runtime
  ROCm EP at serve time (Munin 780M can run small MLPs at line rate)

Each job fits in ~500 MB VRAM, so we parallelize 3-wide on Morrigan's
3 GPUs → **~4–5 hours wall-clock for the full bank.**

### Deployment path A — vLLM fork with `--enable-external-kv`

Patch to vLLM 0.16.1 (~150 LOC):

1. New flag `--enable-external-kv` on `vllm serve`
2. New request field `preseeded_kv: List[LayerBlob]` in OpenAI API
3. `Scheduler.add_request` path: if `preseeded_kv` present, populate
   PagedAttention blocks with the blob instead of running prefill
4. Header bounce: `X-Ygg-KV-Preseed: <blob_id>` routes through
   llama-swap → fabric coordinator fetches + attaches blob

Build: fork `vllm-project/vllm` at the commit matching our ROCm image,
apply patch, rebuild docker image as
`rocm/vllm-dev:rocm7.2.1_navi_ubuntu24.04_py3.12_pytorch_2.9_vllm_0.16.0-ygg`
with the patch applied. Swap into llama-swap configs.

### Deployment path B — text-injection fallback (G.8)

If G.7 bleeds, we still use the trained projections — but we train an
additional tiny **decoder head** that maps projected hidden states back
to text tokens. The fabric then:

1. Projects A's hiddens via learned `P_{A→B}`
2. Decodes with the decoder head into a short summary text
3. Injects the summary as a system-prompt prefix to B's vLLM call

This is slower than true KV injection (B still prefills) but preserves
the L2 quality signal: the injection is informed by A's activation
geometry, not just A's output text.

## 6. Metrics

Exported by `ygg-memory-fabric`:

```
ygg_fabric_l1_hits_total{pair}                 counter  direct KV reuse hits
ygg_fabric_l2_hits_total{pair}                 counter  projected reuse hits
ygg_fabric_l3_hits_total{pair}                 counter  semantic text reuse hits
ygg_fabric_l1_ttft_seconds{pair,quantile}      hist     TTFT with L1 reuse
ygg_fabric_l2_ttft_seconds{pair,quantile}      hist     TTFT with L2 reuse
ygg_fabric_baseline_ttft_seconds{pair,quantile} hist    TTFT without any fabric
ygg_fabric_reuse_ttft_savings_seconds{pair,tier} gauge  (baseline − tier) / baseline
ygg_fabric_l2_quality_cosine{pair,quantile}    hist     B's output cosine, projected vs native
ygg_fabric_bytes_stored{tier}                  gauge    storage pressure per tier
```

## 7. Acceptance (Sprint 069 Phase G, hard-locked)

1. `ygg-memory-fabric` service is `active` on Hugin :11450
2. Valkey backend is populated, visible via `redis-cli -h 10.0.65.8 -p 6479 KEYS 'fabric:*' | wc -l` > 0 within 30 minutes of production traffic
3. Every `run_step_streaming` call auto-publishes AND is enriched with relevant prior-step memory (verified by log probe + `ygg_fabric_l3_hits_total` counter rising)
4. L1 direct KV reuse demonstrated on saga→review pair with ≥ 3× TTFT improvement over baseline on `fabric-bench.py --pair saga_review`
5. All 1,152 L2 projections trained, ONNX-exported, and deployed to Munin 780M
6. L2 path reached production via G.7 (vLLM fork) OR G.8 (text-injection fallback) — exactly one lands, neither deferred
7. `ygg_fabric_l2_hits_total{pair}` non-zero for at least 4 of the 6 primary flow pairs after 1 hr of soak
8. Per-pair findings documented in `docs/research/fabric-findings.md`

## 8. Why this is the right design for Yggdrasil's swarm

- **Fleet-wide, not pairwise.** Adding a new model means training N projections to existing models (a few hours on Morrigan), not re-architecting anything.
- **Graceful degradation.** L1 fails → L2. L2 fails → L3. L3 fails → model works as it does today. Never a correctness regression.
- **Morrigan-local training.** The projection MLPs are the output of our own swarm activity — trained on the exact flows they serve. They improve as the swarm accumulates logs.
- **Phase I compatible.** LMCache's 4-tier (HBM → Hugin-RAM → Munin-RAM over USB4 → Hugin-NVMe) becomes the physical backing of the fabric's storage — no architectural conflict, strict composition.
- **Phase H compatible.** TurboQuant compresses KV blobs; the fabric transports them. Serial composition: fabric retrieves → TurboQuant decompresses → attention consumes.

## 9. Out of scope (for Phase G specifically)

- Cross-organizational KV sharing (e.g. Anthropic's KV in our fabric). Physically impossible; their KV is remote. Our tier L3 handles that case via text.
- Federated projection learning across multiple Yggdrasil instances. Future work; each instance trains its own projections today.
- Live projection retraining. Sprint 069 ships a static trained bank; online adaptation is a Sprint 070+ topic.
