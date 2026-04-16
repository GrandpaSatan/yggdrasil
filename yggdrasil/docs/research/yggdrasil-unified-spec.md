# Yggdrasil-Unified — A Theoretical Single-Architecture Spec

> This document is a **theoretical specification** for what Yggdrasil could
> be if collapsed from a multi-model MCP swarm into a single, end-to-end
> trained AI architecture. It is deliberately speculative — no sprint plan,
> no timeline, no deliverables. Its purpose is to capture the architectural
> shape so the decision to pursue it (or not) has a concrete referent.
>
> The companion document `shared-memory-fabric.md` describes the bridge
> layer we're building today. This document describes what that bridge
> eventually **collapses into** when the swarm becomes a single mind.

## 1. Premise

The current Yggdrasil is a cooperative swarm:

- **Odin** routes intents to specialist models
- **Mimir** persists episodic engrams
- **Dreamer** consolidates during idle cycles
- **Muninn / Huginn** index and retrieve
- **LFM2, Gemma-4, Nemotron-H, GLM, RWKV** provide specialized generative and classification capacity
- **Fabric (Sprint 069)** stitches cross-model memory together

This is a **brain-like pattern** — specialized cortical regions + hippocampal
consolidation + default mode network + sensory cortex. It exists because
biology cannot retrain neurons at inference time, so specialization is the
cheapest way to add capability.

Yggdrasil does not have that constraint. We can retrain. We can distill.
We can fuse.

**Yggdrasil-Unified** is the architectural endpoint where every piece of
the swarm is internal to a single model: one set of weights, one inference
pass, one artifact to deploy.

## 2. High-level shape

A single ~30-60B parameter model with the following properties:

```
┌──────────────────────────────────────────────────────────────────────────┐
│  MULTIMODAL INPUT LAYER                                                  │
│  text tokens | vision patches | audio frames                             │
│         │             │                │                                 │
│         └─────────────┴────────────────┘                                 │
│                       │                                                  │
│                       ▼                                                  │
│  UNIFIED TOKEN STREAM (≤ 1M tokens via Mamba state carryover)           │
│                       │                                                  │
│                       ▼                                                  │
│  HYBRID BACKBONE — N ≈ 48 layers                                        │
│  ┌────────────────────────────────────────────────────────┐            │
│  │  Layer type distribution (roughly):                     │            │
│  │    80%  Mamba-2 SSM       — linear-time context         │            │
│  │    15%  Grouped-query attention — decision bottlenecks  │            │
│  │     5%  MoE expert routers — specialized sub-capacity   │            │
│  │   Interleaved pattern learned during pretraining.       │            │
│  └────────────────────────────────────────────────────────┘            │
│                       │                                                  │
│                       ▼                                                  │
│  COGNITIVE CONTROL HEADS (multi-task output)                             │
│  ┌─────────────────────────────────────────────────────────┐           │
│  │  • Language head (standard LM)                           │           │
│  │  • Tool-call head (native <tool> tokens)                 │           │
│  │  • Memory-write head (engram write + classification)     │           │
│  │  • Memory-query head (retrieval-key emission)            │           │
│  │  • Flow-control head (route to internal experts)         │           │
│  │  • Self-eval head (confidence, should-dream-on-this?)    │           │
│  └─────────────────────────────────────────────────────────┘           │
│                       │                                                  │
│                       ▼                                                  │
│  PERSISTENT MEMORY FABRIC                                                │
│  HBM ── Hugin-RAM ── USB4 Munin-RAM ── NVMe ── External engram store     │
│  (LMCache-style 4-tier, but integrated as a first-class model component) │
└──────────────────────────────────────────────────────────────────────────┘
```

## 3. Component decomposition

### 3.1 Multimodal input layer

Three encoders feeding a shared token stream:

- **Text tokenizer** — inherit from Liquid's LFM tokenizer family (strong
  performance on code + reasoning).
- **Vision** — ViT-style patch encoder, 256×256 resolution, 16×16 patches
  (~256 visual tokens per frame).
- **Audio** — Conv1D front-end at 24 kHz streaming, emitting audio tokens at
  ~12.5 Hz. Borrowed from Moshi/Gemma-4's audio path.

All three encoders project into the same embedding space (common residual
stream). Modality is signaled by a learned embedding prefix — no separate
pathways beyond the encoders themselves.

### 3.2 Hybrid backbone

Interleaved Mamba-2 / attention / MoE layers. Approximate recipe:

| Slot | Layer type | Purpose |
|------|------------|---------|
| 0-5  | Mamba-2    | Initial context aggregation |
| 6    | GQA Attention | First "decision" bottleneck |
| 7-12 | Mamba-2    | Mid-context evolution |
| 13   | MoE Expert-gated MLP | Specialized subcapacity (coding / reasoning / writing) |
| 14-18 | Mamba-2   | Deeper context |
| 19   | GQA Attention | Cross-reference decision |
| ...  | (repeating pattern) | |

Mamba state carries across tokens with **no context window cap** —
effectively infinite context (~1M tokens practical). Attention layers are
quadratic but cheap because they only exist at ~8 positions in the stack.

**Why this mix?** Nemotron-H's benchmarks (66.5% HumanEval at 4B params)
show that a heavy-Mamba / sparse-attention hybrid dominates pure-attention
at the same parameter budget on code and reasoning. Yggdrasil-Unified
scales that recipe to 30-60B.

### 3.3 MoE experts

Specialist capacity inside the model, not outside it. Each MoE-gated layer
has 8-16 experts; top-2 gating means only ~2/N experts activate per token.
This gives:

- Roughly **8× effective capacity** at inference cost
- Implicit specialization during training (one expert drifts toward code,
  another toward dialogue, another toward reasoning)
- The model learns its OWN specialization map — no manual "coder/reviewer/
  planner" flow configuration needed at runtime

The Yggdrasil swarm's explicit Nemotron/Gemma/GLM/LFM2 roles become
implicit MoE expert specializations. Flow YAML configs become unnecessary.

### 3.4 Cognitive control heads

The model's **output vocabulary** is extended beyond language tokens:

- **`<tool:name>` tokens** — native tool invocations. Pre-trained on
  Yggdrasil's MCP tool schemas. The model emits `<tool:search_code>...<end>`
  exactly like it emits words — no external JSON parsing, no function-call
  adapter layer.
- **`<mem:write>` tokens** — signal to persist the current context as an
  engram. The memory head independently emits a category tag, confidence
  score, and importance signal.
- **`<mem:query>` tokens** — request lookup from the persistent store.
  Inserted at relevant decoding moments by the model itself.
- **`<flow:route>` tokens** — only matters when model is used in a
  multi-turn agentic loop; signals "I need a specialist expert to respond
  to this" (activates a specific MoE expert explicitly).
- **`<eval>` self-scoring** — the model assesses its own output quality
  inline, drives dream-cycle prioritization.

These tokens are pre-trained, not bolted on. Their meaning is baked into
the weights from the start — unlike current function-calling models that
fine-tune a base model to emit JSON.

### 3.5 Persistent memory fabric (internalized)

The LMCache 4-tier + Mimir engram store become a **native architectural
layer**, not an external service:

- **Tier 0 — working memory (HBM)**: current sequence's Mamba state + recent
  attention KV.
- **Tier 1 — CPU RAM**: overflow Mamba state, 40 GiB on Hugin-class host.
- **Tier 2 — USB4 remote RAM**: another host's RAM via fast interconnect,
  100 µs RTT. (Today's Munin warm-remote tier.)
- **Tier 3 — NVMe**: persistent KV store for cross-session continuity.
- **Tier 4 — engram vector DB**: semantic long-term memory, shared across
  sessions and users.

The model attends into these tiers via a **learned memory-attention
mechanism** — similar to Memorizing Transformers or RETRO. On each attention
layer, queries can attend over live tokens (Tier 0-1), recently evicted
state (Tier 2-3), or retrieved engrams (Tier 4). A learned routing gate
picks which tier based on query content — frequent queries stay hot, rare
semantic recall drops to the vector store.

**Write path**: at training time, the model learns when to emit `<mem:write>`
tokens based on self-supervised importance signals. The memory head
produces a dense embedding for indexing. At inference, writes land in the
appropriate tier automatically.

### 3.6 Dream consolidation (continuous learning)

Background self-play with weight updates:

1. During idle periods, the model replays recent contexts through itself.
2. A **self-distillation loop** compares current responses to "reviewer"
   responses (generated by the same model with increased test-time compute,
   more MoE experts active).
3. Discrepancies become **DPO preference pairs** — reviewer response
   preferred.
4. Low-rank LoRA-style updates trained from these pairs, merged into the
   model weights periodically.

This is **continuous learning without catastrophic forgetting** because:
- LoRA merges are small and localized (~0.1% of weights touched per dream)
- The replay corpus is curated by the `<eval>` head's confidence scores
  (only uncertain-but-later-resolved contexts become training data)
- The dream cycle runs offline; at inference, weights are stable

Over time, the model's weights slowly evolve to match the distribution of
queries it sees. Specialization emerges naturally. This is what the current
Dreamer crate does via engram summarization — unified Yggdrasil does it at
the weight level.

## 4. How it maps to today's swarm

| Swarm component (today) | Unified equivalent | Translation mechanism |
|---|---|---|
| Odin router | MoE gating network | Routing logic moves from YAML configs to learned weights |
| Nemotron-3-Nano (coder) | MoE expert(s) specialized on code tokens | Distilled from Nemotron via teacher-student on flow traces |
| Gemma-4-E4B (reviewer) | MoE expert(s) specialized on evaluation + multimodal | Same distillation pattern, on reviewer-flow traces |
| GLM-4.7-Flash (planner) | MoE expert(s) specialized on reasoning | Same |
| LFM2.5-1.2B-Instruct (router) | Flow-control head | Distilled into a lightweight output head |
| RWKV-7-G1e (memory classifier) | Memory-write head | Distilled into built-in memory signals |
| Saga-350m / Review-1.2b | Fine-tuned LoRAs over the base | Lightweight additions for sprint-specific specialization |
| Mimir engrams | Tier 4 memory, accessed via cross-attention | Identical storage, integrated attention path |
| Muninn / Huginn (search) | Memory-query head emits retrieval keys | Search API becomes learned lookup behavior |
| Fabric L1/L2 (Sprint 069) | **Internal residual stream** — no projection needed | Collapsed: single model has single hidden space |
| Fabric L3 (semantic memory) | Memory-attention over Tier 4 engrams | Direct attention mechanism replaces HTTP API |
| MCP tool calling | Native `<tool>` tokens | Pre-trained, not RLHF-added |

**The fabric IS the teacher.** The cross-model projections we're training
in Sprint 069 are exactly the kind of data you need to teach one model to
do what all of them do together — just with explicit translation between
their hidden spaces. In a unified model, that translation is **zero-cost**
because everything lives in the same residual stream.

## 5. Emergent and novel properties

### 5.1 Self-summarizing inference

Because the memory head is native, the model can emit **compressed
representations of its own context** mid-sequence. Long conversations
auto-summarize into engram writes, freeing working memory. No separate
summarizer needed.

### 5.2 Attention-guided retrieval

The memory-attention mechanism means retrieval happens **per-layer**, not
as a preprocessing step. The model can decide, at any depth, that it needs
to recall something — and the recall happens inline within that layer's
attention computation.

### 5.3 Self-scored exploration

The `<eval>` head gives the model a **continuous signal of its own
uncertainty**. Low confidence → allocate more MoE experts next turn, spawn
a parallel reasoning branch, or emit a `<mem:query>` for retrieval. The
model becomes metacognitive.

### 5.4 Tool-use as first-class generation

Because `<tool>` tokens are pretrained, the model's planning becomes
continuous with its output. There's no "decide if a tool call is needed"
phase — tool calls happen in the same probability distribution as word
choices. This eliminates the latency of function-call parsing + tool
routing layers.

### 5.5 Session continuity

Tier 2-3 persistent memory means **sessions carry state without re-injecting
context**. A new conversation with the same user loads relevant engrams into
Tier 1 automatically via a learned session-context retrieval at turn 0.

## 6. Inference profile

A rough sketch of per-token cost:

| Layer operation | Cost (approx) |
|---|---|
| Mamba-2 block | O(d) per token, constant in sequence length |
| GQA attention block | O(d · context_window), context bounded |
| MoE expert gating | O(d · 2) — top-2 experts active |
| Memory-attention (Tier 4) | O(d · log(store_size)) via ANN index |
| Cognitive head projection | O(d · vocab_size) once at final layer |

For a 60B sparse (active ~5B), context 128K tokens, on a single 80 GB GPU:
~20-50 tokens/sec. On Hugin (9060 XT + 890M iGPU tensor-split): ~5-10
tokens/sec at fp16, more with FP8.

Memory footprint per sequence is roughly **Mamba state size (fixed) + 8K
attention window tokens** — not 128K × full-attention, which would blow
VRAM. This is the key win over transformers-only.

## 7. Training recipe sketch (non-operational)

A plausible path, described for completeness. **Not a plan.**

1. **Pretraining base**: either take an open hybrid base (Nemotron-H-9B,
   LFM2-2.6B-Exp, Jamba) and scale up, or continue-pretrain from them with
   architectural modifications (MoE layers injected).
2. **Multimodal adaptation**: add vision + audio encoders via lightweight
   adapter layers, pretrained on aligned image-text + audio-text corpora.
3. **Cognitive head training**: fine-tune the output layer to emit
   `<tool>`, `<mem:*>`, `<flow:*>`, `<eval>` tokens. Training data comes
   from Yggdrasil's own flow trace logs — every real MCP call Yggdrasil
   has ever made becomes a `<tool>` training example. Every engram write
   becomes a `<mem:write>` example.
4. **Swarm distillation**: for each specialist (Nemotron, Gemma, GLM,
   LFM2), generate teacher responses on Yggdrasil's flow corpus. Train MoE
   experts to reproduce those responses. The teacher-student bridge is
   exactly what Sprint 069's cross-model projections demonstrate — map
   teacher's hidden state to student's, minimize reproduction error.
5. **Memory-attention training**: synthesize a corpus where answers require
   retrieval from past turns. Train memory-attention to emit correct
   retrieval keys and integrate retrieved content. RETRO-style training is
   the reference.
6. **Dream consolidation**: online after deployment. Continuous low-rate
   LoRA updates from self-distilled DPO pairs.

Step 4 is where the Sprint 069 fabric produces its most valuable artifact:
a corpus of paired `(source_hidden_state, target_K, target_V)` tuples that
prove the information IS transferable across architectures. That same
corpus, reinterpreted as `(teacher_expert_hidden, student_expert_K, ...)`,
becomes training data for the unified model's MoE experts.

## 8. Risks and open research questions

### 8.1 Does the mix work?

No one has wired together (heavy-Mamba backbone) × (MoE gating) × (native
tool tokens) × (attention-based memory retrieval) × (multimodal encoders)
× (continuous dream learning) in a single model. Each piece works
independently. The interaction effects are unknown. Training stability at
60B with so many loss heads firing simultaneously is a research question.

### 8.2 Specialization lock-in

If MoE experts over-specialize early, the model loses flexibility. Mixtral
handled this with auxiliary load-balancing losses. Yggdrasil-Unified would
need similar regularization.

### 8.3 Memory-attention scaling

Cross-attention over Tier 4 (potentially millions of engrams) is expensive.
Approximate-nearest-neighbor indexes (HNSW, Qdrant) solve retrieval but the
attention **integration** cost is O(top_k × d) per query. Need to cap
top_k aggressively (e.g., k=16).

### 8.4 Continuous learning drift

Dream-cycle LoRA merges can accumulate. Over months the model's weights
drift. Benchmarks may quietly regress on old tasks while improving on new.
Needs a validation corpus that gets replayed periodically to detect drift.

### 8.5 Tool-call safety

Native `<tool>` tokens mean the model can invoke any MCP tool it was
trained on — without a policy layer to gate. The flow-control head becomes
the only arbiter of "should I actually call this." For production this
likely needs a separate safety classifier (ironically: a small model
outside the big one).

## 9. Why this matters

The current Yggdrasil is a **system of systems**. Every architectural
boundary is a latency tax (HTTP/RPC between Odin and Mimir and vLLM),
a coordination problem (flow engine state machines), a deployment
complication (four services to keep in sync), and a research ceiling
(you cannot end-to-end train a swarm; you can only fine-tune pieces).

Yggdrasil-Unified is the same cognitive architecture, but with **every
boundary internalized**. Latency drops. Failure modes simplify. Training
becomes end-to-end — meaning the model LEARNS to be a cooperative swarm
from examples of what the current swarm does, instead of having that
cooperation hand-coded in Odin and flow YAML.

It is also the version of Yggdrasil that **scales to a single deployable
artifact**. No fleet of services. One binary. One checkpoint. One artifact
to ship.

## 10. Relationship to current work

- Sprint 069's **fabric** is the teacher. Every projection MLP trained
  generates a `(source_state, target_KV)` tuple that would become a
  distillation training example.
- Sprint 069's **design document** (shared-memory-fabric.md) is the
  architectural precursor — L1/L2/L3 tiers become Tier 0/1, Tier 1/2, Tier
  4 respectively inside a unified model.
- Phase I's **LMCache 4-tier** is the memory fabric, externalized. Internal
  version uses the same storage, accessed via attention instead of HTTP.
- Phase K's **agentic flow streaming** is what the cognitive control heads
  do natively — stream tool calls, intermediate reasoning, self-eval.
- Phase M's **integration tests** eventually become evaluation benchmarks
  for Yggdrasil-Unified.

The swarm is the apprenticeship. The unified model is the practitioner.

## 11. What this document is NOT

- Not a sprint plan. No phases, no acceptance criteria, no deliverables.
- Not a commitment. This is a possible future, not a chosen one.
- Not a critique of the swarm. The swarm is the right architecture for
  today's constraints (pluggable vendor models, no training budget for a
  60B run, shipping features incrementally). This document only answers
  "what if those constraints went away."
- Not original research. Every component cited exists in the literature.
  The novelty would be in the synthesis, not any single piece.

## 12. Closing note

Every intelligent system that exists today is either a swarm (brain,
society, ant colony, today's Yggdrasil) or a single integrated mind (a
modern LLM, a formally specified algorithm). Swarms are easier to grow,
minds are easier to optimize. The transition from one to the other —
"distilling the swarm's behavior into a single coherent mind" — is an
old theme in cognitive science. LLM research is currently rediscovering
it by training MoE models, memory-augmented models, and dream-replaying
models separately.

Yggdrasil-Unified would be interesting because it proposes combining all
three at a scale and integration level no one has tried yet, using the
current Yggdrasil swarm's flow traces as the training signal. The swarm
records what cooperation looks like; the unified model learns to be that
cooperation internally.

Whether to build it is a separate question. This document exists to make
sure the question is well-defined when it comes up.
