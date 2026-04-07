#!/usr/bin/env bash
# Launch 2 forced-generalization experiments in parallel on Morrigan.
#
# GPU 0: Run A — moderate WD (0.3), cosine scheduler, lr=3e-5
# GPU 1: Run B — aggressive WD (0.5), constant scheduler, lr=1e-4
#
# Both use Hugin's qwen3-coder:30b to generate math problems live.
# Python verifies every answer before feeding to training.
#
# Usage (on Morrigan):
#   cd ~/repos/Yggdrasil/yggdrasil/training/experiment
#   bash run_parallel.sh
#
# Or from this workstation:
#   ssh jhernandez@10.0.65.20 "cd ~/repos/Yggdrasil/yggdrasil/training/experiment && bash run_parallel.sh"

set -euo pipefail

# Activate the training venv
VENV="$HOME/fine-tuning/venv"
if [ -f "$VENV/bin/activate" ]; then
    source "$VENV/bin/activate"
    echo "[OK] Activated venv: $(python3 --version)"
else
    echo "[FATAL] No venv at $VENV — run: python3 -m venv $VENV && pip install torch transformers trl peft datasets requests"
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# LLM endpoint for live question generation (Hugin)
LLM_ENDPOINT="http://10.0.65.8:11434/v1/chat/completions"
LLM_MODEL="qwen3-coder:30b-a3b-q4_K_M"

# OOM prevention
export PYTORCH_CUDA_ALLOC_CONF="expandable_segments:True"

# Output directories
OUTPUT_DIR="$HOME/fine-tuning/output-forced-grok"
mkdir -p "$OUTPUT_DIR"

echo "=========================================="
echo "  FORCED GENERALIZATION — PARALLEL RUNS"
echo "=========================================="
echo "  GPU 0: Run A — WD=0.3, cosine, lr=3e-5"
echo "  GPU 1: Run B — WD=0.5, constant, lr=1e-4"
echo "  LLM:   $LLM_MODEL @ $LLM_ENDPOINT"
echo "  Output: $OUTPUT_DIR"
echo "=========================================="
echo ""

# Verify Hugin is reachable
if curl -s --max-time 3 "$LLM_ENDPOINT" > /dev/null 2>&1 || \
   curl -s --max-time 3 "http://10.0.65.8:11434/api/tags" > /dev/null 2>&1; then
    echo "[OK] Hugin LLM endpoint reachable"
else
    echo "[WARN] Hugin LLM endpoint not reachable — will fall back to Python generation"
fi

# Verify GPUs
echo ""
nvidia-smi --query-gpu=index,name,memory.used,memory.total --format=csv,noheader
echo ""

# Run A: GPU 0 — moderate pressure
echo "[$(date +%H:%M:%S)] Starting Run A on GPU 0..."
CUDA_VISIBLE_DEVICES=0 python3 train_forced_grok.py \
    --mode single \
    --gpu 0 \
    --max-steps 5000 \
    --weight-decay 0.3 \
    --lr 3e-5 \
    --scheduler cosine \
    --warmup-steps 200 \
    --batch-size 8 \
    --grad-accum 4 \
    --eval-steps 100 \
    --data-seed 42 \
    --llm-endpoint "$LLM_ENDPOINT" \
    --llm-model "$LLM_MODEL" \
    --output "$OUTPUT_DIR" \
    > "$OUTPUT_DIR/run_a.log" 2>&1 &
PID_A=$!
echo "  PID: $PID_A → $OUTPUT_DIR/run_a.log"

# Run B: GPU 1 — aggressive pressure
echo "[$(date +%H:%M:%S)] Starting Run B on GPU 1..."
CUDA_VISIBLE_DEVICES=1 python3 train_forced_grok.py \
    --mode single \
    --gpu 0 \
    --max-steps 5000 \
    --weight-decay 0.5 \
    --lr 1e-4 \
    --scheduler constant \
    --warmup-steps 100 \
    --batch-size 8 \
    --grad-accum 4 \
    --eval-steps 100 \
    --data-seed 7777 \
    --llm-endpoint "$LLM_ENDPOINT" \
    --llm-model "$LLM_MODEL" \
    --output "$OUTPUT_DIR" \
    > "$OUTPUT_DIR/run_b.log" 2>&1 &
PID_B=$!
echo "  PID: $PID_B → $OUTPUT_DIR/run_b.log"

echo ""
echo "Both runs launched. Monitor with:"
echo "  tail -f $OUTPUT_DIR/run_a.log"
echo "  tail -f $OUTPUT_DIR/run_b.log"
echo ""
echo "  # Check accuracy progress:"
echo "  grep 'ACCURACY' $OUTPUT_DIR/run_a.log"
echo "  grep 'ACCURACY' $OUTPUT_DIR/run_b.log"
echo ""
echo "Waiting for both to complete..."

# Wait and report
wait $PID_A
EXIT_A=$?
echo "[$(date +%H:%M:%S)] Run A finished (exit=$EXIT_A)"

wait $PID_B
EXIT_B=$?
echo "[$(date +%H:%M:%S)] Run B finished (exit=$EXIT_B)"

echo ""
echo "=========================================="
echo "  RESULTS"
echo "=========================================="

for run in "forced-wd0.3-cosine-lr3e-05" "forced-wd0.5-constant-lr0.0001"; do
    summary="$OUTPUT_DIR/$run/summary.json"
    if [ -f "$summary" ]; then
        echo "  $run:"
        python3 -c "
import json
s = json.load(open('$summary'))
print(f\"    Accuracy: {s['final_accuracy']:.1f}% (ID: {s['final_accuracy_id']:.1f}%, OOD: {s['final_accuracy_ood']:.1f}%)\")
print(f\"    Eval loss: {s['final_eval_loss']}\")
print(f\"    Weight norm: {s['final_weight_norm']:.2f}\")
print(f\"    Time: {s['elapsed_minutes']:.1f} min\")
"
    else
        echo "  $run: NO SUMMARY (check logs)"
    fi
done

echo "=========================================="
