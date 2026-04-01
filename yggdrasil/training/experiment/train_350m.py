#!/usr/bin/env python3
"""LFM2-350M Grokking Experiment.

Fast iteration on a small model to determine if grokking is achievable
on LFM hybrid conv/attention architecture. Runs 4 configs sequentially,
logs all metrics for comparison.

Usage:
    # Phase 2: Baseline SFT (Liquid defaults)
    python3 train_350m.py --data data/saga_train.jsonl --mode baseline

    # Phase 3: Grokking sweep (runs A/B/C/D sequentially)
    python3 train_350m.py --data data/saga_train.jsonl --mode sweep

    # Single grokking run with custom params
    python3 train_350m.py --data data/saga_train.jsonl --mode single \
        --epochs 50 --lr 2e-4 --weight-decay 0.1 --scheduler constant
"""
import argparse
import json
import os
import sys
import time
from dataclasses import dataclass
from pathlib import Path

import torch
from datasets import Dataset
from peft import LoraConfig, get_peft_model, prepare_model_for_kbit_training
from transformers import AutoModelForCausalLM, AutoTokenizer, BitsAndBytesConfig
from trl import SFTTrainer, SFTConfig

# Add parent for adaptive_grok import
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lib.adaptive_grok import AdaptiveGrokCallback


BASE_MODEL = "LiquidAI/LFM2-350M"
MAX_SEQ_LEN = 512  # 350M is small, keep sequences short


@dataclass
class RunConfig:
    """Single experiment configuration."""
    name: str
    epochs: int
    lr: float
    weight_decay: float
    scheduler: str  # "constant", "linear", "cosine"
    warmup_ratio: float
    optimizer: str  # "adamw_8bit", "adamw", "grokfast"
    lora_rank: int = 8
    lora_alpha: int = 16
    batch_size: int = 4
    grad_accum: int = 4


# Pre-defined experiment configs
BASELINE_CONFIG = RunConfig(
    name="baseline-sft",
    epochs=3,
    lr=2e-4,
    weight_decay=0.01,
    scheduler="linear",
    warmup_ratio=0.2,
    optimizer="adamw_8bit",
)

SWEEP_CONFIGS = [
    # WD search around 0.1 — all using the winning liquid-internal base
    RunConfig(
        name="wd-005",
        epochs=50,
        lr=3e-5,
        weight_decay=0.05,
        scheduler="cosine",
        warmup_ratio=0.1,
        optimizer="adamw",
    ),
    RunConfig(
        name="wd-008",
        epochs=50,
        lr=3e-5,
        weight_decay=0.08,
        scheduler="cosine",
        warmup_ratio=0.1,
        optimizer="adamw",
    ),
    RunConfig(
        name="wd-012",
        epochs=50,
        lr=3e-5,
        weight_decay=0.12,
        scheduler="cosine",
        warmup_ratio=0.1,
        optimizer="adamw",
    ),
    RunConfig(
        name="wd-015",
        epochs=50,
        lr=3e-5,
        weight_decay=0.15,
        scheduler="linear",
        warmup_ratio=0.2,
        optimizer="adamw_8bit",
    ),
]


def load_jsonl(path: str) -> list[dict]:
    with open(path) as f:
        return [json.loads(line) for line in f if line.strip()]


def format_chat(example: dict) -> dict:
    parts = [
        f"<|im_start|>{m['role']}\n{m['content']}<|im_end|>"
        for m in example["messages"]
    ]
    return {"text": "\n".join(parts)}


def load_model_and_tokenizer(device_idx: int = 0):
    """Load 350M in 4-bit QLoRA."""
    bnb = BitsAndBytesConfig(
        load_in_4bit=True,
        bnb_4bit_quant_type="nf4",
        bnb_4bit_compute_dtype=torch.bfloat16,
        bnb_4bit_use_double_quant=True,
    )
    model = AutoModelForCausalLM.from_pretrained(
        BASE_MODEL,
        quantization_config=bnb,
        device_map={"": device_idx},
        dtype=torch.bfloat16,
        trust_remote_code=True,
    )
    tokenizer = AutoTokenizer.from_pretrained(
        BASE_MODEL, trust_remote_code=True, padding_side="right"
    )
    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token
    return model, tokenizer


def detect_lora_targets(model) -> list[str]:
    """Auto-detect Linear modules for LoRA (excluding lm_head)."""
    targets = sorted({
        name.split(".")[-1]
        for name, module in model.named_modules()
        if isinstance(module, torch.nn.Linear) and name.split(".")[-1] != "lm_head"
    })
    return targets


def run_experiment(
    cfg: RunConfig,
    train_ds: Dataset,
    val_ds: Dataset,
    output_base: Path,
    device_idx: int = 0,
) -> dict:
    """Run a single training experiment. Returns summary dict."""
    output_dir = output_base / cfg.name
    output_dir.mkdir(parents=True, exist_ok=True)

    print(f"\n{'='*60}")
    print(f"EXPERIMENT: {cfg.name}")
    print(f"  Epochs={cfg.epochs} LR={cfg.lr} WD={cfg.weight_decay}")
    print(f"  Scheduler={cfg.scheduler} Optimizer={cfg.optimizer}")
    print(f"  LoRA r={cfg.lora_rank} alpha={cfg.lora_alpha}")
    print(f"{'='*60}")

    # Load fresh model for each run
    model, tokenizer = load_model_and_tokenizer(device_idx)
    targets = detect_lora_targets(model)
    print(f"  LoRA targets: {targets}")

    model = prepare_model_for_kbit_training(model)
    model = get_peft_model(model, LoraConfig(
        r=cfg.lora_rank,
        lora_alpha=cfg.lora_alpha,
        target_modules=targets,
        lora_dropout=0.1,
        bias="none",
        task_type="CAUSAL_LM",
    ))

    trainable = sum(p.numel() for p in model.parameters() if p.requires_grad)
    total = sum(p.numel() for p in model.parameters())
    print(f"  Trainable: {trainable:,} / {total:,} ({100*trainable/total:.2f}%)")

    # Adaptive grokking callback
    log_path = output_dir / "grok_log.jsonl"
    grok_cb = AdaptiveGrokCallback(
        abort_epoch=cfg.epochs + 100,  # don't abort early in experiments
        train_loss_mem_threshold=0.1,  # more realistic than 0.05
        log_path=str(log_path),
    )

    training_args = SFTConfig(
        output_dir=str(output_dir / "checkpoints"),
        num_train_epochs=cfg.epochs,
        per_device_train_batch_size=cfg.batch_size,
        per_device_eval_batch_size=2,
        gradient_accumulation_steps=cfg.grad_accum,
        learning_rate=cfg.lr,
        lr_scheduler_type=cfg.scheduler,
        warmup_ratio=cfg.warmup_ratio,
        weight_decay=cfg.weight_decay,
        logging_steps=5,
        eval_strategy="steps",
        eval_steps=25,
        save_strategy="steps",
        save_steps=500,
        save_total_limit=2,
        bf16=True,
        gradient_checkpointing=True,
        max_grad_norm=1.0,
        report_to="none",
        max_length=MAX_SEQ_LEN,
        load_best_model_at_end=True,
        metric_for_best_model="eval_loss",
        greater_is_better=False,
        optim="adamw_8bit" if cfg.optimizer == "adamw_8bit" else "adamw_torch",
    )

    trainer = SFTTrainer(
        model=model,
        args=training_args,
        train_dataset=train_ds,
        eval_dataset=val_ds,
        processing_class=tokenizer,
        callbacks=[grok_cb],
    )

    start = time.time()
    trainer.train()
    elapsed = time.time() - start

    # Final eval
    metrics = trainer.evaluate()

    # Save adapter
    adapter_path = str(output_dir / "adapter")
    model.save_pretrained(adapter_path)
    tokenizer.save_pretrained(adapter_path)

    # Build summary
    summary = {
        "name": cfg.name,
        "elapsed_seconds": round(elapsed, 1),
        "final_train_loss": round(trainer.state.log_history[-2].get("loss", -1), 4)
            if len(trainer.state.log_history) >= 2 else -1,
        "final_eval_loss": round(metrics.get("eval_loss", -1), 4),
        "grok_phase": grok_cb.phase.name,
        "grokked": grok_cb.phase.name in ("GROKKING", "CONVERGED"),
        "t_mem": grok_cb.t_mem,
        "grok_step": grok_cb.grok_step,
        "interventions": len(grok_cb._interventions),
        "config": {
            "epochs": cfg.epochs,
            "lr": cfg.lr,
            "weight_decay": cfg.weight_decay,
            "scheduler": cfg.scheduler,
            "optimizer": cfg.optimizer,
            "lora_rank": cfg.lora_rank,
        },
    }

    with open(output_dir / "summary.json", "w") as f:
        json.dump(summary, f, indent=2)

    print(f"\n  Result: phase={summary['grok_phase']}, "
          f"train={summary['final_train_loss']}, eval={summary['final_eval_loss']}, "
          f"time={elapsed:.0f}s")

    # Cleanup GPU memory
    del model, trainer
    torch.cuda.empty_cache()

    return summary


def main():
    parser = argparse.ArgumentParser(description="LFM2-350M Grokking Experiment")
    parser.add_argument("--data", type=Path, required=True,
                        help="Path to training JSONL (saga_train.jsonl)")
    parser.add_argument("--val", type=Path, default=None,
                        help="Path to validation JSONL (auto-split if omitted)")
    parser.add_argument("--output", type=Path, default=Path("output-350m-experiment"),
                        help="Output directory for all runs")
    parser.add_argument("--mode", choices=["baseline", "sweep", "single", "all"],
                        default="all", help="Which experiments to run")
    parser.add_argument("--gpu", type=int, default=0, help="GPU index")
    # Single-run overrides
    parser.add_argument("--epochs", type=int, default=50)
    parser.add_argument("--lr", type=float, default=2e-4)
    parser.add_argument("--weight-decay", type=float, default=0.1)
    parser.add_argument("--scheduler", default="constant")
    parser.add_argument("--optimizer", default="adamw_8bit",
                        choices=["adamw", "adamw_8bit"])
    args = parser.parse_args()

    print(f"GPU: {torch.cuda.get_device_name(args.gpu)}")
    print(f"VRAM: {torch.cuda.get_device_properties(args.gpu).total_mem / 1e9:.1f} GB"
          if hasattr(torch.cuda.get_device_properties(args.gpu), 'total_mem')
          else f"VRAM: {torch.cuda.get_device_properties(args.gpu).total_memory / 1e9:.1f} GB")

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

    args.output.mkdir(parents=True, exist_ok=True)
    all_summaries = []

    if args.mode in ("baseline", "all"):
        summary = run_experiment(BASELINE_CONFIG, train_ds, val_ds, args.output, args.gpu)
        all_summaries.append(summary)

    if args.mode in ("sweep", "all"):
        for cfg in SWEEP_CONFIGS:
            summary = run_experiment(cfg, train_ds, val_ds, args.output, args.gpu)
            all_summaries.append(summary)

    if args.mode == "single":
        cfg = RunConfig(
            name=f"single-wd{args.weight_decay}-{args.scheduler}",
            epochs=args.epochs,
            lr=args.lr,
            weight_decay=args.weight_decay,
            scheduler=args.scheduler,
            warmup_ratio=0.03 if args.scheduler == "constant" else 0.2,
            optimizer=args.optimizer,
        )
        summary = run_experiment(cfg, train_ds, val_ds, args.output, args.gpu)
        all_summaries.append(summary)

    # Print comparison table
    if len(all_summaries) > 1:
        print(f"\n{'='*80}")
        print(f"EXPERIMENT COMPARISON")
        print(f"{'='*80}")
        print(f"{'Name':<25} {'Train':>8} {'Eval':>8} {'Phase':<15} {'Grokked':>7} {'Time':>6}")
        print(f"{'-'*25} {'-'*8} {'-'*8} {'-'*15} {'-'*7} {'-'*6}")
        for s in all_summaries:
            print(f"{s['name']:<25} {s['final_train_loss']:>8.4f} "
                  f"{s['final_eval_loss']:>8.4f} {s['grok_phase']:<15} "
                  f"{'YES' if s['grokked'] else 'no':>7} "
                  f"{s['elapsed_seconds']/60:>5.1f}m")
        print(f"{'='*80}")

    # Save combined results
    with open(args.output / "all_results.json", "w") as f:
        json.dump(all_summaries, f, indent=2)
    print(f"\nResults saved to {args.output / 'all_results.json'}")


if __name__ == "__main__":
    main()
