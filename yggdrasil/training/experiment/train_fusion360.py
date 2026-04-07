#!/usr/bin/env python3
"""Forced Generalization — Fusion 360 Code Generation on LFM2.5-1.2B.

Train a specialist that generates Fusion 360 Python API scripts from
natural language shape descriptions. Same approach as the math experiment:
infinite non-repeating synthetic data + aggressive weight decay + full
fine-tuning to force generalization.

Model: LFM2.5-1.2B-Base (full fine-tuning, bf16)
Memory budget with 8-bit Adam:
  - Weights: 1.2B × 2B = 2.4GB
  - Gradients: 1.2B × 2B = 2.4GB
  - Optimizer (8-bit m+v): 1.2B × 4B = 4.8GB
  - Total: ~9.6GB (fits in 12GB with gradient checkpointing)

Usage:
    # Single run with defaults
    python3 train_fusion360.py --gpu 0

    # Custom config
    python3 train_fusion360.py --gpu 0 --max-steps 10000 \
        --weight-decay 0.5 --lr 1e-4 --batch-size 2

    # Resume from checkpoint
    python3 train_fusion360.py --gpu 0 --base-model ./output/model
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
)
from trl import SFTTrainer, SFTConfig

# Local imports
sys.path.insert(0, str(Path(__file__).resolve().parent))
from fusion360_data import (
    Fusion360StreamDataset,
    generate_fusion_eval_set,
    format_chat_text,
)

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lib.adaptive_grok import AdaptiveGrokCallback

DEFAULT_BASE_MODEL = "LiquidAI/LFM2.5-1.2B-Base"
MAX_SEQ_LEN = 1024  # Fusion 360 scripts are longer than math answers


@dataclass
class FusionConfig:
    name: str
    max_steps: int
    lr: float
    weight_decay: float
    scheduler: str
    warmup_steps: int
    batch_size: int = 2       # Small batch — 1.2B is tight on 12GB
    grad_accum: int = 16      # Effective batch = 32
    eval_steps: int = 100
    logging_steps: int = 10
    save_steps: int = 1000
    difficulty: int = 0
    data_seed: int = 42


# ── Code Similarity Eval ────────────────────────────────────────

class CodeEvalCallback(TrainerCallback):
    """Evaluate generated Fusion 360 code quality.

    Metrics:
    - Syntax validity: does the generated code parse as Python?
    - API call accuracy: does it use correct Fusion 360 API patterns?
    - Structure match: correct workflow order (sketch → profile → feature)?
    - Dimension accuracy: are numerical values from the description in the code?
    """

    def __init__(
        self,
        eval_problems: list[dict],
        tokenizer,
        eval_every_steps: int = 500,
        max_new_tokens: int = 512,
        log_path: Optional[Path] = None,
    ):
        self.eval_problems = eval_problems
        self.tokenizer = tokenizer
        self.eval_every_steps = eval_every_steps
        self.max_new_tokens = max_new_tokens
        self.log_path = log_path
        self.history: list[dict] = []

    def _build_prompt(self, problem: dict) -> str:
        text = problem["text"]
        marker = "<|im_start|>assistant\n"
        idx = text.rfind(marker)
        if idx >= 0:
            return text[:idx + len(marker)]
        return text

    def _check_syntax(self, code: str) -> bool:
        """Check if the code is valid Python."""
        try:
            compile(code, "<string>", "exec")
            return True
        except SyntaxError:
            return False

    def _check_api_patterns(self, code: str) -> dict:
        """Check for key Fusion 360 API patterns in generated code."""
        patterns = {
            "has_import": "import adsk" in code,
            "has_run": "def run(" in code,
            "has_rootcomp": "rootComponent" in code or "rootComp" in code,
            "has_sketch": "sketches.add(" in code or "sketches.add (" in code,
            "has_profile": "profiles.item(" in code,
            "has_feature": any(f in code for f in [
                "extrudeFeatures", "revolveFeatures", "filletFeatures",
                "chamferFeatures", "shellFeatures", "combineFeatures",
                "circularPatternFeatures",
            ]),
            "has_try_except": "try:" in code and "except" in code,
        }
        return patterns

    def _check_dimensions(self, description: str, code: str) -> float:
        """Check what fraction of numbers in the description appear in the code."""
        import re
        desc_nums = set(re.findall(r'\d+\.?\d*', description))
        if not desc_nums:
            return 1.0
        found = sum(1 for n in desc_nums if n in code)
        return found / len(desc_nums)

    def _evaluate(self, model, step: int) -> dict:
        model.eval()
        n = len(self.eval_problems)
        syntax_ok = 0
        api_scores = []
        dim_scores = []
        by_operation = {}
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
                    pad_token_id=self.tokenizer.pad_token_id,
                )

            generated = self.tokenizer.decode(
                outputs[0][inputs["input_ids"].shape[1]:],
                skip_special_tokens=True,
            ).strip()
            gen_clean = generated.split("<|im_end|>")[0].strip()

            # Syntax check
            is_valid = self._check_syntax(gen_clean)
            if is_valid:
                syntax_ok += 1

            # API pattern check
            patterns = self._check_api_patterns(gen_clean)
            api_score = sum(patterns.values()) / len(patterns)
            api_scores.append(api_score)

            # Dimension check
            desc = problem.get("description", "")
            dim_score = self._check_dimensions(desc, gen_clean)
            dim_scores.append(dim_score)

            # Per-operation tracking
            op = problem.get("operation", "unknown")
            if op not in by_operation:
                by_operation[op] = {"syntax": 0, "api": [], "dims": [], "total": 0}
            by_operation[op]["total"] += 1
            if is_valid:
                by_operation[op]["syntax"] += 1
            by_operation[op]["api"].append(api_score)
            by_operation[op]["dims"].append(dim_score)

            if not is_valid and len(errors) < 5:
                errors.append({
                    "description": desc[:80],
                    "operation": op,
                    "generated": gen_clean[:200],
                })

        op_summary = {}
        for op, d in sorted(by_operation.items()):
            op_summary[op] = {
                "syntax": round(d["syntax"] / d["total"] * 100, 1) if d["total"] else 0,
                "api": round(sum(d["api"]) / len(d["api"]) * 100, 1) if d["api"] else 0,
                "dims": round(sum(d["dims"]) / len(d["dims"]) * 100, 1) if d["dims"] else 0,
            }

        result = {
            "step": step,
            "syntax_pct": round(syntax_ok / n * 100, 1) if n else 0,
            "api_score_pct": round(sum(api_scores) / n * 100, 1) if n else 0,
            "dim_score_pct": round(sum(dim_scores) / n * 100, 1) if n else 0,
            "by_operation": op_summary,
            "sample_errors": errors[:3],
        }

        self.history.append(result)
        model.train()
        return result

    def on_step_end(self, args, state: TrainerState, control: TrainerControl,
                    model=None, **kwargs):
        if state.global_step % self.eval_every_steps != 0 or model is None:
            return

        result = self._evaluate(model, state.global_step)

        print(f"\n{'─'*60}")
        print(f"  FUSION 360 EVAL @ step {result['step']}:")
        print(f"    Syntax valid:  {result['syntax_pct']:.1f}%")
        print(f"    API patterns:  {result['api_score_pct']:.1f}%")
        print(f"    Dimensions:    {result['dim_score_pct']:.1f}%")
        if result["by_operation"]:
            print(f"    By operation:")
            for op, scores in result["by_operation"].items():
                print(f"      {op:<16} syntax={scores['syntax']:>5.1f}% "
                      f"api={scores['api']:>5.1f}% dims={scores['dims']:>5.1f}%")
        if result["sample_errors"]:
            print(f"    Sample errors:")
            for err in result["sample_errors"][:2]:
                print(f"      [{err['operation']}] {err['description']}")
                print(f"        → {err['generated'][:100]}...")
        print(f"{'─'*60}\n")

        if self.log_path:
            with open(self.log_path, "a") as f:
                f.write(json.dumps(result) + "\n")

    def on_train_end(self, args, state, control, model=None, **kwargs):
        if model is None:
            return
        result = self._evaluate(model, state.global_step)
        print(f"\n{'='*60}")
        print(f"  FINAL FUSION 360 EVAL:")
        print(f"    Syntax valid:  {result['syntax_pct']:.1f}%")
        print(f"    API patterns:  {result['api_score_pct']:.1f}%")
        print(f"    Dimensions:    {result['dim_score_pct']:.1f}%")
        print(f"{'='*60}\n")


# ── Model Loading ───────────────────────────────────────────────

def load_model_and_tokenizer(device_idx: int = 0, base_model: str = DEFAULT_BASE_MODEL):
    """Load model in bf16 for full fine-tuning."""
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

    for param in model.parameters():
        param.requires_grad = True

    return model, tokenizer


# ── HF Stream Wrapper ──────────────────────────────────────────

def make_hf_stream(stream, length: int) -> HFIterableDataset:
    def _gen():
        count = 0
        for item in stream:
            if count >= length:
                break
            yield {"text": item["text"]}
            count += 1
    return HFIterableDataset.from_generator(_gen)


# ── Weight Norm Tracker ─────────────────────────────────────────

class WeightNormCallback(TrainerCallback):
    def __init__(self, log_path: Optional[Path] = None, log_every: int = 100):
        self.log_path = log_path
        self.log_every = log_every
        self.norm_history: list[dict] = []

    def on_step_end(self, args, state: TrainerState, control, model=None, **kwargs):
        if state.global_step % self.log_every != 0 or model is None:
            return
        total_norm = sum(
            p.data.float().norm().item() ** 2
            for p in model.parameters() if p.requires_grad
        ) ** 0.5
        entry = {"step": state.global_step, "weight_norm": round(total_norm, 4)}
        self.norm_history.append(entry)
        if state.global_step % (self.log_every * 5) == 0:
            print(f"  [WeightNorm] step={state.global_step} norm={total_norm:.2f}")
        if self.log_path:
            with open(self.log_path, "a") as f:
                f.write(json.dumps(entry) + "\n")


# ── Training ────────────────────────────────────────────────────

def run_experiment(
    cfg: FusionConfig,
    output_base: Path,
    device_idx: int = 0,
    eval_problems: Optional[list[dict]] = None,
    base_model: str = DEFAULT_BASE_MODEL,
) -> dict:
    output_dir = output_base / cfg.name
    output_dir.mkdir(parents=True, exist_ok=True)

    print(f"\n{'='*60}")
    print(f"FUSION 360 CODE GENERATION: {cfg.name}")
    print(f"  MODE: FULL fine-tuning (bf16, 8-bit Adam)")
    print(f"  base_model={base_model}")
    print(f"  max_steps={cfg.max_steps} LR={cfg.lr} WD={cfg.weight_decay}")
    print(f"  scheduler={cfg.scheduler} warmup={cfg.warmup_steps}")
    print(f"  batch={cfg.batch_size} grad_accum={cfg.grad_accum}")
    print(f"  effective_batch={cfg.batch_size * cfg.grad_accum}")
    print(f"  max_seq_len={MAX_SEQ_LEN}")
    print(f"  DATA: infinite stream, never repeats")
    print(f"{'='*60}")

    model, tokenizer = load_model_and_tokenizer(device_idx, base_model)

    trainable = sum(p.numel() for p in model.parameters() if p.requires_grad)
    total = sum(p.numel() for p in model.parameters())
    print(f"  Trainable: {trainable:,} / {total:,} ({100*trainable/total:.2f}%)")
    est_mem = total * 8 / 1e9  # bf16 weights + grads + 8bit optimizer
    print(f"  Estimated VRAM: ~{est_mem:.1f}GB (8-bit Adam)")

    # Infinite streaming data
    fusion_stream = Fusion360StreamDataset(difficulty=cfg.difficulty, seed=cfg.data_seed)
    stream_length = cfg.max_steps * cfg.batch_size * cfg.grad_accum * 2
    train_ds = make_hf_stream(fusion_stream, stream_length)

    # Fixed eval
    if eval_problems is None:
        eval_problems = generate_fusion_eval_set(n=200, seed=9999)

    eval_ds = Dataset.from_list([format_chat_text({"messages": p["messages"]})
                                  for p in generate_fusion_eval_set(n=50, seed=8888)])

    # Callbacks
    callbacks = []

    grok_cb = AdaptiveGrokCallback(
        abort_epoch=9999,
        train_loss_mem_threshold=0.5,
        log_path=str(output_dir / "grok_log.jsonl"),
    )
    callbacks.append(grok_cb)

    code_cb = CodeEvalCallback(
        eval_problems=eval_problems,
        tokenizer=tokenizer,
        eval_every_steps=500,
        max_new_tokens=512,
        log_path=output_dir / "code_eval_log.jsonl",
    )
    callbacks.append(code_cb)

    norm_cb = WeightNormCallback(
        log_path=output_dir / "weight_norm_log.jsonl",
        log_every=100,
    )
    callbacks.append(norm_cb)

    training_args = SFTConfig(
        output_dir=str(output_dir / "checkpoints"),
        max_steps=cfg.max_steps,
        per_device_train_batch_size=cfg.batch_size,
        per_device_eval_batch_size=1,
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
        optim="adamw_8bit",  # 8-bit Adam to fit 1.2B in 12GB
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

    save_path = str(output_dir / "model")
    model.save_pretrained(save_path)
    tokenizer.save_pretrained(save_path)

    final_eval = code_cb.history[-1] if code_cb.history else {}
    final_norm = norm_cb.norm_history[-1] if norm_cb.norm_history else {}

    summary = {
        "name": cfg.name,
        "mode": "full_finetune_8bit_adam",
        "base_model": base_model,
        "elapsed_minutes": round(elapsed / 60, 1),
        "max_steps": cfg.max_steps,
        "total_params": total,
        "final_eval_loss": round(metrics.get("eval_loss", -1), 4),
        "final_syntax_pct": final_eval.get("syntax_pct", 0),
        "final_api_score_pct": final_eval.get("api_score_pct", 0),
        "final_dim_score_pct": final_eval.get("dim_score_pct", 0),
        "final_weight_norm": final_norm.get("weight_norm", 0),
        "config": {
            "lr": cfg.lr,
            "weight_decay": cfg.weight_decay,
            "scheduler": cfg.scheduler,
            "batch_size": cfg.batch_size,
            "grad_accum": cfg.grad_accum,
        },
        "eval_history": code_cb.history,
    }

    with open(output_dir / "summary.json", "w") as f:
        json.dump(summary, f, indent=2)

    print(f"\n  RESULT: {cfg.name}")
    print(f"    Syntax:     {summary['final_syntax_pct']:.1f}%")
    print(f"    API score:  {summary['final_api_score_pct']:.1f}%")
    print(f"    Dimensions: {summary['final_dim_score_pct']:.1f}%")
    print(f"    Eval loss:  {summary['final_eval_loss']}")
    print(f"    Time:       {elapsed/60:.1f} min")
    print(f"    Model:      {save_path}")

    del model, trainer
    torch.cuda.empty_cache()
    return summary


# ── Main ────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="Fusion 360 Code Generation — LFM2.5-1.2B (Full FT)"
    )
    parser.add_argument("--gpu", type=int, default=0)
    parser.add_argument("--output", type=Path, default=Path("output-fusion360"))
    parser.add_argument("--max-steps", type=int, default=10000)
    parser.add_argument("--lr", type=float, default=1e-4)
    parser.add_argument("--weight-decay", type=float, default=0.5)
    parser.add_argument("--scheduler", default="constant",
                        choices=["cosine", "constant", "linear"])
    parser.add_argument("--warmup-steps", type=int, default=200)
    parser.add_argument("--batch-size", type=int, default=2)
    parser.add_argument("--grad-accum", type=int, default=16)
    parser.add_argument("--eval-steps", type=int, default=100)
    parser.add_argument("--save-steps", type=int, default=1000)
    parser.add_argument("--difficulty", type=int, default=0)
    parser.add_argument("--data-seed", type=int, default=42)
    parser.add_argument("--base-model", type=str, default=DEFAULT_BASE_MODEL)
    args = parser.parse_args()

    os.environ.setdefault("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True")

    print(f"GPU: {torch.cuda.get_device_name(args.gpu)}")
    mem = torch.cuda.get_device_properties(args.gpu).total_memory
    print(f"VRAM: {mem / 1e9:.1f} GB")
    print(f"MODE: FULL fine-tuning + 8-bit Adam (1.2B model)")

    print("\nGenerating eval set (200 problems)...")
    eval_problems = generate_fusion_eval_set(n=200, seed=9999)
    ops = {}
    for p in eval_problems:
        op = p.get("operation", "?")
        ops[op] = ops.get(op, 0) + 1
    print(f"  {len(eval_problems)} problems: {dict(sorted(ops.items()))}")

    args.output.mkdir(parents=True, exist_ok=True)

    cfg = FusionConfig(
        name=f"fusion-wd{args.weight_decay}-{args.scheduler}-lr{args.lr}",
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

    run_experiment(cfg, args.output, args.gpu, eval_problems, args.base_model)


if __name__ == "__main__":
    main()
