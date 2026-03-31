#!/usr/bin/env python3
"""Grok-train lfm-reviewer — Code Review Specialist.

Extended training (50-200 epochs) with high weight decay to trigger grokking
on Yggdrasil code review patterns.  Uses the AdaptiveGrokCallback for
closed-loop hyperparameter adjustment.

Base model: LFM2-2.6B-Exp (RL-trained, stronger reasoning) or LFM2.5-1.2B-Base.

Prerequisites:
  - Unsloth installed on Morrigan: cd ~/fine-tuning && source venv/bin/activate
  - Training data: python prepare_code_data.py --crate-dir ../../crates --output-dir ./data
  - Stop llama-server: sudo systemctl stop morrigan-llama-server.service

Usage:
    python train_reviewer.py --data ./data/review_train.jsonl
    python train_reviewer.py --data ./data/review_train.jsonl --base-model LiquidAI/LFM2.5-1.2B-Base
    python train_reviewer.py --export ./adapters/reviewer-grok-lora --gguf-output ./reviewer.gguf
"""

import argparse
import json
import os
import sys
from pathlib import Path

import torch
from datasets import Dataset

# Add parent to path for adaptive_grok import
sys.path.insert(0, str(Path(__file__).parent.parent))
from lib.adaptive_grok import AdaptiveGrokCallback, GrokPhase

BARN = os.path.dirname(os.path.abspath(__file__))

# ─────────────────────────────────────────────────────────────────
# Defaults — research-backed grokking hyperparameters
# ─────────────────────────────────────────────────────────────────

DEFAULT_BASE_MODEL = "LiquidAI/LFM2-2.6B-Exp"
FALLBACK_BASE_MODEL = "LiquidAI/LFM2.5-1.2B-Base"
MAX_SEQ_LEN = 2048

# Grokking recipe (from research synthesis):
DEFAULT_EPOCHS = 100
DEFAULT_LR = 2e-4          # constant — don't decay before transition
DEFAULT_WEIGHT_DECAY = 0.5  # high — key grokking enabler
DEFAULT_LORA_RANK = 32      # higher than typical — memorize then compress
DEFAULT_LORA_ALPHA = 64     # 2x rank ratio
DEFAULT_LORA_DROPOUT = 0.1  # MC Dropout for Bayesian uncertainty
DEFAULT_BATCH_SIZE = 2
DEFAULT_GRAD_ACCUM = 8      # effective batch = 16


def load_jsonl(path: str) -> list[dict]:
    records = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            records.append(json.loads(line))
    return records


def format_chat(example: dict) -> dict:
    """Format messages into ChatML template for LFM models."""
    msgs = example["messages"]
    parts = []
    for msg in msgs:
        parts.append(f"<|im_start|>{msg['role']}\n{msg['content']}<|im_end|>")
    return {"text": "\n".join(parts)}


def train(args):
    """Run grokking training."""
    from peft import LoraConfig, get_peft_model, prepare_model_for_kbit_training
    from transformers import AutoModelForCausalLM, AutoTokenizer, BitsAndBytesConfig
    from trl import SFTTrainer, SFTConfig

    print(f"GPU: {torch.cuda.get_device_name(0)}")
    print(f"VRAM: {torch.cuda.get_device_properties(0).total_memory / 1e9:.1f} GB")
    print(f"\n{'=' * 60}")
    print(f"GROKKING TRAINING: lfm-reviewer")
    print(f"  Base model:   {args.base_model}")
    print(f"  Epochs:        {args.epochs}")
    print(f"  LR:            {args.lr} (constant — no cosine decay)")
    print(f"  Weight decay:  {args.weight_decay}")
    print(f"  LoRA rank:     {args.lora_rank}")
    print(f"  LoRA alpha:    {args.lora_alpha}")
    print(f"  LoRA dropout:  {args.lora_dropout} (MC Dropout)")
    print(f"  Abort epoch:   {args.abort_epoch}")
    print(f"{'=' * 60}")

    # Load data
    records = load_jsonl(args.data)
    split = int(len(records) * 0.6)  # 60% train, 40% val (grokking zone)
    train_records = records[:split]
    val_records = records[split:]
    print(f"\n  Train: {len(train_records)} examples")
    print(f"  Val:   {len(val_records)} examples")
    print(f"  Split: 60/40 (grokking optimal zone)")

    train_ds = Dataset.from_list(train_records).map(format_chat)
    val_ds = Dataset.from_list(val_records).map(format_chat)

    # 4-bit quantization (QLoRA — quantization noise aids grokking)
    bnb_config = BitsAndBytesConfig(
        load_in_4bit=True,
        bnb_4bit_quant_type="nf4",
        bnb_4bit_compute_dtype=torch.bfloat16,
        bnb_4bit_use_double_quant=True,
    )

    print(f"\nLoading {args.base_model}...")
    model = AutoModelForCausalLM.from_pretrained(
        args.base_model,
        quantization_config=bnb_config,
        device_map="auto",
        torch_dtype=torch.bfloat16,
        trust_remote_code=True,
    )
    tokenizer = AutoTokenizer.from_pretrained(
        args.base_model,
        trust_remote_code=True,
        padding_side="right",
    )
    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token

    # Auto-detect LoRA target modules
    target_modules = set()
    for name, module in model.named_modules():
        if isinstance(module, torch.nn.Linear):
            short = name.split(".")[-1]
            if short != "lm_head":
                target_modules.add(short)
    target_modules = sorted(target_modules)
    print(f"LoRA targets: {target_modules}")

    lora_config = LoraConfig(
        r=args.lora_rank,
        lora_alpha=args.lora_alpha,
        target_modules=target_modules,
        lora_dropout=args.lora_dropout,
        bias="none",
        task_type="CAUSAL_LM",
    )

    model = prepare_model_for_kbit_training(model)
    model = get_peft_model(model, lora_config)

    trainable = sum(p.numel() for p in model.parameters() if p.requires_grad)
    total = sum(p.numel() for p in model.parameters())
    print(f"Trainable params: {trainable:,} / {total:,} ({100*trainable/total:.2f}%)")

    # Adaptive grokking callback
    log_path = Path(args.output) / "grok_log.jsonl"
    grok_cb = AdaptiveGrokCallback(
        t_mem_patience=10,
        abort_epoch=args.abort_epoch,
        wd_increment=0.1,
        wd_max=1.5,
        lr_bump_factor=1.5,
        lr_grok_factor=0.5,
        train_loss_mem_threshold=0.05,
        grok_drop_threshold=0.20,
        convergence_patience=500,
        log_path=str(log_path),
    )

    training_args = SFTConfig(
        output_dir=str(Path(args.output) / "checkpoints"),
        num_train_epochs=args.epochs,
        per_device_train_batch_size=args.batch_size,
        per_device_eval_batch_size=1,
        gradient_accumulation_steps=args.grad_accum,
        learning_rate=args.lr,
        lr_scheduler_type="constant",  # NO decay — critical for grokking
        warmup_ratio=0.03,
        weight_decay=args.weight_decay,
        logging_steps=5,
        eval_strategy="steps",
        eval_steps=50,
        save_strategy="steps",
        save_steps=200,
        save_total_limit=3,
        bf16=True,
        gradient_checkpointing=True,
        max_grad_norm=1.0,
        report_to="none",
        dataloader_pin_memory=False,
        max_length=MAX_SEQ_LEN,
        load_best_model_at_end=True,
        metric_for_best_model="eval_loss",
        greater_is_better=False,
    )

    trainer = SFTTrainer(
        model=model,
        args=training_args,
        train_dataset=train_ds,
        eval_dataset=val_ds,
        processing_class=tokenizer,
        callbacks=[grok_cb],
    )

    eff_batch = args.batch_size * args.grad_accum
    total_steps = len(train_records) * args.epochs // eff_batch
    print(f"\n  Effective batch: {eff_batch}")
    print(f"  Total steps: ~{total_steps}")
    print(f"  Expected: memorize → plateau → (adaptive intervention) → grok")
    print()

    trainer.train()

    # Save adapter
    adapter_path = str(Path(args.output) / "adapter")
    model.save_pretrained(adapter_path)
    tokenizer.save_pretrained(adapter_path)

    # Report
    summary = grok_cb.summary()
    print(f"\n{'=' * 60}")
    print(f"TRAINING COMPLETE")
    print(f"  Phase:        {summary['phase']}")
    print(f"  Grokked:      {summary['grokked']}")
    print(f"  Grok step:    {summary['grok_step']}")
    print(f"  Interventions: {summary['interventions']}")
    print(f"  Adapter:      {adapter_path}")
    print(f"  Grok log:     {log_path}")

    if summary["phase"] == "ABORT":
        print(f"\n  *** GROKKING FAILED — run train_distill.py for fallback ***")
    print(f"{'=' * 60}")

    # Save summary
    with open(Path(args.output) / "summary.json", "w") as f:
        json.dump(summary, f, indent=2)


def export_gguf(lora_dir: Path, gguf_output: Path):
    """Merge LoRA adapter and export to GGUF for Ollama."""
    try:
        from unsloth import FastLanguageModel
        model, tokenizer = FastLanguageModel.from_pretrained(
            model_name=str(lora_dir),
            max_seq_length=MAX_SEQ_LEN,
            load_in_4bit=True,
        )
        model.save_pretrained_gguf(
            str(gguf_output.parent),
            tokenizer,
            quantization_method="q4_k_m",
        )
        print(f"GGUF exported to {gguf_output.parent}")
    except ImportError:
        # Fallback: manual merge + llama.cpp convert
        print("Unsloth not available — using manual merge + llama.cpp conversion")
        print("Steps:")
        print(f"  1. Merge adapter: python -m peft.merge {lora_dir}")
        print(f"  2. Convert: python llama.cpp/convert_hf_to_gguf.py merged/ --outtype q4_k_m")


def main():
    parser = argparse.ArgumentParser(description="Grok-train lfm-reviewer")
    parser.add_argument("--data", type=Path, help="Training JSONL")
    parser.add_argument("--output", type=Path, default=Path(f"{BARN}/output-reviewer"))
    parser.add_argument("--base-model", default=DEFAULT_BASE_MODEL)
    parser.add_argument("--epochs", type=int, default=DEFAULT_EPOCHS)
    parser.add_argument("--lr", type=float, default=DEFAULT_LR)
    parser.add_argument("--weight-decay", type=float, default=DEFAULT_WEIGHT_DECAY)
    parser.add_argument("--lora-rank", type=int, default=DEFAULT_LORA_RANK)
    parser.add_argument("--lora-alpha", type=int, default=DEFAULT_LORA_ALPHA)
    parser.add_argument("--lora-dropout", type=float, default=DEFAULT_LORA_DROPOUT)
    parser.add_argument("--batch-size", type=int, default=DEFAULT_BATCH_SIZE)
    parser.add_argument("--grad-accum", type=int, default=DEFAULT_GRAD_ACCUM)
    parser.add_argument("--abort-epoch", type=int, default=200)
    parser.add_argument("--export", type=Path, help="LoRA dir to export as GGUF")
    parser.add_argument("--gguf-output", type=Path, default=Path(f"{BARN}/reviewer.gguf"))
    args = parser.parse_args()

    if args.export:
        export_gguf(args.export, args.gguf_output)
    elif args.data:
        args.output.mkdir(parents=True, exist_ok=True)
        train(args)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
