# Yggdrasil Architecture

## Overview
Centralized configuration management system with cross-platform support (Linux/Windows) and multi-workstation memory consolidation.

## Core Components

### 1. Centralized Configuration
- **Location:** Munin node
- **Storage:** rsync + symlinks
- **Sync Mechanism:** Automated sync from workstations to central config

### 2. Cross-Platform Support
- **Linux/Windows:** Unified configuration management
- **Tooling:** Platform-specific handlers for consistent behavior

### 3. Multi-Workstation Memory Merge
- **Consolidation:** Centralized memory merging across workstations
- **Synchronization:** Automated sync of local changes to central repository

## Key Concepts

### Configuration Management
- Config files stored on Munin node
- Workstations maintain local symlinks to central configuration
- rsync ensures consistency between nodes

### Memory Consolidation
- Local memory per workstation
- Centralized merging mechanism
- Cross-platform compatibility for memory data

## Architecture Layers

### 1. Client Layer (Workstations)
- Local configuration management
- Memory storage and retrieval
- Platform-specific handlers

### 2. Server Layer (Munin)
- Central configuration repository
- Memory consolidation point
- Sync coordination

### 3. Communication Layer
- SSH-based sync mechanism
- rsync for file synchronization
- stdio for local tool communication

## File Structure
```
/config/
  /central/          # Centralized config files
  /workstations/     # Per-workstation overrides
/memory/
  /local/            # Local memory per workstation  
  /central/          # Consolidated central memory
```

## Sprint 064 Changes

## store_gate

```toml
[store_gate]
model = "LFM2.5-1.2B-Instruct"
primary = "munin"
secondary = "hugin"
timeout = 5000
keep_alive = 10000
feedback_suggest_alternatives_first = true
```

## keep_warm

```toml
[keep_warm]
models = ["glm-4.7-flash","LFM2.5","nemotron","saga","gemma4:e4b","RWKV-7"]
interval = 540
keep_alive = 10000
```

## Sprint 065 Changes

### SDR Query API — Tag Partition

`mimir::sdr_index::query` (and the project-scoped variant) now accepts an optional `tag_filter: Vec<String>` parameter. When provided, candidates whose tag set does not intersect the filter are excluded before the novelty gate runs. The store handler builds the filter from `body.tags` filtered to known partition prefixes (`sprint:`, `project:`, `incident:`). Engrams with different `sprint:NNN` tags are now guaranteed to never merge regardless of SDR similarity — verified post-deploy with paired probes on `sprint:991` / `sprint:992`.

### Secrets Management (partial — Track D deferred)

The `{{secret:NAME}}` substitution primitive (Sprint 064 P7) is in place; flows can declare `secrets:` blocks that resolve against the Mimir vault at `FlowEngine::execute` time. Operator-side migration for `GITEA_PASSWORD`, `HA_TOKEN`, and `BRAVE_SEARCH_API_KEY` — setting them via the Vault panel and removing them from `vault.conf` drop-in + `/opt/yggdrasil/.env` — is deferred. These three secrets still live in systemd env today.

### `ygg-dreamer` Crate (Track C)

New idle-polling daemon running as `yggdrasil-dreamer.service` on Munin. Polls `odin:/internal/activity` for fleet idleness, triggers `dream_*` flows during idle windows, persists resulting engrams. Config at `/opt/yggdrasil/config/dreamer.config.json`; systemd unit installed via path-watcher auto-restart.

## Sprint 066 Changes

## Test Topology
Yggdrasil now has a single E2E harness (`yggdrasil/tests-e2e/`) rather than ~50 scattered in-crate test modules. Every gate touches real services; no mocks at the service boundary. Trade-off: tests require live fleet to run; mitigated by session-cached service probe and explicit `required_services` markers.

## Audit as Executable Spec
VULN/FLAW findings are now strict-xfail tests rather than static documents. When a vuln is fixed, the test flips from XFAIL to XPASS and CI forces the maintainer to remove the marker. This keeps the audit roadmap synchronized with the code without manual tracking.