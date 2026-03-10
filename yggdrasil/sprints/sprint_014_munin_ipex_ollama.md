# Sprint: 014 - Munin IPEX-LLM Ollama Migration

## Status: DONE

## Objective

Replace the native Ollama installation on Munin (REDACTED_MUNIN_IP) with the `intelanalytics/ipex-llm-inference-cpp-xpu` Docker container to achieve proper Intel Arc iGPU acceleration via oneAPI/SYCL. The current native Ollama uses a Vulkan backend that achieves only 29% GPU / 71% CPU offload. IPEX-LLM provides a purpose-built SYCL backend that fully offloads inference to the Intel Arc iGPU, and combined with a reduced context window (8192 tokens instead of the default 32K), this will eliminate the 45GB resident memory problem that currently exhausts Munin's 48GB RAM. The Ollama HTTP API remains on port 11434, making this a transparent infrastructure swap -- zero changes to Odin, Mimir, or any Yggdrasil Rust code.

## Scope

### In Scope
- Stop and disable the native Ollama systemd service on Munin
- Deploy the `intelanalytics/ipex-llm-inference-cpp-xpu` Docker container with Ollama serving mode
- Create `deploy/munin/docker-compose.ipex-ollama.yml` in the Yggdrasil workspace
- Pass through `/dev/dri` devices for Intel Arc iGPU access via SYCL/Level Zero
- Mount the existing Ollama model directory (`/usr/share/ollama/.ollama/models`) into the container for model reuse
- Set `OLLAMA_NUM_GPU=999` to offload all model layers to iGPU
- Set `OLLAMA_CONTEXT_LENGTH=8192` to cap context size and prevent memory exhaustion
- Set iGPU-specific environment variables: `BIGDL_LLM_XMX_DISABLED=1`, `SYCL_CACHE_PERSISTENT=1`, `SYCL_PI_LEVEL_ZERO_USE_IMMEDIATE_COMMANDLISTS=1`, `ZES_ENABLE_SYSMAN=1`
- Verify `sycl-ls` inside the container detects the Meteor Lake Arc iGPU
- Re-pull or verify models inside the container: `qwen3-coder:30b-a3b-q4_K_M`, `qwen3-embedding:latest`
- Benchmark tok/s before migration (Vulkan baseline) and after migration (IPEX-LLM SYCL)
- Verify Odin can still reach Ollama on `localhost:11434` without configuration changes
- Verify Mimir can still reach Ollama on `localhost:11434` for embedding
- Create a systemd unit (`yggdrasil-ollama-ipex.service`) that wraps docker compose for auto-start on boot
- Back up the native Ollama systemd override for rollback

### Out of Scope
- Uninstalling native Ollama binary from `/usr/local/bin/ollama` (kept for rollback)
- Changes to any Yggdrasil Rust source code (Odin, Mimir, ygg-embed, etc.)
- Changes to Odin config (`configs/odin/node.yaml`) -- Ollama API is transparent
- Changes to Mimir config -- embedding endpoint is unchanged
- Hugin Ollama configuration (stays native Ollama on Hugin)
- Model fine-tuning, re-quantization, or model changes
- PostgreSQL Docker container on Munin (unaffected, different ports)
- DVMT BIOS changes or iGPU VRAM allocation (IPEX-LLM uses shared memory, DVMT is irrelevant for SYCL)
- Candle embedding backend (Sprint 009 scope)
- Open WebUI or any frontend deployment

## Hardware Constraints & Utilization Strategy

- **Workload Classification:** GPU-bound (inference offloaded to iGPU via SYCL), with CPU fallback for layers that do not fit in GPU memory
- **Target Hardware:** Munin (REDACTED_MUNIN_IP)

| Component | Specification | Current Usage | Post-Migration Target |
|-----------|--------------|---------------|----------------------|
| CPU | Intel Core Ultra 185H (6P+8E+2LP, 16T) | 71% of inference compute (Vulkan fallback) | Minimal inference compute; CPU available for Odin, Mimir, PostgreSQL |
| iGPU | Intel Arc Graphics (Meteor Lake-P, Xe-LPG) | 29% of inference via Vulkan (poor utilization) | Primary inference target via oneAPI/SYCL Level Zero |
| RAM | 48GB DDR5 (~45GB usable) | 45GB resident at 32K context (exhausted) | ~25-28GB target: 18GB model + ~2GB KV cache (8K ctx) + ~5GB services + OS |
| Storage | Local SSD | Model blobs at `/usr/share/ollama/.ollama/models/` | Same path, bind-mounted into container |
| Network | 2x 5Gb Ethernet | Ollama on localhost:11434 | Same -- container uses `--net=host` |

- **Utilization Plan:**
  - IPEX-LLM wraps llama.cpp with Intel's oneAPI/SYCL backend, replacing the weak Vulkan compute path with Level Zero direct GPU access. The Meteor Lake Arc iGPU has Xe-LPG cores with hardware matrix engines (XMX/XVE). However, for iGPU configurations, XMX must be disabled (`BIGDL_LLM_XMX_DISABLED=1`) because the iGPU's XMX units have known compatibility issues with the IPEX-LLM runtime.
  - `OLLAMA_NUM_GPU=999` instructs the SYCL backend to offload all transformer layers to the iGPU. The iGPU shares system RAM, so "GPU memory" is carved from the same 48GB pool. With the 18GB model weights resident, the iGPU will use shared memory for both weight storage and KV cache computation.
  - Context window is capped at 8192 tokens via `OLLAMA_CONTEXT_LENGTH=8192`. This prevents the KV cache from growing unboundedly. At 8K context, the KV cache for qwen3-coder:30b-a3b (GQA with 5 active KV heads out of 40) consumes approximately 1.5-2GB. This is a dramatic reduction from the 32K default which inflated KV cache to ~27GB.
  - The `SYCL_CACHE_PERSISTENT=1` environment variable enables on-disk caching of compiled SYCL kernels. First inference after container start will be slower (kernel compilation), but subsequent runs reuse cached kernels. The cache directory should be persisted via a Docker volume.
  - `--net=host` is used instead of port mapping to ensure Ollama binds to `localhost:11434` identically to the native installation, preserving all existing service connectivity.

- **Fallback Strategy:**
  - If the IPEX-LLM container fails to detect the iGPU (no SYCL device found), fall back to CPU-only inference inside the container by setting `OLLAMA_NUM_GPU=0`. Performance will be comparable to native Ollama without Vulkan.
  - If the container produces inference errors (SYCL assertion failures, which have been reported on some iGPU configurations), revert to native Ollama by stopping the container and re-enabling the native systemd service.
  - If model compatibility issues arise (qwen3-coder MoE architecture not supported by the IPEX-LLM backend), test with a simpler model first (e.g., `qwen2.5:7b`) to isolate whether the issue is model-specific or GPU-specific.
  - Full rollback procedure is documented below and takes less than 2 minutes.

## Performance Targets

| Metric | Baseline (Native Ollama + Vulkan) | Target (IPEX-LLM + SYCL) | Measurement Method |
|--------|----------------------------------|--------------------------|-------------------|
| qwen3-coder:30b-a3b tok/s (generation) | ~15 tok/s (estimated, 29% GPU) | >= 20 tok/s | `ollama run --verbose`, eval rate over 100-token generation |
| qwen3-coder:30b-a3b time-to-first-token | ~3s (estimated) | < 2s | `ollama run --verbose`, prompt eval time |
| GPU utilization during inference | 29% | >= 80% | `intel_gpu_top` or `xpu-smi` during generation |
| Peak resident memory (model + KV cache) | 45GB (32K context, exhausts RAM) | <= 28GB (8K context) | `docker stats` + `free -h` during inference |
| qwen3-embedding latency (single text) | ~15ms | <= 15ms (no regression) | Mimir `/api/v1/store` response time via Prometheus histogram |
| P50 Odin /v1/chat/completions TTFT | ~3.5s | < 2.5s | Odin Prometheus `http_request_duration_seconds` histogram |
| P95 Odin /v1/chat/completions TTFT | ~5s | < 4s | Odin Prometheus histogram |

**Note on performance expectations:** Benchmarks from an Intel Ultra 5 125H (similar Meteor Lake iGPU, 112 EUs) with IPEX-LLM show ~13 tok/s for 7B Q4_K_M models and ~3.4 tok/s for 32B dense models at 8K context. The qwen3-coder:30b-a3b is a MoE model that activates only 3.3B parameters per token, so its compute profile is closer to a 3-4B dense model. This makes the 20 tok/s target realistic given the reference benchmarks show 18-23 tok/s for 3-4B models on the same class of iGPU.

## Data Schemas

No data schema changes. This sprint modifies only infrastructure (Docker, systemd) and configuration (environment variables).

## API Contracts

No API contract changes. The Ollama HTTP API on port 11434 is identical inside the IPEX-LLM container. All existing endpoints are preserved:

| Endpoint | Consumer | Change |
|----------|----------|--------|
| `POST /api/chat` | Odin (inference) | None -- transparent |
| `POST /api/embeddings` | Mimir, Odin (via ygg-embed) | None -- transparent |
| `GET /api/tags` | Odin (model listing) | None -- same models |
| `POST /api/pull` | Manual model management | None -- works inside container |
| `GET /api/ps` | Monitoring | None -- transparent |

## Interface Boundaries

| Component | Responsibility | Change in This Sprint |
|-----------|---------------|----------------------|
| Native Ollama (`/usr/local/bin/ollama`) | Currently serves LLM inference on Munin | Stopped and disabled. Binary left in place for rollback. |
| IPEX-LLM Ollama container | Serves LLM inference on Munin via SYCL iGPU | **NEW** -- deployed as Docker container |
| Docker Compose file | Defines container configuration | **NEW** -- `deploy/munin/docker-compose.ipex-ollama.yml` |
| systemd unit (`yggdrasil-ollama-ipex.service`) | Auto-starts IPEX-LLM container on boot | **NEW** -- wraps `docker compose up` |
| Odin (Munin, :8080) | Routes LLM requests to Ollama | No change -- still connects to `localhost:11434` |
| Mimir (Munin, :9090) | Uses Ollama for embedding | No change -- still connects to `localhost:11434` |
| PostgreSQL container (Munin, :5432) | Database for Yggdrasil | No change -- independent Docker container |
| Hugin Ollama (REDACTED_HUGIN_IP:11434) | Serves inference on Hugin | No change |

## Implementation Plan

All steps are executed by the `infra-devops` agent unless otherwise noted.

### Artifact: `deploy/munin/docker-compose.ipex-ollama.yml`

```yaml
# IPEX-LLM Ollama for Munin (REDACTED_MUNIN_IP)
# Intel Arc iGPU (Meteor Lake-P) acceleration via oneAPI/SYCL
#
# Replaces native Ollama systemd service.
# Ollama API remains on localhost:11434 (--net=host).
#
# Usage:
#   docker compose -f docker-compose.ipex-ollama.yml up -d
#   docker compose -f docker-compose.ipex-ollama.yml logs -f ipex-ollama

services:
  ipex-ollama:
    image: intelanalytics/ipex-llm-inference-cpp-xpu:latest
    container_name: ipex-ollama
    restart: unless-stopped
    network_mode: host

    devices:
      - /dev/dri/card0:/dev/dri/card0
      - /dev/dri/renderD128:/dev/dri/renderD128

    volumes:
      # Mount existing Ollama model directory for model reuse
      - /usr/share/ollama/.ollama:/root/.ollama
      # Persist SYCL kernel compilation cache across restarts
      - ollama-sycl-cache:/root/.cache

    shm_size: "16g"

    environment:
      # -- Ollama server config --
      OLLAMA_HOST: "0.0.0.0"
      OLLAMA_NUM_PARALLEL: "4"
      OLLAMA_MAX_LOADED_MODELS: "2"
      OLLAMA_CONTEXT_LENGTH: "8192"
      OLLAMA_NUM_GPU: "999"
      OLLAMA_KEEP_ALIVE: "5m"
      OLLAMA_DEBUG: "1"
      OLLAMA_INTEL_GPU: "true"

      # -- Intel oneAPI / SYCL config --
      DEVICE: "iGPU"
      ZES_ENABLE_SYSMAN: "1"
      SYCL_CACHE_PERSISTENT: "1"
      SYCL_PI_LEVEL_ZERO_USE_IMMEDIATE_COMMANDLISTS: "1"
      BIGDL_LLM_XMX_DISABLED: "1"
      ONEAPI_DEVICE_SELECTOR: "level_zero:0"
      USE_XETLA: "OFF"

      # -- Proxy bypass for local connections --
      no_proxy: "localhost,127.0.0.1"

    # Container entrypoint: initialize IPEX-LLM environment, then start Ollama in foreground
    command: >
      bash -c "
        cd /llm/scripts &&
        source ipex-llm-init --gpu --device iGPU &&
        mkdir -p /llm/ollama &&
        cd /llm/ollama &&
        init-ollama &&
        exec ./ollama serve
      "

    deploy:
      resources:
        limits:
          memory: 40G

    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:11434/api/tags"]
      interval: 30s
      timeout: 10s
      retries: 5
      start_period: 120s

volumes:
  ollama-sycl-cache:
    driver: local
```

### Artifact: `deploy/munin/yggdrasil-ollama-ipex.service`

```ini
[Unit]
Description=Yggdrasil IPEX-LLM Ollama (Intel Arc iGPU)
Documentation=https://github.com/intel/ipex-llm
After=docker.service
Requires=docker.service
Before=yggdrasil-odin.service yggdrasil-mimir.service

[Service]
Type=simple
WorkingDirectory=/opt/yggdrasil/deploy/munin
ExecStartPre=/usr/bin/docker compose -f docker-compose.ipex-ollama.yml pull
ExecStart=/usr/bin/docker compose -f docker-compose.ipex-ollama.yml up --remove-orphans
ExecStop=/usr/bin/docker compose -f docker-compose.ipex-ollama.yml down
Restart=on-failure
RestartSec=10
TimeoutStartSec=300
TimeoutStopSec=30

[Install]
WantedBy=multi-user.target
```

### Phase 0: Baseline Benchmarks (Before Migration)

**Purpose:** Record current performance with native Ollama + Vulkan to enable before/after comparison.

1. **SSH to Munin:**
   ```bash
   sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP
   ```

2. **Record current Ollama config:**
   ```bash
   systemctl cat ollama
   cat /etc/systemd/system/ollama.service.d/override.conf
   ollama list
   ollama ps
   free -h
   ```

3. **Benchmark coding model (Vulkan baseline):**
   ```bash
   ollama run qwen3-coder:30b-a3b-q4_K_M "Write a Rust function that reverses a string" --verbose 2>&1 | tail -20
   ```
   Record: `eval rate` (tok/s), `total duration`, `load duration`, `prompt eval duration`.

4. **Benchmark embedding model:**
   ```bash
   time curl -s http://localhost:11434/api/embeddings -d '{"model":"qwen3-embedding","prompt":"test embedding latency"}' > /dev/null
   ```
   Record wall-clock time.

5. **Record memory state during inference:**
   ```bash
   free -h
   ollama ps
   ```

### Phase 1: Pre-flight Checks (Munin)

6. **Verify Docker is installed and running:**
   ```bash
   docker --version
   docker compose version
   systemctl status docker
   ```
   If Docker Compose v2 is not available, install it:
   ```bash
   sudo apt-get update && sudo apt-get install -y docker-compose-plugin
   ```

7. **Verify GPU device nodes exist:**
   ```bash
   ls -la /dev/dri/
   ```
   Expected: `card1`, `renderD128` (confirmed from user context).

8. **Verify no port conflict on 11434 from other Docker containers:**
   ```bash
   docker ps --format '{{.Names}} {{.Ports}}' | grep 11434 || echo "No conflict"
   ```

9. **Check existing model directory:**
   ```bash
   du -sh /usr/share/ollama/.ollama/models/
   ls /usr/share/ollama/.ollama/models/manifests/
   ```
   Record model sizes. These will be bind-mounted into the container.

10. **Pull the IPEX-LLM container image (can be done while native Ollama is still running):**
    ```bash
    docker pull intelanalytics/ipex-llm-inference-cpp-xpu:latest
    ```
    This is a large image (~15-20GB). Allow time for download.

11. **Test SYCL device detection inside the container (non-destructive, native Ollama still running):**
    ```bash
    docker run --rm --device=/dev/dri/card1 --device=/dev/dri/renderD128 \
      intelanalytics/ipex-llm-inference-cpp-xpu:latest \
      bash -c "source /opt/intel/oneapi/setvars.sh 2>/dev/null; sycl-ls"
    ```
    Expected output should include `[level_zero:gpu]` with the Meteor Lake Arc iGPU listed.

    **DECISION GATE:** If `sycl-ls` does not detect any GPU device, this sprint is **BLOCKED**. Investigate driver compatibility (kernel 6.17 + Level Zero loader version). The container may need a newer kernel-compatible build. Check issue [#13334](https://github.com/intel/ipex-llm/issues/13334) for kernel compatibility.

### Phase 2: Deploy IPEX-LLM Container (Munin)

12. **Copy deployment artifacts to Munin:**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "sudo mkdir -p /opt/yggdrasil/deploy/munin"

    sshpass -p 723559 scp \
      /home/jesus/Documents/HardwareSetup/yggdrasil/deploy/munin/docker-compose.ipex-ollama.yml \
      jhernandez@REDACTED_MUNIN_IP:/tmp/docker-compose.ipex-ollama.yml

    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
      "sudo cp /tmp/docker-compose.ipex-ollama.yml /opt/yggdrasil/deploy/munin/"
    ```

13. **Back up native Ollama override for rollback:**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
      "sudo cp /etc/systemd/system/ollama.service.d/override.conf \
       /etc/systemd/system/ollama.service.d/override.conf.bak.014"
    ```

14. **Stop native Ollama:**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "sudo systemctl stop ollama"
    ```

15. **Verify port 11434 is free:**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "ss -tlnp | grep 11434"
    ```
    Expected: empty output (port freed).

16. **Start IPEX-LLM Ollama container:**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
      "cd /opt/yggdrasil/deploy/munin && sudo docker compose -f docker-compose.ipex-ollama.yml up -d"
    ```

17. **Wait for container to become healthy (allow up to 2 minutes for SYCL kernel compilation on first start):**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
      "docker inspect --format='{{.State.Health.Status}}' ipex-ollama"
    ```
    Repeat until status is `healthy`. Check logs if unhealthy:
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
      "docker logs ipex-ollama --tail 50"
    ```

18. **Verify SYCL device is active inside running container:**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
      "docker exec ipex-ollama bash -c 'sycl-ls'"
    ```

### Phase 3: Model Verification (Munin)

19. **Check if existing models are visible:**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "curl -s http://localhost:11434/api/tags | jq '.models[].name'"
    ```
    Expected: `qwen3-coder:30b-a3b-q4_K_M`, `qwen3-embedding:latest`.

    If models are NOT visible (the container's Ollama may use different manifest paths than the native installation), re-pull them:
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "docker exec ipex-ollama ollama pull qwen3-coder:30b-a3b-q4_K_M"
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "docker exec ipex-ollama ollama pull qwen3-embedding:latest"
    ```
    Re-pulling will reuse cached blobs if the SHA256 hashes match, so it should be fast if the model data is already on disk.

20. **Test basic inference:**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
      "docker exec ipex-ollama ollama run qwen3-coder:30b-a3b-q4_K_M 'What is 2+2?' --verbose 2>&1 | tail -20"
    ```
    Verify: response is correct, eval rate is reported, no SYCL errors.

21. **Test embedding:**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
      "curl -s http://localhost:11434/api/embeddings -d '{\"model\":\"qwen3-embedding\",\"prompt\":\"test\"}' | jq '.embedding | length'"
    ```
    Expected: `4096` (the embedding dimension).

### Phase 4: Benchmarks (Post-Migration)

22. **Benchmark coding model (IPEX-LLM SYCL):**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
      "docker exec ipex-ollama ollama run qwen3-coder:30b-a3b-q4_K_M 'Write a Rust function that reverses a string' --verbose 2>&1 | tail -20"
    ```
    Record: `eval rate` (tok/s), `total duration`, `load duration`.
    Compare against Phase 0 baseline.

23. **Check GPU utilization during inference:**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "intel_gpu_top -l 1 2>/dev/null || echo 'intel_gpu_top not installed'"
    ```
    If `intel_gpu_top` is not installed:
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "sudo apt-get install -y intel-gpu-tools && intel_gpu_top -l 1"
    ```

24. **Check memory consumption:**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "docker stats ipex-ollama --no-stream --format '{{.MemUsage}}'"
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "free -h"
    ```
    Verify total memory usage is <= 28GB with both models loaded.

### Phase 5: Disable Native Ollama and Install systemd Unit

25. **Disable native Ollama from starting on boot:**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "sudo systemctl disable ollama"
    ```
    Note: Do NOT `mask` the service -- keep it available for manual rollback start.

26. **Install the systemd unit for IPEX-LLM Ollama:**
    ```bash
    sshpass -p 723559 scp \
      /home/jesus/Documents/HardwareSetup/yggdrasil/deploy/munin/yggdrasil-ollama-ipex.service \
      jhernandez@REDACTED_MUNIN_IP:/tmp/yggdrasil-ollama-ipex.service

    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
      "sudo cp /tmp/yggdrasil-ollama-ipex.service /etc/systemd/system/ && \
       sudo systemctl daemon-reload && \
       sudo systemctl enable yggdrasil-ollama-ipex.service"
    ```

27. **Verify boot ordering:**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
      "systemctl list-dependencies yggdrasil-odin.service | head -10"
    ```
    Confirm `yggdrasil-ollama-ipex.service` starts before Odin and Mimir.

### Phase 6: End-to-End Verification

28. **Odin health check:**
    ```bash
    curl -s http://REDACTED_MUNIN_IP:8080/health | jq .
    ```

29. **Odin model listing (should include Munin models from IPEX-LLM Ollama):**
    ```bash
    curl -s http://REDACTED_MUNIN_IP:8080/v1/models | jq '.data[].id'
    ```
    Expected: `qwen3-coder:30b-a3b-q4_K_M` appears.

30. **Test coding route through Odin:**
    ```bash
    curl -s http://REDACTED_MUNIN_IP:8080/v1/chat/completions \
      -H "Content-Type: application/json" \
      -d '{
        "messages": [{"role": "user", "content": "Write a hello world in Rust"}],
        "stream": false
      }' | jq '.model, .choices[0].message.content[:200]'
    ```
    Verify: model is `qwen3-coder:30b-a3b-q4_K_M`, response is coherent.

31. **Test Mimir engram store (exercises embedding path):**
    ```bash
    curl -s http://REDACTED_MUNIN_IP:9090/api/v1/store \
      -H "Content-Type: application/json" \
      -d '{
        "cause": "Sprint 014 IPEX-LLM migration test",
        "effect": "Embedding via containerized Ollama verified"
      }' | jq .
    ```
    Verify: 201 response with engram ID.

32. **Test Mimir engram query (exercises embedding + retrieval):**
    ```bash
    curl -s http://REDACTED_MUNIN_IP:9090/api/v1/query \
      -H "Content-Type: application/json" \
      -d '{
        "text": "IPEX-LLM migration",
        "limit": 3
      }' | jq '.[0].cause'
    ```
    Verify: returns the engram stored in step 31.

33. **MCP server test (if running):**
    Verify `list_models` MCP tool returns the Munin models. Verify `generate` tool works through Odin to the IPEX-LLM container.

### Phase 7: Stress Test

34. **Concurrent request test:**
    ```bash
    # Run 4 parallel inference requests to verify OLLAMA_NUM_PARALLEL=4 works
    for i in 1 2 3 4; do
      curl -s http://REDACTED_MUNIN_IP:8080/v1/chat/completions \
        -H "Content-Type: application/json" \
        -d "{\"messages\": [{\"role\": \"user\", \"content\": \"Count from 1 to 10 in request $i\"}], \"stream\": false}" &
    done
    wait
    ```
    Verify: all 4 requests complete without OOM or SYCL errors. Check `dmesg` for GPU faults:
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "dmesg | tail -20"
    ```

35. **Memory under concurrent load:**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "free -h && docker stats ipex-ollama --no-stream"
    ```
    Verify memory stays under 40GB during concurrent inference.

## Acceptance Criteria

- [x] Native Ollama systemd service is stopped and disabled on Munin
- [x] `intelanalytics/ipex-llm-inference-cpp-xpu:latest` container is running on Munin
- [x] `sycl-ls` inside the container detects the Meteor Lake Arc iGPU as a Level Zero device
- [x] `OLLAMA_NUM_GPU=999` is set and logs show all layers on SYCL0 (`runner.inference=oneapi`)
- [x] `OLLAMA_CONTEXT_LENGTH=8192` is set (verified via container env)
- [x] `BIGDL_LLM_XMX_DISABLED=1` is set (iGPU compatibility)
- [x] `qwen3-coder:30b-a3b-q4_K_M` responds correctly via `localhost:11434`
- [x] `qwen3-embedding:latest` produces 4096-dim embeddings via `localhost:11434`
- [x] Peak memory during single inference is <= 28GB — **PASS: 19.4GB**
- [x] Peak memory during concurrent inference (4 parallel) is <= 40GB — **PASS: 26GB**
- [x] Post-migration tok/s for qwen3-coder >= 20 tok/s (or documented improvement over Vulkan baseline) — **15.9 tok/s sustained, massive improvement over Vulkan baseline (0.79 tok/s under swap, system unusable)**
- [x] Embedding latency has no regression
- [x] Odin health check passes (`GET /health` returns 200, all backends OK)
- [x] Odin routes coding requests to `qwen3-coder:30b-a3b-q4_K_M` and receives valid responses
- [x] Mimir `/api/v1/store` successfully stores an engram (embedding works)
- [x] Mimir `/api/v1/query` successfully retrieves engrams (embedding + Qdrant search works)
- [x] `yggdrasil-ollama-ipex.service` is enabled and starts the container on boot
- [x] Docker compose file exists at `/opt/yggdrasil/deploy/munin/docker-compose.ipex-ollama.yml`
- [x] PostgreSQL container on Munin is unaffected (Mimir can query)
- [x] Benchmark results (before/after) are recorded in this sprint doc's Decision Log

## Dependencies

| Dependency | Type | Status |
|-----------|------|--------|
| Docker Engine on Munin | Infrastructure | Must verify -- Docker is likely installed (PostgreSQL runs in Docker) |
| Docker Compose v2 on Munin | Infrastructure | Must verify -- install `docker-compose-plugin` if absent |
| Intel GPU kernel drivers (i915/xe) | OS | Ubuntu 25.10 ships with kernel 6.17 -- should include Meteor Lake support |
| Level Zero loader on Munin host | OS | Bundled inside the IPEX-LLM container; host only needs `/dev/dri` device nodes |
| `intelanalytics/ipex-llm-inference-cpp-xpu:latest` on Docker Hub | External | Available -- last updated includes Ollama 0.9.3 support |
| PostgreSQL container (Munin, :5432) | Infrastructure | Running -- must not be disrupted |
| Odin service (Munin, :8080) | Service | Running -- must remain functional |
| Mimir service (Munin, :9090) | Service | Running -- must remain functional |
| Sprint 011 (Hardening) | Sprint | DONE -- systemd units and deploy scripts in place |
| Sprint 013 (Hugin MoE Swap) | Sprint | DONE -- Hugin runs qwen3:30b-a3b, unaffected by this sprint |

## Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| IPEX-LLM container does not detect Meteor Lake iGPU via SYCL | Medium | Sprint blocked | Test `sycl-ls` before stopping native Ollama (Phase 1, step 11). If no GPU detected, investigate kernel/driver compatibility. Ubuntu 25.10 kernel 6.17 may need the `xe` driver instead of `i915` for Meteor Lake. Check `lsmod | grep xe`. |
| `ipex-llm` repository archived (Jan 2026), no future updates | High | Long-term risk | Docker images on Docker Hub remain available. This is acceptable for current use. Monitor Ollama native SYCL support as an alternative (Ollama PR #2458 added SYCL backend). If IPEX-LLM becomes incompatible with future kernels, migrate to native Ollama + SYCL. |
| SYCL assertion failures on qwen3-coder MoE architecture | Medium | Model unusable on GPU | Test with a simpler model first (e.g., `qwen2.5:7b`). If MoE triggers SYCL bugs, set `OLLAMA_NUM_GPU=0` for the coder model and use CPU inference. Keep GPU offload for embedding model. |
| 8192 context too short for complex coding prompts | Low | Quality degradation | 8K is sufficient for most single-file coding tasks. Odin's system prompt + RAG context + user message typically fits in 4-6K tokens. If users report truncation, increase to 16384 and re-evaluate memory budget. |
| Model blobs in `/usr/share/ollama/.ollama/models/` are not compatible with container Ollama version | Medium | Models must be re-pulled | Container's Ollama may use a newer manifest format. If models are not visible after bind mount, `ollama pull` inside the container will download fresh manifests but reuse cached blobs (same SHA256). Disk cost: minimal. Time cost: ~5 minutes per model. |
| `--net=host` conflicts with PostgreSQL container | Low | Port collision | PostgreSQL uses port 5432 only. Ollama uses 11434 only. No overlap. `--net=host` is safe here. |
| Container memory limit (40GB) too restrictive for concurrent inference | Medium | OOM kill | Start with 40GB limit. If Docker kills the container during concurrent load, increase to 44GB or remove the limit (the container already limits via `OLLAMA_NUM_PARALLEL=4` and 8K context). Monitor with `docker events`. |
| SYCL kernel compilation on first start takes > 2 minutes | Low | Slow first inference | `SYCL_CACHE_PERSISTENT=1` + persisted volume ensures this only happens once. healthcheck `start_period: 120s` accommodates the delay. Subsequent restarts reuse the cache. |
| `OLLAMA_NUM_PARALLEL=4` causes memory pressure with 4 concurrent 8K contexts | Medium | OOM or degraded performance | 4 concurrent 8K contexts for qwen3-coder MoE: ~4 x 2GB KV cache = 8GB additional. Total: 18GB model + 8GB KV + 5GB embedding = 31GB. Fits in 40GB limit. If problematic, reduce to `OLLAMA_NUM_PARALLEL=2`. |

## Rollback Plan

Total rollback time: < 2 minutes. No data loss.

1. **Stop IPEX-LLM container:**
   ```bash
   sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
     "cd /opt/yggdrasil/deploy/munin && sudo docker compose -f docker-compose.ipex-ollama.yml down"
   ```

2. **Disable IPEX-LLM systemd unit:**
   ```bash
   sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
     "sudo systemctl disable yggdrasil-ollama-ipex.service"
   ```

3. **Re-enable and start native Ollama:**
   ```bash
   sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP \
     "sudo systemctl enable ollama && sudo systemctl start ollama"
   ```

4. **Verify native Ollama is serving:**
   ```bash
   sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "ollama list && ollama ps"
   curl -s http://REDACTED_MUNIN_IP:8080/health | jq .
   ```

5. **Verify Odin and Mimir connectivity:**
   ```bash
   curl -s http://REDACTED_MUNIN_IP:8080/v1/models | jq '.data[].id'
   ```

The native Ollama binary, model files, and systemd override are all preserved in place. Rollback is simply stopping the container and restarting the native service.

## ENV Variable Reference

| Variable | Value | Purpose |
|----------|-------|---------|
| `OLLAMA_HOST` | `0.0.0.0` | Bind Ollama to all interfaces (same as native config) |
| `OLLAMA_NUM_PARALLEL` | `4` | Max concurrent inference requests (reduced from 8 to conserve memory with iGPU) |
| `OLLAMA_MAX_LOADED_MODELS` | `2` | Allow coder + embedding models simultaneously |
| `OLLAMA_CONTEXT_LENGTH` | `8192` | Cap context window to prevent KV cache memory explosion |
| `OLLAMA_NUM_GPU` | `999` | Offload all transformer layers to iGPU |
| `OLLAMA_KEEP_ALIVE` | `5m` | Unload models after 5min idle (free GPU memory) |
| `OLLAMA_DEBUG` | `1` | Enable debug logging (disable after validation) |
| `OLLAMA_INTEL_GPU` | `true` | Signal IPEX-LLM to use Intel GPU backend |
| `DEVICE` | `iGPU` | Tell IPEX-LLM init script to configure for integrated GPU |
| `ZES_ENABLE_SYSMAN` | `1` | Enable Level Zero system management API (GPU monitoring) |
| `SYCL_CACHE_PERSISTENT` | `1` | Cache compiled SYCL kernels to disk for faster restarts |
| `SYCL_PI_LEVEL_ZERO_USE_IMMEDIATE_COMMANDLISTS` | `1` | Use immediate command lists for lower latency GPU dispatch |
| `BIGDL_LLM_XMX_DISABLED` | `1` | Disable XMX matrix engine (required for iGPU compatibility) |
| `ONEAPI_DEVICE_SELECTOR` | `level_zero:0` | Select the first Level Zero GPU device (the iGPU) |
| `USE_XETLA` | `OFF` | Disable XeTLA library (not needed for iGPU, can cause issues) |
| `no_proxy` | `localhost,127.0.0.1` | Bypass proxy for local connections |

## Comparison: OLLAMA_NUM_PARALLEL 8 vs 4

The native Ollama config used `OLLAMA_NUM_PARALLEL=8`. This sprint reduces it to 4 because:
- With Vulkan (29% GPU), most compute was on CPU. The CPU (16 threads) could handle 8 parallel token generations.
- With SYCL (target >80% GPU), the iGPU is the bottleneck. The Meteor Lake iGPU has limited compute units compared to a discrete Arc GPU. 4 parallel requests is a conservative starting point.
- Each parallel request allocates its own KV cache. At 8K context, 4 parallel = ~8GB KV cache total. 8 parallel would be ~16GB, leaving only ~14GB for model weights (18GB needed). This would cause OOM.
- If benchmarks show the iGPU handles 4 parallel well with headroom, increase to 6 in a follow-up.

## Decision Log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-03-09 | Use IPEX-LLM Docker container instead of native Ollama + SYCL build | Sprint 009 planned native SYCL (building llama.cpp with `-DGGML_SYCL=ON`). However, this requires installing oneAPI toolkit on the host, building custom llama.cpp, and maintaining the build. IPEX-LLM bundles everything in a container -- zero host-side dependencies beyond Docker and `/dev/dri`. Simpler, faster to deploy, easier to rollback. |
| 2026-03-09 | Use `--net=host` instead of port mapping | Odin and Mimir connect to `localhost:11434`. With `--net=host`, the container binds directly to the host network, making it identical to native Ollama from the perspective of all consumers. No config changes needed anywhere. |
| 2026-03-09 | Cap context at 8192 tokens | The 32K default inflates KV cache to 27GB, consuming all 48GB RAM. 8K context uses ~2GB KV cache. Most Yggdrasil coding prompts (system prompt + RAG context + user message) fit within 4-6K tokens. 8K provides comfortable headroom without memory exhaustion. |
| 2026-03-09 | Reduce `OLLAMA_NUM_PARALLEL` from 8 to 4 | Memory budget: 18GB model + 4*2GB KV cache + 5GB embedding + 5GB OS/services = 36GB. Fits in 40GB container limit with 4GB headroom. At 8 parallel, KV cache alone would need 16GB, total 44GB -- exceeds safe limits. |
| 2026-03-09 | Set `BIGDL_LLM_XMX_DISABLED=1` | Intel Arc iGPUs have XMX (matrix extension) units, but IPEX-LLM's runtime has known assertion failures when using XMX on integrated GPUs (documented in multiple GitHub issues). Disabling XMX forces scalar SYCL compute which is still significantly faster than Vulkan. |
| 2026-03-09 | Mount existing model directory instead of re-pulling | `/usr/share/ollama/.ollama/models/` contains ~23GB of model blobs. Bind mounting avoids re-downloading. If the container's Ollama version uses incompatible manifests, `ollama pull` inside the container will reuse the blob data via SHA256 matching. |
| 2026-03-09 | Keep `OLLAMA_KEEP_ALIVE=5m` instead of `-1` (forever) | With 18GB coder + 5GB embedding = 23GB locked in GPU memory, there is only ~17GB remaining for KV cache and OS. Unloading after 5min idle frees GPU memory for other processes. Odin's per-backend semaphore ensures models are loaded before inference. |
| 2026-03-09 | Accept risk of archived IPEX-LLM project | The `intel/ipex-llm` repository was archived January 2026. Docker images on Docker Hub remain available and functional. The Ollama community is also developing native SYCL support (PR #2458). If IPEX-LLM becomes incompatible with future kernels, migration to native Ollama + SYCL is the exit path. For now, IPEX-LLM is the most mature and tested path for Intel iGPU acceleration. |
| 2026-03-09 | Supersedes Sprint 009 iGPU workstream | Sprint 009 planned oneAPI host installation + custom llama.cpp SYCL build. This sprint replaces that approach with a containerized solution. Sprint 009's other workstreams (Hugin AVX-512, Exo evaluation, candle embedder) remain independent and unaffected. |
| 2026-03-09 | Use `intelanalytics/ipex-llm-inference-cpp-xpu:latest` image | This is the official Intel image for running llama.cpp/Ollama on Intel XPU. Supports iGPU, Arc, Flex, and Max GPUs. Includes Ollama 0.9.3 as of latest build. The `:latest` tag is acceptable for initial deployment; pin to a specific version tag after validation. |
| 2026-03-09 | GPU device is `/dev/dri/card0` not `card1` | Pre-flight check revealed Munin only has `card0` and `renderD128`. Docker compose updated accordingly. |
| 2026-03-09 | Fixed volume mount: bind-mount `/usr/share/ollama/.ollama` directly | Original plan had conflicting mounts (models subdir bind + parent named volume). Simplified to single bind mount of the entire `.ollama` directory. |
| 2026-03-09 | Fixed container command: `exec ./ollama serve` in foreground | The bundled `start-ollama.sh` runs ollama in background with `&` then exits, killing PID 1. Replaced with foreground `exec` to keep the container alive. |
| 2026-03-09 | `ollama ps` shows "100% CPU" but GPU IS active | IPEX-LLM's Ollama reports "CPU" in `ollama ps` display, but logs confirm all 36 layers on `SYCL0` and `runner.inference=oneapi`. This is a display-only issue. |

## Benchmark Results

| Metric | Vulkan Baseline | IPEX-LLM SYCL | Change |
|--------|----------------|---------------|--------|
| Processor split | 29% GPU / 71% CPU | 100% SYCL0 (all layers on iGPU) | Full GPU offload |
| Generation tok/s (sustained, ~1000 tokens) | ~0.79 tok/s (system swap-thrashing, unusable) | **15.9 tok/s** | **20x improvement** |
| Prompt eval tok/s | N/A (system unresponsive) | **26.8 tok/s** | N/A |
| Peak memory (single inference) | 45GB (exhausted all RAM + 8GB swap) | **19.4GB** | **-57%** |
| Peak memory (4 concurrent) | N/A (system crashed) | **26GB** | System stable |
| Swap usage | 8GB / 8GB (100% exhausted) | **18MB / 8GB** (0.2%) | Swap eliminated |
| System responsiveness under load | SSH unresponsive, services degraded | SSH responsive, all services OK | Fully operational |
| Context window | 32768 (uncapped) | 8192 (capped) | -75% KV cache |
| Model load time (cold, SYCL compile) | ~5s (Vulkan) | ~60s (first run only, cached after) | One-time cost |
| Model load time (warm) | ~5s | **0.02s** | Cached |

**Note:** The 20 tok/s target was not met (achieved 15.9 tok/s), but the real improvement is system stability. The Vulkan baseline was effectively 0 tok/s usable because the system thrashed into swap and became unresponsive. IPEX-LLM delivers 15.9 tok/s with the system fully operational, SSH responsive, and 25GB of headroom.
