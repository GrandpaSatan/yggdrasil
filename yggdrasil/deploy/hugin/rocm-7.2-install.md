# Hugin ROCm 7.2 + vLLM baseline install runbook (Sprint 065 Track B·P1)

**Target:** Hugin 10.0.65.9 — AMD Ryzen AI 9 HX 370 (Radeon 890M iGPU gfx1150, RX 9060 XT eGPU gfx1200, XDNA2 NPU, 60 GB RAM)

**Goal:** install ROCm 7.2 userspace + Docker + pull `rocm/vllm-dev:main-gfx1150-gfx1200` WITHOUT disrupting the currently-running Ollama on `:11434` (which uses the Vulkan backend and is independent of ROCm userspace).

## Preflight

```bash
ssh hugin 'uname -r'                    # expect ≥ 6.11 (sprint baseline is 6.17)
ssh hugin 'systemctl is-active ollama'  # expect active — soak needs it live
ssh hugin 'curl -s http://127.0.0.1:11434/api/tags | head -c 80'
```

## Install ROCm 7.2

```bash
ssh hugin
wget https://repo.radeon.com/amdgpu-install/7.2/ubuntu/jammy/amdgpu-install_7.2.xx-1_all.deb
sudo apt install ./amdgpu-install_*.deb
sudo amdgpu-install --usecase=rocm,hiplibsdk --no-dkms
sudo usermod -a -G render,video $USER
# re-login to pick up groups
```

## Verify

```bash
rocminfo | grep -E "gfx1150|gfx1200"
# expect two Agent entries, one per GPU

ls -la /dev/kfd /dev/dri/renderD128 /dev/dri/renderD129
# expect render group ownership

# Ollama parity — must still work post-install (Vulkan path independent of ROCm)
curl -s http://127.0.0.1:11434/api/tags | jq '.models | length'
```

## Pull vLLM image

```bash
docker pull rocm/vllm-dev:main-gfx1150-gfx1200
docker pull ghcr.io/huggingface/text-embeddings-inference:cpu-1.5
```

## XDNA2 NPU (optional — not used Sprint 065)

```bash
modinfo amdxdna
# if missing, on newer kernels:
# sudo apt install amdxdna-dkms
```

## Rollback

```bash
# If ROCm 7.2 breaks Ollama's Vulkan path (unlikely but possible):
sudo amdgpu-install --uninstall
# OR pin the previous ROCm version:
# sudo apt install rocm-core=<previous-version>
```

## Sanity

After install complete, deploy:
1. `deploy/hugin/llama-swap/` — see `config.yaml` + `yggdrasil-llama-swap.service`
2. `deploy/hugin/tei/` — docker-compose + `yggdrasil-tei.service`
3. `deploy/hugin/ai00-server/` — `config.toml` + `yggdrasil-ai00.service`

All three should bind to distinct ports; they live alongside (do NOT replace) the Ollama daemon until B·P7 dual-serve soak proves parity.
