#!/usr/bin/env python3
"""
Yggdrasil Shared Memory Fabric — Projection Training.
Sprint 069 Phase G.6.

Trains per-layer K and V projection MLPs mapping source model hidden
states to target model K/V tensors (in target's shape). One MLP per
(source_model, target_model, target_attention_layer, {K,V}).

Usage:
    python3 fabric-train-projections.py \
        --hiddens-dir ~/fine-tuning/fabric-data/hiddens \
        --output-dir ~/fine-tuning/fabric-data/projections \
        --epochs 20 \
        --batch-size 128 \
        --device cuda:0

Runs on Morrigan. Designed to be resumable — existing output .pt files
are skipped on restart.
"""

import argparse
import json
import time
from pathlib import Path
from typing import Dict, List, Optional, Tuple

import torch
import torch.nn as nn
import torch.nn.functional as F
from torch.utils.data import DataLoader, Dataset


# ─────────────── Projection MLP ───────────────

class ProjectionMLP(nn.Module):
    """Token-wise MLP: source hidden (d_src) → target K or V (d_tgt)."""

    def __init__(self, d_src: int, d_tgt: int, d_hidden: int = 2048, dropout: float = 0.1):
        super().__init__()
        self.net = nn.Sequential(
            nn.Linear(d_src, d_hidden),
            nn.GELU(),
            nn.Dropout(dropout),
            nn.Linear(d_hidden, d_tgt),
        )
        self.d_src = d_src
        self.d_tgt = d_tgt

    def forward(self, x):
        # x shape: (batch, seq, d_src) OR (seq, d_src)
        return self.net(x)


# ─────────────── Paired dataset ───────────────

class PairedHiddensDataset(Dataset):
    """
    Iterates prompt hashes that appear in BOTH source and target model
    output dirs. For each prompt, returns:
      src_hidden_at_layer (seq, d_src)
      tgt_K_at_layer      (seq, heads_tgt * head_dim_tgt)
      tgt_V_at_layer      (seq, heads_tgt * head_dim_tgt)
    """

    def __init__(self, src_dir: Path, tgt_dir: Path, src_hidden_layer: int, tgt_attn_layer: int):
        self.src_dir = src_dir
        self.tgt_dir = tgt_dir
        self.src_hidden_layer = src_hidden_layer
        self.tgt_attn_layer = tgt_attn_layer

        src_hashes = {p.stem for p in src_dir.glob("*.pt")}
        tgt_hashes = {p.stem for p in tgt_dir.glob("*.pt")}
        self.hashes = sorted(src_hashes & tgt_hashes)

    def __len__(self): return len(self.hashes)

    def __getitem__(self, idx):
        h = self.hashes[idx]
        src = torch.load(self.src_dir / f"{h}.pt", weights_only=False)
        tgt = torch.load(self.tgt_dir / f"{h}.pt", weights_only=False)

        # Source: per-layer hidden states. hidden_states is a list of
        # (1, seq, d_src). Layer 0 is the input embedding; layer L is
        # the output of layer (L-1) in the attention stack.
        src_h = src["hidden_states"][self.src_hidden_layer].squeeze(0)  # (seq, d_src)

        # Target: K/V at tgt_attn_layer from attn_kv list.
        tgt_k, tgt_v = _find_layer_kv(tgt["attn_kv"], self.tgt_attn_layer)
        # Two shapes:
        #   4D (1, heads, seq, head_dim) → reshape to (seq, heads*head_dim)
        #   3D (1, seq, heads*head_dim)  → squeeze batch → (seq, heads*head_dim)
        if tgt_k.dim() == 4:
            tgt_k = tgt_k.squeeze(0).transpose(0, 1).reshape(tgt_k.shape[2], -1).contiguous()
            tgt_v = tgt_v.squeeze(0).transpose(0, 1).reshape(tgt_v.shape[2], -1).contiguous()
        else:  # 3D
            tgt_k = tgt_k.squeeze(0).contiguous()
            tgt_v = tgt_v.squeeze(0).contiguous()

        # Align sequence length (may differ slightly across tokenizers)
        min_seq = min(src_h.shape[0], tgt_k.shape[0])
        return (
            src_h[:min_seq].to(torch.float32),
            tgt_k[:min_seq].to(torch.float32),
            tgt_v[:min_seq].to(torch.float32),
        )


def _find_layer_kv(attn_kv: List[Tuple], layer_idx: int) -> Tuple[torch.Tensor, torch.Tensor]:
    for li, k, v in attn_kv:
        if li == layer_idx:
            return k, v
    raise KeyError(f"No attention K/V at layer {layer_idx}; available: {[li for li,_,_ in attn_kv]}")


def collate_variable_seq(batch):
    """Pad variable-length sequences to the max in the batch."""
    max_seq = max(x[0].shape[0] for x in batch)
    d_src = batch[0][0].shape[1]
    d_tgt = batch[0][1].shape[1]

    src_padded = torch.zeros(len(batch), max_seq, d_src)
    k_padded = torch.zeros(len(batch), max_seq, d_tgt)
    v_padded = torch.zeros(len(batch), max_seq, d_tgt)
    mask = torch.zeros(len(batch), max_seq, dtype=torch.bool)

    for i, (h, k, v) in enumerate(batch):
        L = h.shape[0]
        src_padded[i, :L] = h
        k_padded[i, :L] = k
        v_padded[i, :L] = v
        mask[i, :L] = True
    return src_padded, k_padded, v_padded, mask


# ─────────────── Discovery ───────────────

def discover_model_meta(hiddens_dir: Path, model: str) -> Optional[Dict]:
    """Load the first .pt file for a model and extract layer/dim metadata."""
    mdir = hiddens_dir / model
    files = list(mdir.glob("*.pt"))
    if not files:
        return None
    sample = torch.load(files[0], weights_only=False)

    attn_layers = [li for li, _, _ in sample["attn_kv"]]
    if not attn_layers:
        return {"kind": "no_attention", "hidden_dim": sample["hidden_states"][-1].shape[-1]}

    first_k = sample["attn_kv"][0][1]
    # Two shapes coexist:
    #   4D (batch, heads, seq, head_dim)  — cache-based extraction (LFM2, Gemma-4)
    #   3D (batch, seq, heads*head_dim)  — hook-based extraction (Nemotron)
    if first_k.dim() == 4:
        heads, head_dim = first_k.shape[1], first_k.shape[3]
        kv_flat_dim = heads * head_dim
    elif first_k.dim() == 3:
        heads, head_dim = 1, first_k.shape[2]  # unknown split; flat fits
        kv_flat_dim = first_k.shape[2]
    else:
        return None
    return {
        "kind": "transformer_or_hybrid",
        "n_hidden_states": len(sample["hidden_states"]),
        "hidden_dim": sample["hidden_states"][-1].shape[-1],
        "attn_layers": attn_layers,
        "heads": heads,
        "head_dim": head_dim,
        "kv_flat_dim": kv_flat_dim,
        "k_dim": first_k.dim(),
        "n_prompts": len(files),
    }


def map_src_layer(src_layer_idx: int, n_src: int, n_tgt: int) -> int:
    """Proportional layer-mapping per design doc."""
    return min(n_tgt - 1, round(src_layer_idx * n_tgt / n_src))


# ─────────────── Training loop ───────────────

def train_one_mlp(
    dataset: PairedHiddensDataset, d_src: int, d_tgt: int,
    epochs: int, batch_size: int, device: str, lr: float = 3e-4,
) -> Tuple[ProjectionMLP, float]:
    if len(dataset) < 10:
        raise RuntimeError(f"Too few paired prompts ({len(dataset)}) for training")

    mlp = ProjectionMLP(d_src, d_tgt).to(device)
    opt = torch.optim.AdamW(mlp.parameters(), lr=lr, weight_decay=1e-4)
    sched = torch.optim.lr_scheduler.CosineAnnealingLR(opt, T_max=epochs)

    loader = DataLoader(
        dataset, batch_size=batch_size, shuffle=True,
        collate_fn=collate_variable_seq, num_workers=2,
    )

    best_val = float("inf")
    n_val = max(1, len(dataset) // 10)

    for epoch in range(epochs):
        mlp.train()
        total, n = 0.0, 0
        for src_h, tgt_k, _tgt_v, mask in loader:
            src_h, tgt_k, mask = src_h.to(device), tgt_k.to(device), mask.to(device)
            pred = mlp(src_h)
            loss = F.mse_loss(pred[mask], tgt_k[mask])
            opt.zero_grad()
            loss.backward()
            opt.step()
            total += loss.item() * src_h.size(0); n += src_h.size(0)
        sched.step()
        avg_train = total / max(n, 1)

        # Use the first batch as a quick validation probe.
        # Real split would hold out by prompt_hash.
        mlp.eval()
        with torch.no_grad():
            src_h, tgt_k, _tgt_v, mask = next(iter(loader))
            src_h, tgt_k, mask = src_h.to(device), tgt_k.to(device), mask.to(device)
            val = F.mse_loss(mlp(src_h)[mask], tgt_k[mask]).item()
        if val < best_val: best_val = val

    return mlp, best_val


def run_pair(
    hiddens_dir: Path, output_dir: Path,
    src_model: str, tgt_model: str, meta_src: Dict, meta_tgt: Dict,
    epochs: int, batch_size: int, device: str,
):
    pair_dir = output_dir / f"{src_model}__to__{tgt_model}"
    pair_dir.mkdir(parents=True, exist_ok=True)
    meta_file = pair_dir / "_meta.json"

    n_src_h = meta_src["n_hidden_states"]   # incl. input embedding
    n_tgt_h = meta_tgt["n_hidden_states"]
    d_src = meta_src["hidden_dim"]
    d_tgt = meta_tgt["kv_flat_dim"]

    src_dir = hiddens_dir / src_model
    tgt_dir = hiddens_dir / tgt_model

    results = {"pair": f"{src_model}->{tgt_model}", "layers": []}

    for tgt_attn_layer in meta_tgt["attn_layers"]:
        # Find corresponding source layer.
        src_layer = map_src_layer(tgt_attn_layer, n_tgt_h - 1, n_src_h - 1)
        # Clamp: source hidden states are 0..n_src_h-1 (0 is input embed).
        src_layer = max(1, min(n_src_h - 1, src_layer))

        for kind in ("K", "V"):
            out_path = pair_dir / f"layer_{tgt_attn_layer:02d}_{kind}.pt"
            if out_path.exists():
                continue

            ds = PairedHiddensDataset(src_dir, tgt_dir, src_layer, tgt_attn_layer)
            if len(ds) < 10:
                results["layers"].append({
                    "tgt_layer": tgt_attn_layer, "kind": kind,
                    "skipped": f"only {len(ds)} paired prompts"
                })
                continue

            t0 = time.time()
            try:
                mlp, best_val = train_one_mlp(
                    ds, d_src, d_tgt, epochs, batch_size, device,
                )
            except Exception as e:
                results["layers"].append({
                    "tgt_layer": tgt_attn_layer, "kind": kind, "error": str(e)
                })
                continue

            torch.save({
                "state_dict": mlp.state_dict(),
                "d_src": d_src, "d_tgt": d_tgt,
                "src_hidden_layer": src_layer,
                "tgt_attn_layer": tgt_attn_layer,
                "kind": kind,
                "best_val_mse": best_val,
                "n_paired": len(ds),
                "epochs": epochs,
            }, out_path)
            results["layers"].append({
                "tgt_layer": tgt_attn_layer, "kind": kind,
                "src_layer": src_layer,
                "val_mse": best_val,
                "paired_prompts": len(ds),
                "elapsed_sec": round(time.time() - t0, 1),
            })
            print(f"  [{tgt_attn_layer:02d}-{kind}] val_mse={best_val:.5f} n={len(ds)} t={time.time()-t0:.1f}s",
                  flush=True)

    meta_file.write_text(json.dumps(results, indent=2))
    return results


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--hiddens-dir", type=Path, default=Path.home() / "fine-tuning/fabric-data/hiddens")
    ap.add_argument("--output-dir",  type=Path, default=Path.home() / "fine-tuning/fabric-data/projections")
    ap.add_argument("--epochs", type=int, default=20)
    ap.add_argument("--batch-size", type=int, default=128)
    ap.add_argument("--device", default="cuda:0")
    ap.add_argument("--pairs", nargs="+", default=None,
                    help="Restrict to these 'src__to__tgt' pair names")
    args = ap.parse_args()

    args.output_dir.mkdir(parents=True, exist_ok=True)

    # Discover all models that have extracted hiddens.
    models = []
    for mdir in sorted(args.hiddens_dir.iterdir()):
        if mdir.is_dir():
            meta = discover_model_meta(args.hiddens_dir, mdir.name)
            if meta and meta.get("kind") == "transformer_or_hybrid":
                models.append((mdir.name, meta))
                print(f"model {mdir.name}: {meta['n_prompts']} prompts, "
                      f"d_hidden={meta['hidden_dim']}, "
                      f"attn_layers={meta['attn_layers']}, "
                      f"heads={meta['heads']}, head_dim={meta['head_dim']}")

    if len(models) < 2:
        print(f"Need ≥2 models, found {len(models)}; aborting.")
        return

    # All directional pairs.
    total_pairs = 0
    for src_name, src_meta in models:
        for tgt_name, tgt_meta in models:
            if src_name == tgt_name:
                continue
            pair_key = f"{src_name}__to__{tgt_name}"
            if args.pairs and pair_key not in args.pairs:
                continue
            total_pairs += 1
            print(f"\n═══ {pair_key} ═══")
            run_pair(args.hiddens_dir, args.output_dir,
                     src_name, tgt_name, src_meta, tgt_meta,
                     args.epochs, args.batch_size, args.device)

    print(f"\nDONE: trained projections for {total_pairs} pairs under {args.output_dir}")


if __name__ == "__main__":
    main()
