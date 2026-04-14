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