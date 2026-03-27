#!/usr/bin/env bash
# Merge LoRA adapters into base model, convert to GGUF, quantize, and import to Ollama.
set -euo pipefail

SCRIPT_DIR_REAL="$(cd "$(dirname "$0")" && pwd)"
BARN="${BARN_DIR:-$SCRIPT_DIR_REAL}"
BASE_MODEL="LiquidAI/LFM2.5-1.2B-Instruct"
ADAPTER_PATH="$BARN/adapters/saga-lora"
MERGED_PATH="$BARN/merged"
GGUF_DIR="$BARN/gguf"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "=== Step 1: Merge LoRA adapters into base model ==="
python3 -c "
from peft import PeftModel
from transformers import AutoModelForCausalLM, AutoTokenizer
import torch

print('Loading base model...')
model = AutoModelForCausalLM.from_pretrained('$BASE_MODEL', torch_dtype=torch.float16, trust_remote_code=True)
tokenizer = AutoTokenizer.from_pretrained('$BASE_MODEL', trust_remote_code=True)

print('Loading LoRA adapters...')
model = PeftModel.from_pretrained(model, '$ADAPTER_PATH')

print('Merging...')
model = model.merge_and_unload()

print('Saving merged model...')
model.save_pretrained('$MERGED_PATH')
tokenizer.save_pretrained('$MERGED_PATH')
print('Done.')
"

echo ""
echo "=== Step 2: Convert to GGUF ==="
# Check if llama-cpp-python or llama.cpp convert script is available
if command -v llama-gguf-convert &>/dev/null; then
    llama-gguf-convert "$MERGED_PATH" "$GGUF_DIR/saga-f16.gguf"
elif [ -f "$HOME/llama.cpp/convert_hf_to_gguf.py" ]; then
    python3 "$HOME/llama.cpp/convert_hf_to_gguf.py" "$MERGED_PATH" \
        --outfile "$GGUF_DIR/saga-f16.gguf" --outtype f16
else
    echo "Installing llama-cpp-python for GGUF conversion..."
    pip install llama-cpp-python 2>/dev/null || true

    # Use transformers + gguf export if available
    python3 -c "
from transformers import AutoModelForCausalLM, AutoTokenizer
print('Attempting gguf export via transformers...')
model = AutoModelForCausalLM.from_pretrained('$MERGED_PATH', torch_dtype='float16')
tokenizer = AutoTokenizer.from_pretrained('$MERGED_PATH')
model.save_pretrained('$GGUF_DIR', safe_serialization=False)
print('Saved PyTorch model. Use llama.cpp convert_hf_to_gguf.py for final GGUF conversion.')
" 2>/dev/null

    # Clone llama.cpp if needed for conversion
    if [ ! -d "$HOME/llama.cpp" ]; then
        echo "Cloning llama.cpp for GGUF conversion..."
        git clone --depth 1 https://github.com/ggerganov/llama.cpp.git "$HOME/llama.cpp"
        pip install -r "$HOME/llama.cpp/requirements/requirements-convert_hf_to_gguf.txt" 2>/dev/null || true
    fi

    python3 "$HOME/llama.cpp/convert_hf_to_gguf.py" "$MERGED_PATH" \
        --outfile "$GGUF_DIR/saga-f16.gguf" --outtype f16
fi

echo ""
echo "=== Step 3: Quantize to Q4_K_M ==="
if command -v llama-quantize &>/dev/null; then
    llama-quantize "$GGUF_DIR/saga-f16.gguf" "$GGUF_DIR/saga-q4_k_m.gguf" Q4_K_M
elif [ -f "$HOME/llama.cpp/build/bin/llama-quantize" ]; then
    "$HOME/llama.cpp/build/bin/llama-quantize" "$GGUF_DIR/saga-f16.gguf" "$GGUF_DIR/saga-q4_k_m.gguf" Q4_K_M
else
    echo "WARNING: llama-quantize not found. Using f16 GGUF directly."
    cp "$GGUF_DIR/saga-f16.gguf" "$GGUF_DIR/saga-q4_k_m.gguf"
fi

echo ""
echo "=== Step 4: Import to Ollama ==="
# Generate Modelfile
cat > "$GGUF_DIR/Modelfile" <<'MODELFILE'
FROM ./saga-q4_k_m.gguf
TEMPLATE """{{- if .System }}<|im_start|>system
{{ .System }}<|im_end|>
{{ end }}<|im_start|>user
{{ .Prompt }}<|im_end|>
<|im_start|>assistant
"""
SYSTEM "You are Saga, Yggdrasil's memory engine. Respond ONLY in valid JSON."
PARAMETER temperature 0.1
PARAMETER top_p 0.9
PARAMETER num_predict 256
PARAMETER stop "<|im_end|>"
MODELFILE

# Also copy Modelfile to training dir for reference
cp "$GGUF_DIR/Modelfile" "$SCRIPT_DIR/Modelfile"

cd "$GGUF_DIR"
ollama create saga:1.2b -f Modelfile

echo ""
echo "=== Done ==="
echo "Model imported as saga:1.2b"
echo "Test: ollama run saga:1.2b 'CLASSIFY\ntool: Edit\nfile: src/main.rs\ncontent: Fixed null pointer crash'"
ls -lh "$GGUF_DIR/"
