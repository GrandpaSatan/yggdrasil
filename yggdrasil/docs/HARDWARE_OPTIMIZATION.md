# Hardware Optimization — Yggdrasil

Sprint: 009
Status: IN PROGRESS — benchmark values marked TBD, to be filled by hardware-optimizer agent after physical execution.

This document is the canonical reference for all hardware optimization work on the two production nodes: **Munin** (Intel Core Ultra 185H) and **Hugin** (AMD Ryzen 7 255 / Zen 5). It covers iGPU activation, AVX-512 verification, thread pinning, distributed inference evaluation, and in-process embedding benchmarks.

---

## Table of Contents

1. [Node Overview](#1-node-overview)
2. [Munin — Intel ARC iGPU Activation](#2-munin--intel-arc-igpu-activation)
   - [2.1 DVMT Pre-Allocated Memory — BIOS Procedure](#21-dvmt-pre-allocated-memory--bios-procedure)
   - [2.2 Intel oneAPI 2025.2 Installation](#22-intel-oneapi-20252-installation)
   - [2.3 llama.cpp SYCL Build](#23-llamacpp-sycl-build)
   - [2.4 Ollama iGPU Configuration](#24-ollama-igpu-configuration)
   - [2.5 Known Issues](#25-known-issues)
3. [Hugin — AMD Zen 5 CPU Optimization](#3-hugin--amd-zen-5-cpu-optimization)
   - [3.1 AVX-512 Verification](#31-avx-512-verification)
   - [3.2 Thread Pinning and NUMA Configuration](#32-thread-pinning-and-numa-configuration)
   - [3.3 Ollama Thread Configuration](#33-ollama-thread-configuration)
4. [Exo Framework Evaluation](#4-exo-framework-evaluation)
5. [Candle Embedding Benchmark](#5-candle-embedding-benchmark)
6. [Benchmark Results Tables](#6-benchmark-results-tables)
7. [Systemd Override Files](#7-systemd-override-files)
8. [Decision Log](#8-decision-log)

---

## 1. Node Overview

| Property | Munin | Hugin |
|----------|-------|-------|
| IP | REDACTED_MUNIN_IP | REDACTED_HUGIN_IP |
| CPU | Intel Core Ultra 185H (6P+8E+2LP, 16T) | AMD Ryzen 7 255 (Zen 5, 8C/16T) |
| iGPU | Intel ARC (Xe-LPG, 8 Xe-cores) | AMD RDNA (integrated, not targeted) |
| RAM | 48 GB DDR5 | 64 GB DDR5 |
| Network | 2x 5 Gb Ethernet | Standard Ethernet |
| Ollama model | qwen3-coder-30b-a3b | qwq-32b |
| Services | Mimir, Odin | Huginn, Muninn |

---

## 2. Munin — Intel ARC iGPU Activation

### 2.1 DVMT Pre-Allocated Memory — BIOS Procedure

**Background:** The Intel Core Ultra 185H (Meteor Lake) platform allocates shared system RAM for the ARC iGPU via the DVMT (Dynamic Video Memory Technology) Pre-Allocated setting in UEFI firmware. By default, this is often set to 32 MB or 64 MB (the minimum for display output). For running inference layers on the iGPU, a larger allocation (4–16 GB) is required. On laptop-class hardware, this setting is frequently locked in the OEM BIOS.

**Research findings:**
- The Core Ultra 185H (Meteor Lake-H) is a laptop/embedded SoC. The DVMT setting is typically in: `Advanced > System Agent (SA) Configuration > Graphics Configuration > DVMT Pre-Allocated`.
- Common OEM lock: many vendors hide or lock this setting. If invisible, use the grub parameter or UEFI variable approach below.
- The `DVMT-Unlocker` tool can unlock hidden UEFI variables on Intel platforms. Exercise extreme caution — incorrect UEFI variable modification can brick the firmware.

**Procedure (execute only after confirming BIOS availability):**

1. Enter UEFI setup (typically Del or F2 on POST).
2. Navigate to: `Advanced > System Agent Configuration > Graphics Configuration`.
3. Locate `DVMT Pre-Allocated`. If visible, set to `8192M` (8 GB) or `16384M` (16 GB) depending on RAM budget.
4. Save and exit.

**If DVMT setting is locked (common):**

Option A — Kernel parameter (partial workaround, limited VRAM):
```
# In /etc/default/grub, append to GRUB_CMDLINE_LINUX_DEFAULT:
i915.force_probe=7d45 i915.enable_guc=3
# Then: update-grub && reboot
```
This forces i915 driver probe and enables GuC/HuC firmware, which may allow better iGPU utilization without changing DVMT allocation, but VRAM is still limited to the pre-allocated amount.

Option B — UEFI variable via `efivar` (advanced, research only):
```bash
# List all EFI variables related to graphics
efivar -l | grep -i dvmt
# DO NOT write variables without confirming the exact variable name and value encoding.
# Consult hardware-optimizer agent before proceeding.
```

**Memory budget with DVMT allocation:**

| DVMT Setting | System RAM Available | Model RSS (est.) | Services RSS (est.) | Headroom |
|-------------|---------------------|------------------|---------------------|----------|
| Default (64 MB) | ~48 GB | ~19 GB | ~8 GB | ~21 GB |
| 4 GB | ~44 GB | ~19 GB | ~8 GB | ~17 GB |
| 8 GB | ~40 GB | ~19 GB | ~8 GB | ~13 GB |
| 16 GB | ~32 GB | ~19 GB | ~8 GB | ~5 GB |

Minimum required headroom for stable service operation: 18 GB. Therefore 16 GB DVMT allocation may be too aggressive unless model RSS is confirmed lower than 19 GB.

**Status: TBD** — hardware-optimizer agent to confirm DVMT setting availability in Munin's UEFI and execute the appropriate procedure.

---

### 2.2 Intel oneAPI 2025.2 Installation

Install the Intel oneAPI Base Toolkit on Munin to provide the SYCL runtime, DPC++ compiler, and Level Zero GPU driver interface required by llama.cpp's SYCL backend.

**Prerequisites:**
```bash
# Verify Ubuntu version
lsb_release -a
# Expected: Ubuntu 25.10 (Oracular)
```

**Installation steps:**

```bash
# 1. Add Intel APT repository
wget -O- https://apt.repos.intel.com/intel-gpg-keys/GPG-PUB-KEY-INTEL-SW-PRODUCTS.PUB \
  | gpg --dearmor \
  | sudo tee /usr/share/keyrings/oneapi-archive-keyring.gpg > /dev/null

echo "deb [signed-by=/usr/share/keyrings/oneapi-archive-keyring.gpg] \
  https://apt.repos.intel.com/oneapi all main" \
  | sudo tee /etc/apt/sources.list.d/oneAPI.list

sudo apt update

# 2. Install Base Toolkit 2025.2
sudo apt install intel-basekit-2025.2

# 3. Verify SYCL device detection
source /opt/intel/oneapi/setvars.sh
sycl-ls
```

**Expected sycl-ls output (if ARC iGPU is accessible):**
```
[opencl:cpu:0] Intel(R) OpenCL, Intel(R) Core(TM) Ultra 5 185H ...
[ext_oneapi_level_zero:gpu:0] Intel(R) Level-Zero, Intel(R) Arc(TM) Graphics ...
```

If only the CPU device appears, the DVMT allocation is insufficient or the Level Zero driver is not loaded.

**Level Zero driver setup (if not auto-installed):**
```bash
sudo apt install intel-level-zero-gpu intel-opencl-icd
# Verify
ls /dev/dri/renderD*  # Should show renderD128 or similar
sudo usermod -aG render $USER
```

**Status: TBD** — hardware-optimizer agent to execute and confirm `sycl-ls` output.

---

### 2.3 llama.cpp SYCL Build

Build llama.cpp with SYCL backend to benchmark iGPU inference. Ollama manages its own llama.cpp binary, so this build is for benchmarking only — results inform the Ollama `num_gpu` parameter.

```bash
# Source oneAPI environment
source /opt/intel/oneapi/setvars.sh

# Clone llama.cpp
git clone https://github.com/ggerganov/llama.cpp /opt/llama-cpp-sycl
cd /opt/llama-cpp-sycl

# Configure with SYCL backend
cmake -B build \
  -DGGML_SYCL=ON \
  -DCMAKE_C_COMPILER=icx \
  -DCMAKE_CXX_COMPILER=icpx \
  -DGGML_SYCL_F16=ON

cmake --build build --config Release -j$(nproc)

# Verify build
./build/bin/llama-cli --version
```

**Known issue:** Flash attention must be disabled on Intel ARC iGPU (see section 2.5).

**Benchmark run (template):**
```bash
# Replace MODEL_PATH with the qwen3-coder-30b-a3b GGUF path
MODEL=/opt/models/qwen3-coder-30b-a3b.Q4_K_M.gguf

# CPU only (baseline)
./build/bin/llama-bench -m $MODEL -ngl 0 -n 100 -p 512

# 10 GPU layers
./build/bin/llama-bench -m $MODEL -ngl 10 -n 100 -p 512

# 20 GPU layers
./build/bin/llama-bench -m $MODEL -ngl 20 -n 100 -p 512

# All GPU layers
./build/bin/llama-bench -m $MODEL -ngl 99 -n 100 -p 512
```

**Status: TBD** — hardware-optimizer agent to execute after sycl-ls confirms ARC GPU detection.

---

### 2.4 Ollama iGPU Configuration

Once SYCL benchmarks confirm a beneficial GPU layer count, configure Ollama's systemd service to use the iGPU:

**File: `/etc/systemd/system/ollama.service.d/override.conf`**
```ini
[Service]
Environment="OLLAMA_NUM_GPU=20"
Environment="OLLAMA_FLASH_ATTENTION=0"
Environment="OLLAMA_HOST=0.0.0.0"
```

The `OLLAMA_NUM_GPU` value (here `20`) must be replaced with the optimal value determined from benchmarks in section 6. `OLLAMA_FLASH_ATTENTION=0` is unconditional — see section 2.5.

**Apply the override:**
```bash
sudo systemctl daemon-reload
sudo systemctl restart ollama
# Verify GPU detection in Ollama logs
sudo journalctl -u ollama -n 50 | grep -i "gpu\|arc\|intel\|layer"
```

---

### 2.5 Known Issues

**Flash attention incompatibility with Intel ARC iGPU:**
The `--flash-attn` flag (controlled by `OLLAMA_FLASH_ATTENTION` environment variable) causes incorrect output or crashes when using the SYCL backend on Intel ARC iGPUs, including the Xe-LPG architecture in the Core Ultra 185H. This is a known upstream issue in llama.cpp's SYCL path. Set `OLLAMA_FLASH_ATTENTION=0` unconditionally on Munin.

**Level Zero vs OpenCL:**
The SYCL backend in llama.cpp preferentially uses Level Zero for Intel GPUs, which gives lower overhead than OpenCL. Ensure `intel-level-zero-gpu` is installed before benchmarking. If Level Zero is unavailable, llama.cpp may fall back to OpenCL or refuse to use the GPU.

**DVMT allocation side effects:**
Each GB of DVMT allocated reduces system RAM by the same amount. Monitor available RAM during inference:
```bash
watch -n 2 free -h
```

---

## 3. Hugin — AMD Zen 5 CPU Optimization

### 3.1 AVX-512 Verification

AMD Zen 5 (Ryzen 7 255) implements full-width 512-bit AVX-512 execution units, unlike Zen 4 which used two fused 256-bit uops. This means AVX-512 instructions on Zen 5 execute in a single cycle (no throughput penalty vs. AVX2 at the instruction level), but clock speed throttling under sustained AVX-512 workloads must be monitored.

**Verification commands:**

```bash
# Method 1: lscpu
lscpu | grep -i avx

# Method 2: /proc/cpuinfo flags (look for avx512f, avx512bw, avx512vl, avx512vnni)
grep -o 'avx512[a-z_]*' /proc/cpuinfo | sort -u

# Method 3: Confirm Ollama detects AVX-512
sudo journalctl -u ollama -n 100 | grep -i "avx512\|avx"
```

**Expected flags on Zen 5 Ryzen 7 255:**
- `avx512f` — 512-bit foundation
- `avx512bw` — byte and word operations
- `avx512vl` — vector length extensions (enables AVX-512 on 128/256-bit vectors)
- `avx512vnni` — vector neural network instructions (dot products, critical for inference)
- `avx512_bf16` — bfloat16 (if supported on this SKU)

**AVX-512 throttling check:**
```bash
# During an active Ollama inference call, in another terminal:
watch -n 1 "cat /proc/cpuinfo | grep 'cpu MHz' | head -8"
# Or use turbostat if available:
sudo turbostat --interval 2 --show Pkg_MHz,CoreTmp,PkgWatt
```

If CPU frequency drops below the base clock (3.4 GHz for Ryzen 7 255) during AVX-512 workloads, throttling is occurring. Document the sustained frequency in section 6.

If throttling negates throughput gains, test with AVX-512 disabled:
```bash
# Disable AVX-512 for Ollama process (not recommended long-term, for testing only)
GGML_NO_AVX512=1 ollama run qwq-32b
```

**Status: TBD** — hardware-optimizer agent to execute and document flag list and throttling behavior.

---

### 3.2 Thread Pinning and NUMA Configuration

**Rationale:** Matrix multiplication (the core GGML operation) benefits from cache locality. SMT (hyperthreading) threads share L1 and L2 cache, causing thrashing when two threads compete for the same physical core's cache lines. Pinning Ollama to physical cores only (8 on Ryzen 7 255) gives each thread exclusive cache access.

The Ryzen 7 255 is a single-socket, single-NUMA-node CPU, so `numactl` provides deterministic memory allocation (prefer local NUMA node 0) but does not split across multiple nodes.

**Thread pinning approaches:**

Option A — `OLLAMA_NUM_THREADS` only (preferred first test):
```ini
# In /etc/systemd/system/ollama.service.d/override.conf
[Service]
Environment="OLLAMA_NUM_THREADS=8"
```

Option B — `numactl` memory binding (add after confirming Option A):
```ini
[Service]
Environment="OLLAMA_NUM_THREADS=8"
Environment="OLLAMA_HOST=0.0.0.0"
ExecStartPre=/usr/bin/numactl --cpunodebind=0 --preferred=0
```

Option C — `taskset` CPU affinity (if fine-grained pinning is needed):
```bash
# Identify physical core CPU IDs (cores 0-7, excluding HT siblings 8-15)
lscpu -e | head -20
# Then apply taskset to Ollama's PID, or use systemd's CPUAffinity=
```

**Incremental test order:** Test Option A alone first, then add Option B, then test 16 threads (all SMT) as a comparison. Revert to whichever configuration achieves the highest sustained tokens/sec.

**Status: TBD** — hardware-optimizer agent to execute all variants and fill benchmark table.

---

### 3.3 Ollama Thread Configuration

**Systemd override file for Hugin:**

File: `/etc/systemd/system/ollama.service.d/override.conf`

```ini
[Service]
Environment="OLLAMA_NUM_THREADS=8"
Environment="OLLAMA_HOST=0.0.0.0"
ExecStartPre=/usr/bin/numactl --cpunodebind=0 --preferred=0
```

The `ExecStartPre` line with `numactl` is optional and should only be applied if benchmarks confirm it improves throughput. The `OLLAMA_NUM_THREADS=8` value targets the 8 physical cores; adjust based on benchmark results.

**Apply:**
```bash
sudo systemctl daemon-reload
sudo systemctl restart ollama
sudo journalctl -u ollama -n 30
```

---

## 4. Exo Framework Evaluation

**This section is evaluation-only. No production deployment of Exo occurs in sprint 009.**

**Background:** Exo is an open-source distributed inference framework (Python-based) that shards a single model's tensor layers across multiple machines. It uses a peer-to-peer communication model and supports GGUF models via its MLX/tinygrad backends.

**Goal:** Determine if running Qwen2.5-72B-Q4_K_M split between Munin (48 GB RAM) and Hugin (64 GB RAM) over the 5 Gb Ethernet link achieves practical inference throughput (target: >= 8 tok/s).

**Installation:**

On both Munin and Hugin:
```bash
pip install exo-lang
# Or from source:
git clone https://github.com/exo-explore/exo /opt/exo
cd /opt/exo && pip install -e .
```

**Test run:**

On Munin (primary node):
```bash
# Start Exo, it will auto-discover Hugin
exo run qwen2.5:72b-instruct-q4_K_M
```

On Hugin:
```bash
# Join the Exo cluster
exo
```

**Measurements to collect:**
- Tokens/second (generation phase)
- Time to first token (TTFT)
- Inter-node tensor transfer latency (from Exo debug logging: `--debug`)
- Peak RAM per node during inference
- Network bandwidth utilization (use `iftop` or `nethogs`)

**Feasibility verdict criteria:**
- Feasible if: >= 8 tok/s sustained and inter-node latency < 50 ms per layer
- Infeasible if: < 4 tok/s or significant instability (dropped connections, OOM)

**Comparison baseline:** 32B single-node on each machine currently achieves ~12-15 tok/s. A 70B distributed model must meaningfully outperform in output quality (not just raw speed) to justify the operational complexity.

**Status: TBD** — hardware-optimizer agent to execute and document verdict.

---

## 5. Candle Embedding Benchmark

**Background:** `ygg-embed` currently calls Ollama's HTTP API (`POST /api/embeddings`) for all embedding requests. Round-trip overhead includes HTTP framing (~0.5 ms), Ollama request scheduling (~2-5 ms), and model loading (amortized). The total observed P50 latency is approximately 15 ms per call.

The `candle` feature flag in `ygg-embed` adds an in-process embedding path using the `candle-core` / `candle-transformers` crates. This eliminates the HTTP round-trip and Ollama scheduling overhead. Target: P95 < 5 ms for a single 128-token input.

**Prerequisite — verify candle GGUF support for qwen3-embedding:**

The qwen3-embedding model architecture must be supported by `candle-transformers`. Check:
```rust
// In candle-transformers, look for:
candle_transformers::models::qwen2  // Qwen3 may use Qwen2 architecture
// or:
candle_transformers::models::bert   // If qwen3-embedding is BERT-derived
```

If the architecture is not supported, the `CandelEmbedModel::load()` stub must be updated to use the correct model type before the benchmark can proceed.

**Benchmark methodology:**

1. Load model once (measure load time separately, amortized over service lifetime).
2. Run 1000 embed_single calls with a fixed 128-token input.
3. Collect P50, P95, P99 latencies using `std::time::Instant`.
4. Compare against Ollama HTTP baseline (same 1000 calls).

**Code location:** `crates/ygg-embed/src/candle_embed.rs` (stub, to be implemented by hardware-optimizer after architecture is confirmed).

**Feature flag:** compile with `cargo build -p ygg-embed --features candle` to enable.

**Decision gate:** If candle P95 >= Ollama P95 (i.e., no meaningful improvement), the candle path remains behind the feature flag and the Ollama backend stays as production default. The feature flag adds zero overhead when not compiled in.

**Status: TBD** — hardware-optimizer agent to implement `CandelEmbedModel` and run benchmarks after confirming candle architecture support.

---

## 6. Benchmark Results Tables

All values marked TBD are to be filled by the hardware-optimizer agent after physical execution.

### Benchmark: Munin iGPU Offload (qwen3-coder-30b-a3b)

Model: qwen3-coder-30b-a3b.Q4_K_M.gguf
Measurement method: `llama-bench -n 100 -p 512` (100 generation tokens, 512 prompt tokens)

| Config | GPU Layers | tok/s (gen) | TTFT (ms) | Peak RSS (GB) | Notes |
|--------|-----------|-------------|-----------|---------------|-------|
| CPU only | 0 | TBD | TBD | TBD | Baseline |
| SYCL 10 layers | 10 | TBD | TBD | TBD | |
| SYCL 20 layers | 20 | TBD | TBD | TBD | |
| SYCL all layers | all | TBD | TBD | TBD | OOM expected if DVMT < 8 GB |

Target: >= 25 tok/s generation, < 1000 ms TTFT with iGPU offload.

### Benchmark: Hugin Thread Pinning (qwq-32b)

Model: qwq-32b.Q4_K_M.gguf
Measurement method: `ollama run qwq-32b --verbose`, over 100-token generation

| Config | Threads | tok/s (gen) | TTFT (ms) | CPU Util% | Notes |
|--------|---------|-------------|-----------|-----------|-------|
| Default (auto) | auto | TBD | TBD | TBD | Baseline |
| 8 threads (phys cores) | 8 | TBD | TBD | TBD | OLLAMA_NUM_THREADS=8 |
| 8 threads + numactl | 8 | TBD | TBD | TBD | + cpunodebind=0 |
| 16 threads (all SMT) | 16 | TBD | TBD | TBD | OLLAMA_NUM_THREADS=16 |

Target: >= 15 tok/s generation with optimal thread configuration.

### Benchmark: Embedding Latency (qwen3-embedding)

Measurement method: 1000 sequential `embed_single` calls, fixed 128-token input, wall clock via `std::time::Instant`.

| Backend | P50 (ms) | P95 (ms) | P99 (ms) | Notes |
|---------|----------|----------|----------|-------|
| Ollama HTTP API (Munin) | TBD | TBD | TBD | Baseline |
| Ollama HTTP API (Hugin) | TBD | TBD | TBD | Baseline |
| Candle (CPU, AVX-512, Hugin) | TBD | TBD | TBD | Requires candle feature + AVX-512 |
| Candle (SYCL iGPU, Munin) | TBD | TBD | TBD | Requires DVMT + oneAPI + candle |

Target: Candle P95 < 5 ms. If not achieved, keep Ollama as default.

### Benchmark: Exo Distributed Inference (Qwen2.5-72B-Q4_K_M)

| Config | tok/s (gen) | TTFT (ms) | Network util (Gb/s) | RSS Munin (GB) | RSS Hugin (GB) | Notes |
|--------|-------------|-----------|---------------------|----------------|----------------|-------|
| Exo distributed | TBD | TBD | TBD | TBD | TBD | |
| Baseline: qwq-32b single-node Hugin | TBD | TBD | N/A | N/A | TBD | |

Target: >= 8 tok/s sustained, inter-node latency < 50 ms per layer.

### Candle Model Load Time

| Platform | Load time (ms) | RSS after load (MB) | Notes |
|----------|---------------|---------------------|-------|
| Hugin CPU (AVX-512) | TBD | TBD | |
| Munin CPU | TBD | TBD | |
| Munin iGPU (SYCL) | TBD | TBD | If DVMT + SYCL available |

Target: candle embedder RSS < 600 MB.

---

## 7. Systemd Override Files

### Munin — Ollama with iGPU

File: `/etc/systemd/system/ollama.service.d/override.conf`

```ini
[Service]
Environment="OLLAMA_NUM_GPU=20"
Environment="OLLAMA_FLASH_ATTENTION=0"
Environment="OLLAMA_HOST=0.0.0.0"
```

Replace `OLLAMA_NUM_GPU=20` with the value determined to be optimal from the benchmark table above. `OLLAMA_FLASH_ATTENTION=0` is mandatory on Munin (ARC iGPU incompatibility).

### Hugin — Ollama with Thread Pinning

File: `/etc/systemd/system/ollama.service.d/override.conf`

```ini
[Service]
Environment="OLLAMA_NUM_THREADS=8"
Environment="OLLAMA_HOST=0.0.0.0"
ExecStartPre=/usr/bin/numactl --cpunodebind=0 --preferred=0
```

Only apply the `ExecStartPre` line if `numactl` is installed and benchmarks confirm it improves throughput.

---

## 8. Decision Log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-03-09 | Target SYCL backend before IPEX-LLM | SYCL is the standard Intel GPU compute API. llama.cpp has native SYCL support. IPEX-LLM adds dependencies. Simpler path first. |
| 2026-03-09 | OLLAMA_FLASH_ATTENTION=0 unconditional on Munin | Known upstream issue: flash attention causes crashes or incorrect output on Intel ARC iGPU (Xe-LPG). No exceptions. |
| 2026-03-09 | 8 physical cores for Ollama on Hugin, not 16 SMT | GGML matrix multiplication benefits from cache locality. SMT threads share L1/L2, causing thrashing. Pinning to physical cores gives exclusive cache access per thread. Benchmark will confirm. |
| 2026-03-09 | candle embedder behind feature flag, not replacing Ollama | Ollama embedding works today. candle is a latency optimization. If it does not deliver P95 < 5 ms, it is not worth the maintenance burden of a second code path. Zero impact on production when feature is disabled. |
| 2026-03-09 | Exo is evaluation-only this sprint | 32B per-node is the production strategy. Distributed 70B is speculative. If it works well, productionize in a future sprint. If not, no code changes were made. |
| 2026-03-09 | candle deps local to ygg-embed, not workspace | candle is large (multiple C++ compilation units). Adding to workspace would increase compile time for all crates. Keeping it optional and local minimizes impact on the default build. |
| 2026-03-09 | DVMT procedure documented but not executed by core-executor | BIOS modifications require physical hardware access and carry bricking risk. The infra-devops agent executes hardware changes with direct hardware access. |
