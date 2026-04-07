#!/usr/bin/env python3
"""Forced Generalization Grokking — Pure Math on LFM2-350M (FULL fine-tune).

Hypothesis: If memorization is structurally impossible (infinite non-repeating
data + aggressive weight decay), the model's ONLY path to reducing loss is
learning the underlying mathematical rules — skipping the memorization phase
entirely and forcing direct generalization.

Key differences from standard grokking:
  - Traditional: finite data → memorize → plateau → phase transition → generalize
  - This: infinite data → can't memorize → MUST generalize or loss stays high

CRITICAL: Uses FULL fine-tuning (no LoRA). All 20+ prior LoRA grokking attempts
failed because the phase transition requires restructuring base model
representations, which LoRA cannot do (base weights frozen). 350M in fp16 is
only ~700MB — fits trivially on RTX 3060 12GB with full optimizer state.

Memory budget (bf16, AdamW):
  - Weights: 350M × 2B = 700MB
  - Gradients: 350M × 2B = 700MB
  - Optimizer (m+v): 350M × 4B × 2 = 2.8GB
  - Total: ~4.2GB (fits in 12GB with room for activations)

Design:
  - MathStreamDataset: infinite unique problems, never repeats
  - max_steps instead of epochs (no concept of "epoch" with infinite data)
  - Aggressive weight decay (0.3-0.5) prevents weight crystallization
  - Fixed eval set (500 problems) measures true generalization
  - Exact-match accuracy as primary metric (not just loss)
  - OOD eval: train on 2-3 digit, test includes 4-5 digit operands

Usage:
    # Single run with defaults (recommended first run)
    python3 train_forced_grok.py --gpu 0

    # Sweep weight decay values
    python3 train_forced_grok.py --mode sweep --gpu 0

    # Custom single run
    python3 train_forced_grok.py --mode single --max-steps 5000 \
        --weight-decay 0.4 --lr 3e-5 --gpu 0

    # Full sweep (baseline + all WD configs)
    python3 train_forced_grok.py --mode all --gpu 0
"""

import argparse
import json
import os
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

import torch
from datasets import Dataset, IterableDataset as HFIterableDataset
from transformers import (
    AutoModelForCausalLM,
    AutoTokenizer,
    TrainerCallback,
    TrainerControl,
    TrainerState,
    TrainingArguments,
)
from trl import SFTTrainer, SFTConfig

# Local imports
sys.path.insert(0, str(Path(__file__).resolve().parent))
from math_data import (
    MathStreamDataset,
    LLMMathStreamDataset,
    generate_eval_set,
    format_chat_text,
)

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lib.adaptive_grok import AdaptiveGrokCallback


DEFAULT_BASE_MODEL = "LiquidAI/LFM2-350M"
MAX_SEQ_LEN = 256  # Math problems are short — save memory


@dataclass
class ForcedGrokConfig:
    """Experiment configuration."""
    name: str
    max_steps: int
    lr: float
    weight_decay: float
    scheduler: str  # "cosine", "constant", "linear"
    warmup_steps: int
    batch_size: int = 8
    grad_accum: int = 4
    eval_steps: int = 100
    logging_steps: int = 10
    save_steps: int = 1000
    difficulty: int = 0  # 0=mixed, 1-3=fixed
    data_seed: int = 42


# ── Experiment Configs ────��─────────────────────────────────────

# Baseline: standard SFT weight decay for comparison
BASELINE_CONFIG = ForcedGrokConfig(
    name="baseline-wd01",
    max_steps=3000,
    lr=3e-5,
    weight_decay=0.1,
    scheduler="cosine",
    warmup_steps=100,
)

# Forced generalization sweep: aggressive weight decay
SWEEP_CONFIGS = [
    ForcedGrokConfig(
        name="forced-wd03",
        max_steps=5000,
        lr=3e-5,
        weight_decay=0.3,
        scheduler="cosine",
        warmup_steps=200,
    ),
    ForcedGrokConfig(
        name="forced-wd04",
        max_steps=5000,
        lr=3e-5,
        weight_decay=0.4,
        scheduler="cosine",
        warmup_steps=200,
    ),
    ForcedGrokConfig(
        name="forced-wd05",
        max_steps=5000,
        lr=3e-5,
        weight_decay=0.5,
        scheduler="cosine",
        warmup_steps=200,
    ),
    # Higher LR + high WD — more aggressive exploration
    ForcedGrokConfig(
        name="forced-wd04-lr1e4",
        max_steps=5000,
        lr=1e-4,
        weight_decay=0.4,
        scheduler="cosine",
        warmup_steps=300,
    ),
    # Constant LR — no decay, pure WD pressure
    ForcedGrokConfig(
        name="forced-wd04-const",
        max_steps=5000,
        lr=3e-5,
        weight_decay=0.4,
        scheduler="constant",
        warmup_steps=100,
    ),
]


# ── Exact-Match Accuracy Callback ──��───────────────────────────

class AccuracyEvalCallback(TrainerCallback):
    """Periodically evaluate exact-match accuracy on the fixed eval set.

    This is the TRUE metric — can the model produce the correct number?
    Loss alone doesn't tell us if the model learned math rules.
    """

    def __init__(
        self,
        eval_problems: list[dict],
        tokenizer,
        eval_every_steps: int = 500,
        max_new_tokens: int = 32,
        log_path: Optional[Path] = None,
    ):
        self.eval_problems = eval_problems
        self.tokenizer = tokenizer
        self.eval_every_steps = eval_every_steps
        self.max_new_tokens = max_new_tokens
        self.log_path = log_path
        self.accuracy_history: list[dict] = []

    def _build_prompt(self, problem: dict) -> str:
        """Build generation prompt (everything except assistant response)."""
        text = problem["text"]
        # Strip the assistant response — we want the model to generate it
        marker = "<|im_start|>assistant\n"
        idx = text.rfind(marker)
        if idx >= 0:
            return text[:idx + len(marker)]
        return text

    def _evaluate(self, model, step: int) -> dict:
        """Run exact-match evaluation."""
        model.eval()
        correct = 0
        correct_id = 0
        correct_ood = 0
        total_id = 0
        total_ood = 0
        by_operation = {}  # track accuracy per operation type
        errors = []

        for problem in self.eval_problems:
            prompt = self._build_prompt(problem)
            inputs = self.tokenizer(
                prompt, return_tensors="pt", truncation=True, max_length=MAX_SEQ_LEN
            ).to(model.device)

            with torch.no_grad():
                outputs = model.generate(
                    **inputs,
                    max_new_tokens=self.max_new_tokens,
                    do_sample=False,
                    temperature=1.0,
                    pad_token_id=self.tokenizer.pad_token_id,
                )

            generated = self.tokenizer.decode(
                outputs[0][inputs["input_ids"].shape[1]:],
                skip_special_tokens=True,
            ).strip()

            expected = problem["answer"]
            gen_clean = generated.split("<|im_end|>")[0].strip()
            match = gen_clean == expected

            # Track per-operation accuracy
            op = problem.get("metadata", {}).get("operation", "unknown")
            if op not in by_operation:
                by_operation[op] = {"correct": 0, "total": 0}
            by_operation[op]["total"] += 1
            if match:
                by_operation[op]["correct"] += 1
                correct += 1

            is_ood = problem.get("ood", False)
            if is_ood:
                total_ood += 1
                if match:
                    correct_ood += 1
            else:
                total_id += 1
                if match:
                    correct_id += 1

            if not match and len(errors) < 10:
                errors.append({
                    "question": problem.get("question", ""),
                    "expected": expected,
                    "got": gen_clean[:100],
                    "operation": op,
                    "ood": is_ood,
                })

        total = len(self.eval_problems)
        op_breakdown = {
            op: round(d["correct"] / d["total"] * 100, 1) if d["total"] else 0
            for op, d in sorted(by_operation.items())
        }

        result = {
            "step": step,
            "accuracy": round(correct / total * 100, 1) if total else 0,
            "correct": correct,
            "total": total,
            "accuracy_id": round(correct_id / total_id * 100, 1) if total_id else 0,
            "accuracy_ood": round(correct_ood / total_ood * 100, 1) if total_ood else 0,
            "by_operation": op_breakdown,
            "sample_errors": errors[:5],
        }

        self.accuracy_history.append(result)
        model.train()
        return result

    def on_step_end(self, args, state: TrainerState, control: TrainerControl,
                    model=None, **kwargs):
        if state.global_step % self.eval_every_steps != 0:
            return
        if model is None:
            return

        result = self._evaluate(model, state.global_step)

        print(f"\n{'─'*60}")
        print(f"  ACCURACY @ step {result['step']}: "
              f"{result['accuracy']:.1f}% ({result['correct']}/{result['total']})")
        print(f"  In-distribution: {result['accuracy_id']:.1f}%  |  "
              f"OOD (harder): {result['accuracy_ood']:.1f}%")
        print(f"  By operation: {result['by_operation']}")
        if result["sample_errors"]:
            print(f"  Sample errors:")
            for err in result["sample_errors"][:3]:
                print(f"    [{err['operation']}] Q: {err['question'][:50]}")
                print(f"    Expected: {err['expected']}  Got: {err['got']}")
        print(f"{'─'*60}\n")

        if self.log_path:
            with open(self.log_path, "a") as f:
                f.write(json.dumps(result) + "\n")

    def on_train_end(self, args, state, control, model=None, **kwargs):
        if model is None:
            return
        result = self._evaluate(model, state.global_step)
        print(f"\n{'='*60}")
        print(f"  FINAL ACCURACY: {result['accuracy']:.1f}% "
              f"({result['correct']}/{result['total']})")
        print(f"  In-distribution: {result['accuracy_id']:.1f}%  |  "
              f"OOD: {result['accuracy_ood']:.1f}%")
        print(f"  By operation: {result['by_operation']}")
        print(f"{'='*60}\n")


# ── Model Loading ──────���────────────────────────────────────────

def load_model_and_tokenizer(device_idx: int = 0, base_model: str = DEFAULT_BASE_MODEL):
    """Load model in bf16 for FULL fine-tuning.

    Accepts either a HuggingFace model ID or a local checkpoint path
    (for resuming training from a saved model).
    """
    model = AutoModelForCausalLM.from_pretrained(
        base_model,
        torch_dtype=torch.bfloat16,
        device_map={"": device_idx},
        trust_remote_code=True,
    )
    tokenizer = AutoTokenizer.from_pretrained(
        base_model, trust_remote_code=True, padding_side="right"
    )
    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token

    # Enable gradient computation on all parameters
    for param in model.parameters():
        param.requires_grad = True

    return model, tokenizer


# ── Finite Wrapper for HF Trainer ──────────────────────────────

def make_hf_stream(stream, length: int) -> HFIterableDataset:
    """Wrap our streaming dataset as an HF IterableDataset.

    trl 0.24+ SFTTrainer requires .map() and .column_names, which only
    HuggingFace's IterableDataset provides. We use from_generator() to
    bridge our infinite stream into the HF ecosystem.
    """
    def _gen():
        count = 0
        for item in stream:
            if count >= length:
                break
            yield {"text": item["text"]}
            count += 1

    return HFIterableDataset.from_generator(_gen)


# ── Weight Norm Tracker ────��────────────────────────────────────

class WeightNormCallback(TrainerCallback):
    """Track total model weight norm over training.

    With aggressive weight decay on full fine-tuning, we expect weight norms
    to be controlled (not exploding). This helps diagnose if WD is too high
    (norms collapse → model forgets everything) or too low (norms grow →
    memorization possible).
    """

    def __init__(self, log_path: Optional[Path] = None, log_every: int = 100):
        self.log_path = log_path
        self.log_every = log_every
        self.norm_history: list[dict] = []

    def on_step_end(self, args, state: TrainerState, control, model=None, **kwargs):
        if state.global_step % self.log_every != 0 or model is None:
            return

        total_norm = 0.0
        n_params = 0
        for p in model.parameters():
            if p.requires_grad:
                total_norm += p.data.float().norm().item() ** 2
                n_params += p.numel()
        total_norm = total_norm ** 0.5

        entry = {
            "step": state.global_step,
            "weight_norm": round(total_norm, 4),
            "n_params": n_params,
        }
        self.norm_history.append(entry)

        if state.global_step % (self.log_every * 5) == 0:
            print(f"  [WeightNorm] step={state.global_step} norm={total_norm:.2f}")

        if self.log_path:
            with open(self.log_path, "a") as f:
                f.write(json.dumps(entry) + "\n")


# ── Training ────────���───────────────────────────────────────────

def run_experiment(
    cfg: ForcedGrokConfig,
    output_base: Path,
    device_idx: int = 0,
    eval_problems: Optional[list[dict]] = None,
    llm_endpoint: Optional[str] = None,
    llm_model: Optional[str] = None,
    base_model: str = DEFAULT_BASE_MODEL,
) -> dict:
    """Run a single forced-generalization experiment with full fine-tuning."""
    output_dir = output_base / cfg.name
    output_dir.mkdir(parents=True, exist_ok=True)

    data_source = "LLM-backed" if llm_endpoint else "Python-only"
    print(f"\n{'='*60}")
    print(f"FORCED GENERALIZATION EXPERIMENT: {cfg.name}")
    print(f"  MODE: FULL fine-tuning (no LoRA, no quantization)")
    print(f"  max_steps={cfg.max_steps} LR={cfg.lr} WD={cfg.weight_decay}")
    print(f"  scheduler={cfg.scheduler} warmup={cfg.warmup_steps}")
    print(f"  batch={cfg.batch_size} grad_accum={cfg.grad_accum}")
    print(f"  effective_batch={cfg.batch_size * cfg.grad_accum}")
    print(f"  DATA: {data_source}, infinite stream, never repeats")
    if llm_endpoint:
        print(f"  LLM: {llm_model} @ {llm_endpoint}")
    print(f"{'='*60}")

    # Load model (full bf16, all params trainable)
    print(f"  base_model={base_model}")
    model, tokenizer = load_model_and_tokenizer(device_idx, base_model=base_model)

    trainable = sum(p.numel() for p in model.parameters() if p.requires_grad)
    total = sum(p.numel() for p in model.parameters())
    print(f"  Trainable: {trainable:,} / {total:,} ({100*trainable/total:.2f}%)")
    print(f"  Memory: ~{total * 2 / 1e9:.1f}GB weights, "
          f"~{total * 8 / 1e9:.1f}GB total (w/ optimizer + grads)")

    # Infinite streaming training data
    if llm_endpoint:
        math_stream = LLMMathStreamDataset(
            endpoint=llm_endpoint,
            model=llm_model or "qwen3-coder:30b-a3b-q4_K_M",
            batch_size=20,
            buffer_size=200,
            temperature=0.9,
            seed=cfg.data_seed,
        )
    else:
        math_stream = MathStreamDataset(difficulty=cfg.difficulty, seed=cfg.data_seed)
    stream_length = cfg.max_steps * cfg.batch_size * cfg.grad_accum * 2
    train_ds = make_hf_stream(math_stream, stream_length)

    # Fixed eval set
    if eval_problems is None:
        eval_problems = generate_eval_set(n=500, seed=9999)

    eval_ds = Dataset.from_list([format_chat_text({"messages": p["messages"]})
                                  for p in generate_eval_set(n=200, seed=8888)])

    # Callbacks
    callbacks = []

    # Adaptive grok phase detector
    grok_log = output_dir / "grok_log.jsonl"
    grok_cb = AdaptiveGrokCallback(
        abort_epoch=9999,
        train_loss_mem_threshold=0.5,  # higher for math (diverse problem types)
        log_path=str(grok_log),
    )
    callbacks.append(grok_cb)

    # Exact-match accuracy
    acc_log = output_dir / "accuracy_log.jsonl"
    acc_cb = AccuracyEvalCallback(
        eval_problems=eval_problems,
        tokenizer=tokenizer,
        eval_every_steps=500,
        log_path=acc_log,
    )
    callbacks.append(acc_cb)

    # Weight norm tracker (important for full fine-tuning + high WD)
    norm_log = output_dir / "weight_norm_log.jsonl"
    norm_cb = WeightNormCallback(log_path=norm_log, log_every=100)
    callbacks.append(norm_cb)

    training_args = SFTConfig(
        output_dir=str(output_dir / "checkpoints"),
        max_steps=cfg.max_steps,
        per_device_train_batch_size=cfg.batch_size,
        per_device_eval_batch_size=4,
        gradient_accumulation_steps=cfg.grad_accum,
        learning_rate=cfg.lr,
        lr_scheduler_type=cfg.scheduler,
        warmup_steps=cfg.warmup_steps,
        weight_decay=cfg.weight_decay,
        logging_steps=cfg.logging_steps,
        eval_strategy="steps",
        eval_steps=cfg.eval_steps,
        save_strategy="steps",
        save_steps=cfg.save_steps,
        save_total_limit=5,
        bf16=True,
        gradient_checkpointing=True,
        max_grad_norm=1.0,
        report_to="none",
        max_length=MAX_SEQ_LEN,
        optim="adamw_torch",
        dataloader_num_workers=0,
        remove_unused_columns=False,
    )

    trainer = SFTTrainer(
        model=model,
        args=training_args,
        train_dataset=train_ds,
        eval_dataset=eval_ds,
        processing_class=tokenizer,
        callbacks=callbacks,
    )

    start = time.time()
    trainer.train()
    elapsed = time.time() - start

    metrics = trainer.evaluate()

    # Save full model (not adapter — this is full fine-tuning)
    save_path = str(output_dir / "model")
    model.save_pretrained(save_path)
    tokenizer.save_pretrained(save_path)

    # Build summary
    final_acc = acc_cb.accuracy_history[-1] if acc_cb.accuracy_history else {}
    final_norm = norm_cb.norm_history[-1] if norm_cb.norm_history else {}
    summary = {
        "name": cfg.name,
        "mode": "full_finetune",
        "elapsed_seconds": round(elapsed, 1),
        "elapsed_minutes": round(elapsed / 60, 1),
        "max_steps": cfg.max_steps,
        "total_params": total,
        "trainable_params": trainable,
        "final_eval_loss": round(metrics.get("eval_loss", -1), 4),
        "final_accuracy": final_acc.get("accuracy", 0),
        "final_accuracy_id": final_acc.get("accuracy_id", 0),
        "final_accuracy_ood": final_acc.get("accuracy_ood", 0),
        "final_by_operation": final_acc.get("by_operation", {}),
        "final_weight_norm": final_norm.get("weight_norm", 0),
        "grok_phase": grok_cb.phase.name,
        "config": {
            "lr": cfg.lr,
            "weight_decay": cfg.weight_decay,
            "scheduler": cfg.scheduler,
            "warmup_steps": cfg.warmup_steps,
            "batch_size": cfg.batch_size,
            "grad_accum": cfg.grad_accum,
        },
        "accuracy_history": [
            {"step": a["step"], "acc": a["accuracy"],
             "id": a["accuracy_id"], "ood": a["accuracy_ood"]}
            for a in acc_cb.accuracy_history
        ],
    }

    with open(output_dir / "summary.json", "w") as f:
        json.dump(summary, f, indent=2)

    print(f"\n  RESULT: {cfg.name}")
    print(f"    Mode:      FULL fine-tuning ({trainable:,} params)")
    print(f"    Eval loss:  {summary['final_eval_loss']}")
    print(f"    Accuracy:   {summary['final_accuracy']:.1f}% "
          f"(ID: {summary['final_accuracy_id']:.1f}%, OOD: {summary['final_accuracy_ood']:.1f}%)")
    print(f"    By op:      {summary['final_by_operation']}")
    print(f"    Weight norm: {summary['final_weight_norm']:.2f}")
    print(f"    Time:       {elapsed/60:.1f} min")
    print(f"    Model:      {save_path}")

    del model, trainer
    torch.cuda.empty_cache()

    return summary


# ── Main ��───────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="Forced Generalization Grokking — Pure Math on LFM2-350M (Full FT)"
    )
    parser.add_argument("--mode", choices=["baseline", "sweep", "single", "all"],
                        default="single", help="Experiment mode")
    parser.add_argument("--gpu", type=int, default=0, help="GPU index")
    parser.add_argument("--output", type=Path, default=Path("output-forced-grok"),
                        help="Output directory")
    parser.add_argument("--max-steps", type=int, default=5000)
    parser.add_argument("--lr", type=float, default=3e-5)
    parser.add_argument("--weight-decay", type=float, default=0.4)
    parser.add_argument("--scheduler", default="cosine",
                        choices=["cosine", "constant", "linear"])
    parser.add_argument("--warmup-steps", type=int, default=200)
    parser.add_argument("--batch-size", type=int, default=8)
    parser.add_argument("--grad-accum", type=int, default=4)
    parser.add_argument("--difficulty", type=int, default=0,
                        help="0=mixed, 1=easy, 2=medium, 3=hard")
    parser.add_argument("--eval-steps", type=int, default=100)
    parser.add_argument("--acc-eval-steps", type=int, default=500,
                        help="Steps between accuracy evaluations (expensive)")
    parser.add_argument("--data-seed", type=int, default=42)
    parser.add_argument("--save-steps", type=int, default=1000,
                        help="Save checkpoint every N steps")
    parser.add_argument("--base-model", type=str, default=DEFAULT_BASE_MODEL,
                        help="Base model (HF ID or local checkpoint path for resume)")
    # LLM-backed data generation
    parser.add_argument("--llm-endpoint", type=str, default=None,
                        help="LLM endpoint for live question generation "
                        "(e.g. http://10.0.65.8:11434/v1/chat/completions)")
    parser.add_argument("--llm-model", type=str, default="qwen3-coder:30b-a3b-q4_K_M",
                        help="Model name for LLM generation")
    args = parser.parse_args()

    os.environ.setdefault("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True")

    print(f"GPU: {torch.cuda.get_device_name(args.gpu)}")
    mem = torch.cuda.get_device_properties(args.gpu).total_memory
    print(f"VRAM: {mem / 1e9:.1f} GB")
    print(f"MODE: FULL fine-tuning (all 350M params trainable)")
    if args.llm_endpoint:
        print(f"DATA: LLM-backed ({args.llm_model} @ {args.llm_endpoint})")
    else:
        print(f"DATA: Python-only synthetic")

    # Generate fixed eval set ONCE
    print("\nGenerating fixed eval set (500 problems, 20% OOD)...")
    eval_problems = generate_eval_set(n=500, seed=9999, include_ood=True)
    n_ood = sum(1 for p in eval_problems if p.get("ood"))
    print(f"  {len(eval_problems)} problems ({len(eval_problems) - n_ood} ID, {n_ood} OOD)")

    args.output.mkdir(parents=True, exist_ok=True)
    all_summaries = []

    llm_kw = dict(llm_endpoint=args.llm_endpoint, llm_model=args.llm_model,
                  base_model=args.base_model)

    if args.mode in ("baseline", "all"):
        s = run_experiment(BASELINE_CONFIG, args.output, args.gpu, eval_problems, **llm_kw)
        all_summaries.append(s)

    if args.mode in ("sweep", "all"):
        for cfg in SWEEP_CONFIGS:
            s = run_experiment(cfg, args.output, args.gpu, eval_problems, **llm_kw)
            all_summaries.append(s)

    if args.mode == "single":
        cfg = ForcedGrokConfig(
            name=f"forced-wd{args.weight_decay}-{args.scheduler}-lr{args.lr}",
            max_steps=args.max_steps,
            lr=args.lr,
            weight_decay=args.weight_decay,
            scheduler=args.scheduler,
            warmup_steps=args.warmup_steps,
            batch_size=args.batch_size,
            grad_accum=args.grad_accum,
            eval_steps=args.eval_steps,
            save_steps=args.save_steps,
            difficulty=args.difficulty,
            data_seed=args.data_seed,
        )
        s = run_experiment(cfg, args.output, args.gpu, eval_problems, **llm_kw)
        all_summaries.append(s)

    # Comparison table
    if len(all_summaries) > 1:
        print(f"\n{'='*95}")
        print(f"FORCED GENERALIZATION — FULL FINE-TUNING COMPARISON")
        print(f"{'='*95}")
        print(f"{'Name':<28} {'Loss':>7} {'Acc%':>6} {'ID%':>6} {'OOD%':>6} "
              f"{'WNorm':>7} {'Phase':<15} {'Time':>6}")
        print(f"{'-'*28} {'-'*7} {'-'*6} {'-'*6} {'-'*6} "
              f"{'-'*7} {'-'*15} {'-'*6}")
        for s in all_summaries:
            print(f"{s['name']:<28} {s['final_eval_loss']:>7.4f} "
                  f"{s['final_accuracy']:>5.1f}% "
                  f"{s['final_accuracy_id']:>5.1f}% "
                  f"{s['final_accuracy_ood']:>5.1f}% "
                  f"{s['final_weight_norm']:>7.2f} "
                  f"{s['grok_phase']:<15} "
                  f"{s['elapsed_minutes']:>5.1f}m")
        print(f"{'='*95}")

    with open(args.output / "all_results.json", "w") as f:
        json.dump(all_summaries, f, indent=2)
    print(f"\nResults saved to {args.output / 'all_results.json'}")


if __name__ == "__main__":
    main()
