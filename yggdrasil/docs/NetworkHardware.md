# Yggdrasil Network Hardware Reference

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



# This contains an inventory of relevant network hardware
TYPE: HARDWARE 
OS: UBUNTU SERVER 25.10 
NAME: munin
PURPOSE: LOW POWER AI TRANSLATION 
HARDWARE:
    CPU: Intel Core 9 Ultra 185H
    GPU: ARC iGPU
    RAM: 48GB DDR5
     
IP: REDACTED_MUNIN_IP
RUNS: [Ollama (qwen3 14b), Whisper] 
NETWORK: 2x 5GB Ethernet 
SSH:
    USER: jhernandez
    PASS: 723559

TYPE: HARDWARE
OS: UBUNTU SERVER 25.10
NAME: hugin
PURPOSE: AI WORKLOADS
HARDWARE:
    CPU: AMD Ryzen 7 255
    GPU: iGPU 
    RAM: 64GB DDR5
IP: REDACTED_HUGIN_IP 
RUNS: [Experimental AI]
SSH:
    USER: jhernandez
    PASS: 723559

TYPE: HARDWARE
OS: Proxmox
NAME: thor
PURPOSE: COMPUTE WORKHORSE (only turns on when needed)
HARDWARE:
    CPU: AMD Ryzen Threadripper 2990wx
    GPU(s): RTX 2070 Super (CC 7.5), RTX 5070 (CC 12.0), RTX 3060 12GB (CC 8.6)
    RAM: 128 GB
IP: REDACTED_THOR_IP
RUNS: [Gaming VMs, AI Training Workloads]
    Virtual Machine(s):
    VMID:100 - FergusVM
       NAME: morrigan
        CPU: 32 Cores
        GPU(s): RTX 2070 Super (excluded, pre-Ampere), RTX 5070 (Blackwell), RTX 3060 12GB
        RAM: 96GB
         OS: Ubuntu 25.10
         IP: REDACTED_THOR_IP0 - SSHKEY configured.
        STORAGE:
            /     : nvme1 SSD 116GB (OS + system)
            /data : nvme0 SSD 117GB (fast working storage, Rust binaries)
            /barn : HDD 2.9TB (training data backups, checkpoints, datasets)
        NOTE: /barn replaces the former /models mount. All training data and backups live here.
    NOTE: Training scripts auto-select Ampere+ GPUs (5070 + 3060). 2070 Super excluded.

TYPE: HARDWARE
OS: Proxmox
NAME: plume
PURPOSE: LowPower Media Server 
HARDWARE:
    CPU:  AMD Ryzen 5 PRO 4650GE with 
    GPU(s):
        : iGPU Radeon Graphics
        : Intel ARC A380 
    RAM: 48GB
IP: REDACTED_PLUME_IP  
RUNS: [Containers and VMS]
    Container(s):
    ID: 100 - gitea 
       CPU: 1 Core x86-64
       GPU: none
       RAM: 2GB
        OS: Debian
        IP: REDACTED_GITEA_IP - SSHKEY configured.
      APPS: gitea
    ID: 102 - peckhole
       CPU: 1 Core(s)
       GPU: none
       RAM: 2GB
        OS: Debian
        IP: REDACTED_GITEA_IP - SSHKEY configured.
      APPS: PiHole, Unbound, Nginx
    Virtual Machine(s):
    VMID: 101 - nightjar
       CPU: 4 Cores x86-64
       GPU: INTEL ARC A380 6GB 
       RAM: 16GB/24GB - Ballon enabled
        OS: Debian
        IP: REDACTED_NIGHTJAR_IP
       SSH: jhernandez : 723559
      APPS: Jellyfin, Radarr, sonarr, qbittorrent, ripper (self built app) , prowlarr, flaresolver, bazarr, lingarr, searxng, redis-searxng, openwebUI
    VMID: 103 - chirp
       CPU: 2 Cores x86-64
       GPU: none
       RAM: 4GB
        OS: HomeAssistant
        IP: REDACTED_CHIRP_IP
       SSH: jhernandez : OhTQge4bp69EySDI
      APPS: Home Assistant

TYPE: HARDWARE 
OS: TrueNAS Scale ( 25.04.2.6 )
NAME: hades
PURPOSE: DB, Media and Personal Storage
HARDWARE:
    CPU: Intel N150 
    GPU: Intel iGPU
    RAM: 32gb SODIMM DDR5
IP: REDACTED_HADES_IP
DATA POOLS:
    Condor: 11.77 TiB available (Media Storage) (2 HDD)
    Merlin: 444.32 GiB available (pgadmin, postgre, qrant) (3 SATA SSD)
    OWL: 14.4 TiB (Personal Storage and Games) (6 HDD)
    RAVEN: 2.63 TiB high (high speed high risk no redundancy) (2 SSD)

TYPE: HARDWARE
OS: None (yet)
NAME: None (yet)
PURPOSE: Orchestrator (if it fits and runs)
HARDWARE: Raspberry Pi 4

TYPE: Container
    HOSTNAME: cerberus (Truenas)
CONTAINERNAME: postgre
PURPOSE: ALWAYS ON LAB DB - LOW POWER
IP: REDACTED_HADES_IP
USER: jhernandez
PASS: K6m4B129CF9u

TYPE: Container
    HOSTNAME: cerberus (Truenas)
CONTAINERNAME:qdrant (replace pgvector with this)
PURPOSE: AI RAG AND OTHER
IP: REDACTED_HADES_IP 
NOTE: I have never used this before, no idea if it's configured yet. Please ensure AI is using this. 








