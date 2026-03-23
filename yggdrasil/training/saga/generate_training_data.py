#!/usr/bin/env python3
"""Generate Saga training data from extracted engrams + synthetic negatives + git log.

Produces instruction pairs for all 4 tasks:
  CLASSIFY: tool + file + content → {category, should_store, confidence}
  DISTILL:  tool + file + content → {cause, effect, tags}
  QUERY:    file + snippet → {query}
  FILTER:   recall results + context → {relevant_indices}
"""

import json
import os
import random
import subprocess
import re
from pathlib import Path

BARN = "/data/saga/data"
REPO = "./yggdrasil"
SYSTEM_PROMPT = "You are Saga, Yggdrasil's memory engine. Respond ONLY in valid JSON."

# Category mapping from tags to Saga categories
TAG_TO_CATEGORY = {
    "bugfix": "bug_fix",
    "bug": "bug_fix",
    "fix": "bug_fix",
    "error": "bug_fix",
    "architecture": "architecture_decision",
    "decision": "architecture_decision",
    "refactor": "architecture_decision",
    "sprint": "sprint_lifecycle",
    "milestone": "sprint_lifecycle",
    "planning": "sprint_lifecycle",
    "deployment": "deployment_change",
    "infrastructure": "deployment_change",
    "deploy": "deployment_change",
    "config": "deployment_change",
    "gotcha": "gotcha",
    "workaround": "gotcha",
    "quirk": "gotcha",
    "finding-warning": "gotcha",
    "feedback": "user_feedback",
    "preference": "user_feedback",
    "user": "user_feedback",
}

CATEGORIES = ["bug_fix", "architecture_decision", "sprint_lifecycle",
              "user_feedback", "deployment_change", "gotcha"]

# Simulated tool names for training variety
TOOLS = ["Edit", "Write", "Bash"]

# Common Rust/Yggdrasil file paths for realistic examples
SAMPLE_FILES = [
    "crates/mimir/src/handlers.rs",
    "crates/odin/src/router.rs",
    "crates/ygg-domain/src/engram.rs",
    "crates/ygg-config/src/lib.rs",
    "crates/huginn/src/watcher.rs",
    "crates/muninn/src/search.rs",
    "crates/ygg-mesh/src/discovery.rs",
    "crates/ygg-ha/src/notify.rs",
    "deploy/workstation/ygg-hooks-init.sh",
    "docs/ARCHITECTURE.md",
]


def load_engrams():
    """Load raw engrams from extraction output."""
    engrams = []
    with open(f"{BARN}/engrams_raw.jsonl") as f:
        for line in f:
            engrams.append(json.loads(line))
    return engrams


def classify_engram(engram):
    """Determine Saga category from engram tags."""
    tags = [t.lower() for t in (engram.get("tags") or [])]
    for tag in tags:
        if tag in TAG_TO_CATEGORY:
            return TAG_TO_CATEGORY[tag]
    # Heuristic: check cause/effect text for category signals
    text = (engram.get("cause", "") + " " + engram.get("effect", "")).lower()
    if any(w in text for w in ["bug", "fix", "crash", "error", "panic", "segfault"]):
        return "bug_fix"
    if any(w in text for w in ["refactor", "architect", "split crate", "api contract", "module"]):
        return "architecture_decision"
    if any(w in text for w in ["sprint", "phase", "milestone", "acceptance"]):
        return "sprint_lifecycle"
    if any(w in text for w in ["deploy", "systemd", "docker", "scp", "nginx", "systemctl"]):
        return "deployment_change"
    if any(w in text for w in ["gotcha", "workaround", "quirk", "silent", "unexpected"]):
        return "gotcha"
    if any(w in text for w in ["user", "feedback", "prefer", "correct", "don't"]):
        return "user_feedback"
    return random.choice(CATEGORIES)


def make_message(system, user, assistant):
    """Create a chat-format training example."""
    return {
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
            {"role": "assistant", "content": assistant},
        ]
    }


def truncate(text, max_chars):
    """Truncate text to max_chars at word boundary."""
    if len(text) <= max_chars:
        return text
    return text[:max_chars].rsplit(" ", 1)[0] + "..."


def generate_classify_pairs(engrams):
    """Generate CLASSIFY task pairs from real engrams (positive examples)."""
    pairs = []
    for eng in engrams:
        category = classify_engram(eng)
        cause = truncate(eng["cause"], 300)
        tool = random.choice(TOOLS)
        file_path = random.choice(SAMPLE_FILES)

        user_text = f"CLASSIFY\ntool: {tool}\nfile: {file_path}\ncontent: {cause}"
        assistant_text = json.dumps({
            "category": category,
            "should_store": True,
            "confidence": round(random.uniform(0.80, 0.98), 2),
        })
        pairs.append(make_message(SYSTEM_PROMPT, user_text, assistant_text))
    return pairs


def generate_classify_negatives():
    """Generate CLASSIFY negative examples (should_store: false)."""
    pairs = []

    # Type 1: Random English prose
    noise_texts = [
        "The quick brown fox jumps over the lazy dog",
        "Hello world this is a test",
        "Lorem ipsum dolor sit amet consectetur adipiscing elit",
        "TODO: clean up this comment later",
        "// placeholder for future implementation",
        "Updated formatting and whitespace",
        "Minor style fix",
        "Adjusted indentation to match project style",
        "Reordered imports alphabetically",
        "Removed trailing whitespace from all files",
        "Added newline at end of file",
        "Fixed typo in comment: recieve -> receive",
        "Bumped version number to 0.2.1",
        "Ran cargo fmt on all crates",
        "Updated .gitignore to exclude build artifacts",
    ]

    # Type 2: Code snippets that are routine, not insight-worthy
    routine_code = [
        "use std::collections::HashMap;\nuse serde::{Deserialize, Serialize};",
        "fn main() {\n    println!(\"Hello, world!\");\n}",
        "#[derive(Debug, Clone)]\npub struct Config {\n    pub name: String,\n}",
        "let mut v = Vec::new();\nv.push(42);\nv.push(99);",
        "if let Some(x) = opt {\n    process(x);\n}",
        "impl Display for MyError {\n    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {\n        write!(f, \"error\")\n    }\n}",
        "async fn handler() -> impl IntoResponse {\n    Json(json!({\"status\": \"ok\"}))\n}",
        "pub fn new() -> Self {\n    Self { inner: Default::default() }\n}",
        "let config = Config::from_file(\"config.json\")?;",
        "tracing::info!(\"server started on port {}\", port);",
    ]

    # Type 3: Bash commands that are routine
    routine_bash = [
        "ls -la",
        "git status",
        "cargo check",
        "cat README.md",
        "pwd",
        "cd /home/jesus/Documents",
        "grep -r 'TODO' src/",
        "wc -l src/*.rs",
        "git log --oneline -5",
        "npm install",
    ]

    for text in noise_texts:
        tool = random.choice(TOOLS)
        user_text = f"CLASSIFY\ntool: {tool}\nfile: {random.choice(SAMPLE_FILES)}\ncontent: {text}"
        assistant_text = json.dumps({
            "category": "none",
            "should_store": False,
            "confidence": round(random.uniform(0.85, 0.99), 2),
        })
        pairs.append(make_message(SYSTEM_PROMPT, user_text, assistant_text))

    for code in routine_code:
        user_text = f"CLASSIFY\ntool: Edit\nfile: {random.choice(SAMPLE_FILES)}\ncontent: {code}"
        assistant_text = json.dumps({
            "category": "none",
            "should_store": False,
            "confidence": round(random.uniform(0.80, 0.95), 2),
        })
        pairs.append(make_message(SYSTEM_PROMPT, user_text, assistant_text))

    for cmd in routine_bash:
        user_text = f"CLASSIFY\ntool: Bash\nfile: \ncontent: {cmd}"
        assistant_text = json.dumps({
            "category": "none",
            "should_store": False,
            "confidence": round(random.uniform(0.85, 0.99), 2),
        })
        pairs.append(make_message(SYSTEM_PROMPT, user_text, assistant_text))

    return pairs


def generate_distill_pairs(engrams):
    """Generate DISTILL task pairs from real engrams."""
    pairs = []
    for eng in engrams:
        cause = eng["cause"]
        effect = eng["effect"]
        tags = eng.get("tags") or []
        # Keep only relevant tags (strip project: prefix etc)
        clean_tags = [t for t in tags if not t.startswith("project:")][:5]

        # Use cause as the "raw content" input (simulates what a hook would see)
        content = truncate(cause + "\n" + effect, 2000)
        tool = random.choice(TOOLS)
        file_path = random.choice(SAMPLE_FILES)

        user_text = f"DISTILL\ntool: {tool}\nfile: {file_path}\ncontent: {content}"
        assistant_text = json.dumps({
            "cause": truncate(cause, 200),
            "effect": truncate(effect, 500),
            "tags": clean_tags[:3] if clean_tags else [classify_engram(eng)],
        }, ensure_ascii=False)
        pairs.append(make_message(SYSTEM_PROMPT, user_text, assistant_text))
    return pairs


def generate_query_pairs(engrams):
    """Generate QUERY task pairs — given file context, produce optimal recall query."""
    pairs = []
    for eng in engrams:
        cause = eng["cause"]
        tags = eng.get("tags") or []

        # The "query" should be a focused search string that would recall this engram
        # Extract key terms from the cause
        words = cause.split()
        key_terms = [w for w in words if len(w) > 4 and w.isalpha()][:8]
        query = " ".join(key_terms) if key_terms else truncate(cause, 80)

        file_path = random.choice(SAMPLE_FILES)
        snippet = truncate(cause, 200)

        user_text = f"QUERY\nfile: {file_path}\ncontent: {snippet}"
        assistant_text = json.dumps({"query": query})
        pairs.append(make_message(SYSTEM_PROMPT, user_text, assistant_text))
    return pairs


def generate_filter_pairs(engrams):
    """Generate FILTER task pairs — given recall results, pick relevant ones."""
    pairs = []
    # Create groups of 3-5 engrams, mark 1-2 as relevant based on shared tags
    random.shuffle(engrams)
    for i in range(0, len(engrams) - 5, 3):
        group = engrams[i:i+5]
        # Pick a "context" from the first engram
        context_eng = group[0]
        context_tags = set(context_eng.get("tags") or [])
        context_file = random.choice(SAMPLE_FILES)
        context_snippet = truncate(context_eng["cause"], 150)

        results = []
        relevant = []
        for j, eng in enumerate(group):
            eng_tags = set(eng.get("tags") or [])
            overlap = context_tags & eng_tags
            results.append({
                "cause": truncate(eng["cause"], 100),
                "effect": truncate(eng["effect"], 100),
                "similarity": round(random.uniform(0.6, 0.9), 2),
            })
            # Relevant if tags overlap or same category
            if overlap and j > 0:  # index 0 is always relevant (it's the context)
                relevant.append(j)
        relevant.insert(0, 0)  # context engram is always relevant

        user_text = f"FILTER\nresults: {json.dumps(results)}\ncontext: {context_file} {context_snippet}"
        assistant_text = json.dumps({"relevant_indices": sorted(set(relevant))})
        pairs.append(make_message(SYSTEM_PROMPT, user_text, assistant_text))
    return pairs


def mine_git_log():
    """Extract realistic tool-output examples from git history."""
    pairs = []
    try:
        result = subprocess.run(
            ["git", "log", "--diff-filter=M", "--format=%H %s", "-200"],
            capture_output=True, text=True, cwd=REPO
        )
        commits = result.stdout.strip().split("\n")

        for line in commits[:100]:
            parts = line.split(" ", 1)
            if len(parts) < 2:
                continue
            sha, msg = parts

            # Classify commit message
            msg_lower = msg.lower()
            if any(w in msg_lower for w in ["fix", "bug", "crash", "error"]):
                category = "bug_fix"
            elif any(w in msg_lower for w in ["refactor", "split", "reorganize", "architect"]):
                category = "architecture_decision"
            elif any(w in msg_lower for w in ["sprint", "phase", "milestone"]):
                category = "sprint_lifecycle"
            elif any(w in msg_lower for w in ["deploy", "docker", "systemd", "config"]):
                category = "deployment_change"
            elif any(w in msg_lower for w in ["chore", "fmt", "lint", "style", "bump"]):
                # Routine commits — negative examples
                user_text = f"CLASSIFY\ntool: Bash\nfile: \ncontent: git commit: {msg}"
                assistant_text = json.dumps({
                    "category": "none",
                    "should_store": False,
                    "confidence": round(random.uniform(0.85, 0.95), 2),
                })
                pairs.append(make_message(SYSTEM_PROMPT, user_text, assistant_text))
                continue
            else:
                # Default: might be worth storing
                category = random.choice(CATEGORIES)

            user_text = f"CLASSIFY\ntool: Bash\nfile: \ncontent: git commit: {msg}"
            assistant_text = json.dumps({
                "category": category,
                "should_store": True,
                "confidence": round(random.uniform(0.75, 0.95), 2),
            })
            pairs.append(make_message(SYSTEM_PROMPT, user_text, assistant_text))

    except Exception as e:
        print(f"Warning: git log mining failed: {e}", file=sys.stderr)

    return pairs


def main():
    import sys
    random.seed(42)  # Reproducible

    print("Loading engrams...")
    engrams = load_engrams()
    print(f"  {len(engrams)} engrams loaded")

    all_pairs = []

    print("Generating CLASSIFY pairs (positive)...")
    classify_pos = generate_classify_pairs(engrams)
    print(f"  {len(classify_pos)} classify positive pairs")
    all_pairs.extend(classify_pos)

    print("Generating CLASSIFY pairs (negative)...")
    classify_neg = generate_classify_negatives()
    print(f"  {len(classify_neg)} classify negative pairs")
    all_pairs.extend(classify_neg)

    print("Generating DISTILL pairs...")
    distill = generate_distill_pairs(engrams)
    print(f"  {len(distill)} distill pairs")
    all_pairs.extend(distill)

    print("Generating QUERY pairs...")
    query = generate_query_pairs(engrams)
    print(f"  {len(query)} query pairs")
    all_pairs.extend(query)

    print("Generating FILTER pairs...")
    filter_pairs = generate_filter_pairs(engrams)
    print(f"  {len(filter_pairs)} filter pairs")
    all_pairs.extend(filter_pairs)

    print("Mining git log...")
    git_pairs = mine_git_log()
    print(f"  {len(git_pairs)} git-mined pairs")
    all_pairs.extend(git_pairs)

    # Shuffle
    random.shuffle(all_pairs)

    # Write combined output
    out_path = f"{BARN}/saga_combined.jsonl"
    with open(out_path, "w") as f:
        for pair in all_pairs:
            f.write(json.dumps(pair, ensure_ascii=False) + "\n")

    # Stats
    task_counts = {}
    neg_count = 0
    for p in all_pairs:
        user_msg = p["messages"][1]["content"]
        task = user_msg.split("\n")[0]
        task_counts[task] = task_counts.get(task, 0) + 1
        asst = json.loads(p["messages"][2]["content"])
        if asst.get("should_store") is False:
            neg_count += 1

    print(f"\n=== Dataset Summary ===")
    print(f"Total pairs: {len(all_pairs)}")
    for task, count in sorted(task_counts.items()):
        print(f"  {task}: {count}")
    print(f"  Negatives (should_store=false): {neg_count}")
    print(f"Output: {out_path}")


if __name__ == "__main__":
    main()
