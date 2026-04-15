#!/usr/bin/env bash
# Sprint 069 Phase F — stage every model the new llama-swap config expects
# into /opt/yggdrasil/models/ on Hugin so the vLLM containers mounting that
# directory can find them.
#
# Sources:
#   - Ollama blob store on Hugin (primary for GGUF-available models)
#   - Morrigan /home/jhernandez/fine-tuning/merged-models (distilled safetensors)
#   - HuggingFace (glm-4.7-flash only)
#
# Idempotent: skips any symlink/file that already exists and matches the
# expected source. Safe to re-run after a partial failure.
set -euo pipefail

HUGIN_MODELS=/opt/yggdrasil/models
OLLAMA_BLOBS=/usr/share/ollama/.ollama/models/blobs
MORRIGAN_SSH=jhernandez@10.0.65.20
MORRIGAN_BASE=/home/jhernandez/fine-tuning/merged-models

need_sudo() { sudo -n true 2>/dev/null || { echo "error: this script must run as root (or with passwordless sudo)"; exit 2; }; }

# --- Helpers ------------------------------------------------------------

# Symlink an Ollama blob file into /opt/yggdrasil/models with a sane GGUF name.
link_blob() {
    local sha="$1" dest_name="$2"
    local blob="$OLLAMA_BLOBS/sha256-$sha"
    local dest="$HUGIN_MODELS/$dest_name"
    if [[ ! -f "$blob" ]]; then
        echo "SKIP $dest_name: blob $sha not found — run 'ollama pull' first"
        return 1
    fi
    if [[ -L "$dest" && "$(readlink -f "$dest")" == "$blob" ]]; then
        echo "OK   $dest_name (symlink up to date)"
        return 0
    fi
    rm -f "$dest"
    ln -s "$blob" "$dest"
    echo "LINK $dest_name -> sha256-${sha:0:12}..."
}

# Resolve an Ollama model name to its primary GGUF blob sha256.
blob_sha_of() {
    local model="$1"
    ollama show --modelfile "$model" 2>/dev/null \
        | awk '/^FROM .*\/blobs\/sha256-[0-9a-f]+/ { sub(".*sha256-",""); print; exit }'
}

# Rsync a Morrigan safetensors directory (HF format) into /opt/yggdrasil/models.
pull_hf_dir() {
    local src_rel="$1" dest_name="$2"
    local dest="$HUGIN_MODELS/$dest_name"
    mkdir -p "$dest"
    echo "RSYNC $dest_name <- Morrigan:$src_rel"
    rsync -az --delete --info=progress2 \
        "$MORRIGAN_SSH:$MORRIGAN_BASE/$src_rel/" \
        "$dest/"
}

# --- Plan ---------------------------------------------------------------

need_sudo
mkdir -p "$HUGIN_MODELS"

echo "=== Phase 1: Ollama blob symlinks (GGUF) ==="
# Map: Ollama model name -> /opt/yggdrasil/models/<dest filename>
declare -A OLLAMA_MAP=(
    ["gemma4:e4b"]="gemma4-e4b.gguf"
    ["gemma4:e2b"]="gemma4-e2b.gguf"
    ["nemotron-3-nano:4b"]="nemotron-3-nano-4b.gguf"
    ["hf.co/LiquidAI/LFM2.5-1.2B-Instruct-GGUF:Q4_K_M"]="lfm25-1.2b-instruct.gguf"
    ["lfm-1.2b:latest"]="lfm-1.2b.gguf"
    ["lfm25-tools:latest"]="lfm25-tools.gguf"
    ["code-cleaner-350m:latest"]="code-cleaner-350m.gguf"
)
for model in "${!OLLAMA_MAP[@]}"; do
    sha=$(blob_sha_of "$model") || true
    if [[ -z "$sha" ]]; then
        echo "SKIP $model: not in Ollama — run 'ollama pull $model' first"
        continue
    fi
    link_blob "$sha" "${OLLAMA_MAP[$model]}"
done

echo
echo "=== Phase 2: Morrigan distilled safetensors (HF format) ==="
# Map: Morrigan relative path -> /opt/yggdrasil/models/<dest dirname>
# Distilled artifacts come straight from merged-LoRA output; vLLM reads them
# as standard HuggingFace model directories.
pull_hf_dir "lfm-saga-v3"   "saga-350m"
pull_hf_dir "lfm-review-v2" "review-1.2b"
pull_hf_dir "lfm-saga-tool" "lfm25-tools-hf"   # HF copy alongside the GGUF alias

# Fusion specialist — the "v6" name is aspirational; use the latest V4 run
# (fusion-wd1.0-constant-lr0.0001) until a V6 pipeline lands on Morrigan.
# If V6 appears later, re-run this script and the rsync will swap it in place.
echo "RSYNC fusion-v6 <- Morrigan:output-fusion360-v4/fusion-wd1.0-constant-lr0.0001/checkpoints (best)"
rsync -az --delete --info=progress2 \
    "$MORRIGAN_SSH:/home/jhernandez/fine-tuning/output-fusion360-v4/fusion-wd1.0-constant-lr0.0001/checkpoints/best/" \
    "$HUGIN_MODELS/fusion-v6/" || echo "WARN: fusion-v6 best checkpoint not found — check Morrigan layout"

echo
echo "=== Phase 3: HuggingFace download ==="
# GLM-4.7-Flash via bartowski's GGUF repo. Pulls ~5GB on first run.
if [[ ! -f "$HUGIN_MODELS/glm-4.7-flash.gguf" ]]; then
    if ! command -v huggingface-cli >/dev/null 2>&1; then
        echo "INSTALL huggingface-cli"
        pip3 install -q --break-system-packages huggingface_hub
    fi
    echo "DOWNLOAD bartowski/zai-org_GLM-4.7-Flash-GGUF -> glm-4.7-flash.gguf"
    huggingface-cli download bartowski/zai-org_GLM-4.7-Flash-GGUF \
        zai-org_GLM-4.7-Flash-Q4_K_M.gguf \
        --local-dir "$HUGIN_MODELS/_glm_dl" \
        --local-dir-use-symlinks False
    mv "$HUGIN_MODELS/_glm_dl/zai-org_GLM-4.7-Flash-Q4_K_M.gguf" \
       "$HUGIN_MODELS/glm-4.7-flash.gguf"
    rm -rf "$HUGIN_MODELS/_glm_dl"
else
    echo "OK   glm-4.7-flash.gguf already staged"
fi

echo
echo "=== Final inventory ==="
ls -lh "$HUGIN_MODELS" | awk 'NR>1 {printf "  %-40s  %s\n", $NF, $5}' | head -25

echo
echo "done — all models staged under $HUGIN_MODELS"
