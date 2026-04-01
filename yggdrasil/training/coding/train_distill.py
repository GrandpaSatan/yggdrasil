#!/usr/bin/env python3
"""Progressive Distillation Fallback — if grokking fails.

Uses Qwen3.5-27B on Morrigan as teacher to generate soft labels,
then trains an LFM student to match the teacher's output distribution.
This is well-established and reliably produces specialized small models.

Workflow:
  1. For each training example, query teacher for soft logits / high-quality completion
  2. Train student via standard SFT on teacher's completions (3-5 epochs)
  3. No grokking needed — straightforward knowledge distillation

Usage:
    python train_distill.py --data ./data/review_train.jsonl --teacher-url http://${MORRIGAN_URL}
    python train_distill.py --data ./data/test_train.jsonl --student LiquidAI/LFM2.5-1.2B-Base
"""

import argparse
import json
import os
import sys
import time
from pathlib import Path

import requests

BARN = os.path.dirname(os.path.abspath(__file__))

TEACHER_MODEL = "Qwen3.5-27B-Q4_K_M.gguf"
TEACHER_URL = os.environ.get("MORRIGAN_URL", "http://localhost:8080")
STUDENT_MODEL = "LiquidAI/LFM2.5-1.2B-Base"
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


def distill_completions(data: list[dict], teacher_url: str, teacher_model: str,
                        output_path: Path) -> list[dict]:
    """Query teacher model for each example, replace assistant response."""
    distilled = []
    total = len(data)

    print(f"Distilling {total} examples via {teacher_model}...")
    for i, example in enumerate(data):
        messages = example["messages"]
        # Send system + user, get teacher's completion
        system_msg = next((m for m in messages if m["role"] == "system"), None)
        user_msg = next((m for m in messages if m["role"] == "user"), None)

        if not user_msg:
            continue

        api_messages = []
        if system_msg:
            api_messages.append(system_msg)
        api_messages.append(user_msg)

        try:
            resp = requests.post(
                f"{teacher_url}/v1/chat/completions",
                json={
                    "model": teacher_model,
                    "messages": api_messages,
                    "temperature": 0.1,
                    "max_tokens": 1024,
                    "chat_template_kwargs": {"enable_thinking": False},
                },
                timeout=120,
            )
            if resp.status_code != 200:
                print(f"  [{i+1}/{total}] SKIP (HTTP {resp.status_code})")
                continue

            msg = resp.json()["choices"][0]["message"]
            teacher_response = msg.get("content") or msg.get("reasoning_content") or ""
            if not teacher_response.strip():
                print(f"  [{i+1}/{total}] SKIP (empty response)")
                continue

            # Replace original assistant message with teacher's response
            distilled_messages = list(api_messages) + [
                {"role": "assistant", "content": teacher_response}
            ]
            distilled.append({"messages": distilled_messages})

            if (i + 1) % 10 == 0:
                print(f"  [{i+1}/{total}] distilled ({len(teacher_response)} chars)")

        except requests.RequestException as e:
            print(f"  [{i+1}/{total}] ERROR: {e}")
            continue

    # Save distilled data
    with open(output_path, "w") as f:
        for entry in distilled:
            f.write(json.dumps(entry, ensure_ascii=False) + "\n")

    print(f"  Distilled {len(distilled)}/{total} examples → {output_path}")
    return distilled


def train_student(data_path: Path, output_dir: Path, base_model: str,
                  epochs: int = 5, lr: float = 3e-5):
    """Standard SFT training on teacher-distilled data (no grokking)."""
    import torch
    from datasets import Dataset
    from peft import LoraConfig, get_peft_model, prepare_model_for_kbit_training
    from transformers import AutoModelForCausalLM, AutoTokenizer, BitsAndBytesConfig
    from trl import SFTTrainer, SFTConfig

    records = load_jsonl(str(data_path))
    print(f"Training on {len(records)} distilled examples")

    def format_chat(example):
        msgs = example["messages"]
        parts = [f"<|im_start|>{m['role']}\n{m['content']}<|im_end|>" for m in msgs]
        return {"text": "\n".join(parts)}

    dataset = Dataset.from_list(records).map(format_chat)

    bnb_config = BitsAndBytesConfig(
        load_in_4bit=True, bnb_4bit_quant_type="nf4",
        bnb_4bit_compute_dtype=torch.bfloat16, bnb_4bit_use_double_quant=True,
    )

    model = AutoModelForCausalLM.from_pretrained(
        base_model, quantization_config=bnb_config,
        device_map="auto", torch_dtype=torch.bfloat16, trust_remote_code=True,
    )
    tokenizer = AutoTokenizer.from_pretrained(
        base_model, trust_remote_code=True, padding_side="right",
    )
    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token

    target_modules = sorted({
        name.split(".")[-1]
        for name, mod in model.named_modules()
        if isinstance(mod, torch.nn.Linear) and name.split(".")[-1] != "lm_head"
    })

    model = prepare_model_for_kbit_training(model)
    model = get_peft_model(model, LoraConfig(
        r=8, lora_alpha=16, target_modules=target_modules,
        lora_dropout=0.1, bias="none", task_type="CAUSAL_LM",
    ))

    trainable = sum(p.numel() for p in model.parameters() if p.requires_grad)
    total = sum(p.numel() for p in model.parameters())
    print(f"Trainable: {trainable:,} / {total:,} ({100*trainable/total:.2f}%)")

    # Split for eval
    split = max(len(dataset) - len(dataset) // 5, 1)
    train_ds = dataset.select(range(split))
    eval_ds = dataset.select(range(split, len(dataset)))
    print(f"  Train: {len(train_ds)}, Eval: {len(eval_ds)}")

    trainer = SFTTrainer(
        model=model,
        args=SFTConfig(
            output_dir=str(output_dir / "checkpoints"),
            num_train_epochs=epochs,
            per_device_train_batch_size=4,
            gradient_accumulation_steps=4,
            learning_rate=lr,
            weight_decay=0.1,
            lr_scheduler_type="cosine",
            warmup_ratio=0.1,
            logging_steps=5,
            eval_strategy="epoch",
            save_strategy="epoch",
            save_total_limit=2,
            bf16=True,
            optim="adamw_8bit",
            max_grad_norm=1.0,
            max_length=MAX_SEQ_LEN,
            load_best_model_at_end=True,
            metric_for_best_model="eval_loss",
            greater_is_better=False,
            report_to="none",
        ),
        train_dataset=train_ds,
        eval_dataset=eval_ds,
        processing_class=tokenizer,
    )

    trainer.train()

    adapter_path = str(output_dir / "adapter")
    model.save_pretrained(adapter_path)
    tokenizer.save_pretrained(adapter_path)
    print(f"Adapter saved to {adapter_path}")


def main():
    parser = argparse.ArgumentParser(description="Progressive distillation fallback")
    parser.add_argument("--data", type=Path, required=True, help="Original training JSONL")
    parser.add_argument("--output", type=Path, default=Path(f"{BARN}/output-distill"))
    parser.add_argument("--teacher-url", default=TEACHER_URL)
    parser.add_argument("--teacher-model", default=TEACHER_MODEL)
    parser.add_argument("--student", default=STUDENT_MODEL)
    parser.add_argument("--epochs", type=int, default=5)
    parser.add_argument("--skip-distill", action="store_true",
                        help="Skip teacher distillation, train on existing distilled data")
    parser.add_argument("--distill-only", action="store_true",
                        help="Only distill, don't train")
    args = parser.parse_args()

    args.output.mkdir(parents=True, exist_ok=True)
    distilled_path = args.output / "distilled_train.jsonl"

    if not args.skip_distill:
        data = load_jsonl(str(args.data))
        distill_completions(data, args.teacher_url, args.teacher_model, distilled_path)

    if args.distill_only:
        print(f"Distill-only mode — skipping training. Data at {distilled_path}")
        return

    if distilled_path.exists():
        train_student(distilled_path, args.output, args.student, args.epochs)
    else:
        print(f"No distilled data at {distilled_path} — run without --skip-distill first")


if __name__ == "__main__":
    main()
