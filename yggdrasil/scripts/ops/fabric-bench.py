#!/usr/bin/env python3
"""
Yggdrasil Shared Memory Fabric — Projection Bench.
Sprint 069 Phase G.9.

For each trained projection pair, computes:
  - Projection reconstruction MSE (predicted vs native K/V)
  - Per-head cosine similarity
  - Attention-output cosine similarity (does projected K/V preserve
    downstream attention behavior?)
  - Per-layer + aggregate stats

Results emitted as JSONL to docs/research/fabric-bench-results.jsonl
+ a human-readable summary table.

Usage:
    python3 fabric-bench.py \
        --hiddens-dir ~/fine-tuning/fabric-data/hiddens \
        --projections-dir ~/fine-tuning/fabric-data/projections \
        --output ~/fine-tuning/fabric-data/bench-results.jsonl \
        --holdout-fraction 0.1 \
        --device cuda:0
"""

import argparse
import hashlib
import json
import time
from pathlib import Path
from typing import Dict, List, Optional, Tuple

import torch
import torch.nn as nn
import torch.nn.functional as F


class ProjectionMLP(nn.Module):
    def __init__(self, d_src: int, d_tgt: int, d_hidden: int = 2048, dropout: float = 0.1):
        super().__init__()
        self.net = nn.Sequential(
            nn.Linear(d_src, d_hidden),
            nn.GELU(),
            nn.Dropout(dropout),
            nn.Linear(d_hidden, d_tgt),
        )

    def forward(self, x):
        return self.net(x)


def load_projection(pt_path: Path, device: str) -> Tuple[ProjectionMLP, dict]:
    state = torch.load(pt_path, map_location=device, weights_only=False)
    mlp = ProjectionMLP(state["d_src"], state["d_tgt"]).to(device)
    mlp.load_state_dict(state["state_dict"])
    mlp.eval()
    return mlp, state


def cosine_per_row(pred: torch.Tensor, target: torch.Tensor) -> float:
    """Mean cosine similarity across the seq dim, averaged."""
    pred_n = F.normalize(pred, dim=-1)
    tgt_n = F.normalize(target, dim=-1)
    return (pred_n * tgt_n).sum(dim=-1).mean().item()


def bench_one_layer(
    src_dir: Path, tgt_dir: Path,
    src_hidden_layer: int, tgt_attn_layer: int,
    mlp_k: ProjectionMLP, mlp_v: ProjectionMLP,
    holdout_hashes: List[str], device: str,
) -> dict:
    """For a single (target_layer, K+V) projection, compute bench metrics."""
    if not holdout_hashes:
        return {"error": "empty holdout set"}

    n = 0
    mse_k_sum = 0.0
    mse_v_sum = 0.0
    cos_k_sum = 0.0
    cos_v_sum = 0.0
    attn_cos_sum = 0.0

    for h in holdout_hashes:
        src_f = src_dir / f"{h}.pt"
        tgt_f = tgt_dir / f"{h}.pt"
        if not (src_f.exists() and tgt_f.exists()):
            continue
        try:
            src = torch.load(src_f, weights_only=False)
            tgt = torch.load(tgt_f, weights_only=False)
        except Exception:
            continue

        # Find source hidden state + target K/V at mapped layer
        src_h = src["hidden_states"][src_hidden_layer].squeeze(0).to(torch.float32).to(device)
        tgt_k_raw = None
        tgt_v_raw = None
        for li, k, v in tgt["attn_kv"]:
            if li == tgt_attn_layer:
                tgt_k_raw = k
                tgt_v_raw = v
                break
        if tgt_k_raw is None:
            continue

        # Flatten target K/V from (1, heads, seq, head_dim) → (seq, heads*head_dim)
        heads, head_dim = tgt_k_raw.shape[1], tgt_k_raw.shape[3]
        tgt_k_flat = tgt_k_raw.squeeze(0).transpose(0, 1).reshape(tgt_k_raw.shape[2], -1).to(torch.float32).to(device)
        tgt_v_flat = tgt_v_raw.squeeze(0).transpose(0, 1).reshape(tgt_v_raw.shape[2], -1).to(torch.float32).to(device)

        min_seq = min(src_h.shape[0], tgt_k_flat.shape[0])
        src_h = src_h[:min_seq]
        tgt_k_flat = tgt_k_flat[:min_seq]
        tgt_v_flat = tgt_v_flat[:min_seq]

        with torch.no_grad():
            pred_k = mlp_k(src_h)
            pred_v = mlp_v(src_h)

        mse_k_sum += F.mse_loss(pred_k, tgt_k_flat).item()
        mse_v_sum += F.mse_loss(pred_v, tgt_v_flat).item()
        cos_k_sum += cosine_per_row(pred_k, tgt_k_flat)
        cos_v_sum += cosine_per_row(pred_v, tgt_v_flat)

        # Attention-output cosine: compute Attn(Q, K, V) with native and
        # projected K/V, compare outputs. Q is target's Q at this layer,
        # which we don't have extracted — proxy: use K itself as Q
        # (self-attention flavor). Gives a reasonable behavioral signal.
        pred_k_r = pred_k.reshape(min_seq, heads, head_dim).transpose(0, 1)  # (heads, seq, dim)
        pred_v_r = pred_v.reshape(min_seq, heads, head_dim).transpose(0, 1)
        tgt_k_r = tgt_k_flat.reshape(min_seq, heads, head_dim).transpose(0, 1)
        tgt_v_r = tgt_v_flat.reshape(min_seq, heads, head_dim).transpose(0, 1)
        scale = head_dim ** -0.5
        q = tgt_k_r  # self-K as Q proxy
        with torch.no_grad():
            a_native = torch.softmax(q @ tgt_k_r.transpose(-1, -2) * scale, dim=-1) @ tgt_v_r
            a_pred = torch.softmax(q @ pred_k_r.transpose(-1, -2) * scale, dim=-1) @ pred_v_r
        # Average cosine across heads and positions
        a_native_n = F.normalize(a_native.reshape(-1, head_dim), dim=-1)
        a_pred_n = F.normalize(a_pred.reshape(-1, head_dim), dim=-1)
        attn_cos_sum += (a_native_n * a_pred_n).sum(dim=-1).mean().item()

        n += 1

    if n == 0:
        return {"error": "no paired prompts in holdout"}
    return {
        "n_probes": n,
        "mse_K": mse_k_sum / n,
        "mse_V": mse_v_sum / n,
        "cosine_K": cos_k_sum / n,
        "cosine_V": cos_v_sum / n,
        "attention_output_cosine": attn_cos_sum / n,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--hiddens-dir", type=Path, default=Path.home() / "fine-tuning/fabric-data/hiddens")
    ap.add_argument("--projections-dir", type=Path, default=Path.home() / "fine-tuning/fabric-data/projections")
    ap.add_argument("--output", type=Path, default=Path.home() / "fine-tuning/fabric-data/bench-results.jsonl")
    ap.add_argument("--holdout-fraction", type=float, default=0.1)
    ap.add_argument("--device", default="cuda:0")
    args = ap.parse_args()

    args.output.parent.mkdir(parents=True, exist_ok=True)
    out = args.output.open("w")

    summary = []

    for pair_dir in sorted(args.projections_dir.iterdir()):
        if not pair_dir.is_dir():
            continue
        parts = pair_dir.name.split("__to__", 1)
        if len(parts) != 2:
            continue
        src_model, tgt_model = parts

        src_hiddens_dir = args.hiddens_dir / src_model
        tgt_hiddens_dir = args.hiddens_dir / tgt_model
        if not (src_hiddens_dir.exists() and tgt_hiddens_dir.exists()):
            continue

        # Deterministic holdout based on hash mod.
        all_hashes = sorted({p.stem for p in src_hiddens_dir.glob("*.pt")} &
                            {p.stem for p in tgt_hiddens_dir.glob("*.pt")})
        holdout = [h for h in all_hashes
                   if int(hashlib.sha256(h.encode()).hexdigest()[:8], 16) % 10 == 0]
        if not holdout:
            continue

        # Group projection files by target_attn_layer.
        layers_kv = {}
        for pt in sorted(pair_dir.glob("layer_*_*.pt")):
            # layer_NN_{K,V}.pt
            parts2 = pt.stem.split("_")
            tgt_layer = int(parts2[1])
            kind = parts2[2]
            layers_kv.setdefault(tgt_layer, {})[kind] = pt

        print(f"\n═══ {src_model} → {tgt_model} ═══")
        pair_rows = []
        for tgt_layer, files in sorted(layers_kv.items()):
            if "K" not in files or "V" not in files:
                continue
            mlp_k, meta_k = load_projection(files["K"], args.device)
            mlp_v, meta_v = load_projection(files["V"], args.device)
            src_hidden_layer = meta_k["src_hidden_layer"]

            t0 = time.time()
            stats = bench_one_layer(
                src_hiddens_dir, tgt_hiddens_dir,
                src_hidden_layer, tgt_layer,
                mlp_k, mlp_v, holdout, args.device,
            )
            stats.update({
                "pair": f"{src_model}->{tgt_model}",
                "tgt_attn_layer": tgt_layer,
                "src_hidden_layer": src_hidden_layer,
                "wallclock_sec": round(time.time() - t0, 2),
            })
            out.write(json.dumps(stats) + "\n")
            out.flush()
            pair_rows.append(stats)
            if "error" not in stats:
                print(f"  L{tgt_layer:02d}  "
                      f"mse_K={stats['mse_K']:.4f}  mse_V={stats['mse_V']:.4f}  "
                      f"cos_K={stats['cosine_K']:.3f}  cos_V={stats['cosine_V']:.3f}  "
                      f"attn_cos={stats['attention_output_cosine']:.3f}  "
                      f"n={stats['n_probes']}")
        if pair_rows:
            means = {k: sum(r.get(k, 0) for r in pair_rows if "error" not in r) / max(1, len([r for r in pair_rows if "error" not in r]))
                     for k in ("mse_K", "mse_V", "cosine_K", "cosine_V", "attention_output_cosine")}
            summary.append({"pair": f"{src_model}->{tgt_model}", **means, "n_layers": len(pair_rows)})

    out.close()

    print("\n═══ Summary ═══")
    for s in summary:
        print(f"{s['pair']:50s}  layers={s['n_layers']:2d}  "
              f"mse_K={s['mse_K']:.4f}  mse_V={s['mse_V']:.4f}  "
              f"cos_K={s['cosine_K']:.3f}  cos_V={s['cosine_V']:.3f}  "
              f"attn_cos={s['attention_output_cosine']:.3f}")


if __name__ == "__main__":
    main()
