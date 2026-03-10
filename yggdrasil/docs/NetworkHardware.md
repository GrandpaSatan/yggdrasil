# Yggdrasil Network Hardware Reference

Canonical source: `/home/jesus/Documents/HardwareSetup/NetworkHardware.md`
This file extracts the hardware targets relevant to the Yggdrasil workspace.

---

## Munin (Primary Deployment Target for Mimir, Odin)

- **IP:** REDACTED_MUNIN_IP
- **OS:** Ubuntu Server 25.10
- **CPU:** Intel Core Ultra 185H (6P + 8E + 2LP cores, 16 threads)
- **GPU:** Intel ARC iGPU (used by Ollama)
- **RAM:** 48GB DDR5
- **Network:** 2x 5Gb Ethernet
- **Runs:** Ollama (qwen3 14b, qwen3-embedding), Whisper, Mimir, Odin
- **SSH:** jhernandez / 723559

## Hades (Database Host)

- **IP:** REDACTED_HADES_IP
- **OS:** TrueNAS Scale 25.04.2.6
- **CPU:** Intel N150 (4 cores, low power)
- **GPU:** Intel iGPU (unused)
- **RAM:** 32GB SODIMM DDR5
- **Storage Pools:**
  - Merlin: 444 GiB (SATA SSD) -- PostgreSQL, pgadmin, Qdrant
  - Condor: 11.77 TiB (HDD) -- Media
  - OWL: 14.4 TiB (HDD) -- Personal/Games
  - RAVEN: 2.63 TiB (SSD, no redundancy) -- High speed scratch
- **Services:**
  - PostgreSQL: `postgres://jhernandez:K6m4B129CF9u@REDACTED_HADES_IP:5432/postgres`
  - Qdrant: `http://REDACTED_HADES_IP:6334` (gRPC)

## Thor (Compute Workhorse -- On-Demand)

- **IP:** REDACTED_THOR_IP
- **OS:** Proxmox
- **CPU:** AMD Ryzen Threadripper 2990WX (32 cores, 64 threads)
- **GPUs:** RTX 2070 Super (CC 7.5), RTX 5070 (CC 12.0), RTX 3060 12GB (CC 8.6)
- **RAM:** 128GB
- **VM: Morrigan** (REDACTED_THOR_IP0) -- 32 cores, 96GB RAM, RTX 5070 + RTX 3060 12GB
  - Training workloads only. Not used for Yggdrasil services.

## Hugin (Experimental AI)

- **IP:** REDACTED_HUGIN_IP
- **OS:** Ubuntu Server 25.10
- **CPU:** AMD Ryzen 7 255
- **GPU:** iGPU
- **RAM:** 64GB DDR5
- **Runs:** Experimental AI workloads

---

Last updated: 2026-03-09
