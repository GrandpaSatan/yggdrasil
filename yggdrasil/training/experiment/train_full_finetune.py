#!/usr/bin/env python3
"""Full fine-tuning grokking experiment — NO LoRA.

The definitive test of whether LFM hybrid conv/attention architecture
can exhibit grokking. Previous 20+ attempts all used LoRA (0.14-1.4%
params trainable) which prevents the base weight restructuring that
grokking requires.

This script trains ALL parameters of LFM2-350M in fp16 on a single GPU.
Memory: ~5.7GB on RTX 3060 (12GB), with gradient checkpointing.

Usage:
    # Defect detection (binary classification — most grokkable)
    python3 train_full_finetune.py --data data/defect_only_train.jsonl \
        --val data/defect_only_val.jsonl --output output-fullft-defect

    # Custom settings
    python3 train_full_finetune.py --data data/defect_only_train.jsonl \
        --epochs 200 --lr 3e-5 --weight-decay 0.1
"""
import argparse
import json
import os
import sys
import time
from pathlib import Path

os.environ.setdefault("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True")

import torch
from datasets import Dataset
from transformers import AutoModelForCausalLM, AutoTokenizer
from trl import SFTTrainer, SFTConfig

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lib.adaptive_grok import AdaptiveGrokCallback

BASE_MODEL = "LiquidAI/LFM2-350M"
MAX_SEQ_LEN = 512


def load_jsonl(path: str) -> list[dict]:
    with open(path) as f:
        return [json.loads(line) for line in f if line.strip()]


def format_chat(example: dict) -> dict:
    parts = [
        f"<|im_start|>{m['role']}\n{m['content']}<|im_end|>"
        for m in example["messages"]
    ]
    return {"text": "\n".join(parts)}


def main():
    parser = argparse.ArgumentParser(description="Full fine-tuning grokking experiment")
    parser.add_argument("--data", type=Path, required=True)
    parser.add_argument("--val", type=Path, default=None,
                        help="Validation JSONL (auto-split 80/20 if omitted)")
    parser.add_argument("--output", type=Path, default=Path("output-fullft"))
    parser.add_argument("--base-model", default=BASE_MODEL)
    parser.add_argument("--epochs", type=int, default=200)
    parser.add_argument("--lr", type=float, default=3e-5)
    parser.add_argument("--weight-decay", type=float, default=0.1)
    parser.add_argument("--scheduler", default="cosine",
                        choices=["cosine", "constant", "linear"])
    parser.add_argument("--warmup-ratio", type=float, default=0.1)
    parser.add_argument("--batch-size", type=int, default=4)
    parser.add_argument("--grad-accum", type=int, default=4)
    parser.add_argument("--gpu", type=int, default=0)
    parser.add_argument("--mem-threshold", type=float, default=0.1,
                        help="Train loss threshold for memorization detection")
    parser.add_argument("--max-grad-norm", type=float, default=1.0,
                        help="Max gradient norm (0 to disable clipping)")
    parser.add_argument("--resume-from", type=str, default=None,
                        help="Resume from checkpoint (restores optimizer state too)")
    parser.add_argument("--load-weights", type=str, default=None,
                        help="Load model weights from checkpoint dir (fresh optimizer)")
    args = parser.parse_args()

    args.output.mkdir(parents=True, exist_ok=True)

    print(f"GPU: {torch.cuda.get_device_name(args.gpu)}")
    vram = torch.cuda.get_device_properties(args.gpu).total_memory / 1e9
    print(f"VRAM: {vram:.1f} GB")

    print(f"\n{'='*60}")
    print(f"FULL FINE-TUNING GROKKING EXPERIMENT")
    print(f"  Model: {args.base_model}")
    print(f"  Epochs: {args.epochs}")
    print(f"  LR: {args.lr}, WD: {args.weight_decay}")
    print(f"  Scheduler: {args.scheduler}, Warmup: {args.warmup_ratio}")
    print(f"  NO LoRA — all parameters trainable (fp16)")
    print(f"{'='*60}\n")

    # Load data
    records = load_jsonl(str(args.data))
    if args.val:
        val_records = load_jsonl(str(args.val))
        train_ds = Dataset.from_list(records).map(format_chat)
        val_ds = Dataset.from_list(val_records).map(format_chat)
    else:
        split = max(len(records) - len(records) // 5, 1)
        train_ds = Dataset.from_list(records[:split]).map(format_chat)
        val_ds = Dataset.from_list(records[split:]).map(format_chat)

    print(f"Data: {len(train_ds)} train, {len(val_ds)} val")

    # Load model — bf16, NO quantization, NO LoRA
    load_path = args.load_weights if args.load_weights else args.base_model
    print(f"Loading model from: {load_path}")
    model = AutoModelForCausalLM.from_pretrained(
        load_path,
        dtype=torch.bfloat16,
        device_map={"": args.gpu},
        trust_remote_code=True,
    )
    tokenizer = AutoTokenizer.from_pretrained(
        args.base_model, trust_remote_code=True, padding_side="right"
    )
    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token

    # Enable gradient checkpointing to save VRAM
    model.gradient_checkpointing_enable()

    total_params = sum(p.numel() for p in model.parameters())
    trainable = sum(p.numel() for p in model.parameters() if p.requires_grad)
    print(f"Parameters: {total_params:,} total, {trainable:,} trainable ({100*trainable/total_params:.1f}%)")
    print(f"GPU memory after load: {torch.cuda.memory_allocated(args.gpu)/1e9:.2f} GB")

    # Adaptive grokking callback
    log_path = args.output / "grok_log.jsonl"
    grok_cb = AdaptiveGrokCallback(
        abort_epoch=args.epochs + 50,
        train_loss_mem_threshold=args.mem_threshold,
        log_path=str(log_path),
    )

    training_args = SFTConfig(
        output_dir=str(args.output / "checkpoints"),
        num_train_epochs=args.epochs,
        per_device_train_batch_size=args.batch_size,
        per_device_eval_batch_size=2,
        gradient_accumulation_steps=args.grad_accum,
        learning_rate=args.lr,
        lr_scheduler_type=args.scheduler,
        warmup_ratio=args.warmup_ratio,
        weight_decay=args.weight_decay,
        logging_steps=5,
        eval_strategy="steps",
        eval_steps=25,
        save_strategy="steps",
        save_steps=500,
        save_total_limit=3,
        bf16=True,  # bf16 avoids fp16 gradient scaler issues
        gradient_checkpointing=True,
        gradient_checkpointing_kwargs={"use_reentrant": False},
        max_grad_norm=args.max_grad_norm if args.max_grad_norm > 0 else 1e9,
        report_to="none",
        max_length=MAX_SEQ_LEN,
        load_best_model_at_end=False,  # don't reload best — we want to see the full trajectory
        optim="adamw_torch",  # full precision optimizer for full fine-tuning
    )

    trainer = SFTTrainer(
        model=model,
        args=training_args,
        train_dataset=train_ds,
        eval_dataset=val_ds,
        processing_class=tokenizer,
        callbacks=[grok_cb],
    )

    print(f"\nStarting full fine-tuning...")
    print(f"  Grad clipping: {'OFF' if args.max_grad_norm <= 0 else args.max_grad_norm}")
    print(f"  Resume from: {args.resume_from or 'scratch'}")
    print(f"GPU memory before train: {torch.cuda.memory_allocated(args.gpu)/1e9:.2f} GB")
    start = time.time()

    trainer.train(resume_from_checkpoint=args.resume_from)

    elapsed = time.time() - start
    metrics = trainer.evaluate()

    # Save model (full weights, not adapter)
    model_path = str(args.output / "model")
    trainer.save_model(model_path)
    tokenizer.save_pretrained(model_path)
    print(f"\nFull model saved to {model_path}")

    # Summary
    summary = {
        "name": f"fullft-{args.output.name}",
        "elapsed_seconds": round(elapsed, 1),
        "final_eval_loss": round(metrics.get("eval_loss", -1), 4),
        "grok_phase": grok_cb.phase.name,
        "grokked": grok_cb.phase.name in ("GROKKING", "CONVERGED"),
        "t_mem": grok_cb.t_mem,
        "grok_step": grok_cb.grok_step,
        "interventions": len(grok_cb._interventions),
        "total_params": total_params,
        "trainable_params": trainable,
        "trainable_pct": 100.0,
        "config": {
            "epochs": args.epochs,
            "lr": args.lr,
            "weight_decay": args.weight_decay,
            "scheduler": args.scheduler,
            "batch_size": args.batch_size,
            "grad_accum": args.grad_accum,
            "lora": False,
            "base_model": args.base_model,
        },
    }

    with open(args.output / "summary.json", "w") as f:
        json.dump(summary, f, indent=2)

    print(f"\n{'='*60}")
    print(f"FULL FINE-TUNING COMPLETE")
    print(f"  Time: {elapsed/60:.1f} min")
    print(f"  Final eval loss: {summary['final_eval_loss']}")
    print(f"  Grok phase: {summary['grok_phase']}")
    print(f"  Grokked: {summary['grokked']}")
    if grok_cb.t_mem:
        print(f"  Memorization step: {grok_cb.t_mem}")
    if grok_cb.grok_step:
        print(f"  Grok step: {grok_cb.grok_step}")
    print(f"  Interventions: {summary['interventions']}")
    print(f"{'='*60}")


if __name__ == "__main__":
    main()
