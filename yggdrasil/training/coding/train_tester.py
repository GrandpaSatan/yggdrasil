#!/usr/bin/env python3
"""Grok-train lfm-tester — Test Generation Specialist.

Same grokking approach as train_reviewer.py but targeting Rust test scaffolding.
Uses LFM2.5-1.2B-Base (smaller — test patterns are more repetitive than review).

Usage:
    python train_tester.py --data ./data/test_train.jsonl
    python train_tester.py --export ./adapters/tester-grok-lora --gguf-output ./tester.gguf
"""

import argparse
import json
import os
import sys
from pathlib import Path

import torch
from datasets import Dataset

sys.path.insert(0, str(Path(__file__).parent.parent))
from lib.adaptive_grok import AdaptiveGrokCallback

BARN = os.path.dirname(os.path.abspath(__file__))
DEFAULT_BASE_MODEL = "LiquidAI/LFM2.5-1.2B-Base"
MAX_SEQ_LEN = 2048


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
    msgs = example["messages"]
    parts = []
    for msg in msgs:
        parts.append(f"<|im_start|>{msg['role']}\n{msg['content']}<|im_end|>")
    return {"text": "\n".join(parts)}


def train(args):
    from peft import LoraConfig, get_peft_model, prepare_model_for_kbit_training
    from transformers import AutoModelForCausalLM, AutoTokenizer, BitsAndBytesConfig
    from trl import SFTTrainer, SFTConfig

    print(f"GPU: {torch.cuda.get_device_name(0)}")
    print(f"\n{'=' * 60}")
    print(f"GROKKING TRAINING: lfm-tester")
    print(f"  Base model:   {args.base_model}")
    print(f"  Epochs:        {args.epochs}")
    print(f"  LR:            {args.lr} (constant)")
    print(f"  Weight decay:  {args.weight_decay}")
    print(f"  LoRA rank:     {args.lora_rank}, alpha: {args.lora_alpha}")
    print(f"{'=' * 60}")

    records = load_jsonl(args.data)
    split = int(len(records) * 0.6)
    train_ds = Dataset.from_list(records[:split]).map(format_chat)
    val_ds = Dataset.from_list(records[split:]).map(format_chat)
    print(f"  Train: {split}, Val: {len(records) - split}")

    bnb_config = BitsAndBytesConfig(
        load_in_4bit=True,
        bnb_4bit_quant_type="nf4",
        bnb_4bit_compute_dtype=torch.bfloat16,
        bnb_4bit_use_double_quant=True,
    )

    model = AutoModelForCausalLM.from_pretrained(
        args.base_model, quantization_config=bnb_config,
        device_map="auto", torch_dtype=torch.bfloat16, trust_remote_code=True,
    )
    tokenizer = AutoTokenizer.from_pretrained(
        args.base_model, trust_remote_code=True, padding_side="right",
    )
    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token

    target_modules = sorted({
        name.split(".")[-1]
        for name, mod in model.named_modules()
        if isinstance(mod, torch.nn.Linear) and name.split(".")[-1] != "lm_head"
    })
    print(f"LoRA targets: {target_modules}")

    model = prepare_model_for_kbit_training(model)
    model = get_peft_model(model, LoraConfig(
        r=args.lora_rank, lora_alpha=args.lora_alpha,
        target_modules=target_modules, lora_dropout=0.1,
        bias="none", task_type="CAUSAL_LM",
    ))

    trainable = sum(p.numel() for p in model.parameters() if p.requires_grad)
    total = sum(p.numel() for p in model.parameters())
    print(f"Trainable: {trainable:,} / {total:,} ({100*trainable/total:.2f}%)")

    log_path = Path(args.output) / "grok_log.jsonl"
    grok_cb = AdaptiveGrokCallback(
        abort_epoch=args.abort_epoch,
        log_path=str(log_path),
    )

    trainer = SFTTrainer(
        model=model,
        args=SFTConfig(
            output_dir=str(Path(args.output) / "checkpoints"),
            num_train_epochs=args.epochs,
            per_device_train_batch_size=args.batch_size,
            per_device_eval_batch_size=1,
            gradient_accumulation_steps=args.grad_accum,
            learning_rate=args.lr,
            lr_scheduler_type="constant",
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
            max_length=MAX_SEQ_LEN,
            load_best_model_at_end=True,
            metric_for_best_model="eval_loss",
            greater_is_better=False,
        ),
        train_dataset=train_ds,
        eval_dataset=val_ds,
        processing_class=tokenizer,
        callbacks=[grok_cb],
    )

    trainer.train()

    adapter_path = str(Path(args.output) / "adapter")
    model.save_pretrained(adapter_path)
    tokenizer.save_pretrained(adapter_path)

    summary = grok_cb.summary()
    print(f"\n  Phase: {summary['phase']}, Grokked: {summary['grokked']}")
    print(f"  Adapter saved to: {adapter_path}")

    with open(Path(args.output) / "summary.json", "w") as f:
        json.dump(summary, f, indent=2)

    if summary["phase"] == "ABORT":
        print("  *** GROKKING FAILED — run train_distill.py ***")


def export_gguf(lora_dir: Path, gguf_output: Path):
    try:
        from unsloth import FastLanguageModel
        model, tokenizer = FastLanguageModel.from_pretrained(
            model_name=str(lora_dir), max_seq_length=MAX_SEQ_LEN, load_in_4bit=True,
        )
        model.save_pretrained_gguf(str(gguf_output.parent), tokenizer, quantization_method="q4_k_m")
        print(f"GGUF exported to {gguf_output.parent}")
    except ImportError:
        print("Unsloth not available — use llama.cpp convert manually")


def main():
    parser = argparse.ArgumentParser(description="Grok-train lfm-tester")
    parser.add_argument("--data", type=Path)
    parser.add_argument("--output", type=Path, default=Path(f"{BARN}/output-tester"))
    parser.add_argument("--base-model", default=DEFAULT_BASE_MODEL)
    parser.add_argument("--epochs", type=int, default=100)
    parser.add_argument("--lr", type=float, default=2e-4)
    parser.add_argument("--weight-decay", type=float, default=0.5)
    parser.add_argument("--lora-rank", type=int, default=32)
    parser.add_argument("--lora-alpha", type=int, default=64)
    parser.add_argument("--batch-size", type=int, default=2)
    parser.add_argument("--grad-accum", type=int, default=8)
    parser.add_argument("--abort-epoch", type=int, default=200)
    parser.add_argument("--export", type=Path)
    parser.add_argument("--gguf-output", type=Path, default=Path(f"{BARN}/tester.gguf"))
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
