# Sprint: 013 - Hugin MoE Model Swap (QwQ-32B-AWQ -> Qwen3:30B-A3B)

## Status: DONE

## Outcome

Replaced dense QwQ-32B-AWQ (vLLM, 1.4 tok/s) with **qwen3:30b-a3b** (Ollama, 26.48 tok/s) on Hugin.
Original plan targeted qwen3-coder-next (80B/3B, 52GB Q4_K_M) but Hugin only has 46GB system RAM
(64GB physical - 16GB iGPU VRAM reservation). Pivoted to qwen3:30b-a3b (30B/3B MoE, 18GB) which
fits easily and outperforms QwQ-32B in benchmarks.

**Performance improvement: 1.4 tok/s -> 26.48 tok/s (19x faster)**

## Objective

Replace the dense QwQ-32B-AWQ model running under vLLM on Hugin (REDACTED_HUGIN_IP) with an MoE-architecture model running under the already-deployed Ollama instance. This eliminates the vLLM container dependency, unifies Hugin's inference stack on a single runtime (Ollama), and delivers a massive throughput improvement. No Rust code changes required -- pure infrastructure and configuration sprint.

## Scope

### In Scope
- Shut down vLLM Docker container on Hugin
- Pull `qwen3-coder-next` (Q4_K_M or Ollama-selected best quant) via Ollama on Hugin
- Verify the model fits in Hugin's 64GB alongside qwen3-embedding (4.7GB) and OS overhead
- Benchmark tok/s on Hugin under Ollama to confirm improvement over 1.4 tok/s baseline
- Update Odin config (`configs/odin/node.yaml`) to retire the `hugin-vllm` (OpenAI) backend and route reasoning/home_automation intents to the new model on the existing `hugin` (Ollama) backend
- Restart Odin on Munin to pick up config changes
- End-to-end verification: health checks, model listing, reasoning prompt, MCP generate tool

### Out of Scope
- Deleting the QwQ-32B-AWQ model weights from Hugin (left in place for potential future use)
- Removing vLLM Docker images from Hugin (cleanup deferred)
- Any Rust source code changes to Odin, ygg-mcp, or ygg-domain (the config structs and routing logic support this swap as-is)
- Updating the MCP server config (`configs/mcp-server/config.yaml`) -- the MCP server calls Odin endpoints, not backends directly; it is model-agnostic
- Changes to Munin's model stack (qwen3-coder:30b-a3b-q4_K_M remains the coding/default model)

## Hardware Constraints & Utilization Strategy

- **Workload Classification:** CPU-bound inference (MoE sparse model, large parameter count but low active parameter count per token)
- **Target Hardware:** Hugin -- AMD Ryzen 7 255 (Zen 5, 8C/16T), 64GB DDR5, AMD iGPU (gfx1100)
- **Utilization Plan:**
  - Qwen3-Coder-Next Q4_K_M (~46GB on disk, ~48GB resident) + qwen3-embedding (~4.7GB resident) = ~53GB model memory. With OS and service overhead (~4GB), total ~57GB of 64GB available. Headroom is tight but sufficient.
  - MoE architecture activates only 3B of 80B parameters per token, meaning the inference compute is comparable to a 3B dense model despite the large weight footprint. This should yield dramatically higher tok/s than the dense 32B QwQ model.
  - Ollama CPU inference will use all 16 threads by default. The Ryzen 7 255 (Zen 5) supports AVX-512, which Ollama's llama.cpp backend will auto-detect and use for GEMM operations.
  - The AMD iGPU (RDNA 3.5, ~8 compute units) may be used by Ollama for partial offload if ROCm is available, but CPU-only inference is the expected primary path.
- **Fallback Strategy:**
  - If `qwen3-coder-next` does not fit in 64GB at Q4_K_M, try a smaller quant (Q3_K_M, IQ4_XS) or fall back to a smaller MoE variant if one exists.
  - If tok/s does not improve over QwQ-32B baseline, the sprint is abandoned and vLLM is restarted with QwQ-32B-AWQ. The old config is restored from the rollback copy.
  - If Ollama does not yet support Qwen3-Coder-Next, the sprint is blocked until the model is available in the Ollama library. Check `ollama list` and `ollama show qwen3-coder-next` before proceeding.

## Performance Targets

| Metric | Baseline (QwQ-32B on vLLM) | Target (Qwen3-Coder-Next on Ollama) | Measurement Method |
|--------|---------------------------|--------------------------------------|-------------------|
| Token generation rate | 1.4 tok/s | >= 5 tok/s | `ollama run --verbose`, extract eval rate |
| Time to first token | unmeasured | < 5s | `ollama run --verbose`, extract prompt eval time |
| Memory usage (model) | ~20GB (AWQ 4-bit) | <= 52GB (model + embedding) | `ollama ps` |
| Odin routing latency | N/A | < 5ms overhead | Odin debug logs, Prometheus histogram |

## Data Schemas

No schema changes. This sprint modifies only YAML configuration and infrastructure state.

## API Contracts

No API contract changes. Odin's `/v1/chat/completions`, `/v1/models`, and all MCP tool endpoints remain identical. The only observable difference is:
- `GET /v1/models` will return `qwen3-coder-next` (from the `hugin` Ollama backend) instead of `Qwen/QwQ-32B-AWQ` (from the now-removed `hugin-vllm` OpenAI backend).
- Routing for `reasoning` and `home_automation` intents will resolve to model `qwen3-coder-next` on backend `hugin` instead of `Qwen/QwQ-32B-AWQ` on backend `hugin-vllm`.

## Interface Boundaries

| Component | Responsibility | Change in This Sprint |
|-----------|---------------|----------------------|
| Hugin Ollama (REDACTED_HUGIN_IP:11434) | Serves inference models | Gains `qwen3-coder-next` model |
| Hugin vLLM (REDACTED_HUGIN_IP:8000) | Serves QwQ-32B-AWQ | Shut down (container stopped) |
| Odin (Munin, :8080) | Routes requests to backends | Config change: remove `hugin-vllm` backend, add model to `hugin` backend, update routing rules |
| MCP Server (Munin, stdio) | Calls Odin endpoints | No change (model-agnostic) |
| Mimir, Muninn, Huginn | Memory, retrieval, indexing | No change |

## Files to Modify

### 1. Odin Config: `configs/odin/node.yaml`

**Current state:**
```yaml
backends:
  - name: "munin"
    url: "http://localhost:11434"
    backend_type: "ollama"
    models:
      - "qwen3-coder:30b-a3b-q4_K_M"
      - "qwen3-embedding"
    max_concurrent: 2
  - name: "hugin-vllm"
    url: "http://REDACTED_HUGIN_IP:8000"
    backend_type: "openai"
    models:
      - "Qwen/QwQ-32B-AWQ"
    max_concurrent: 2
  - name: "hugin"
    url: "http://REDACTED_HUGIN_IP:11434"
    backend_type: "ollama"
    models:
      - "qwen3-embedding"
    max_concurrent: 2

routing:
  default_model: "qwen3-coder:30b-a3b-q4_K_M"
  rules:
    - intent: "coding"
      model: "qwen3-coder:30b-a3b-q4_K_M"
      backend: "munin"
    - intent: "reasoning"
      model: "Qwen/QwQ-32B-AWQ"
      backend: "hugin-vllm"
    - intent: "home_automation"
      model: "Qwen/QwQ-32B-AWQ"
      backend: "hugin-vllm"
```

**Target state:**
```yaml
backends:
  - name: "munin"
    url: "http://localhost:11434"
    backend_type: "ollama"
    models:
      - "qwen3-coder:30b-a3b-q4_K_M"
      - "qwen3-embedding"
    max_concurrent: 2
  - name: "hugin"
    url: "http://REDACTED_HUGIN_IP:11434"
    backend_type: "ollama"
    models:
      - "qwen3-coder-next"
      - "qwen3-embedding"
    max_concurrent: 2

routing:
  default_model: "qwen3-coder:30b-a3b-q4_K_M"
  rules:
    - intent: "coding"
      model: "qwen3-coder:30b-a3b-q4_K_M"
      backend: "munin"
    - intent: "reasoning"
      model: "qwen3-coder-next"
      backend: "hugin"
    - intent: "home_automation"
      model: "qwen3-coder-next"
      backend: "hugin"
```

**Changes explained:**
- The `hugin-vllm` backend entry is removed entirely (no more OpenAI-type backend).
- The existing `hugin` backend gains `qwen3-coder-next` in its models list (alongside existing `qwen3-embedding`).
- Routing rules for `reasoning` and `home_automation` change from `Qwen/QwQ-32B-AWQ` / `hugin-vllm` to `qwen3-coder-next` / `hugin`.
- `max_concurrent: 2` is preserved on `hugin` -- MoE inference is lighter than dense, but two concurrent requests on 64GB with a ~48GB model leaves little headroom for a second model load. Monitor and increase if tok/s holds under concurrency.

### 2. MCP Server Config: `configs/mcp-server/config.yaml`

**No changes required.** The MCP server config contains `odin_url`, `muninn_url`, `timeout_secs`, and `ha`. It does not reference backend names or model names. The `generate` tool passes an optional `model` parameter to Odin, and `list_models` queries Odin's `/v1/models` endpoint. Both will pick up the new model automatically after Odin restarts.

## Implementation Plan

All steps are executed by the `infra-devops` agent unless otherwise noted.

### Phase 1: Pre-flight Checks (Hugin)

1. **SSH to Hugin** and verify current state:
   ```bash
   sshpass -p 723559 ssh jhernandez@REDACTED_HUGIN_IP
   ```

2. **Check Ollama is running and accessible:**
   ```bash
   systemctl status ollama
   ollama list
   ```
   Expected: `qwen3-embedding:latest` is listed. Ollama is active.

3. **Check if `qwen3-coder-next` is available in Ollama library:**
   ```bash
   ollama show qwen3-coder-next --modelfile 2>&1 || echo "MODEL NOT YET AVAILABLE"
   ```
   If the model is not in the Ollama library, this sprint is **BLOCKED**. The model name may differ from `qwen3-coder-next` -- check the Ollama model library for the exact tag. Possible alternative names:
   - `qwen3-coder-next`
   - `qwen3-coder-next:q4_K_M`
   - `qwen3-coder-next:80b-a3b`

   **IMPORTANT:** Confirm the exact model tag before proceeding. The model name used in `ollama pull` must exactly match the model name used in `node.yaml`.

4. **Check available memory:**
   ```bash
   free -h
   ollama ps
   ```
   Verify at least 55GB is available for model loading (total minus OS and services).

### Phase 2: Model Swap (Hugin)

5. **Stop vLLM container:**
   ```bash
   cd /home/jhernandez/vllm && docker compose down
   ```
   Verify port 8000 is freed:
   ```bash
   ss -tlnp | grep 8000
   ```

6. **Pull Qwen3-Coder-Next via Ollama:**
   ```bash
   ollama pull qwen3-coder-next
   ```
   This will download ~46GB. Monitor progress. If a specific quant tag is needed:
   ```bash
   ollama pull qwen3-coder-next:q4_K_M
   ```

7. **Benchmark the model:**
   ```bash
   ollama run qwen3-coder-next "What is the capital of France?" --verbose 2>&1 | tail -5
   ```
   Record:
   - `eval rate` (must be >= 5 tok/s to proceed)
   - `total duration`
   - `load duration`

   If eval rate < 5 tok/s, evaluate whether to proceed or abort.

8. **Verify memory footprint:**
   ```bash
   ollama ps
   free -h
   ```
   Confirm total resident model memory is <= 55GB.

### Phase 3: Odin Config Update (Dev Machine)

9. **Back up current config:**
   ```bash
   cp /home/jesus/Documents/HardwareSetup/yggdrasil/configs/odin/node.yaml \
      /home/jesus/Documents/HardwareSetup/yggdrasil/configs/odin/node.yaml.bak.013
   ```

10. **Edit `configs/odin/node.yaml`** to match the target state defined above in "Files to Modify". Use the exact model tag confirmed in step 3.

### Phase 4: Deploy Config and Restart Odin (Munin)

11. **Copy updated config to Munin:**
    ```bash
    sshpass -p 723559 scp /home/jesus/Documents/HardwareSetup/yggdrasil/configs/odin/node.yaml \
      jhernandez@REDACTED_MUNIN_IP:/etc/yggdrasil/odin/node.yaml
    ```

12. **Restart Odin:**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "sudo systemctl restart yggdrasil-odin"
    ```

13. **Verify Odin started cleanly:**
    ```bash
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "sudo systemctl status yggdrasil-odin"
    sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "journalctl -u yggdrasil-odin --since '1 min ago' --no-pager"
    ```
    Check logs for:
    - No warnings about unknown backend names
    - No warnings about unresolved models
    - Successful binding to `0.0.0.0:8080`

### Phase 5: End-to-End Verification

14. **Health check:**
    ```bash
    curl -s http://REDACTED_MUNIN_IP:8080/health | jq .
    ```

15. **Model listing:**
    ```bash
    curl -s http://REDACTED_MUNIN_IP:8080/v1/models | jq .
    ```
    Verify:
    - `qwen3-coder-next` appears (from `hugin` backend)
    - `qwen3-coder:30b-a3b-q4_K_M` appears (from `munin` backend)
    - `Qwen/QwQ-32B-AWQ` does **NOT** appear

16. **Test reasoning routing:**
    ```bash
    curl -s http://REDACTED_MUNIN_IP:8080/v1/chat/completions \
      -H "Content-Type: application/json" \
      -d '{
        "messages": [{"role": "user", "content": "Explain why Rust uses ownership instead of garbage collection"}],
        "stream": false
      }' | jq '.model, .choices[0].message.content[:200]'
    ```
    Verify the response model field is `qwen3-coder-next` (reasoning intent keywords: "explain", "why").

17. **Test explicit model selection:**
    ```bash
    curl -s http://REDACTED_MUNIN_IP:8080/v1/chat/completions \
      -H "Content-Type: application/json" \
      -d '{
        "model": "qwen3-coder-next",
        "messages": [{"role": "user", "content": "Hello"}],
        "stream": false
      }' | jq '.model'
    ```
    Verify explicit model routing works.

18. **Test MCP generate tool** (via Claude Code or direct MCP stdio interaction):
    - Call `list_models` tool -- should show `qwen3-coder-next`
    - Call `generate` tool with a reasoning prompt -- should route to `qwen3-coder-next` and respond faster than the old QwQ baseline

19. **Test home_automation routing** (if HA is configured):
    ```bash
    curl -s http://REDACTED_MUNIN_IP:8080/v1/chat/completions \
      -H "Content-Type: application/json" \
      -d '{
        "messages": [{"role": "user", "content": "Turn on the living room light"}],
        "stream": false
      }' | jq '.model'
    ```
    Verify model is `qwen3-coder-next`.

## Acceptance Criteria

- [ ] vLLM container on Hugin is stopped and port 8000 is free
- [ ] `qwen3-coder-next` is pulled and available via `ollama list` on Hugin
- [ ] `ollama run qwen3-coder-next --verbose` shows eval rate >= 5 tok/s
- [ ] Total model memory on Hugin (qwen3-coder-next + qwen3-embedding) <= 55GB resident
- [ ] `hugin-vllm` backend is removed from `configs/odin/node.yaml`
- [ ] `qwen3-coder-next` is listed in the `hugin` backend's models array
- [ ] Routing rules for `reasoning` and `home_automation` point to `qwen3-coder-next` on `hugin`
- [ ] Odin restarts cleanly with no routing warnings in logs
- [ ] `GET /v1/models` returns `qwen3-coder-next` and does not return `Qwen/QwQ-32B-AWQ`
- [ ] A reasoning-classified prompt routes to `qwen3-coder-next` and returns a valid response
- [ ] A home_automation-classified prompt routes to `qwen3-coder-next` and returns a valid response
- [ ] Explicit `"model": "qwen3-coder-next"` in request body routes correctly
- [ ] MCP `list_models` tool shows the new model
- [ ] MCP `generate` tool works with the new model

## Dependencies

| Dependency | Type | Status |
|-----------|------|--------|
| Ollama library has `qwen3-coder-next` | External | **MUST VERIFY** -- model may not yet be published |
| Hugin Ollama service running | Infrastructure | Confirmed running (systemd, bound to 0.0.0.0:11434) |
| Odin deployed on Munin | Infrastructure | Confirmed running (Sprint 011) |
| 64GB RAM on Hugin | Hardware | Confirmed (NetworkHardware.md) |
| Sprint 011 (Hardening) | Sprint | DONE -- Odin systemd unit and deploy scripts in place |

## Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| `qwen3-coder-next` not yet in Ollama library | Medium | Sprint blocked | Check Ollama library before starting. If unavailable, defer sprint. Alternative: create a custom Modelfile from GGUF weights if available on HuggingFace. |
| Model does not fit in 64GB alongside qwen3-embedding | Low | Sprint blocked | Try smaller quant (Q3_K_M, IQ4_XS). If still too large, consider dropping qwen3-embedding from Hugin (Munin can handle all embedding). |
| tok/s does not meet 5 tok/s target | Low | Sprint fails acceptance | MoE with 3B active params should be fast on 16-thread Zen 5. If not, investigate Ollama thread settings (`OLLAMA_NUM_THREADS`). Worst case: revert to vLLM + QwQ-32B-AWQ. |
| Ollama OOM-kills under concurrent load | Medium | Service instability | Set `max_concurrent: 1` on the `hugin` backend if memory is tight. Monitor with `dmesg` and `journalctl -k`. |
| Exact model tag differs from `qwen3-coder-next` | Medium | Config mismatch | Confirm exact tag from `ollama list` output after pull. Use that exact string in `node.yaml`. The model name in config must be byte-for-byte identical to what Ollama reports. |
| Odin config test in `ygg-domain` references `hugin-vllm` | Low | Test failure | The unit test in `config.rs` (line 276) uses a JSON fixture with `hugin-vllm`. This test validates deserialization, not runtime routing, and is not affected by YAML config changes. No code change needed. |

## Rollback Plan

1. **Restore Odin config:**
   ```bash
   cp /home/jesus/Documents/HardwareSetup/yggdrasil/configs/odin/node.yaml.bak.013 \
      /home/jesus/Documents/HardwareSetup/yggdrasil/configs/odin/node.yaml
   sshpass -p 723559 scp configs/odin/node.yaml jhernandez@REDACTED_MUNIN_IP:/etc/yggdrasil/odin/node.yaml
   sshpass -p 723559 ssh jhernandez@REDACTED_MUNIN_IP "sudo systemctl restart yggdrasil-odin"
   ```

2. **Restart vLLM on Hugin:**
   ```bash
   sshpass -p 723559 ssh jhernandez@REDACTED_HUGIN_IP "cd /home/jhernandez/vllm && docker compose up -d"
   ```

3. **Verify rollback:**
   ```bash
   curl -s http://REDACTED_MUNIN_IP:8080/v1/models | jq .
   ```
   Confirm `Qwen/QwQ-32B-AWQ` is back.

Total rollback time: < 5 minutes. No data is lost. The `qwen3-coder-next` weights can remain on Hugin's disk for future use.

## Decision Log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-03-09 | Use Ollama instead of vLLM for Qwen3-Coder-Next | Ollama is already deployed and running on Hugin. Eliminates Docker container dependency. Single runtime simplifies operations. |
| 2026-03-09 | Remove `hugin-vllm` backend entirely rather than repurpose it | The OpenAI-type backend was only needed for vLLM. With Ollama serving both models on Hugin, the existing `hugin` backend entry absorbs the new model. Cleaner config, fewer moving parts. |
| 2026-03-09 | Keep `max_concurrent: 2` on `hugin` backend | MoE inference uses only 3B active params, so concurrent requests are feasible. However, the ~48GB model weight footprint leaves ~12GB for OS + embedding + second inference context. Monitor and reduce to 1 if OOM occurs. |
| 2026-03-09 | No Rust code changes | `BackendType::Ollama` routing, `SemanticRouter`, and `resolve_backend_for_model` all work with any model name string. The config structs accept arbitrary model names. The OpenAI-type backend code path remains in the codebase but is simply unused (no `openai` backend in config). |
| 2026-03-09 | Do not delete QwQ-32B-AWQ weights or vLLM Docker images | Zero cost to keep them on disk. Enables fast rollback. Cleanup can be a future housekeeping task. |
| 2026-03-09 | MCP server config unchanged | MCP server is model-agnostic -- it calls Odin endpoints. The `generate` tool's optional `model` parameter is passed through to Odin, which handles routing. No config coupling to backend model names. |
