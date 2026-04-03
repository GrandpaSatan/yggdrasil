#!/usr/bin/env python3
"""Generate synthetic saga training data matching the exact production schema.

Produces CLASSIFY and DISTILL examples that perfectly match what
mimir/src/saga.rs expects:

CLASSIFY response: {"category": "<cat>", "should_store": <bool>, "confidence": <float>}
DISTILL response:  {"cause": "<text>", "effect": "<text>", "tags": ["tag1", "tag2"]}

Categories: bug_fix, architecture_decision, sprint_lifecycle, routine,
            deployment_change, gotcha, user_feedback
"""
import json
import random
from pathlib import Path

SYSTEM_PROMPT = "You are Saga, Yggdrasil's memory engine. Respond ONLY in valid JSON."

CATEGORIES = {
    "bug_fix": {
        "should_store": True,
        "examples": [
            ("Edit", "odin/src/handlers.rs", "Fixed session timeout bug — sessions were expiring after 60s instead of 3600s due to wrong default value"),
            ("Edit", "mimir/src/saga.rs", "Fixed panic in CLASSIFY handler when Ollama returns empty response — added None check before JSON parse"),
            ("Edit", "odin/src/proxy.rs", "Fixed streaming SSE conversion dropping last token — off-by-one in buffer flush logic"),
            ("Edit", "ygg-config/src/impls.rs", "Fixed port conflict validator — was comparing against same port instead of other backends"),
            ("Edit", "muninn/src/chunker.rs", "Fixed tree-sitter markdown heading extraction — heading_content changed to positional inline child"),
            ("Edit", "odin/src/agent.rs", "Fixed agent loop retry — was retrying with same prompt instead of including tool error feedback"),
            ("Edit", "ygg-store/src/postgres/engrams.rs", "Fixed SDR dedup — novelty gate threshold was 0.90, too aggressive, lowered to 0.85"),
            ("Edit", "odin/src/voice_ws.rs", "Fixed echo cancellation — was comparing raw PCM instead of transcripts, causing false matches"),
            ("Bash", "", "Fixed ollama service crash — was binding to port 11435 instead of 11434 after config migration"),
            ("Edit", "ygg-ha/src/client.rs", "Fixed HA 401 error — stale auth token in systemd drop-in, replaced with valid jhernandez token"),
            ("Edit", "odin/src/router.rs", "Fixed keyword router matching 'gaming' intent for all requests containing 'game' in any context"),
            ("Edit", "ygg-node/src/discovery.rs", "Fixed mDNS broadcast interval — was 30s but spec requires 1s for initial announcement"),
        ],
    },
    "architecture_decision": {
        "should_store": True,
        "examples": [
            ("Edit", "ygg-domain/src/config.rs", "Added BackendConfig struct with url, models, max_concurrent fields for multi-backend LLM routing"),
            ("Edit", "odin/src/flow.rs", "Created flow engine for multi-model pipelines — sequential step execution with context passing between specialist models"),
            ("Edit", "odin/src/sdr_router.rs", "Replaced keyword router with hybrid SDR + LLM classification — SDR for fast System 1, LLM for System 2 confirmation"),
            ("Edit", "ygg-domain/src/config.rs", "Added FlowConfig with FlowTrigger (Intent/Modality/Manual/Cron/Idle) for configurable multi-model flows"),
            ("Edit", "mimir/src/handlers.rs", "Split memory storage into auto-ingest (fire-and-forget) vs explicit store (user-triggered) paths"),
            ("Edit", "odin/src/agent.rs", "Agent loop now executes tools in parallel via join_all — single model call per iteration, all tools concurrent"),
            ("Edit", "ygg-store/src/postgres/engrams.rs", "Switched from cosine similarity to SDR Hamming distance for novelty gate — 4μs vs 50ms per check"),
            ("Edit", "odin/src/state.rs", "Added per-backend semaphore for admission control — try_acquire returns 503 immediately instead of blocking"),
        ],
    },
    "sprint_lifecycle": {
        "should_store": True,
        "examples": [
            ("Edit", "sprints/sprint-054.md", "Sprint 054 started — LLM Fleet Optimization with grokked specialist models"),
            ("Edit", "sprints/sprint-055.md", "Sprint 055 started — Agentic AI Flows with multi-model pipelines and dream flows"),
            ("Edit", "sprints/sprint-053.md", "Sprint 053 complete — parallel model capacity, fleet audit, Morrigan backend added"),
            ("Edit", "sprints/sprint-051.md", "Sprint 051 mega-sprint complete — infra fixes, test harness, schema consolidation, Antigravity IDE"),
            ("Edit", "docs/ARCHITECTURE.md", "Updated architecture doc for Sprint 054 — added flow engine, fleet model assignments"),
        ],
    },
    "deployment_change": {
        "should_store": True,
        "examples": [
            ("Edit", "deploy/munin/odin.service", "Changed OLLAMA_MAX_LOADED_MODELS from 2 to 6 in systemd unit — Munin eGPU can handle parallel model loading"),
            ("Bash", "", "Deployed odin binary to Munin — scp to /tmp then sudo cp to /opt/yggdrasil/bin/odin with 755 permissions"),
            ("Edit", "deploy/munin/.env", "Updated MORRIGAN_URL to http://10.0.65.20:8080 after VLAN 65 migration"),
            ("Bash", "", "Updated Ollama on Munin from custom 0.0.0 to 0.20.0 — needed for Gemma 4 architecture support"),
            ("Edit", "deploy/workstation/local-config.yaml", "Added gemma4:e2b as default code gen model in Odin routing config"),
            ("Bash", "", "Pulled Gemma 4 E2B on Munin — ollama pull gemma4:e2b — 7.2GB, deployed as code gen specialist"),
            ("Edit", "/etc/yggdrasil/odin/config.json", "Added flow definitions for code review and reasoning flows to Odin config"),
        ],
    },
    "gotcha": {
        "should_store": True,
        "examples": [
            ("Edit", "odin/src/proxy.rs", "GOTCHA: Ollama returns newline-delimited JSON for streaming, not SSE. Had to switch from eventsource to line-by-line parsing"),
            ("Edit", "odin/src/handlers.rs", "GOTCHA: Gemma 4 uses thinking mode by default — content field is empty, response is in thinking field. Must pass think:false in Ollama API"),
            ("Edit", "training/experiment/train_350m.py", "GOTCHA: GrokFast optimizer prevents learning on LFM architecture — train loss stuck at 2.0 while standard AdamW reaches 0.04"),
            ("Bash", "", "GOTCHA: Ollama gemma4:e4b tag returns EOF on Munin ROCm build but works on Hugin — use Hugin for E4B or copy model files"),
            ("Edit", "training/coding/train_distill.py", "GOTCHA: Qwen3.5 thinking mode puts response in reasoning_content not content — need chat_template_kwargs enable_thinking=False"),
            ("Edit", "ygg-voice/src/lib.rs", "GOTCHA: Kokoro TTS cannot run on iGPU — 3D interpolation and ScatterNDUpdate are unsupported ops in OpenVINO"),
            ("Bash", "", "GOTCHA: ollama create fails on Munin ROCm build with 'invalid model name' for any name — use ollama pull or direct llama-server instead"),
            ("Edit", "training/experiment/train_350m.py", "GOTCHA: fp16=True causes ValueError 'Attempting to unscale FP16 gradients' — use bf16=True instead for full fine-tuning"),
            ("Edit", "training/lib/adaptive_grok.py", "GOTCHA: LoRA prevents grokking — adapter weights can't restructure base model representations. Need full fine-tuning for grokking."),
            ("Bash", "", "GOTCHA: /var/log/syslog.1 grew to 1.5TB on Munin — ygg-sentinel debug logging filled the disk. Set logrotate maxsize 1G."),
            ("Edit", "odin/src/handlers.rs", "GOTCHA: Ollama format:json returns empty content when model uses thinking mode — must pass think:false alongside format:json"),
            ("Bash", "", "GOTCHA: LFM2.5-VL-1.6B crashes on Ollama 0.20.0 with 'panic in loadModel' — need ONNX/OpenVINO path instead"),
            ("Edit", "deploy/workstation/ygg-memory.sh", "GOTCHA: SSH with raw IPs causes 'too many auth failures' — use hostname aliases in SSH config with IdentityFile per host"),
            ("Bash", "", "GOTCHA: Qwen3.5-27B checkpoint resume in HuggingFace Trainer restores old optimizer state including LR — use --load-weights to load model only with fresh optimizer"),
            ("Edit", "training/eval/review_bench.py", "GOTCHA: scoring LGTM+suggestions as false positive penalizes thorough reviewers — check verdict only, not issues array length"),
        ],
    },
    "user_feedback": {
        "should_store": True,
        "examples": [
            ("Edit", "", "User: don't mock the database in tests — we got burned when mocked tests passed but prod migration failed"),
            ("Edit", "", "User: stop summarizing what you just did at the end of every response, I can read the diff"),
            ("Edit", "", "User: review model must reach 90% or higher — 80% is not acceptable for QA"),
            ("Edit", "", "User: saga needs to be perfect, can't just rely on fine-tuning alone — need structured output enforcement"),
        ],
    },
    "routine": {
        "should_store": False,
        "examples": [
            ("Edit", "src/main.rs", "Added missing semicolon"),
            ("Bash", "", "cargo check\n    Compiling yggdrasil v1.0.0\n    Finished dev [unoptimized + debuginfo]"),
            ("Read", "README.md", "# Yggdrasil\nAI homelab orchestrator"),
            ("Bash", "", "git status\nM odin/src/main.rs"),
            ("Bash", "", "ls -la /opt/yggdrasil/bin/"),
            ("Edit", "handlers.rs", "Updated import order"),
            ("Bash", "", "cargo fmt -- all files"),
            ("Read", "Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]"),
            ("Bash", "", "cat /etc/yggdrasil/odin/config.json"),
            ("Edit", "src/lib.rs", "Added newline at end of file"),
            ("Bash", "", "ollama list"),
            ("Read", "config.rs", "Reading config struct definition"),
            ("Bash", "", "ssh jhernandez@10.0.65.9 hostname"),
            ("Bash", "", "nvidia-smi --query-gpu=utilization.gpu --format=csv"),
            ("Bash", "", "df -h / | tail -1"),
            ("Bash", "", "systemctl status ollama"),
            ("Bash", "", "curl -s http://localhost:8080/health"),
            ("Bash", "", "wc -l training/data/*.jsonl"),
            ("Read", "deploy/munin/.env", "OLLAMA_URL=http://localhost:11434"),
            ("Bash", "", "git log --oneline -5"),
            ("Bash", "", "git diff --stat HEAD"),
            ("Bash", "", "cargo test 2>&1 | tail -5"),
            ("Edit", "main.rs", "Removed unused import"),
            ("Bash", "", "head -20 output.log"),
            ("Bash", "", "tail -f /var/log/syslog"),
            ("Bash", "", "ps aux | grep ollama"),
            ("Read", "Cargo.lock", "# This file is automatically generated"),
            ("Bash", "", "free -h"),
            ("Bash", "", "top -bn1 | head -5"),
            ("Edit", "lib.rs", "Reordered module declarations"),
        ],
    },
}


def generate_classify_example(tool: str, file_path: str, content: str, category: str, should_store: bool) -> dict:
    """Generate a single CLASSIFY training example."""
    user_msg = f"CLASSIFY\ntool: {tool}\nfile: {file_path}\ncontent: {content}"
    confidence = round(random.uniform(0.85, 0.99), 2) if should_store else round(random.uniform(0.70, 0.95), 2)
    assistant_msg = json.dumps({
        "category": category,
        "should_store": should_store,
        "confidence": confidence,
    })
    return {
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": user_msg},
            {"role": "assistant", "content": assistant_msg},
        ]
    }


def generate_distill_example(tool: str, file_path: str, content: str, category: str) -> dict:
    """Generate a single DISTILL training example."""
    user_msg = f"DISTILL\ntool: {tool}\nfile: {file_path}\ncontent: {content}"

    # Generate cause/effect from content
    cause = content[:150].strip()
    if category == "bug_fix":
        effect = f"Bug fixed in {file_path or 'system'}. {content[50:200].strip()}"
    elif category == "architecture_decision":
        effect = f"Architecture change: {content[:200].strip()}"
    elif category == "deployment_change":
        effect = f"Deployment updated: {content[:200].strip()}"
    elif category == "gotcha":
        effect = f"Non-obvious finding documented: {content[:200].strip()}"
    elif category == "sprint_lifecycle":
        effect = f"Sprint event: {content[:200].strip()}"
    elif category == "user_feedback":
        effect = f"User preference recorded: {content[:200].strip()}"
    else:
        effect = content[:200].strip()

    # Generate tags
    tags = [category]
    if file_path:
        parts = file_path.replace("/", ".").split(".")
        tags.extend([p for p in parts if len(p) > 2 and p not in ("src", "rs", "py", "ts")][:2])

    assistant_msg = json.dumps({
        "cause": cause,
        "effect": effect,
        "tags": tags,
    })
    return {
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": user_msg},
            {"role": "assistant", "content": assistant_msg},
        ]
    }


AUGMENT_FILES = [
    "odin/src/handlers.rs", "odin/src/proxy.rs", "odin/src/agent.rs", "odin/src/flow.rs",
    "odin/src/router.rs", "odin/src/state.rs", "odin/src/main.rs",
    "mimir/src/handlers.rs", "mimir/src/saga.rs", "mimir/src/main.rs",
    "muninn/src/parser.rs", "muninn/src/chunker.rs", "muninn/src/indexer.rs",
    "ygg-domain/src/config.rs", "ygg-domain/src/engram.rs", "ygg-domain/src/sdr.rs",
    "ygg-config/src/impls.rs", "ygg-config/src/lib.rs",
    "ygg-store/src/postgres/engrams.rs", "ygg-store/src/postgres/migrations.rs",
    "ygg-ha/src/client.rs", "ygg-ha/src/automation.rs",
    "ygg-node/src/discovery.rs", "ygg-node/src/main.rs",
    "ygg-voice/src/lib.rs", "ygg-voice/src/stt.rs",
    "deploy/munin/odin.service", "deploy/munin/.env",
    "sprints/sprint-055.md", "docs/ARCHITECTURE.md", "docs/USAGE.md",
    "training/eval/review_bench.py", "training/experiment/train_350m.py",
]

AUGMENT_TOOLS = ["Edit", "Bash", "Read", "Write"]

BUG_VERBS = [
    "Fixed", "Resolved", "Patched", "Corrected", "Repaired",
    "Addressed", "Eliminated", "Debugged", "Hotfixed", "Squashed",
]

ARCH_VERBS = [
    "Added", "Introduced", "Created", "Designed", "Implemented",
    "Refactored", "Restructured", "Migrated", "Split", "Consolidated",
]

ROUTINE_CONTENT = [
    "Added missing semicolon", "Fixed whitespace", "Updated import order",
    "cargo check passed", "cargo fmt applied", "git status clean",
    "Reading file contents", "Listing directory", "cat config file",
    "Added newline at EOF", "Removed trailing spaces", "Updated comment",
    "ollama list", "systemctl status", "df -h", "ls -la /opt/",
    "grep pattern in file", "head -20 output", "tail -f log",
    "echo test", "pwd", "whoami", "date", "uptime",
]


def augment_examples(base_examples: list, category: str, should_store: bool, target_count: int = 80) -> list:
    """Multiply base examples into target_count via template augmentation."""
    augmented = []
    for i in range(target_count):
        base = random.choice(base_examples)
        tool, file_path, content = base

        # Vary tool type
        if random.random() < 0.3:
            tool = random.choice(AUGMENT_TOOLS)

        # Vary file path
        if file_path and random.random() < 0.5:
            file_path = random.choice(AUGMENT_FILES)

        # Vary content phrasing for bug_fix
        if category == "bug_fix" and random.random() < 0.4:
            verb = random.choice(BUG_VERBS)
            parts = content.split(" — ", 1)
            if len(parts) == 2:
                content = f"{verb} {parts[0].split(' ', 1)[-1]} — {parts[1]}"

        # Vary content for architecture
        if category == "architecture_decision" and random.random() < 0.4:
            verb = random.choice(ARCH_VERBS)
            parts = content.split(" ", 1)
            if len(parts) == 2:
                content = f"{verb} {parts[1]}"

        # Generate routine variations
        if category == "routine":
            content = random.choice(ROUTINE_CONTENT)
            file_path = random.choice(AUGMENT_FILES) if random.random() < 0.5 else ""
            tool = random.choice(["Bash", "Read", "Edit"])

        augmented.append((tool, file_path, content))

    return augmented


def main():
    output_dir = Path(__file__).parent / "data"
    output_dir.mkdir(exist_ok=True)

    random.seed(42)
    all_examples = []

    for category, data in CATEGORIES.items():
        should_store = data["should_store"]
        base_examples = data["examples"]

        # More examples for weak categories (routine, gotcha)
        count = 150 if category in ("routine", "gotcha") else 80
        augmented = augment_examples(base_examples, category, should_store, target_count=count)

        for tool, file_path, content in augmented:
            all_examples.append(generate_classify_example(tool, file_path, content, category, should_store))
            if should_store:
                all_examples.append(generate_distill_example(tool, file_path, content, category))

    random.shuffle(all_examples)

    # Split 80/20
    split = int(len(all_examples) * 0.8)
    train = all_examples[:split]
    val = all_examples[split:]

    train_path = output_dir / "saga_synthetic_train.jsonl"
    val_path = output_dir / "saga_synthetic_val.jsonl"

    with open(train_path, "w") as f:
        for ex in train:
            f.write(json.dumps(ex) + "\n")

    with open(val_path, "w") as f:
        for ex in val:
            f.write(json.dumps(ex) + "\n")

    # Stats
    classify_count = sum(1 for e in all_examples if "CLASSIFY" in e["messages"][1]["content"])
    distill_count = sum(1 for e in all_examples if "DISTILL" in e["messages"][1]["content"])
    cat_counts = {}
    for e in all_examples:
        if "CLASSIFY" in e["messages"][1]["content"]:
            resp = json.loads(e["messages"][2]["content"])
            cat = resp.get("category", "?")
            cat_counts[cat] = cat_counts.get(cat, 0) + 1

    print(f"Generated {len(all_examples)} examples ({len(train)} train, {len(val)} val)")
    print(f"  CLASSIFY: {classify_count}, DISTILL: {distill_count}")
    print(f"  Categories:")
    for cat, count in sorted(cat_counts.items()):
        print(f"    {cat}: {count}")
    print(f"  Train: {train_path}")
    print(f"  Val: {val_path}")


if __name__ == "__main__":
    main()
