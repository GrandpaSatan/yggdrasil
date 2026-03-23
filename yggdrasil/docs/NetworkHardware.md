# Yggdrasil Network Hardware Reference

## Munin (Primary Deployment Target for Mimir, Odin)

- **IP:** `<munin-ip>` (VLAN 65 — compute)
- **OS:** Ubuntu Server 25.10
- **CPU:** Intel Core Ultra 185H (6P + 8E + 2LP cores, 16 threads)
- **iGPU:** Intel ARC Graphics (Xe-LPG, NO XMX — not suitable for LLM inference)
- **eGPU:** AMD Radeon RX 9060 XT 16GB (USB4/Thunderbolt, RDNA 4)
- **RAM:** 48GB DDR5
- **Network:** 2x 5Gb Ethernet
- **Runs:** Odin (port 8080), Mimir (port 9090), MCP Remote (port 9093), Sentinel, ygg-node
- **SSH:** key-based authentication required (`yggdrasil@munin`)

### Power Management (Laptop Server Mode)

Munin is a laptop running as a headless server. Suspend/hibernate is fully disabled at three levels:
1. **logind.conf override** (`/etc/systemd/logind.conf.d/99-yggdrasil-nosuspend.conf`): Ignores lid switch, power key, and idle actions
2. **sleep.conf override** (`/etc/systemd/sleep.conf.d/99-yggdrasil-nosleep.conf`): Disables all sleep states (suspend, hibernate, hybrid)
3. **Masked targets**: `sleep.target`, `suspend.target`, `hibernate.target`, `hybrid-sleep.target` are masked

**Safety net:** `yggdrasil-resume.service` (oneshot, `WantedBy=suspend.target`) automatically restarts all Yggdrasil services in dependency order if a resume somehow occurs. Recovery script: `/opt/yggdrasil/bin/yggdrasil-resume.sh`.

**Emergency shutdown:** Long-press power button (5s) still triggers poweroff for hardware failure scenarios.

## Hades (Database Host)

- **IP:** `<hades-ip>`
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
  - PostgreSQL: **MOVED TO MUNIN** (pgvector/pgvector:pg16 container, localhost:5432/yggdrasil — TrueNAS lacked pgvector)
  - Qdrant: `http://<hades-ip>:6333` (HTTP) / `http://<hades-ip>:6334` (gRPC), collections: engrams_sdr (256-dim dot), code_chunks (384-dim cosine)

## Thor (Compute Workhorse -- On-Demand)

- **IP:** `<thor-ip>`
- **OS:** Proxmox
- **CPU:** AMD Ryzen Threadripper 2990WX (32 cores, 64 threads)
- **GPUs:** RTX 2070 Super (CC 7.5), RTX 5070 (CC 12.0), RTX 3060 12GB (CC 8.6)
- **RAM:** 128GB
- **VM: Morrigan** (`<morrigan-ip>`) -- 32 cores, 96GB RAM, RTX 5070 + RTX 3060 12GB
  - Training workloads only. Not used for Yggdrasil services.

## Hugin (Experimental AI)

- **IP:** `<hugin-ip>`
- **OS:** Ubuntu Server 25.10
- **CPU:** AMD Ryzen 7 255
- **GPU:** iGPU
- **RAM:** 64GB DDR5
- **Runs:** Ollama (qwen3-coder:30b-a3b-q4_K_M), Huginn (code indexer, health port 9092), Muninn (retrieval, port 9091)
- **Note:** 16GB reserved by AMD iGPU VRAM; effective system RAM ~46GB

## Other Infrastructure Nodes

| Node | Role | IP |
|------|------|----|
| Plume | Proxmox host (media/containers) | `<plume-ip>` |
| Nightjar | VM on Plume — Grafana + Prometheus | `<nightjar-ip>` |
| Gitea | Container on Plume — Git server | `<gitea-ip>` |
| Home Assistant (chirp) | VM on Plume — Smart home | `<ha-ip>` |
| Workstation | Dev machine (SSHFS source) | `<workstation-ip>` |

---

## SSH Access

All nodes use key-based SSH authentication. Configure `~/.ssh/config` with appropriate host entries:

```
Host munin
    HostName <munin-ip>
    User your-user
    IdentityFile ~/.ssh/id_ed25519

Host hugin
    HostName <hugin-ip>
    User your-user
    IdentityFile ~/.ssh/id_ed25519
```

---

Last updated: 2026-03-10
