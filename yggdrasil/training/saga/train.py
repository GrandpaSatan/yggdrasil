#!/usr/bin/env python3
"""Fine-tune LFM2.5-1.2B into Saga using QLoRA on RTX 2070S.

Usage:
    python3 train.py [--epochs 3] [--batch-size 4] [--lr 2e-4]
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

BARN = os.environ.get("BARN_DIR", os.path.dirname(os.path.abspath(__file__)))
BASE_MODEL = "LiquidAI/LFM2.5-1.2B-Instruct"
MAX_SEQ_LEN = 512


def load_jsonl(path):
    """Load JSONL chat-format dataset."""
    records = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            records.append(json.loads(line))
    return records


def format_chat(example):
    """Format messages into a single text string for SFT training."""
    msgs = example["messages"]
    parts = []
    for msg in msgs:
        role = msg["role"]
        content = msg["content"]
        parts.append(f"<|im_start|>{role}\n{content}<|im_end|>")
    return {"text": "\n".join(parts)}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--epochs", type=int, default=3)
    parser.add_argument("--batch-size", type=int, default=4)
    parser.add_argument("--grad-accum", type=int, default=4)
    parser.add_argument("--lr", type=float, default=2e-4)
    parser.add_argument("--lora-rank", type=int, default=32)
    parser.add_argument("--lora-alpha", type=int, default=64)
    args = parser.parse_args()

    print(f"GPU: {torch.cuda.get_device_name(0)}")
    print(f"VRAM: {torch.cuda.get_device_properties(0).total_memory / 1e9:.1f} GB")

    # Load datasets
    train_path = f"{BARN}/data/saga_train.jsonl"
    val_path = f"{BARN}/data/saga_val.jsonl"

    print(f"\nLoading training data from {train_path}...")
    train_records = load_jsonl(train_path)
    val_records = load_jsonl(val_path)
    print(f"  Train: {len(train_records)} examples")
    print(f"  Val: {len(val_records)} examples")

    train_ds = Dataset.from_list(train_records).map(format_chat)
    val_ds = Dataset.from_list(val_records).map(format_chat)

    # 4-bit quantization config
    bnb_config = BitsAndBytesConfig(
        load_in_4bit=True,
        bnb_4bit_quant_type="nf4",
        bnb_4bit_compute_dtype=torch.bfloat16,
        bnb_4bit_use_double_quant=True,
    )

    # Load model + tokenizer
    print(f"\nLoading {BASE_MODEL} with 4-bit quantization...")
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

    # LoRA config
    # Auto-detect LoRA target modules from model linear layers
    target_modules = set()
    for name, module in model.named_modules():
        if isinstance(module, torch.nn.Linear):
            # Extract the last part of the name (e.g. "q_proj", "gate_proj", "in_proj")
            short = name.split(".")[-1]
            if short in ("lm_head",):
                continue  # skip output head
            target_modules.add(short)
    target_modules = sorted(target_modules)
    print(f"LoRA target modules: {target_modules}")

    lora_config = LoraConfig(
        r=args.lora_rank,
        lora_alpha=args.lora_alpha,
        target_modules=target_modules,
        lora_dropout=0.05,
        bias="none",
        task_type="CAUSAL_LM",
    )

    model = prepare_model_for_kbit_training(model)
    model = get_peft_model(model, lora_config)

    trainable = sum(p.numel() for p in model.parameters() if p.requires_grad)
    total = sum(p.numel() for p in model.parameters())
    print(f"Trainable params: {trainable:,} / {total:,} ({100*trainable/total:.2f}%)")

    # Training args
    output_dir = f"{BARN}/checkpoints"
    training_args = SFTConfig(
        output_dir=output_dir,
        num_train_epochs=args.epochs,
        per_device_train_batch_size=args.batch_size,
        per_device_eval_batch_size=1,
        gradient_accumulation_steps=args.grad_accum,
        learning_rate=args.lr,
        lr_scheduler_type="cosine",
        warmup_ratio=0.05,
        weight_decay=0.01,
        logging_steps=10,
        eval_strategy="steps",
        eval_steps=50,
        save_strategy="steps",
        save_steps=100,
        save_total_limit=3,
        bf16=True,
        gradient_checkpointing=True,
        max_grad_norm=1.0,
        report_to="none",
        dataloader_pin_memory=False,
        max_length=MAX_SEQ_LEN,
    )

    # Trainer
    trainer = SFTTrainer(
        model=model,
        args=training_args,
        train_dataset=train_ds,
        eval_dataset=val_ds,
        processing_class=tokenizer,
    )

    print(f"\nStarting training...")
    print(f"  Epochs: {args.epochs}")
    print(f"  Batch size: {args.batch_size} x {args.grad_accum} accum = {args.batch_size * args.grad_accum} effective")
    print(f"  Learning rate: {args.lr}")
    print(f"  LoRA rank: {args.lora_rank}, alpha: {args.lora_alpha}")
    print(f"  Max seq len: {MAX_SEQ_LEN}")
    print()

    trainer.train()

    # Save final adapter
    adapter_path = f"{BARN}/adapters/saga-lora"
    model.save_pretrained(adapter_path)
    tokenizer.save_pretrained(adapter_path)
    print(f"\nAdapter saved to {adapter_path}")

    # Final eval
    metrics = trainer.evaluate()
    print(f"\nFinal eval loss: {metrics['eval_loss']:.4f}")

    print("\nDone! Next steps:")
    print(f"  1. python3 eval.py")
    print(f"  2. bash export_gguf.sh")
    print(f"  3. ollama create saga:0.6b -f Modelfile")


if __name__ == "__main__":
    main()
