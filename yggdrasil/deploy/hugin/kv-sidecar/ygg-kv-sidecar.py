#!/usr/bin/env python3
"""
Yggdrasil Shared Memory Fabric — L1 Direct-KV Sidecar.
Sprint 069 Phase G.4.

Runs alongside llama-swap on Hugin. For same-family pairs (LFM2
internal at minimum, Qwen internal eventually), exposes an HTTP API
that applies a learned linear projection between the source model's
K/V tensor space and the target model's K/V tensor space.

Architecture:
    consumer caller  ──(POST /project)──▶  sidecar
                                              │
                                              ▼
                                       load projection .pt
                                              │
                                              ▼
                                       apply MLP, return target K/V

The sidecar does NOT hook vLLM's KV cache directly — that's Phase
G.7 (vLLM fork with --enable-external-kv). This sidecar ships the
projection API so the vLLM fork can call it at prefill-skip time.

Until G.7 lands, the sidecar is exercised via:
  - `POST /project` — unit-test the projection math end-to-end
  - `GET  /pairs`   — enumerate trained projection pairs available
  - `GET  /health`  — liveness

Runs at 0.0.0.0:11451 on Hugin.
"""

import argparse
import base64
import logging
from pathlib import Path
from typing import Dict, Optional, Tuple

import torch
import torch.nn as nn
from fastapi import FastAPI, HTTPException
from fastapi.responses import JSONResponse
from pydantic import BaseModel
import uvicorn

logging.basicConfig(level=logging.INFO,
                    format="%(asctime)s %(levelname)s %(name)s %(message)s")
log = logging.getLogger("ygg-kv-sidecar")


# ───────────────── Projection MLP (matches fabric-train-projections.py) ─────────────────

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


# ───────────────── Projection registry ─────────────────

class ProjectionBank:
    """Loads all projection .pt files from a directory on disk."""

    def __init__(self, root: Path, device: str = "cpu"):
        self.root = root
        self.device = device
        # (src_model, tgt_model, tgt_attn_layer, kind) -> MLP
        self.projections: Dict[Tuple[str, str, int, str], ProjectionMLP] = {}
        self.metadata: Dict[str, dict] = {}
        self._load_all()

    def _load_all(self):
        if not self.root.exists():
            log.warning(f"Projection root {self.root} does not exist; bank is empty")
            return
        for pair_dir in sorted(self.root.iterdir()):
            if not pair_dir.is_dir():
                continue
            parts = pair_dir.name.split("__to__", 1)
            if len(parts) != 2:
                continue
            src, tgt = parts
            count = 0
            for pt_file in pair_dir.glob("layer_*_*.pt"):
                try:
                    state = torch.load(pt_file, map_location=self.device, weights_only=False)
                    mlp = ProjectionMLP(state["d_src"], state["d_tgt"]).to(self.device)
                    mlp.load_state_dict(state["state_dict"])
                    mlp.eval()
                    key = (src, tgt, state["tgt_attn_layer"], state["kind"])
                    self.projections[key] = mlp
                    count += 1
                except Exception as e:
                    log.warning(f"skip {pt_file}: {e}")
            if count:
                self.metadata[f"{src}__to__{tgt}"] = {
                    "n_projections": count,
                    "path": str(pair_dir),
                }
                log.info(f"loaded {count} projections for {src} → {tgt}")

    def get(self, src: str, tgt: str, layer: int, kind: str) -> Optional[ProjectionMLP]:
        return self.projections.get((src, tgt, layer, kind))

    def list_pairs(self) -> Dict[str, dict]:
        return self.metadata


# ───────────────── HTTP API ─────────────────

class ProjectRequest(BaseModel):
    source_model: str
    target_model: str
    target_attn_layer: int
    kind: str  # "K" or "V"
    # Source hidden state as base64-encoded fp16 bytes, shape (seq, d_src).
    # Also accepts a raw Python list for unit-testing.
    source_hidden_b64: Optional[str] = None
    source_hidden_list: Optional[list] = None
    seq_len: int
    d_src: int


class ProjectResponse(BaseModel):
    target_kv_b64: str  # base64 fp16 bytes, shape (seq, d_tgt)
    d_tgt: int
    seq_len: int


def build_app(bank: ProjectionBank) -> FastAPI:
    app = FastAPI(title="ygg-kv-sidecar", version="0.1.0")

    @app.get("/health")
    def health():
        return {"ok": True, "projections": len(bank.projections),
                "pairs": len(bank.metadata)}

    @app.get("/pairs")
    def pairs():
        return bank.list_pairs()

    @app.post("/project", response_model=ProjectResponse)
    def project(req: ProjectRequest):
        mlp = bank.get(req.source_model, req.target_model,
                       req.target_attn_layer, req.kind)
        if mlp is None:
            raise HTTPException(404,
                f"no projection for {req.source_model}→{req.target_model} "
                f"layer {req.target_attn_layer} {req.kind}")

        if req.source_hidden_b64:
            raw = base64.b64decode(req.source_hidden_b64)
            src = torch.frombuffer(bytearray(raw), dtype=torch.float16).reshape(
                req.seq_len, req.d_src).to(torch.float32)
        elif req.source_hidden_list:
            src = torch.tensor(req.source_hidden_list, dtype=torch.float32).reshape(
                req.seq_len, req.d_src)
        else:
            raise HTTPException(400, "source_hidden_b64 or source_hidden_list required")

        with torch.no_grad():
            tgt = mlp(src.to(bank.device)).to(torch.float16).cpu().contiguous()
        return ProjectResponse(
            target_kv_b64=base64.b64encode(tgt.numpy().tobytes()).decode(),
            d_tgt=tgt.shape[1],
            seq_len=tgt.shape[0],
        )

    @app.post("/reload")
    def reload():
        nonlocal_bank = bank
        nonlocal_bank.projections.clear()
        nonlocal_bank.metadata.clear()
        nonlocal_bank._load_all()
        return {"ok": True, "pairs": len(nonlocal_bank.metadata)}

    return app


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--projections-dir", type=Path,
                    default=Path("/opt/yggdrasil/fabric-projections"))
    ap.add_argument("--host", default="0.0.0.0")
    ap.add_argument("--port", type=int, default=11451)
    ap.add_argument("--device", default="cpu",
                    help="cpu or cuda:N — MLPs are tiny so CPU is fine")
    args = ap.parse_args()

    bank = ProjectionBank(args.projections_dir, args.device)
    log.info(f"loaded {len(bank.projections)} projections across {len(bank.metadata)} pairs")
    app = build_app(bank)
    uvicorn.run(app, host=args.host, port=args.port, log_level="info")


if __name__ == "__main__":
    main()
