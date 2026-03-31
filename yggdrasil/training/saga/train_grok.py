#!/usr/bin/env python3
"""Grokking + Bayesian LoRA fine-tune for Saga.

Grokking recipe: many epochs, high weight decay, low LR.
Bayesian: MC Dropout in LoRA layers for uncertainty estimation.

Usage:
    python3 train_grok.py [--epochs 60] [--batch-size 2] [--lr 5e-5]
"""

import argparse
import json
import os
from pathlib import Path

import torch
from datasets import Dataset
from peft import LoraConfig, get_peft_model, prepare_model_for_kbit_training
from transformers import (
    AutoModelForCausalLM,
    AutoTokenizer,
    BitsAndBytesConfig,
)
from trl import SFTTrainer, SFTConfig

BARN = os.path.dirname(os.path.abspath(__file__))
BASE_MODEL = "LiquidAI/LFM2.5-1.2B-Instruct"
MAX_SEQ_LEN = 512


def load_jsonl(path):
    records = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            records.append(json.loads(line))
    return records


def format_chat(example):
    msgs = example["messages"]
    parts = []
    for msg in msgs:
        parts.append(f"<|im_start|>{msg['role']}\n{msg['content']}<|im_end|>")
    return {"text": "\n".join(parts)}


from transformers import TrainerCallback

class GrokCallback(TrainerCallback):
    """Log grokking-relevant metrics: train vs val loss divergence."""

    def __init__(self):
        self.train_losses = []
        self.eval_losses = []
        self.grokked = False

    def on_log(self, args, state, control, logs=None, **kwargs):
        if logs and "loss" in logs:
            self.train_losses.append(logs["loss"])
        if logs and "eval_loss" in logs:
            self.eval_losses.append(logs["eval_loss"])
            # Detect grokking: train loss low but eval loss suddenly drops
            if len(self.eval_losses) >= 3:
                recent = self.eval_losses[-3:]
                if recent[-1] < recent[0] * 0.8 and not self.grokked:
                    print(f"\n*** GROKKING DETECTED: eval loss dropped {recent[0]:.3f} → {recent[-1]:.3f} ***\n")
                    self.grokked = True


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--epochs", type=int, default=100,
                        help="Many epochs for grokking (100+ recommended)")
    parser.add_argument("--batch-size", type=int, default=2)
    parser.add_argument("--grad-accum", type=int, default=8)
    parser.add_argument("--lr", type=float, default=2e-4,
                        help="Constant LR for grokking (2e-4 research-backed)")
    parser.add_argument("--weight-decay", type=float, default=0.5,
                        help="High weight decay for grokking (0.1-1.0)")
    parser.add_argument("--lora-rank", type=int, default=32,
                        help="Rank 32 — memorize then compress (Sprint 054 research)")
    parser.add_argument("--lora-alpha", type=int, default=64)
    parser.add_argument("--lora-dropout", type=float, default=0.1,
                        help="MC Dropout rate for Bayesian inference")
    args = parser.parse_args()

    print(f"GPU: {torch.cuda.get_device_name(0)}")
    print(f"VRAM: {torch.cuda.get_device_properties(0).total_memory / 1e9:.1f} GB")
    print(f"\n=== GROKKING CONFIG ===")
    print(f"  Epochs: {args.epochs} (grokking needs many)")
    print(f"  LR: {args.lr} (low for grokking)")
    print(f"  Weight decay: {args.weight_decay} (high for grokking)")
    print(f"  LoRA rank: {args.lora_rank} (small for compression)")
    print(f"  LoRA dropout: {args.lora_dropout} (for Bayesian MC Dropout)")

    # Load datasets
    train_path = f"{BARN}/data/saga_train.jsonl"
    val_path = f"{BARN}/data/saga_val.jsonl"

    train_records = load_jsonl(train_path)
    val_records = load_jsonl(val_path)
    print(f"\n  Train: {len(train_records)} examples")
    print(f"  Val: {len(val_records)} examples")

    train_ds = Dataset.from_list(train_records).map(format_chat)
    val_ds = Dataset.from_list(val_records).map(format_chat)

    # 4-bit quantization
    bnb_config = BitsAndBytesConfig(
        load_in_4bit=True,
        bnb_4bit_quant_type="nf4",
        bnb_4bit_compute_dtype=torch.bfloat16,
        bnb_4bit_use_double_quant=True,
    )

    print(f"\nLoading {BASE_MODEL}...")
    model = AutoModelForCausalLM.from_pretrained(
        BASE_MODEL,
        quantization_config=bnb_config,
        device_map="auto",
        torch_dtype=torch.bfloat16,
        trust_remote_code=True,
    )
    tokenizer = AutoTokenizer.from_pretrained(
        BASE_MODEL,
        trust_remote_code=True,
        padding_side="right",
    )
    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token

    # Auto-detect LoRA targets
    target_modules = set()
    for name, module in model.named_modules():
        if isinstance(module, torch.nn.Linear):
            short = name.split(".")[-1]
            if short != "lm_head":
                target_modules.add(short)
    target_modules = sorted(target_modules)
    print(f"LoRA targets: {target_modules}")

    # LoRA with dropout for Bayesian MC Dropout
    lora_config = LoraConfig(
        r=args.lora_rank,
        lora_alpha=args.lora_alpha,
        target_modules=target_modules,
        lora_dropout=args.lora_dropout,  # Key: enables MC Dropout
        bias="none",
        task_type="CAUSAL_LM",
    )

    model = prepare_model_for_kbit_training(model)
    model = get_peft_model(model, lora_config)

    trainable = sum(p.numel() for p in model.parameters() if p.requires_grad)
    total = sum(p.numel() for p in model.parameters())
    print(f"Trainable params: {trainable:,} / {total:,} ({100*trainable/total:.2f}%)")

    output_dir = f"{BARN}/checkpoints-grok"
    training_args = SFTConfig(
        output_dir=output_dir,
        num_train_epochs=args.epochs,
        per_device_train_batch_size=args.batch_size,
        per_device_eval_batch_size=1,
        gradient_accumulation_steps=args.grad_accum,
        learning_rate=args.lr,
        lr_scheduler_type="constant",  # constant LR — don't decay before grokking transition
        warmup_ratio=0.03,
        weight_decay=args.weight_decay,  # High for grokking
        logging_steps=10,
        eval_strategy="steps",
        eval_steps=100,
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

    grok_cb = GrokCallback()

    trainer = SFTTrainer(
        model=model,
        args=training_args,
        train_dataset=train_ds,
        eval_dataset=val_ds,
        processing_class=tokenizer,
        callbacks=[grok_cb],
    )

    print(f"\n=== Starting Grokking Training ===")
    print(f"  Effective batch: {args.batch_size * args.grad_accum}")
    print(f"  Steps: ~{len(train_records) * args.epochs // (args.batch_size * args.grad_accum)}")
    print(f"  Expected: overfit → plateau → sudden generalization")
    print()

    trainer.train()

    # Save best adapter
    adapter_path = f"{BARN}/adapters/saga-grok-lora"
    model.save_pretrained(adapter_path)
    tokenizer.save_pretrained(adapter_path)
    print(f"\nAdapter saved to {adapter_path}")

    # Final eval
    metrics = trainer.evaluate()
    print(f"\nFinal eval loss: {metrics['eval_loss']:.4f}")
    print(f"Grokking detected: {grok_cb.grokked}")

    # === Bayesian Inference Demo ===
    print("\n=== Bayesian MC Dropout Inference Demo ===")
    model.train()  # Enable dropout for MC sampling

    test_input = "<|im_start|>system\nYou are Saga, Yggdrasil's memory engine. Respond ONLY in valid JSON.<|im_end|>\n<|im_start|>user\nCLASSIFY\ntool: Edit\nfile: src/main.rs\ncontent: Fixed session timeout bug<|im_end|>\n<|im_start|>assistant\n"
    inputs = tokenizer(test_input, return_tensors="pt").to(model.device)

    N_SAMPLES = 5
    outputs = []
    for i in range(N_SAMPLES):
        with torch.no_grad():
            out = model.generate(**inputs, max_new_tokens=60, do_sample=True, temperature=0.1)
        text = tokenizer.decode(out[0][inputs["input_ids"].shape[1]:], skip_special_tokens=True)
        outputs.append(text)
        print(f"  Sample {i+1}: {text[:100]}")

    # Measure agreement
    unique = len(set(o[:50] for o in outputs))
    print(f"\n  Unique outputs (of {N_SAMPLES}): {unique}")
    print(f"  Agreement: {100*(1 - unique/N_SAMPLES):.0f}% (higher = more confident)")

    print("\nDone! Next: merge + export + deploy")


if __name__ == "__main__":
    main()
