#!/bin/bash
# store-insights.sh — Extract ★ Insight blocks from transcript and store as Yggdrasil engrams.
# Fires on the Stop hook event. Receives session JSON on stdin.
# All failures exit 0 to never block Claude.

set -uo pipefail

# Bail if MUNIN_IP not set (no Odin endpoint)
if [[ -z "${MUNIN_IP:-}" ]]; then
  exit 0
fi

# Read hook input from stdin
HOOK_INPUT=$(cat)

ODIN_URL="http://${MUNIN_IP}:8080/api/v1/store"
SESSION_ID=$(echo "$HOOK_INPUT" | jq -r '.session_id // ""' 2>/dev/null)
TRANSCRIPT_PATH=$(echo "$HOOK_INPUT" | jq -r '.transcript_path // ""' 2>/dev/null)

if [[ -z "$TRANSCRIPT_PATH" ]] || [[ ! -f "$TRANSCRIPT_PATH" ]]; then
  exit 0
fi

mkdir -p /tmp/ygg-hooks

export TRANSCRIPT_PATH ODIN_URL SESSION_ID

python3 << 'PYEOF'
import json, re, hashlib, subprocess, sys, os

transcript_path = os.environ.get("TRANSCRIPT_PATH", "")
odin_url = os.environ.get("ODIN_URL", "")
session_id = os.environ.get("SESSION_ID", "")

if not transcript_path or not odin_url:
    sys.exit(0)

seen_file = f"/tmp/ygg-hooks/insights_seen_{session_id}"

# Load already-seen hashes
seen = set()
try:
    with open(seen_file) as f:
        seen = set(line.strip() for line in f if line.strip())
except FileNotFoundError:
    pass

# Regex for insight blocks (backtick-wrapped markers with variable-length dashes)
insight_pattern = re.compile(
    r'`\u2605 Insight \u2500+`\s*\n(.*?)\n`\u2500+`',
    re.DOTALL
)

insights = []
last_user_msg = ""

with open(transcript_path) as f:
    for line in f:
        try:
            entry = json.loads(line.strip())
        except (json.JSONDecodeError, ValueError):
            continue

        msg = entry.get("message", {})
        if not isinstance(msg, dict):
            continue
        role = msg.get("role", "")

        if role == "user":
            for block in msg.get("content", []):
                if isinstance(block, dict) and block.get("type") == "text":
                    last_user_msg = block["text"][:200]
                elif isinstance(block, str):
                    last_user_msg = block[:200]

        elif role == "assistant":
            for block in msg.get("content", []):
                if not isinstance(block, dict) or block.get("type") != "text":
                    continue
                text = block.get("text", "")
                for match in insight_pattern.finditer(text):
                    insight_text = match.group(1).strip()
                    if not insight_text:
                        continue
                    h = hashlib.md5(insight_text.encode()).hexdigest()[:16]
                    if h not in seen:
                        seen.add(h)
                        insights.append({
                            "cause": f"Claude Code session insight: {last_user_msg}",
                            "effect": insight_text,
                            "tags": ["insight", "claude-code", "session"],
                            "force": False
                        })

# Write updated seen hashes
with open(seen_file, "w") as f:
    for h in seen:
        f.write(h + "\n")

# Fire-and-forget POST each insight
for insight in insights:
    body = json.dumps(insight)
    try:
        subprocess.Popen(
            ["curl", "-s", "-X", "POST", odin_url,
             "-H", "Content-Type: application/json",
             "-d", body,
             "--max-time", "5",
             "-o", "/dev/null"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL
        )
    except Exception:
        pass
PYEOF

exit 0
