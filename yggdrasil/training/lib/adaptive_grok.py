"""Adaptive Grokking Controller — closed-loop training optimizer.

Monitors training dynamics in real-time and adjusts hyperparameters to
maximize grokking probability.  Implements a phase-detecting state machine
that transitions through: MEMORIZATION → PLATEAU → PRE_GROK → GROKKING →
CONVERGED, with an ABORT escape for distillation fallback.

Usage:
    from training.lib.adaptive_grok import AdaptiveGrokCallback, GrokPhase

    callback = AdaptiveGrokCallback(
        t_mem_patience=10,    # multiplier for plateau detection
        abort_epoch=200,      # give up and distill after this
    )
    trainer = SFTTrainer(..., callbacks=[callback])
    trainer.train()

    if callback.phase == GrokPhase.ABORT:
        print("Grokking failed — run distillation fallback")
    else:
        print(f"Grokked at step {callback.grok_step}")
"""

from __future__ import annotations

import json
import math
import time
from collections import deque
from dataclasses import dataclass, field
from enum import Enum, auto
from pathlib import Path
from typing import Optional

import torch
from transformers import TrainerCallback, TrainerControl, TrainerState, TrainingArguments


class GrokPhase(Enum):
    MEMORIZATION = auto()
    PLATEAU = auto()
    PRE_GROK = auto()
    GROKKING = auto()
    CONVERGED = auto()
    ABORT = auto()


@dataclass
class GrokMetrics:
    """Rolling window metrics for phase detection."""

    step: int = 0
    train_loss: float = float("inf")
    eval_loss: float = float("inf")
    lora_param_norm: float = 0.0
    grad_norm: float = 0.0
    learning_rate: float = 0.0
    weight_decay: float = 0.0
    timestamp: float = 0.0


@dataclass
class GrokLog:
    """Persistent log entry written to disk after each eval."""

    step: int
    epoch: float
    phase: str
    train_loss: float
    eval_loss: float
    lora_norm: float
    grad_norm: float
    lr: float
    wd: float
    intervention: str = ""


class AdaptiveGrokCallback(TrainerCallback):
    """Closed-loop grokking optimizer.

    Detects training phases via diagnostic signals and adjusts optimizer
    hyperparameters (LR, weight decay) to maximize grokking probability.

    Parameters
    ----------
    t_mem_patience : int
        After memorization, wait this many multiples of T_mem (steps to
        memorize) before intervening in the PLATEAU phase.  Default 10.
    abort_epoch : int
        Maximum epochs before declaring grokking failed.  Default 200.
    wd_increment : float
        How much to bump weight decay per PLATEAU intervention.  Default 0.1.
    wd_max : float
        Ceiling for adaptive weight decay.  Default 1.5.
    lr_bump_factor : float
        Multiply LR by this when gradients are near-zero.  Default 1.5.
    lr_grok_factor : float
        Reduce LR to this fraction when grokking detected.  Default 0.5.
    pre_grok_lr_factor : float
        In PRE_GROK, perturb LR by this factor (>1 = spike, <1 = reduce).
        Default 1.5 (spike to escape flat basins).
    train_loss_mem_threshold : float
        Train loss below this → memorization complete.  Default 0.05.
    grok_drop_threshold : float
        Eval loss must drop by this fraction to detect grokking.  Default 0.20.
    convergence_patience : int
        Steps of stable eval loss after grokking before CONVERGED.  Default 500.
    log_path : str or Path or None
        Write JSON-lines log here.  None disables logging.
    """

    def __init__(
        self,
        t_mem_patience: int = 10,
        abort_epoch: int = 200,
        wd_increment: float = 0.1,
        wd_max: float = 1.5,
        lr_bump_factor: float = 1.5,
        lr_grok_factor: float = 0.5,
        pre_grok_lr_factor: float = 1.5,
        train_loss_mem_threshold: float = 0.05,
        grok_drop_threshold: float = 0.20,
        convergence_patience: int = 500,
        log_path: Optional[str | Path] = None,
    ):
        super().__init__()
        self.t_mem_patience = t_mem_patience
        self.abort_epoch = abort_epoch
        self.wd_increment = wd_increment
        self.wd_max = wd_max
        self.lr_bump_factor = lr_bump_factor
        self.lr_grok_factor = lr_grok_factor
        self.pre_grok_lr_factor = pre_grok_lr_factor
        self.train_loss_mem_threshold = train_loss_mem_threshold
        self.grok_drop_threshold = grok_drop_threshold
        self.convergence_patience = convergence_patience
        self.log_path = Path(log_path) if log_path else None

        # State
        self.phase = GrokPhase.MEMORIZATION
        self.t_mem: Optional[int] = None  # steps when memorization completed
        self.plateau_start_step: Optional[int] = None
        self.plateau_eval_loss: Optional[float] = None  # eval loss at plateau entry
        self.grok_step: Optional[int] = None
        self.grok_eval_loss: Optional[float] = None
        self.last_intervention_step: int = 0

        # Rolling buffers
        self._train_losses: deque[float] = deque(maxlen=100)
        self._eval_losses: deque[float] = deque(maxlen=50)
        self._grad_norms: deque[float] = deque(maxlen=100)
        self._lora_norms: deque[float] = deque(maxlen=50)
        self._interventions: list[str] = []

        # Log file handle
        self._log_fh = None
        if self.log_path:
            self.log_path.parent.mkdir(parents=True, exist_ok=True)

    # ── Metric helpers ───────────────────────────────────────────

    @staticmethod
    def _lora_param_norm(model: torch.nn.Module) -> float:
        """Compute L2 norm of all LoRA adapter parameters."""
        total = 0.0
        for name, param in model.named_parameters():
            if param.requires_grad and ("lora" in name.lower() or "loftq" in name.lower()):
                total += param.data.float().norm().item() ** 2
        return math.sqrt(total)

    def _train_loss_variance(self) -> float:
        """Rolling variance of recent training losses (detects oscillation)."""
        if len(self._train_losses) < 10:
            return 0.0
        recent = list(self._train_losses)[-20:]
        mean = sum(recent) / len(recent)
        return sum((x - mean) ** 2 for x in recent) / len(recent)

    def _norm_slope(self) -> float:
        """Linear regression slope of LoRA param norms (negative = decreasing)."""
        norms = list(self._lora_norms)
        if len(norms) < 5:
            return 0.0
        n = len(norms)
        x_mean = (n - 1) / 2
        y_mean = sum(norms) / n
        num = sum((i - x_mean) * (y - y_mean) for i, y in enumerate(norms))
        den = sum((i - x_mean) ** 2 for i in range(n))
        return num / den if den > 0 else 0.0

    def _grad_norm_ratio(self) -> float:
        """Ratio of recent grad norm to early grad norm (detects second peak)."""
        norms = list(self._grad_norms)
        if len(norms) < 20:
            return 1.0
        early = sum(norms[:10]) / 10
        recent = sum(norms[-10:]) / 10
        return recent / early if early > 1e-8 else 1.0

    # ── Optimizer manipulation ───────────────────────────────────

    @staticmethod
    def _set_weight_decay(optimizer: torch.optim.Optimizer, wd: float):
        for group in optimizer.param_groups:
            group["weight_decay"] = wd

    @staticmethod
    def _set_lr(optimizer: torch.optim.Optimizer, lr: float):
        for group in optimizer.param_groups:
            group["lr"] = lr

    @staticmethod
    def _get_lr(optimizer: torch.optim.Optimizer) -> float:
        return optimizer.param_groups[0].get("lr", 0.0)

    @staticmethod
    def _get_wd(optimizer: torch.optim.Optimizer) -> float:
        return optimizer.param_groups[0].get("weight_decay", 0.0)

    # ── Logging ──────────────────────────────────────────────────

    def _write_log(self, entry: GrokLog):
        if not self.log_path:
            return
        with open(self.log_path, "a") as f:
            f.write(json.dumps(entry.__dict__) + "\n")

    # ── Phase transitions ────────────────────────────────────────

    def _transition(self, new_phase: GrokPhase, step: int, reason: str):
        old = self.phase.name
        self.phase = new_phase
        msg = f"[AdaptiveGrok] {old} → {new_phase.name} at step {step}: {reason}"
        print(f"\n*** {msg} ***\n")
        self._interventions.append(f"step={step}: {old}→{new_phase.name} ({reason})")

    # ── Core callback hooks ──────────────────────────────────────

    def on_log(self, args: TrainingArguments, state: TrainerState,
               control: TrainerControl, logs=None, model=None, **kwargs):
        if not logs:
            return

        step = state.global_step

        if "loss" in logs:
            self._train_losses.append(logs["loss"])

        if "grad_norm" in logs:
            self._grad_norms.append(logs["grad_norm"])

        # Phase: MEMORIZATION → detect when train loss bottoms out
        if self.phase == GrokPhase.MEMORIZATION:
            if len(self._train_losses) >= 5:
                recent_avg = sum(list(self._train_losses)[-5:]) / 5
                if recent_avg < self.train_loss_mem_threshold and self.t_mem is None:
                    self.t_mem = step
                    self._transition(GrokPhase.PLATEAU, step,
                                     f"train_loss={recent_avg:.4f} < {self.train_loss_mem_threshold}")

    def on_evaluate(self, args: TrainingArguments, state: TrainerState,
                    control: TrainerControl, metrics=None, model=None, **kwargs):
        if not metrics:
            return

        step = state.global_step
        epoch = state.epoch or 0.0
        eval_loss = metrics.get("eval_loss", float("inf"))
        self._eval_losses.append(eval_loss)

        # Compute diagnostic metrics
        lora_norm = self._lora_param_norm(model) if model else 0.0
        self._lora_norms.append(lora_norm)

        optimizer = kwargs.get("optimizer")
        lr = self._get_lr(optimizer) if optimizer else 0.0
        wd = self._get_wd(optimizer) if optimizer else 0.0
        recent_grad = self._grad_norms[-1] if self._grad_norms else 0.0

        # Log
        intervention = ""
        train_loss = self._train_losses[-1] if self._train_losses else float("inf")

        # ── PLATEAU phase logic ──────────────────────────────────
        if self.phase == GrokPhase.PLATEAU and optimizer:
            if self.plateau_start_step is None:
                self.plateau_start_step = step
                self.plateau_eval_loss = eval_loss

            assert self.t_mem is not None
            steps_in_plateau = step - self.plateau_start_step
            patience_steps = self.t_mem * self.t_mem_patience

            # Check if norms are decreasing (weight decay working)
            norm_slope = self._norm_slope()

            # Intervention: norms not decreasing → increase weight decay
            if steps_in_plateau > patience_steps and norm_slope >= -1e-6:
                current_wd = self._get_wd(optimizer)
                new_wd = min(current_wd + self.wd_increment, self.wd_max)
                if new_wd > current_wd and step - self.last_intervention_step > patience_steps:
                    self._set_weight_decay(optimizer, new_wd)
                    intervention = f"WD {current_wd:.3f}→{new_wd:.3f} (norms not decreasing, slope={norm_slope:.6f})"
                    print(f"  [Intervention] {intervention}")
                    self.last_intervention_step = step

            # Intervention: gradients near zero → bump LR
            if recent_grad < 1e-6 and step - self.last_intervention_step > patience_steps // 2:
                current_lr = self._get_lr(optimizer)
                new_lr = current_lr * self.lr_bump_factor
                self._set_lr(optimizer, new_lr)
                intervention = f"LR {current_lr:.2e}→{new_lr:.2e} (grad_norm≈0)"
                print(f"  [Intervention] {intervention}")
                self.last_intervention_step = step

            # Intervention: extremely long plateau → max weight decay
            if steps_in_plateau > patience_steps * 5:
                current_wd = self._get_wd(optimizer)
                if current_wd < 1.0:
                    self._set_weight_decay(optimizer, 1.0)
                    intervention = f"WD→1.0 (extended plateau {steps_in_plateau} steps)"
                    print(f"  [Intervention] {intervention}")
                    self.last_intervention_step = step

            # Detect PRE_GROK: training loss oscillating + gradient norm second peak
            train_var = self._train_loss_variance()
            grad_ratio = self._grad_norm_ratio()
            if train_var > 0.001 and grad_ratio > 1.5:
                self._transition(GrokPhase.PRE_GROK, step,
                                 f"train_var={train_var:.4f}, grad_ratio={grad_ratio:.2f}")

            # Detect direct GROKKING (skip PRE_GROK if sudden)
            if self.plateau_eval_loss and eval_loss < self.plateau_eval_loss * (1 - self.grok_drop_threshold):
                self.grok_step = step
                self.grok_eval_loss = eval_loss
                self._transition(GrokPhase.GROKKING, step,
                                 f"eval_loss {self.plateau_eval_loss:.4f}→{eval_loss:.4f} "
                                 f"({(1 - eval_loss / self.plateau_eval_loss) * 100:.1f}% drop)")

        # ── PRE_GROK phase logic ─────────────────────────────────
        elif self.phase == GrokPhase.PRE_GROK and optimizer:
            # Perturbation strategy: spike LR to escape flat basin,
            # then let the scheduler (or next eval) pull it back.
            # Only perturb every 200+ steps to give the spike time to work.
            if step - self.last_intervention_step > 200:
                current_lr = self._get_lr(optimizer)
                # Alternate: spike up, then restore base LR
                if not hasattr(self, '_pre_grok_base_lr'):
                    self._pre_grok_base_lr = current_lr
                if current_lr <= self._pre_grok_base_lr:
                    # Spike phase: temporarily increase LR
                    new_lr = self._pre_grok_base_lr * self.pre_grok_lr_factor
                    self._set_lr(optimizer, new_lr)
                    intervention = f"LR {current_lr:.2e}→{new_lr:.2e} (PRE_GROK perturbation spike)"
                else:
                    # Restore phase: bring LR back to base
                    new_lr = self._pre_grok_base_lr
                    self._set_lr(optimizer, new_lr)
                    intervention = f"LR {current_lr:.2e}→{new_lr:.2e} (PRE_GROK restore base)"
                print(f"  [Intervention] {intervention}")
                self.last_intervention_step = step

            # Detect GROKKING
            if self.plateau_eval_loss and eval_loss < self.plateau_eval_loss * (1 - self.grok_drop_threshold):
                self.grok_step = step
                self.grok_eval_loss = eval_loss
                self._transition(GrokPhase.GROKKING, step,
                                 f"eval_loss dropped to {eval_loss:.4f}")

        # ── GROKKING phase logic ─────────────────────────────────
        elif self.phase == GrokPhase.GROKKING and optimizer:
            # Consolidate: reduce LR
            if self.grok_step and step == self.grok_step:
                current_lr = self._get_lr(optimizer)
                new_lr = current_lr * self.lr_grok_factor
                self._set_lr(optimizer, new_lr)
                intervention = f"LR {current_lr:.2e}→{new_lr:.2e} (consolidate grokking)"
                print(f"  [Intervention] {intervention}")

            # Detect CONVERGED: eval loss stable for convergence_patience steps
            if len(self._eval_losses) >= 5:
                recent = list(self._eval_losses)[-5:]
                spread = max(recent) - min(recent)
                if spread < 0.01 and self.grok_step and step - self.grok_step > self.convergence_patience:
                    self._transition(GrokPhase.CONVERGED, step,
                                     f"eval_loss stable at {eval_loss:.4f}")

        # ── ABORT check (applies to all non-terminal phases) ─────
        if self.phase in (GrokPhase.MEMORIZATION, GrokPhase.PLATEAU, GrokPhase.PRE_GROK):
            if epoch >= self.abort_epoch:
                self._transition(GrokPhase.ABORT, step,
                                 f"epoch {epoch:.0f} >= {self.abort_epoch}, no grokking")
                control.should_training_stop = True

        # Write log entry
        self._write_log(GrokLog(
            step=step,
            epoch=round(epoch, 2),
            phase=self.phase.name,
            train_loss=round(train_loss, 6),
            eval_loss=round(eval_loss, 6),
            lora_norm=round(lora_norm, 4),
            grad_norm=round(recent_grad, 6),
            lr=lr,
            wd=wd,
            intervention=intervention,
        ))

        # Status print
        print(f"  [Step {step}] phase={self.phase.name} eval={eval_loss:.4f} "
              f"norm={lora_norm:.2f} slope={self._norm_slope():.6f} "
              f"grad={recent_grad:.4f} var={self._train_loss_variance():.6f}")

    def on_train_end(self, args, state, control, **kwargs):
        print(f"\n{'=' * 60}")
        print(f"AdaptiveGrok Training Complete")
        print(f"  Final phase: {self.phase.name}")
        print(f"  T_mem (memorization step): {self.t_mem}")
        print(f"  Grok step: {self.grok_step}")
        print(f"  Interventions ({len(self._interventions)}):")
        for iv in self._interventions:
            print(f"    {iv}")
        if self.log_path:
            print(f"  Full log: {self.log_path}")
        print(f"{'=' * 60}\n")

    def summary(self) -> dict:
        """Return a summary dict for programmatic use."""
        return {
            "phase": self.phase.name,
            "grokked": self.phase in (GrokPhase.GROKKING, GrokPhase.CONVERGED),
            "t_mem": self.t_mem,
            "grok_step": self.grok_step,
            "grok_eval_loss": self.grok_eval_loss,
            "interventions": len(self._interventions),
            "intervention_log": self._interventions,
        }
