# Sprint: 009 - Hardware Optimization
## Status: PLANNING

## Objective

Maximize inference throughput and embedding latency on the two production nodes (Munin and Hugin) by unlocking hardware capabilities that are currently unused or misconfigured. This sprint covers four workstreams: (1) Intel ARC iGPU activation on Munin via DVMT Pre-Allocated memory BIOS unlock and oneAPI/SYCL backend for Ollama, (2) AMD Zen 5 CPU optimization on Hugin via AVX-512 verification, thread pinning, and NUMA-aware Ollama configuration, (3) evaluation of the Exo framework for distributed inference of 70B+ models across both nodes, and (4) an optional `candle`-based embedding path in `ygg-embed` to replace the Ollama HTTP API for embedding with in-process inference targeting sub-5ms latency. This is an **Optimization Track** sprint -- it is primarily research, configuration, benchmarking, and one optional code change (candle embedder). No new service features are added.

## Scope

### In Scope
- **Munin iGPU (Intel ARC):**
  - Research DVMT Pre-Allocated memory BIOS setting on the Core Ultra 185H platform
  - Document BIOS unlock procedure (if available) or alternative firmware approaches
  - Install Intel oneAPI Base Toolkit 2025.2 on Munin
  - Verify `sycl-ls` detects the ARC iGPU
  - Build `llama.cpp` with SYCL backend (`-DGGML_SYCL=ON`) and benchmark against CPU-only
  - If SYCL works: configure Ollama to use GPU layers (`OLLAMA_NUM_GPU` / `num_gpu` parameter)
  - Document: known issue with `--flash-attn` on iGPU (must be disabled)
  - Benchmark: tokens/sec for qwen3-coder-30b-a3b with 0, 10, 20, all GPU layers
  - Memory budget: iGPU VRAM is shared from system RAM. Document how much DVMT allocation reduces available RAM for services.

- **Hugin CPU (AMD Zen 5):**
  - Verify AVX-512 support on Ryzen 7 255: `lscpu | grep avx512` and `cat /proc/cpuinfo | grep flags`
  - Verify Ollama detects and uses AVX-512 (check Ollama startup logs for "AVX512" detection)
  - Test thread pinning via `taskset` or `numactl` for Ollama process
  - Benchmark: tokens/sec for qwq-32b with default vs pinned threads
  - Document Zen 5 AVX-512 execution characteristics (full-width vs half-width, clock throttling behavior)
  - Configure Ollama `OLLAMA_NUM_THREADS` to match physical core count (8)

- **Exo Framework Evaluation:**
  - Install Exo on both Munin and Hugin
  - Test distributed inference of a 70B model (e.g., Qwen2.5-72B-Q4_K_M split across both nodes)
  - Measure: tokens/sec, inter-node latency, memory usage per node
  - Document: feasibility verdict -- is distributed 70B inference practical over the 5Gb Ethernet link?
  - Compare: 70B distributed vs 32B single-node for coding/reasoning quality
  - This is evaluation only -- no production deployment of Exo in this sprint

- **Candle Embedding (optional, code change):**
  - Add `candle-core`, `candle-nn`, `candle-transformers` as optional workspace dependencies
  - Add `CandelEmbedder` implementation in `ygg-embed` behind a `candle` feature flag
  - Load qwen3-embedding GGUF weights from local disk (no Ollama dependency)
  - Benchmark: embedding latency vs Ollama HTTP API (target: < 5ms for single text)
  - If candle latency is not significantly better than Ollama, leave as feature-flagged alternative
  - Config: `EmbedConfig` gains an optional `backend` field (`"ollama"` default, `"candle"` alternative)

- **Documentation & Configuration Artifacts:**
  - All findings documented in `docs/HARDWARE_OPTIMIZATION.md` (new file)
  - Ollama configuration recommendations for each node
  - Systemd environment overrides for Ollama on each node
  - Benchmark results tables with methodology

### Out of Scope
- Production deployment of Exo (evaluation only)
- BIOS modifications that risk bricking hardware (document procedure, user executes)
- Custom llama.cpp builds for production (Ollama manages its own llama.cpp; this sprint evaluates compatibility)
- IPEX-LLM integration (alternative to SYCL; evaluated only if SYCL fails)
- GPU passthrough or PCIe changes
- Changes to any Yggdrasil service code except `ygg-embed` (candle embedder)
- Model fine-tuning or quantization
- Changes to database configuration or schema
- Thor (Threadripper) optimization (on-demand machine, not part of steady-state Yggdrasil deployment)

## Hardware Constraints & Utilization Strategy

- **Workload Classification:** GPU-bound (inference), CPU-bound (embedding, AVX-512), I/O-bound (inter-node distributed inference).
- **Target Hardware:**

### Munin (REDACTED_MUNIN_IP)
| Component | Specification | Current Usage | Optimization Target |
|-----------|--------------|---------------|---------------------|
| CPU | Intel Core Ultra 185H (6P+8E+2LP, 16T) | Ollama CPU inference, Mimir, Odin | Verify turbo boost enabled, P-core affinity for Ollama |
| iGPU | Intel ARC (Xe-LPG, 8 Xe-cores) | Unused by Ollama | Offload inference layers via SYCL backend |
| RAM | 48GB DDR5 | ~28GB used (model + services) | DVMT allocation reduces available RAM -- must keep >= 18GB free for services |
| Network | 2x 5Gb Ethernet | Standard | Evaluate bonding for Exo distributed inference |

### Hugin (REDACTED_HUGIN_IP)
| Component | Specification | Current Usage | Optimization Target |
|-----------|--------------|---------------|---------------------|
| CPU | AMD Ryzen 7 255 (Zen 5, 8C/16T) | Ollama CPU inference, Huginn, Muninn | AVX-512 verification, thread pinning to physical cores |
| iGPU | AMD RDNA (integrated) | Unused | Not targeted (ROCm for iGPU is immature on Zen 5) |
| RAM | 64GB DDR5 | ~29GB used (model + services) | Verify dual-channel configuration |
| Network | Standard Ethernet | Standard | Evaluate throughput for Exo tensor sharding |

- **Utilization Plan:**
  - **Munin iGPU:** If DVMT can be set to 8-16GB, the ARC iGPU can offload 10-20 transformer layers from CPU to GPU. This reduces CPU compute pressure and may improve tokens/sec by 30-100% depending on how many layers fit in GPU memory. The tradeoff is reduced system RAM (48GB - 16GB DVMT = 32GB available, still sufficient for model + services).
  - **Hugin AVX-512:** Zen 5 implements full-width 512-bit AVX execution (unlike Zen 4 which ran AVX-512 as two 256-bit uops). Confirming AVX-512 detection in Ollama and ensuring the CPU is not throttling under AVX-512 workloads is critical for inference throughput.
  - **Thread pinning:** Ollama's thread pool should be pinned to physical cores only (8 on Hugin) to avoid SMT contention. Hyperthreads introduce cache thrashing for matrix multiplication workloads. `OLLAMA_NUM_THREADS=8` plus `numactl --cpunodebind=0` if NUMA is relevant.
  - **Candle embedding:** In-process embedding avoids HTTP round-trip (~2ms per call) and Ollama's inference scheduling overhead (~3-5ms). Direct GGUF model loading via candle with AVX-512/ARC SYCL acceleration could achieve < 5ms for a 128-token input.

- **Fallback Strategy:**
  - If DVMT BIOS unlock is not available: skip iGPU optimization, document the limitation, file as a hardware procurement decision (external GPU or different BIOS firmware).
  - If SYCL build of llama.cpp fails or produces worse performance than CPU: keep Ollama on CPU-only. Document the benchmark results.
  - If Exo is not practical over 5Gb Ethernet: document the finding, close the workstream. 32B per-node models are already the production strategy.
  - If candle embedding is not significantly faster than Ollama: leave behind the feature flag, use Ollama API as default.

## Performance Targets

| Metric | Current (Estimated) | Target | Measurement Method |
|--------|-------------------|--------|--------------------|
| Munin qwen3-coder-30b-a3b tokens/sec (CPU only) | ~15 tok/s | >= 25 tok/s (with iGPU) | `ollama run` with `--verbose`, measure over 100-token generation |
| Munin qwen3-coder-30b-a3b time-to-first-token | ~2s | < 1s (with iGPU) | `ollama run` with `--verbose` |
| Hugin qwq-32b tokens/sec (CPU only) | ~12 tok/s | >= 15 tok/s (with AVX-512 + pinning) | `ollama run` with `--verbose` |
| Embedding latency (Ollama API, single text) | ~15ms | < 10ms (Ollama) or < 5ms (candle) | `tracing` span in ygg-embed |
| Embedding latency (candle, single text) | N/A | < 5ms | `tracing` span in ygg-embed candle path |
| Exo 70B distributed tokens/sec | N/A | >= 8 tok/s (if feasible) | Exo benchmark output |
| Exo inter-node tensor transfer latency | N/A | < 50ms per layer | Exo debug logging |
| Memory overhead of candle embedder | N/A | < 600MB RSS | `/proc/self/status` VmRSS |

## Data Schemas

### Updated `EmbedConfig` (in `ygg-domain/src/config.rs`)

```rust
/// Embedding service configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedConfig {
    /// Ollama API URL (used when backend is "ollama").
    pub ollama_url: String,
    /// Model name for embedding.
    pub model: String,
    /// Embedding backend: "ollama" (default) or "candle".
    #[serde(default = "default_embed_backend")]
    pub backend: String,
    /// Path to GGUF model weights (required when backend is "candle").
    #[serde(default)]
    pub model_path: Option<String>,
}

fn default_embed_backend() -> String {
    "ollama".to_string()
}
```

### Config file updates

**`configs/mimir/config.yaml`** (candle example, optional):
```yaml
embed:
  ollama_url: "http://localhost:11434"
  model: "qwen3-embedding"
  backend: "ollama"  # default, or "candle"
  # model_path: "/opt/models/qwen3-embedding-q8_0.gguf"  # required for candle backend
```

### Ollama systemd environment (Munin)

File: `/etc/systemd/system/ollama.service.d/override.conf`
```ini
[Service]
Environment="OLLAMA_NUM_GPU=20"
Environment="OLLAMA_FLASH_ATTENTION=0"
Environment="OLLAMA_HOST=0.0.0.0"
```

### Ollama systemd environment (Hugin)

File: `/etc/systemd/system/ollama.service.d/override.conf`
```ini
[Service]
Environment="OLLAMA_NUM_THREADS=8"
Environment="OLLAMA_HOST=0.0.0.0"
ExecStartPre=/usr/bin/numactl --cpunodebind=0 --preferred=0
```

### Benchmark results table template (for `docs/HARDWARE_OPTIMIZATION.md`)

```markdown
## Benchmark: Munin iGPU Offload

| Config | GPU Layers | tok/s (gen) | TTFT (ms) | Peak RSS (GB) | Notes |
|--------|-----------|-------------|-----------|---------------|-------|
| CPU only | 0 | TBD | TBD | TBD | Baseline |
| SYCL 10 layers | 10 | TBD | TBD | TBD | |
| SYCL 20 layers | 20 | TBD | TBD | TBD | |
| SYCL all layers | all | TBD | TBD | TBD | |

## Benchmark: Hugin Thread Pinning

| Config | Threads | tok/s (gen) | TTFT (ms) | CPU Util% | Notes |
|--------|---------|-------------|-----------|-----------|-------|
| Default | auto | TBD | TBD | TBD | Baseline |
| 8 threads (phys cores) | 8 | TBD | TBD | TBD | |
| 8 threads + numactl | 8 | TBD | TBD | TBD | |
| 16 threads (all SMT) | 16 | TBD | TBD | TBD | |

## Benchmark: Embedding Latency

| Backend | P50 (ms) | P95 (ms) | P99 (ms) | Notes |
|---------|----------|----------|----------|-------|
| Ollama HTTP API | TBD | TBD | TBD | Baseline |
| Candle (CPU, AVX-512) | TBD | TBD | TBD | |
| Candle (SYCL iGPU) | TBD | TBD | TBD | If applicable |
```

## API Contracts

### No new service endpoints

This sprint does not add or modify any HTTP endpoints. All changes are:
1. Ollama configuration (environment variables, systemd overrides)
2. Optional `candle` backend in `ygg-embed` (internal, no API change)
3. Documentation artifacts

### `EmbedClient` API (internal, if candle backend added)

The `EmbedClient` public API remains unchanged:

```rust
impl EmbedClient {
    pub fn new(ollama_url: &str, model: &str) -> Self;
    pub async fn embed_single(&self, text: &str) -> Result<Vec<f32>, EmbedError>;
    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError>;
}
```

A new constructor is added for candle:

```rust
impl EmbedClient {
    /// Create an embedding client using candle for in-process inference.
    /// Falls back to Ollama if candle feature is not enabled.
    pub fn with_candle(model_path: &str) -> Result<Self, EmbedError>;
}
```

Internally, `EmbedClient` dispatches between Ollama HTTP and candle based on which constructor was used:

```rust
pub struct EmbedClient {
    inner: EmbedBackend,
}

enum EmbedBackend {
    Ollama { http: reqwest::Client, base_url: String, model: String },
    #[cfg(feature = "candle")]
    Candle { model: Arc<CandelEmbedModel> },
}
```

## Interface Boundaries

| Module | Owns | Exposes | Depends On |
|--------|------|---------|------------|
| `ygg-embed` (extended) | Ollama HTTP embedding + optional candle in-process embedding, backend dispatch | `EmbedClient`, `EmbedClient::with_candle()`, unchanged `embed_single()` / `embed_batch()` API | `reqwest`, optionally `candle-core`, `candle-nn`, `candle-transformers` |
| `ygg-domain::config` (extended) | `EmbedConfig` with `backend` and `model_path` fields | `EmbedConfig` (updated) | `serde` |
| `docs/HARDWARE_OPTIMIZATION.md` (new) | All benchmark results, BIOS procedures, Ollama configs, Exo evaluation | Human-readable reference | N/A |
| `infra-devops` agent | Ollama systemd overrides, oneAPI installation, BIOS changes | Deployment artifacts | Hardware access |

**Ownership rules:**
- Only the `infra-devops` agent executes hardware changes (BIOS, package installation, systemd overrides). This sprint document specifies what to do; `infra-devops` does it.
- Only `ygg-embed` owns the candle embedding implementation. No other crate imports candle.
- The `hardware-optimizer` agent executes the benchmarking and fills in the TBD values in `docs/HARDWARE_OPTIMIZATION.md`.
- Ollama configuration is owned by `infra-devops` and documented in `docs/HARDWARE_OPTIMIZATION.md`.

## File-Level Implementation Plan

### `docs/HARDWARE_OPTIMIZATION.md` (NEW)

New documentation file containing:
- DVMT BIOS procedure for Intel Core Ultra 185H
- oneAPI 2025.2 installation steps for Ubuntu 25.10
- llama.cpp SYCL build instructions
- Ollama GPU layer configuration
- Hugin AVX-512 verification results
- Thread pinning and NUMA configuration
- Exo evaluation results
- Candle embedder benchmark results
- All benchmark data tables (filled in by `hardware-optimizer` agent)

### `crates/ygg-domain/src/config.rs` (MODIFY)

Add `backend` and `model_path` fields to `EmbedConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedConfig {
    pub ollama_url: String,
    pub model: String,
    #[serde(default = "default_embed_backend")]
    pub backend: String,
    #[serde(default)]
    pub model_path: Option<String>,
}

fn default_embed_backend() -> String {
    "ollama".to_string()
}
```

### `crates/ygg-embed/Cargo.toml` (MODIFY)

Add optional candle dependencies:

```toml
[features]
default = []
candle = ["dep:candle-core", "dep:candle-nn", "dep:candle-transformers", "dep:tokenizers", "dep:hf-hub"]

[dependencies]
reqwest = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }

# Candle (optional, for in-process embedding)
candle-core = { version = "0.8", optional = true }
candle-nn = { version = "0.8", optional = true }
candle-transformers = { version = "0.8", optional = true }
tokenizers = { version = "0.20", optional = true }
hf-hub = { version = "0.3", optional = true }
```

### `crates/ygg-embed/src/lib.rs` (MODIFY)

Refactor `EmbedClient` to support dual backends:

```rust
/// Embedding backend dispatch.
enum EmbedBackend {
    Ollama {
        http: reqwest::Client,
        base_url: String,
        model: String,
    },
    #[cfg(feature = "candle")]
    Candle {
        model: std::sync::Arc<candle_embed::CandelEmbedModel>,
    },
}

pub struct EmbedClient {
    inner: EmbedBackend,
}

impl EmbedClient {
    /// Create an embedding client using the Ollama HTTP API (default).
    pub fn new(ollama_url: &str, model: &str) -> Self {
        Self {
            inner: EmbedBackend::Ollama {
                http: reqwest::Client::new(),
                base_url: ollama_url.trim_end_matches('/').to_string(),
                model: model.to_string(),
            },
        }
    }

    /// Create an embedding client using candle for in-process inference.
    #[cfg(feature = "candle")]
    pub fn with_candle(model_path: &str) -> Result<Self, EmbedError> {
        let model = candle_embed::CandelEmbedModel::load(model_path)?;
        Ok(Self {
            inner: EmbedBackend::Candle {
                model: std::sync::Arc::new(model),
            },
        })
    }

    pub async fn embed_single(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        match &self.inner {
            EmbedBackend::Ollama { http, base_url, model } => {
                // existing Ollama implementation
                self.embed_single_ollama(http, base_url, model, text).await
            }
            #[cfg(feature = "candle")]
            EmbedBackend::Candle { model } => {
                // candle implementation (runs on blocking thread)
                let model = model.clone();
                let text = text.to_string();
                tokio::task::spawn_blocking(move || model.embed(&text))
                    .await
                    .map_err(|e| EmbedError::Parse(e.to_string()))?
            }
        }
    }

    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        match &self.inner {
            EmbedBackend::Ollama { http, base_url, model } => {
                // existing Ollama batch implementation
                self.embed_batch_ollama(http, base_url, model, texts).await
            }
            #[cfg(feature = "candle")]
            EmbedBackend::Candle { model } => {
                let model = model.clone();
                let texts = texts.to_vec();
                tokio::task::spawn_blocking(move || {
                    texts.iter().map(|t| model.embed(t)).collect::<Result<Vec<_>, _>>()
                })
                .await
                .map_err(|e| EmbedError::Parse(e.to_string()))?
            }
        }
    }
}
```

### `crates/ygg-embed/src/candle_embed.rs` (NEW, only compiled with `candle` feature)

```rust
#[cfg(feature = "candle")]
pub struct CandelEmbedModel {
    model: candle_transformers::models::bert::BertModel,  // or appropriate model type
    tokenizer: tokenizers::Tokenizer,
    device: candle_core::Device,
}

#[cfg(feature = "candle")]
impl CandelEmbedModel {
    pub fn load(model_path: &str) -> Result<Self, EmbedError> {
        // 1. Detect device: prefer SYCL/Metal/CUDA if available, fallback to CPU
        // 2. Load GGUF weights from model_path
        // 3. Load tokenizer from the same directory or HuggingFace Hub
        // 4. Build model
        todo!("implementation by core-executor after hardware-optimizer validates candle compatibility")
    }

    pub fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        // 1. Tokenize text
        // 2. Run forward pass
        // 3. Mean-pool token embeddings
        // 4. L2-normalize
        // 5. Return Vec<f32>
        todo!("implementation by core-executor")
    }
}
```

Note: The exact candle model type depends on whether qwen3-embedding uses BERT, RoBERTa, or a custom architecture. The `hardware-optimizer` agent will determine the correct model class during evaluation and update this file specification.

### Workspace `Cargo.toml` (MODIFY -- only if candle is pursued)

Add candle workspace dependencies:

```toml
# Candle (optional, for in-process embedding)
candle-core = { version = "0.8", optional = true }
candle-nn = { version = "0.8", optional = true }
candle-transformers = { version = "0.8", optional = true }
tokenizers = { version = "0.20", optional = true }
hf-hub = { version = "0.3", optional = true }
```

### Config file updates

All config files that reference `EmbedConfig` (`configs/mimir/config.yaml`, `configs/huginn/config.yaml`, `configs/muninn/config.yaml`) gain two new optional fields. The defaults preserve current behavior (Ollama backend):

```yaml
embed:
  ollama_url: "http://localhost:11434"
  model: "qwen3-embedding"
  # backend: "ollama"       # default, omit for backward compat
  # model_path: null        # only needed for candle backend
```

## Acceptance Criteria

### Munin iGPU
- [ ] DVMT Pre-Allocated memory setting documented (value set, or documented as unavailable with alternative path)
- [ ] Intel oneAPI 2025.2 installed on Munin and `sycl-ls` detects ARC iGPU
- [ ] llama.cpp compiled with SYCL backend and runs inference on ARC iGPU
- [ ] Benchmark table for qwen3-coder-30b-a3b with 0/10/20/all GPU layers filled in
- [ ] If iGPU offload is beneficial: Ollama systemd override configured with optimal `OLLAMA_NUM_GPU`
- [ ] `OLLAMA_FLASH_ATTENTION=0` set (known incompatibility with iGPU)
- [ ] Memory budget documented: available RAM after DVMT allocation >= 18GB for services

### Hugin CPU
- [ ] AVX-512 support confirmed on Ryzen 7 255 via `lscpu` and Ollama startup logs
- [ ] Benchmark table for qwq-32b with default/pinned/8-thread/16-thread configurations filled in
- [ ] If thread pinning is beneficial: Ollama systemd override configured
- [ ] `OLLAMA_NUM_THREADS=8` set if physical-core-only is optimal

### Exo Evaluation
- [ ] Exo installed on both Munin and Hugin
- [ ] 70B model distributed inference attempted and results documented
- [ ] Feasibility verdict written in `docs/HARDWARE_OPTIMIZATION.md` with supporting data
- [ ] If infeasible: documented reason (latency, throughput, stability)

### Candle Embedding (optional)
- [ ] `candle` feature flag compiles cleanly in `ygg-embed`
- [ ] `EmbedClient::with_candle()` loads qwen3-embedding weights and produces embeddings
- [ ] Benchmark: candle embedding P95 < 5ms for single 128-token input (on Munin CPU or iGPU)
- [ ] If candle is not faster than Ollama: feature remains behind flag, documented in benchmark results
- [ ] Backward compatibility: all existing services work unchanged without `candle` feature

### Documentation
- [ ] `docs/HARDWARE_OPTIMIZATION.md` exists with all benchmark tables filled
- [ ] All configuration recommendations documented with rationale
- [ ] Ollama systemd override files specified per node

## Dependencies

| Dependency | Type | Status |
|------------|------|--------|
| Munin hardware access | `infra-devops` agent | Must have SSH access and sudo |
| Hugin hardware access | `infra-devops` agent | Must have SSH access and sudo |
| Intel oneAPI 2025.2 | External package | Available from Intel APT repo |
| Ollama installed on both nodes | Infrastructure | Sprint 000 |
| qwen3-coder-30b-a3b on Munin | Infrastructure | Sprint 000 (currently runs qwen3 14b -- needs model pull) |
| qwq-32b on Hugin | Infrastructure | Sprint 000 |
| qwen3-embedding on both nodes | Infrastructure | Sprint 000 |
| Exo framework | External software | Open source, Python-based |
| candle crates v0.8+ | External dependency | Published on crates.io |
| Sprint 002 (Mimir) | Must be running | For embedding latency benchmarks with real workload |

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| DVMT Pre-Allocated memory is locked in BIOS (common on laptop-class chipsets like Core Ultra 185H) | Research `DVMT-Unlocker` or `grub-shim` approaches for UEFI. If BIOS is truly locked, document as hardware limitation. Alternative: use `i915` kernel parameter `i915.force_probe=*` for preliminary iGPU access without DVMT changes. |
| SYCL backend for llama.cpp is unstable or slow on ARC Xe-LPG iGPU | Benchmark rigorously before committing to production config. If SYCL is worse than CPU, keep CPU-only and document. IPEX-LLM is an alternative backend to evaluate if SYCL fails. |
| AVX-512 causes CPU clock throttling on Zen 5 under sustained inference | Monitor CPU frequency during benchmarks (`turbostat` or `/proc/cpuinfo` scaling_cur_freq). If throttling negates the throughput gain, fall back to AVX2-only via `GGML_NO_AVX512=1` environment variable. |
| Exo adds significant complexity for marginal quality improvement | This is evaluation-only. If 70B distributed does not meaningfully outperform 32B single-node for coding/reasoning tasks, the finding is documented and Exo is not deployed. |
| candle GGUF loader does not support qwen3-embedding architecture | Check candle-transformers model support before implementation. If qwen3-embedding is not supported, try safetensors format from HuggingFace or skip candle embedder entirely. |
| Thread pinning interacts poorly with Ollama's internal thread management | Test incrementally. Start with `OLLAMA_NUM_THREADS` alone, then add `numactl`, then add `taskset`. Revert if any configuration causes hangs or degradation. |
| oneAPI 2025.2 package conflicts with existing Ubuntu 25.10 packages | Install in a dedicated prefix (`/opt/intel/oneapi/`) and source environment only for Ollama. Containerize if necessary. |
| Reducing DVMT allocation leaves insufficient RAM for services | Budget carefully: 48GB - 8GB DVMT = 40GB. Model (19GB) + services (8GB) = 27GB. Still 13GB headroom. If 16GB DVMT is needed, headroom drops to 5GB -- may be tight. Test and document. |

## Decision Log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-03-09 | Optimization Track, not Feature Track | No new service features. This sprint is research, configuration, and benchmarking with one optional code change (candle embedder). |
| 2026-03-09 | `infra-devops` agent executes hardware changes | BIOS modifications, package installation, and systemd configuration are infrastructure tasks. The `hardware-optimizer` agent designs and benchmarks; `infra-devops` deploys. |
| 2026-03-09 | candle embedder behind feature flag, not replacing Ollama | Ollama embedding works today. Candle is a latency optimization. If it does not deliver < 5ms, it is not worth the maintenance burden of a second embedding path. The feature flag ensures zero impact on existing code when not used. |
| 2026-03-09 | Exo is evaluation-only | 32B per-node is the production strategy. Distributed 70B is speculative. If it works well, it can be productionized in a future sprint. If not, no code changes were made. |
| 2026-03-09 | Target SYCL backend before IPEX-LLM | SYCL is the standard Intel GPU compute API. llama.cpp has native SYCL support (`-DGGML_SYCL=ON`). IPEX-LLM is an Intel-specific optimization layer that adds dependencies. Try the simpler path first. |
| 2026-03-09 | Disable flash attention on iGPU | Known issue from master plan: `--flash-attn` causes crashes or incorrect output on Intel ARC iGPUs. Set `OLLAMA_FLASH_ATTENTION=0` unconditionally on Munin. |
| 2026-03-09 | 8 physical cores for Ollama on Hugin, not 16 SMT threads | Matrix multiplication (the core GGML operation) benefits from cache locality. SMT threads share L1/L2 cache, causing thrashing. Pinning to physical cores gives each thread exclusive cache access. Benchmark will confirm. |
| 2026-03-09 | `EmbedConfig.backend` field uses string, not enum | Forward compatibility. New backends (e.g., ONNX, TensorRT) can be added without modifying the enum. Validation happens at construction time in `EmbedClient`. |
