#!/usr/bin/env python3
"""Fine-tune LFM2.5-1.2B as an intent router using Unsloth QLoRA.

This script trains the model to classify user messages into intents
(coding, reasoning, home_automation, gaming, default) for Odin's
hybrid SDR+LLM routing pipeline.

Prerequisites:
  - Unsloth installed (see Sprint 053 setup on Morrigan)
  - Training data from prepare_data.py (--synthetic or --input)
  - GPU with >= 4GB VRAM (RTX 3060 12GB recommended)

Usage:
  # On Morrigan (after stopping llama-server):
  sudo systemctl stop morrigan-llama-server.service
  cd ~/fine-tuning && source venv/bin/activate
  python train_unsloth.py --data training_data.jsonl --output ./router-lora

  # After training, export to GGUF:
  python train_unsloth.py --export ./router-lora --gguf-output ./router.gguf
"""

import argparse
from pathlib import Path


def train(data_path: Path, output_dir: Path, epochs: int = 3, lr: float = 2e-4):
    """Run QLoRA fine-tuning on LFM2.5-1.2B."""
    from unsloth import FastLanguageModel
    from trl import SFTTrainer, SFTConfig
    from datasets import load_dataset

    model, tokenizer = FastLanguageModel.from_pretrained(
        model_name="LiquidAI/LFM2.5-1.2B-Instruct",
        max_seq_length=512,
        load_in_4bit=True,
    )

    model = FastLanguageModel.get_peft_model(
        model,
        r=16,
        target_modules=[
            "q_proj", "k_proj", "v_proj", "o_proj",
            "gate_proj", "up_proj", "down_proj",
        ],
        lora_alpha=16,
        lora_dropout=0,
        use_gradient_checkpointing="unsloth",
    )

    dataset = load_dataset("json", data_files=str(data_path), split="train")

    trainable = sum(p.numel() for p in model.parameters() if p.requires_grad)
    total = sum(p.numel() for p in model.parameters())
    print(f"Trainable: {trainable:,} / {total:,} ({100 * trainable / total:.2f}%)")

    trainer = SFTTrainer(
        model=model,
        tokenizer=tokenizer,
        train_dataset=dataset,
        args=SFTConfig(
            output_dir=str(output_dir),
            per_device_train_batch_size=4,
            gradient_accumulation_steps=4,
            num_train_epochs=epochs,
            learning_rate=lr,
            warmup_steps=5,
            logging_steps=1,
            save_strategy="epoch",
            fp16=True,
            optim="adamw_8bit",
            seed=42,
        ),
    )

    trainer.train()
    model.save_pretrained(output_dir)
    tokenizer.save_pretrained(output_dir)
    print(f"LoRA adapter saved to {output_dir}")


def export_gguf(lora_dir: Path, gguf_output: Path):
    """Merge LoRA adapter and export to GGUF for Ollama deployment."""
    from unsloth import FastLanguageModel

    model, tokenizer = FastLanguageModel.from_pretrained(
        model_name=str(lora_dir),
        max_seq_length=512,
        load_in_4bit=True,
    )

    model.save_pretrained_gguf(
        str(gguf_output.parent),
        tokenizer,
        quantization_method="q4_k_m",
    )
    print(f"GGUF exported to {gguf_output.parent}")


def main():
    parser = argparse.ArgumentParser(description="Fine-tune LFM2.5-1.2B router with Unsloth")
    parser.add_argument("--data", type=Path, help="Training data JSONL")
    parser.add_argument("--output", type=Path, default=Path("./router-lora"))
    parser.add_argument("--epochs", type=int, default=3)
    parser.add_argument("--lr", type=float, default=2e-4)
    parser.add_argument("--export", type=Path, help="LoRA dir to export as GGUF")
    parser.add_argument("--gguf-output", type=Path, default=Path("./router.gguf"))
    args = parser.parse_args()

    if args.export:
        export_gguf(args.export, args.gguf_output)
    elif args.data:
        train(args.data, args.output, args.epochs, args.lr)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
