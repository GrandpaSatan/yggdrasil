# AMD ROCm Official Documentation
Source: AMD ROCm repos + docs, community references
Retrieved: 2026-04-07

---

## Installation

### ROCm Quick Start (v7.2.1)

Source: https://rocm.docs.amd.com/projects/install-on-linux/en/latest/install/quick-start.html

#### Prerequisites

- Verify kernel version matches ROCm system requirements
- Remove any AMD GPU driver from a previous installation before proceeding
- Quick Start enables GPU access for the current user only

#### Ubuntu 24.04 / 22.04

```bash
# Download and install the amdgpu-install package
# For 24.04 (Noble):
wget https://repo.radeon.com/amdgpu-install/<version>/ubuntu/noble/amdgpu-install_<version>_all.deb
# For 22.04 (Jammy):
wget https://repo.radeon.com/amdgpu-install/<version>/ubuntu/jammy/amdgpu-install_<version>_all.deb

sudo apt install ./amdgpu-install_<version>_all.deb
sudo apt update

# Install prerequisites
sudo apt install -y python3-setuptools python3-wheel

# Add user to required groups
sudo usermod -a -G render,video $LOGNAME

# Install ROCm
sudo amdgpu-install --usecase=rocm
```

#### RHEL 10.x / 9.x / 8.10

```bash
# Register and update Enterprise Linux first
sudo subscription-manager register
sudo dnf update

# Install amdgpu-install RPM
sudo dnf install https://repo.radeon.com/amdgpu-install/<version>/rhel/<version>/amdgpu-install-<version>.rpm

# Enable EPEL and CodeReady Builder
sudo dnf install epel-release
sudo crb enable

# Install Python dependencies
sudo dnf install python3-setuptools python3-wheel

# Add user to groups
sudo usermod -a -G render,video $LOGNAME

# Install ROCm
sudo amdgpu-install --usecase=rocm
```

#### Oracle Linux 10.1 / 9.7 / 8.10

Similar to RHEL with appropriate repository configuration adjustments.

#### SUSE Linux Enterprise Server 15.7

```bash
# Enable required modules
sudo SUSEConnect --product sle-module-desktop-applications/<version>/x86_64
sudo SUSEConnect --product sle-module-development-tools/<version>/x86_64

# Add science repository
sudo zypper addrepo <science-repo-url>

# Install Python packages
sudo zypper install python3-setuptools python3-wheel

# Add user to groups
sudo usermod -a -G render,video $LOGNAME

# Install ROCm
sudo amdgpu-install --usecase=rocm
```

#### Rocky Linux 9.7

```bash
# Uses DNF with EPEL and CodeReady Builder repositories
sudo dnf install epel-release
sudo crb enable
sudo amdgpu-install --usecase=rocm
```

#### Debian 13 / 12

Same pattern as Ubuntu with appropriate repository selections.

#### Driver Installation (all distros)

```bash
# Install kernel headers and development packages first
# Ubuntu/Debian:
sudo apt install linux-headers-$(uname -r)
# RHEL/Rocky:
sudo dnf install kernel-headers kernel-devel

# Install AMDGPU DKMS driver
sudo amdgpu-install --usecase=dkms
```

#### Post-Installation

- **Reboot required** to apply all settings
- Default group assignment for all future users:

```bash
echo 'ADD_EXTRA_GROUPS=1' | sudo tee -a /etc/adduser.conf
echo 'EXTRA_GROUPS=video' | sudo tee -a /etc/adduser.conf
echo 'EXTRA_GROUPS=render' | sudo tee -a /etc/adduser.conf
```

### Full Installation Example (ROCm 7.1.1 on Ubuntu 24.04, Multi-GPU)

Source: https://github.com/eliranwong/MultiAMDGPU_AIDev_Ubuntu

```bash
# Remove previous installations
amdgpu-install --uninstall
sudo apt remove --purge amdgpu-install

# Install ROCm with all use cases
sudo apt update
sudo apt install -y libstdc++-12-dev
wget https://repo.radeon.com/amdgpu-install/7.1.1/ubuntu/noble/amdgpu-install_7.1.1.70101-1_all.deb
sudo apt install ./amdgpu-install_7.1.1.70101-1_all.deb
sudo amdgpu-install --usecase=graphics,multimedia,rocm,rocmdev,rocmdevtools,lrt,opencl,openclsdk,hip,hiplibsdk,openmpsdk,mllib,mlsdk --no-dkms -y
```

### GRUB Configuration for Multi-GPU

```bash
# Prevent multi-GPU application hangs — add IOMMU passthrough
sudo nano /etc/default/grub
# Change:
#   GRUB_CMDLINE_LINUX_DEFAULT="quiet splash"
# To:
#   GRUB_CMDLINE_LINUX_DEFAULT="quiet splash iommu=pt"
sudo update-grub
sudo reboot
```

### Supported Operating Systems (ROCm 7.2.1)

| OS | Versions |
|----|----------|
| Ubuntu | 22.04, 24.04 |
| RHEL | 8.10, 9.4, 9.6, 9.7, 10.0, 10.1 |
| SLES | 15 SP7 |
| Oracle Linux | 8.10, 9.7, 10.1 |
| Debian | 12, 13 |
| Rocky Linux | 9.7 |
| Azure Linux | Supported |
| CentOS | 7.9 (legacy) |

---

## GPU Architecture & Compatibility

Source: https://rocm.docs.amd.com/en/latest/reference/gpu-arch-specs.html
Source: https://rocm.docs.amd.com/en/latest/compatibility/compatibility-matrix.html

### ROCm 7.2.1 Supported GPU LLVM Targets

| Architecture | LLVM Target | Support Level |
|-------------|-------------|---------------|
| CDNA4 | gfx950 | Compute (Instinct) |
| CDNA3 | gfx942 | Compute (Instinct) |
| CDNA2 | gfx90a | Compute (Instinct) |
| CDNA | gfx908 | Compute (Instinct) |
| RDNA4 | gfx1201 | Compute (Radeon RX 9070 series) |
| RDNA4 | gfx1200 | Compute (Radeon RX 9060 series) |
| RDNA3 | gfx1100 | Compute (Radeon RX 7900 series, PRO W7900/W7800) |
| RDNA3 | gfx1101 | Compute (Radeon RX 7800/7700 series, PRO W7700) |
| RDNA2 | gfx1030 | Compute (select models) |

### AMD Instinct GPUs (Data Center)

| GPU | Architecture | LLVM Target | VRAM | Compute Units | Wavefront | L2 Cache | L3 Cache |
|-----|-------------|-------------|------|---------------|-----------|----------|----------|
| MI355X | CDNA4 | gfx950 | 288GB | 256 | 64 | 32MB | 256MB |
| MI350X | CDNA4 | gfx950 | 288GB | 256 | 64 | 32MB | 256MB |
| MI325X | CDNA3 | gfx942 | 256GB | 304 | 64 | 32MB | 256MB |
| MI300X | CDNA3 | gfx942 | 192GB | 304 | 64 | 32MB | 256MB |
| MI300A | CDNA3 | gfx942 | 128GB | 228 | 64 | 24MB | 256MB |
| MI250X | CDNA2 | gfx90a | 128GB | 220 | 64 | 16MB | 16MB |
| MI250 | CDNA2 | gfx90a | 128GB | 208 | 64 | 16MB | 16MB |
| MI210 | CDNA2 | gfx90a | 64GB | 104 | 64 | 16MB | 8MB |
| MI100 | CDNA | gfx908 | 32GB | 120 | 64 | 16MB | 8MB |
| MI60 | GCN5.1 | gfx906 | 32GB | 64 | 64 | 16MB | 4MB |
| MI50 (32GB) | GCN5.1 | gfx906 | 32GB | 60 | 64 | 16MB | 4MB |
| MI50 (16GB) | GCN5.1 | gfx906 | 16GB | 60 | 64 | 16MB | 4MB |
| MI25 | GCN5.0 | gfx900 | 16GB | 64 | 64 | 16MB | 4MB |
| MI8 | GCN3.0 | gfx803 | 4GB | 64 | 64 | 16MB | 2MB |
| MI6 | GCN4.0 | gfx803 | 16GB | 36 | 64 | 16MB | 2MB |

### AMD Radeon PRO GPUs (Workstation)

| GPU | Architecture | LLVM Target | VRAM | Compute Units | Wavefront | L2 Cache | Infinity Cache |
|-----|-------------|-------------|------|---------------|-----------|----------|----------------|
| Radeon AI PRO R9700 | RDNA4 | gfx1201 | 32GB | 64 | 32/64 | 8MB | 64MB |
| Radeon AI PRO R9600D | RDNA4 | gfx1201 | 32GB | 48 | 32/64 | 8MB | 48MB |
| Radeon PRO V710 | RDNA3 | gfx1101 | 28GB | 54 | 32/64 | 4MB | 56MB |
| Radeon PRO W7900 Dual Slot | RDNA3 | gfx1100 | 48GB | 96 | 32/64 | 6MB | 96MB |
| Radeon PRO W7900 | RDNA3 | gfx1100 | 48GB | 96 | 32/64 | 6MB | 96MB |
| Radeon PRO W7800 48GB | RDNA3 | gfx1100 | 48GB | 70 | 32/64 | 6MB | 96MB |
| Radeon PRO W7800 | RDNA3 | gfx1100 | 32GB | 70 | 32/64 | 6MB | 64MB |
| Radeon PRO W7700 | RDNA3 | gfx1101 | 16GB | 48 | 32/64 | 4MB | 64MB |
| Radeon PRO W6800 | RDNA2 | gfx1030 | 32GB | 60 | 32/64 | 4MB | 128MB |
| Radeon PRO W6600 | RDNA2 | gfx1032 | 8GB | 28 | 32/64 | 2MB | 32MB |
| Radeon PRO V620 | RDNA2 | gfx1030 | 32GB | 72 | 32/64 | 4MB | 128MB |
| Radeon Pro W5500 | RDNA | gfx1012 | 8GB | 22 | 32/64 | 128MB | 4MB |
| Radeon Pro VII | GCN5.1 | gfx906 | 16GB | 60 | 64 | 16MB | 4MB |

### AMD Radeon GPUs (Consumer)

| GPU | Architecture | LLVM Target | VRAM | Compute Units | Wavefront | L2 Cache | Infinity Cache |
|-----|-------------|-------------|------|---------------|-----------|----------|----------------|
| Radeon RX 9070 XT | RDNA4 | gfx1201 | 16GB | 64 | 32/64 | 8MB | 64MB |
| Radeon RX 9070 GRE | RDNA4 | gfx1201 | 16GB | 48 | 32/64 | 6MB | 48MB |
| Radeon RX 9070 | RDNA4 | gfx1201 | 16GB | 56 | 32/64 | 8MB | 64MB |
| Radeon RX 9060 XT LP | RDNA4 | gfx1200 | 16GB | 32 | 32/64 | 4MB | 32MB |
| **Radeon RX 9060 XT** | **RDNA4** | **gfx1200** | **16GB** | **32** | **32/64** | **4MB** | **32MB** |
| Radeon RX 9060 | RDNA4 | gfx1200 | 8GB | 28 | 32/64 | 4MB | 32MB |
| Radeon RX 7900 XTX | RDNA3 | gfx1100 | 24GB | 96 | 32/64 | 6MB | 96MB |
| Radeon RX 7900 XT | RDNA3 | gfx1100 | 20GB | 84 | 32/64 | 6MB | 80MB |
| Radeon RX 7900 GRE | RDNA3 | gfx1100 | 16GB | 80 | 32/64 | 6MB | 64MB |
| Radeon RX 7800 XT | RDNA3 | gfx1101 | 16GB | 60 | 32/64 | 4MB | 64MB |
| Radeon RX 7700 | RDNA3 | gfx1101 | 16GB | 40 | 32/64 | 4MB | 64MB |
| Radeon RX 7700 XT | RDNA3 | gfx1101 | 12GB | 54 | 32/64 | 4MB | 48MB |
| Radeon RX 7600 | RDNA3 | gfx1102 | 8GB | 32 | 32/64 | 2MB | 32MB |
| Radeon RX 6950 XT | RDNA2 | gfx1030 | 16GB | 80 | 32/64 | 4MB | 128MB |
| Radeon RX 6900 XT | RDNA2 | gfx1030 | 16GB | 80 | 32/64 | 4MB | 128MB |
| Radeon RX 6800 XT | RDNA2 | gfx1030 | 16GB | 72 | 32/64 | 4MB | 128MB |
| Radeon RX 6800 | RDNA2 | gfx1030 | 16GB | 60 | 32/64 | 4MB | 128MB |
| Radeon RX 6750 XT | RDNA2 | gfx1031 | 12GB | 40 | 32/64 | 3MB | 96MB |
| Radeon RX 6700 XT | RDNA2 | gfx1031 | 12GB | 40 | 32/64 | 3MB | 96MB |
| Radeon RX 6700 | RDNA2 | gfx1031 | 10GB | 36 | 32/64 | 3MB | 80MB |
| Radeon RX 6650 XT | RDNA2 | gfx1032 | 8GB | 32 | 32/64 | 2MB | 32MB |
| Radeon RX 6600 XT | RDNA2 | gfx1032 | 8GB | 32 | 32/64 | 2MB | 32MB |
| Radeon RX 6600 | RDNA2 | gfx1032 | 8GB | 28 | 32/64 | 2MB | 32MB |
| Radeon VII | GCN5.1 | gfx906 | 16GB | 60 | 64 | 16MB | 4MB |

### AMD Ryzen APUs (Integrated Graphics)

| GPU | Architecture | LLVM Target | Compute Units | VRAM |
|-----|-------------|-------------|---------------|------|
| **Radeon 780M** (Ryzen 7 7840U, Ryzen 9 270) | **RDNA3** | **gfx1103** | **12** | **Dynamic + carveout** |
| **Radeon 890M** (Ryzen AI 9 HX 375) | **RDNA3.5** | **gfx1150** | **16** | **Dynamic + carveout** |
| Radeon 8060S (Ryzen AI Max+ PRO 395) | RDNA3.5 | gfx1151 | 40 | Dynamic + carveout |
| Radeon (Ryzen AI 7 350) | RDNA3.5 | gfx1152 | 8 | Dynamic + carveout |

**Note on APU VRAM**: APUs use "Dynamic + carveout" memory, meaning they share system RAM. The amount available depends on BIOS VRAM allocation settings and system memory configuration.

### Yggdrasil Fleet GPU Quick Reference

| Node | GPU | Architecture | LLVM Target | GFX Version Override |
|------|-----|-------------|-------------|---------------------|
| Munin | Radeon 780M (iGPU) | RDNA3 | gfx1103 | `11.0.3` or `11.0.0` |
| Hugin | Radeon 890M (iGPU) | RDNA3.5 | gfx1150 | `11.5.0` or `11.0.0` |
| Hugin | RX 9060 XT (eGPU) | RDNA4 | gfx1200 | `12.0.0` |

### Architecture Family Reference

| Architecture | GFX Series | HSA_OVERRIDE_GFX_VERSION | Key Features |
|-------------|-----------|--------------------------|--------------|
| GCN5.1 | gfx906 | 9.0.6 | Vega, HBM2 |
| RDNA | gfx1010/1012 | 10.1.0 | First RDNA, dual compute units |
| RDNA2 | gfx1030/1031/1032 | 10.3.0 | Infinity Cache, ray tracing |
| RDNA3 | gfx1100/1101/1102 | 11.0.0 | Chiplet design, AV1, 2nd-gen RT |
| RDNA3 (APU) | gfx1103 | 11.0.3 | Integrated, shared memory |
| RDNA3.5 | gfx1150/1151/1152 | 11.5.0 | Zen 5 APU, enhanced ML |
| RDNA4 | gfx1200/1201 | 12.0.0 | 3rd-gen RT, enhanced AI |
| CDNA | gfx908 | 9.0.8 | MI100, Matrix cores |
| CDNA2 | gfx90a | 9.0.10 | MI200 series, unified memory |
| CDNA3 | gfx942 | 9.4.2 | MI300 series, chiplet |
| CDNA4 | gfx950 | 9.5.0 | MI350/355X |

### ROCm on Radeon & Ryzen (Consumer)

Source: https://rocm.docs.amd.com/projects/radeon-ryzen/en/latest/index.html

**Supported consumer hardware (ROCm 7.2.1):**
- Radeon RX 9000 Series (RDNA4) -- full support
- Select Radeon RX 7000 Series (RDNA3) -- full support
- Ryzen AI Max 300 Series APUs -- full support
- Select Ryzen AI 400/300 Series APUs -- supported

**Framework support (Linux):**
- PyTorch: full training + inference
- TensorFlow: full training + inference
- vLLM: complete compatibility
- JAX: inference
- Llama.cpp: efficient inference (ROCm + HIP backend)
- FlashAttention-2: with backward pass
- ONNX Runtime: INT8/INT4 inference

**Memory capabilities:**
- Radeon GPUs: up to 48GB VRAM
- Ryzen APUs: up to 128GB shared memory (Ryzen AI Max)

---

## Environment Variables

### GPU Isolation & Device Selection

Source: https://rocm.docs.amd.com/en/latest/conceptual/gpu-isolation.html
Source: https://rocm.docs.amd.com/projects/HIP/en/latest/reference/env_variables.html

**IMPORTANT**: These environment variables restrict application-level access only. They should NOT be used for isolating untrusted applications, as an application can reset them.

#### ROCR_VISIBLE_DEVICES
- **Purpose**: Exposes specific device indices or UUIDs to applications using the ROCm Software Runtime (HSA)
- **Scope**: ROCm runtime level (lowest level, most reliable)
- **Platform**: Linux (recommended over HIP_VISIBLE_DEVICES on Linux)
- **Format**: Comma-separated device indices or UUIDs
- **Example**:
```bash
export ROCR_VISIBLE_DEVICES="0"                    # Single GPU
export ROCR_VISIBLE_DEVICES="0,1"                  # Two GPUs by index
export ROCR_VISIBLE_DEVICES="GPU-4b2c1a9f-8d3e-6f7a-b5c9-2e4d8a1f6c3b"  # By UUID
export ROCR_VISIBLE_DEVICES="0,GPU-4b2c1a9f-8d3e-6f7a-b5c9-2e4d8a1f6c3b"  # Mixed
```
- **Note**: UUIDs are obtainable from `rocminfo` output. Using UUIDs is more reliable than indices for multi-GPU systems since indices can change across reboots.

#### HIP_VISIBLE_DEVICES
- **Purpose**: Device indices exposed to HIP applications
- **Scope**: HIP runtime level
- **Platform**: Windows (primary), Linux (works but ROCR_VISIBLE_DEVICES preferred)
- **Format**: Comma-separated 0-based device indices
- **Example**:
```bash
export HIP_VISIBLE_DEVICES="0,2"   # Expose first and third GPU
export HIP_VISIBLE_DEVICES="0,1"   # Expose first two GPUs
```

#### CUDA_VISIBLE_DEVICES
- **Purpose**: CUDA compatibility alias -- same effect as HIP_VISIBLE_DEVICES on AMD platforms
- **Scope**: HIP runtime level (cross-vendor portability)
- **Example**:
```bash
export CUDA_VISIBLE_DEVICES="0,1"
```

#### GPU_DEVICE_ORDINAL
- **Purpose**: Device indices exposed to OpenCL and HIP applications through ROCclr abstraction
- **Scope**: ROCclr level
- **Example**:
```bash
export GPU_DEVICE_ORDINAL="0,2"
```

#### OMP_DEFAULT_DEVICE
- **Purpose**: Sets the default device for OpenMP target offloading
- **Example**:
```bash
export OMP_DEFAULT_DEVICE="1"   # Use second GPU for OpenMP
```

### GFX Version Override (Critical for Unsupported/Mixed GPUs)

Source: https://adamniederer.com/blog/rocm-cross-arch.html
Source: https://tkamucheka.github.io/blog/2026/02/08/ollama-dual-rocm-gpu/

#### HSA_OVERRIDE_GFX_VERSION
- **Purpose**: Forces ROCm to treat the GPU as a specific GFX architecture version. Essential for running ROCm on GPUs not officially supported or when mixing GPU architectures.
- **Format**: `MAJOR.MINOR.PATCH` (e.g., `11.0.0`)
- **Global override** (applies to ALL GPUs):
```bash
export HSA_OVERRIDE_GFX_VERSION=11.0.0
```

#### HSA_OVERRIDE_GFX_VERSION_{N} (Per-Device Override)
- **Purpose**: Override GFX version for a specific GPU node. Essential for multi-GPU systems with different architectures.
- **CRITICAL**: Uses **1-based indexing** for device numbers, unlike other ROCm variables which are 0-based. Node numbers are obtained from `rocminfo` output.
- **Format**: `HSA_OVERRIDE_GFX_VERSION_{node_number}=MAJOR.MINOR.PATCH`

**Single GPU override:**
```bash
export HSA_OVERRIDE_GFX_VERSION_1="11.0.0"
```

**Multi-GPU with different architectures:**
```bash
# Example: RX 7700 XT (gfx1101 -> 11.0.1) + RX 6600 (gfx1032 -> 10.3.0)
export HSA_OVERRIDE_GFX_VERSION_1=11.0.1
export HSA_OVERRIDE_GFX_VERSION_2=10.3.0
```

**Mixed approach (global default + specific node exception):**
```bash
# Default all GPUs to 10.3.0, override node 3 to 11.0.0
export HSA_OVERRIDE_GFX_VERSION="10.3.0"
export HSA_OVERRIDE_GFX_VERSION_3="11.0.0"
```

**Systemd service configuration (e.g., Ollama):**
```bash
# systemctl edit ollama.service
[Service]
Environment="HSA_OVERRIDE_GFX_VERSION_1=11.0.1"
Environment="HSA_OVERRIDE_GFX_VERSION_2=10.3.0"
```

**Common GFX version mappings:**

| GPU / Architecture | GFX Target | Override Value |
|-------------------|-----------|----------------|
| RDNA2 (RX 6000) | gfx1030 | 10.3.0 |
| RDNA2 (RX 6700) | gfx1031 | 10.3.1 |
| RDNA2 (RX 6600) | gfx1032 | 10.3.2 |
| RDNA3 (RX 7900) | gfx1100 | 11.0.0 |
| RDNA3 (RX 7800/7700) | gfx1101 | 11.0.1 |
| RDNA3 (RX 7600) | gfx1102 | 11.0.2 |
| RDNA3 APU (780M) | gfx1103 | 11.0.3 |
| RDNA3.5 APU (890M) | gfx1150 | 11.5.0 |
| RDNA3.5 APU (AI Max) | gfx1151 | 11.5.1 |
| RDNA3.5 APU (AI 7) | gfx1152 | 11.5.2 |
| RDNA4 (RX 9060) | gfx1200 | 12.0.0 |
| RDNA4 (RX 9070) | gfx1201 | 12.0.1 |

**Important notes:**
- Do NOT set `HIP_VISIBLE_DEVICES` or `ROCR_VISIBLE_DEVICES` when using per-device GFX overrides for multi-architecture setups -- it can interfere with device numbering
- Use `rocminfo` to determine node numbers (node 0 is typically the CPU)
- Use `rocm-smi` to verify both devices are active and showing VRAM usage under load

### Compute Unit Masking

#### HSA_CU_MASK
- **Purpose**: Sets the compute unit mask at the driver level during queue creation
- **Format**: `device:cu_range`
- **Example**:
```bash
export HSA_CU_MASK="1:0-8"   # Enable CUs 0-8 on device 1
```

#### ROC_GLOBAL_CU_MASK
- **Purpose**: Sets the CU mask on queues created by HIP or OpenCL runtimes
- **Format**: Hex bitmask
- **Example**:
```bash
export ROC_GLOBAL_CU_MASK="0xf"   # Enable only 4 CUs
```

### Profiling Variables

#### HIP_FORCE_QUEUE_PROFILING
- **Purpose**: Forces command queue profiling on by default
- **Values**: `0` (Disable), `1` (Enable)

### Debug & Logging Variables

#### AMD_LOG_LEVEL
- **Default**: `0`
- **Purpose**: Enables HIP logging at various verbosity levels
- **Values**:
  - `0`: Disable logging
  - `1`: Error logs only
  - `2`: Warning logs + lower levels
  - `3`: Information logs + lower levels
  - `4`: Debug logs + lower levels
  - `5`: Debug extra logs + lower levels

#### AMD_LOG_LEVEL_FILE
- **Default**: stderr
- **Purpose**: Redirects AMD_LOG_LEVEL output to a file

#### AMD_LOG_MASK
- **Default**: `0x7FFFFFFF`
- **Purpose**: Specifies HIP log filters (bitwise OR of desired categories)
- **Values**:
  - `0x1`: API calls
  - `0x2`: Kernel and copy commands
  - `0x4`: Synchronization operations
  - `0x8`: AQL packet display
  - `0x10`: Queue commands
  - `0x20`: Signal creation
  - `0x40`: Locks and threading
  - `0x80`: Kernel creation
  - `0x100`: Copy debug
  - `0x200`: Detailed copy debug
  - `0x400`: Resource allocation
  - `0x800`: Initialization/shutdown
  - `0x1000`: Miscellaneous debug
  - `0x2000`: Raw AQL packet bytes
  - `0x4000`: Code creation debug
  - `0x8000`: Detailed command info
  - `0x10000`: Message location logging
  - `0x20000`: Memory allocation
  - `0x40000`: Memory pool allocation
  - `0x80000`: Timestamp details
  - `0x100000`: Comgr path information
  - `0xFFFFFFFF`: Log always (all categories)

#### HIP_LAUNCH_BLOCKING
- **Default**: `0`
- **Purpose**: Serializes kernel execution for debugging
- **Values**: `0` (Normal async execution), `1` (Serializes kernel enqueue -- all kernels run synchronously)

#### AMD_SERIALIZE_KERNEL
- **Default**: `0`
- **Purpose**: Serializes kernel enqueue for debugging
- **Values**: `0` (Disable), `1` (before enqueue), `2` (after enqueue), `3` (both before and after)

#### AMD_SERIALIZE_COPY
- **Default**: `0`
- **Purpose**: Serializes copy operations for debugging
- **Values**: `0` (Disable), `1` (before enqueue), `2` (after enqueue), `3` (both)

#### GPU_DUMP_CODE_OBJECT
- **Default**: `0`
- **Purpose**: Dumps compiled code objects for inspection
- **Values**: `0` (Disable), `1` (Enable)

#### HIP_FORCE_DEV_KERNARG
- **Default**: `1`
- **Purpose**: Forces kernel arguments to be stored in device memory to reduce latency
- **Values**: `0` (Disable), `1` (Enable)
- **Note**: Can improve performance by 2-3 microseconds for some kernels

### Hardware Queue Configuration

#### GPU_MAX_HW_QUEUES
- **Default**: `4`
- **Purpose**: The maximum number of hardware queues allocated per device
- **Note**: Applies per process per device. Additional streams reuse queues in round-robin fashion. Does not apply to CU-masked or cooperative queues.

### Memory Management Variables

#### HIP_HIDDEN_FREE_MEM
- **Default**: `0`
- **Purpose**: Amount of memory (in MB) to hide from the free memory reported by `hipMemGetInfo`
- **Unit**: Megabytes

#### HIP_HOST_COHERENT
- **Default**: `0`
- **Purpose**: Specifies if memory is coherent between host and GPU in `hipHostMalloc`
- **Values**: `0` (Non-coherent), `1` (Coherent)
- **Conditions**: Applies when `hipHostMallocDefault`, `hipHostMallocPortable`, `hipHostMallocWriteCombined`, or `hipHostMallocNumaUser` flags are set, and `hipHostMallocCoherent`, `hipHostMallocNonCoherent`, and `hipHostMallocMapped` flags are NOT set.

#### HIP_INITIAL_DM_SIZE
- **Default**: `8388608` (8 MB)
- **Purpose**: Sets initial heap size for device malloc
- **Unit**: Bytes

#### HIP_MEM_POOL_SUPPORT
- **Default**: `0`
- **Purpose**: Enables memory pool support in HIP
- **Values**: `0` (Disable), `1` (Enable)

#### HIP_MEM_POOL_USE_VM
- **Default**: `0` (Linux), `1` (Windows)
- **Purpose**: Use virtual memory for memory pools
- **Values**: `0` (Disable), `1` (Enable)

#### HIP_VMEM_MANAGE_SUPPORT
- **Default**: `1`
- **Purpose**: Virtual memory management support
- **Values**: `0` (Disable), `1` (Enable)

#### GPU_SINGLE_ALLOC_PERCENT
- **Default**: `100`
- **Purpose**: Limits the maximum size of a single memory allocation as a percentage of total GPU memory
- **Unit**: Percentage

#### GPU_MAX_HEAP_SIZE
- **Default**: `100`
- **Purpose**: Sets maximum size of the GPU heap as a percentage of board memory
- **Unit**: Percentage

#### GPU_MAX_REMOTE_MEM_SIZE
- **Default**: `2`
- **Purpose**: Maximum size that allows device memory substitution with system memory
- **Unit**: Kilobytes

#### GPU_NUM_MEM_DEPENDENCY
- **Default**: `256`
- **Purpose**: Number of memory objects for dependency tracking

#### GPU_STREAMOPS_CP_WAIT
- **Default**: `0`
- **Purpose**: Force the stream memory operation to wait on command processor
- **Values**: `0` (Disable), `1` (Enable)

#### HSA_LOCAL_MEMORY_ENABLE
- **Default**: `1`
- **Purpose**: Enable HSA device local memory usage
- **Values**: `0` (Disable), `1` (Enable)

#### PAL_ALWAYS_RESIDENT
- **Default**: `0`
- **Purpose**: Force memory resources to become resident at allocation time
- **Values**: `0` (Disable), `1` (Enable)

#### PAL_PREPINNED_MEMORY_SIZE
- **Default**: `64`
- **Purpose**: Size of prepinned memory in kilobytes
- **Unit**: KB

#### REMOTE_ALLOC
- **Default**: `0`
- **Purpose**: Use remote memory for global heap allocation
- **Values**: `0` (Disable), `1` (Enable)

### ROCR-Runtime Variables

#### HSA_NO_SCRATCH_RECLAIM
- **Default**: `0`
- **Purpose**: Permanent scratch memory allocation (no reclaim)
- **Values**: `0` (Allow reclaim), `1` (Permanent allocation)

#### HSA_SCRATCH_SINGLE_LIMIT
- **Default**: `146800640` bytes (~140MB)
- **Purpose**: Scratch memory threshold per allocation

#### HSA_SCRATCH_SINGLE_LIMIT_ASYNC
- **Default**: `3221225472` bytes (~3GB)
- **Purpose**: Async scratch threshold on supported GPUs

#### HSA_ENABLE_SCRATCH_ASYNC_RECLAIM
- **Default**: `1`
- **Purpose**: Enable async scratch memory reclamation
- **Values**: `0` (Disable), `1` (Enable)

#### HSA_XNACK
- **Purpose**: Enable XNACK (page retry) support
- **Values**: `1` (Enable)
- **Note**: Required for unified memory on supported architectures

#### HSA_ENABLE_SDMA
- **Default**: `1`
- **Purpose**: Enable System DMA engines for memory transfers
- **Values**: `0` (Disable -- use shader copy), `1` (Enable)

#### HSA_ENABLE_PEER_SDMA
- **Default**: `1`
- **Purpose**: Enable peer-to-peer DMA between GPUs
- **Values**: `0` (Disable), `1` (Enable)

#### HSA_ENABLE_MWAITX
- **Default**: `0`
- **Purpose**: Use mwaitx instruction for HSA signal wait
- **Values**: `0` (Disable), `1` (Enable)

#### HSA_OVERRIDE_CPU_AFFINITY_DEBUG
- **Default**: `1`
- **Purpose**: Control helper thread CPU affinity
- **Values**: `0` (Disable), `1` (Enable)

#### HSA_ENABLE_DEBUG
- **Default**: `0`
- **Purpose**: Enable debug validation checks in HSA runtime
- **Values**: `0` (Disable), `1` (Enable)

#### HSA_DISABLE_FRAGMENT_ALLOCATOR
- **Default**: `0`
- **Purpose**: Disable memory fragment caching
- **Values**: `0` (Normal caching), `1` (Disable)

#### HSAKMT_DEBUG_LEVEL
- **Default**: `3`
- **Purpose**: KMT driver debug verbosity level
- **Values**: `3` (Minimum) to `7` (Maximum verbosity)

#### HSA_ENABLE_INTERRUPT
- **Default**: `1`
- **Purpose**: Enable hardware interrupts for signal notification
- **Values**: `0` (Polling mode), `1` (Interrupt mode)

#### HSA_SVM_GUARD_PAGES
- **Default**: `1`
- **Purpose**: Enable SVM (Shared Virtual Memory) guard pages
- **Values**: `0` (Disable), `1` (Enable)

#### HSA_DISABLE_CACHE
- **Default**: `0`
- **Purpose**: Disable L2 cache
- **Values**: `0` (Cache enabled), `1` (Cache disabled)

### HIPCC Compiler Variables

#### HIP_PLATFORM
- **Purpose**: Target platform selection
- **Values**: `amd` (ROCm), `nvidia` (CUDA)

#### ROCM_PATH
- **Default**: `/opt/rocm`
- **Purpose**: ROCm installation path (Linux)

#### CUDA_PATH
- **Default**: `/usr/local/cuda`
- **Purpose**: CUDA SDK path (NVIDIA platforms)

#### HIP_CLANG_PATH
- **Purpose**: Path to Clang compiler for AMD platforms

#### HIP_LIB_PATH
- **Purpose**: HIP device library path

#### HIP_DEVICE_LIB_PATH
- **Purpose**: HIP device library installation path

#### HIPCC_COMPILE_FLAGS_APPEND
- **Purpose**: Additional compilation flags appended to hipcc

#### HIPCC_LINK_FLAGS_APPEND
- **Purpose**: Additional linker flags appended to hipcc

#### HIPCC_VERBOSE
- **Purpose**: Compilation verbosity control
- **Values**: `1`-`7` (different combinations of output detail)

### Compilation & Code Generation Variables

#### HIPRTC_COMPILE_OPTIONS_APPEND
- **Purpose**: Sets compile options for hiprtc (runtime compilation)
- **Example**:
```bash
export HIPRTC_COMPILE_OPTIONS_APPEND="--gpu-architecture=gfx906:sramecc+:xnack -fgpu-rdc"
```

#### AMD_COMGR_SAVE_TEMPS
- **Purpose**: Controls deletion of temporary files generated during Comgr compilation
- **Values**: `0` (Auto-delete), non-zero (Keep files)
- **Note**: Files stored in platform temp directory, not current working directory

#### AMD_COMGR_EMIT_VERBOSE_LOGS
- **Purpose**: Enable verbose Comgr logging
- **Values**: `0` (Disabled), non-zero (Enabled)

#### AMD_COMGR_REDIRECT_LOGS
- **Purpose**: Redirect Comgr log output
- **Values**: `stdout` or `-` (standard output), `stderr` (error stream)

### Other Library Variables

#### ROCALUTION_LAYER
- **Purpose**: Enable rocALUTION file logging
- **Values**: `1` (Enable)

---

## Multi-GPU Configuration (Complete Reference)

Source: https://github.com/eliranwong/MultiAMDGPU_AIDev_Ubuntu

### Full Environment Setup for Multi-GPU

```bash
# === Core ROCm paths ===
export ROCM_HOME=/opt/rocm
export ROCM_PATH=/opt/rocm
export PATH=$HOME/.local/bin:/opt/rocm/bin:/opt/rocm/llvm/bin:$PATH
export LD_LIBRARY_PATH=/opt/rocm/include:/opt/rocm/lib:/opt/rocm/lib/llvm/lib:$LD_LIBRARY_PATH

# === GPU Architecture ===
export GFX_ARCH=gfx1100                    # Set to your primary GPU arch
export HCC_AMDGPU_TARGET=gfx1100           # Compiler target
export HSA_OVERRIDE_GFX_VERSION=11.0.0     # Global GFX override

# === Device Selection ===
# Use UUIDs for reliability (get from rocminfo):
export ROCR_VISIBLE_DEVICES=GPU-<uuid1>,GPU-<uuid2>
export GPU_DEVICE_ORDINAL=0,1
export HIP_VISIBLE_DEVICES=0,1
export CUDA_VISIBLE_DEVICES=0,1

# === Application-specific ===
export LLAMA_HIPLAS=0,1                    # llama.cpp HIP layer assignment
export DRI_PRIME=1                         # Use discrete GPU for rendering
export OMP_DEFAULT_DEVICE=1                # OpenMP default device

# === Vulkan (for llama.cpp Vulkan backend) ===
export GGML_VULKAN_DEVICE=0,1
export GGML_VK_VISIBLE_DEVICES=0,1
export VULKAN_SDK=/usr/share/vulkan
export VK_LAYER_PATH=$VULKAN_SDK/explicit_layer.d
```

### Verification Commands

```bash
# List all GPU agents and their properties
rocminfo

# Monitor GPU utilization, temperature, VRAM
rocm-smi

# Watch GPU usage in real-time
watch -n 1 rocm-smi

# Map node numbers to device numbers
rocm-smi --showuniqueid

# Check ROCm version
cat /opt/rocm/.info/version

# Verify GPU access
/opt/rocm/bin/rocminfo | grep -E "Name|Marketing|gfx"
```

### Hardware Requirements for Multi-GPU

- PCIe slots must have matching lane widths / bifurcation settings
- PCIe 3.0 Atomics support is mandatory
- Use CPU-connected PCIe slots (not chipset-routed)
- Sufficient PSU wattage for all GPUs
- BIOS: Enable IOMMU / ACS if available
- GRUB: Add `iommu=pt` to prevent multi-GPU hangs

### Running Ollama with Mixed AMD GPUs

Source: https://tkamucheka.github.io/blog/2026/02/08/ollama-dual-rocm-gpu/

For mixed-architecture GPU setups (e.g., RDNA3 + RDNA2), use per-device GFX overrides:

```bash
# In systemd service override (systemctl edit ollama.service):
[Service]
Environment="OLLAMA_HOST=0.0.0.0"
Environment="OLLAMA_ORIGINS=*"
Environment="HSA_OVERRIDE_GFX_VERSION_1=11.0.1"
Environment="HSA_OVERRIDE_GFX_VERSION_2=10.3.0"
```

Key points:
- `HSA_OVERRIDE_GFX_VERSION_{N}` is **1-indexed** (node 0 = CPU)
- Do NOT combine with `HIP_VISIBLE_DEVICES` or `ROCR_VISIBLE_DEVICES` for this use case
- Validate with `rocm-smi` that both GPUs show non-zero VRAM and utilization under load

### Running ROCm Across Different Architectures (Experimental)

Source: https://adamniederer.com/blog/rocm-cross-arch.html

For truly heterogeneous multi-GPU (e.g., RDNA3 + RDNA2 running the same workload), a patched ROCT-Thunk-Interface supports per-node GFX overrides:

```bash
git clone https://github.com/AdamNiederer/ROCT-Thunk-Interface.git
cd ROCT-Thunk-Interface
git checkout per-node-overrides
mkdir build && cd build
cmake -DBUILD_SHARED_LIBS=on ..
make
sudo install -sbm755 libhsakmt.so.1.* /opt/rocm/lib/
```

After installation, per-device overrides work via `HSA_OVERRIDE_GFX_VERSION_{N}`.

Performance example: LLaMA3-70B across RX 7900XT + RX 6600 + CPU achieved 7.4 tokens/second with 77/81 layers offloaded.

**Note**: This is experimental software. Standard ROCm builds may have varying support for per-device overrides depending on version.

---

## Appendix: Useful Diagnostic Commands

```bash
# Full system GPU info
rocminfo

# GPU monitoring (temp, utilization, VRAM, fan speed)
rocm-smi

# Show GPU topology (NUMA, PCIe, XGMI links)
rocm-smi --showtopo

# Show all GPU properties
rocm-smi --showallinfo

# Show memory info
rocm-smi --showmeminfo vram

# List supported ISA for each GPU
rocminfo | grep -A5 "ISA Info"

# Check kernel driver version
dmesg | grep amdgpu

# Check ROCm library versions
apt list --installed 2>/dev/null | grep rocm
dpkg -l | grep rocm

# Verify HIP installation
hipcc --version
hipconfig --full
```
