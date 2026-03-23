#!/usr/bin/env bash
# Seed insight template engrams into Mimir for SDR template matching.
# Run once after deploying Sprint 044, or after wiping the engrams table.
# Each template is stored with tags ["insight_template", "<category>"] and force=true.
#
# Template text is intentionally verbose and keyword-rich to produce distinct SDR
# fingerprints that separate well in Hamming space. Short generic descriptions
# produce overlapping embeddings that cause misclassification.

MIMIR_URL="${MIMIR_URL:-http://<munin-ip>:9090}"

echo "=== Seeding Mimir insight templates ==="
echo "Target: ${MIMIR_URL}"
echo ""

# Delete existing insight templates first to avoid accumulation
echo "Clearing old insight templates..."
OLD_IDS=$(curl -s -X POST "${MIMIR_URL}/api/v1/query" \
    -H "Content-Type: application/json" \
    -d '{"text":"insight_template","limit":20}' | jq -r '.[] | select(.tags != null) | select(.tags[] == "insight_template") | .id // empty' 2>/dev/null)
for OLD_ID in $OLD_IDS; do
    curl -s -X DELETE "${MIMIR_URL}/api/v1/engrams/${OLD_ID}" -o /dev/null
    echo "  cleared $OLD_ID"
done
echo ""

seed_template() {
    local category="$1"
    local cause="$2"
    local effect="$3"

    payload=$(jq -n \
        --arg cause "$cause" \
        --arg effect "$effect" \
        --arg cat "$category" \
        '{cause: $cause, effect: $effect, tags: ["insight_template", $cat], force: true}')

    response=$(curl --silent -X POST \
        -H "Content-Type: application/json" \
        -d "$payload" \
        "${MIMIR_URL}/api/v1/store")

    id=$(echo "$response" | jq -r '.id // "error"' 2>/dev/null || echo "error")
    if [ "$id" != "error" ] && [ "$id" != "null" ]; then
        echo "[OK] $category -> $id"
    else
        echo "[FAIL] $category: $response"
    fi
}

seed_template "bug_fix" \
    "Bug fix: Fixed a crash, segfault, panic, error, exception, or failure in the code. Resolved a regression, null pointer, off-by-one, race condition, deadlock, memory leak, or stack overflow. Debugging session identified root cause. Error handling corrected. Test now passes after fix applied." \
    "bug_fix"

seed_template "architecture_decision" \
    "Architecture decision: Refactored module structure, split crate, defined API contract, changed service boundary, added new endpoint, reorganized imports and dependencies. Designed schema, defined data flow, chose between implementation approaches. Updated ARCHITECTURE.md with new component diagram or dependency graph." \
    "architecture_decision"

seed_template "sprint_lifecycle" \
    "Sprint lifecycle: Started sprint, completed sprint, modified sprint scope, sprint planning session, sprint review, acceptance criteria defined, phase transition, milestone reached, sprint document created or updated, backlog groomed, velocity tracked." \
    "sprint_lifecycle"

seed_template "user_feedback" \
    "User feedback: User corrected approach, user said stop, user said don't do that, user preference noted, user wants different behavior, user confirmed approach works, user approved design, user rejected proposal, coding style preference, workflow preference expressed." \
    "user_feedback"

seed_template "deployment_change" \
    "Deployment change: Deployed binary to server, restarted systemd service, updated Docker container, changed environment variables, modified nginx config, edited config.yaml or config.json on production node, ran scp to copy files, SSH to server to install, systemctl restart, port changed, service endpoint moved." \
    "deployment_change"

seed_template "gotcha" \
    "Gotcha discovered: Non-obvious behavior found, workaround needed, silent failure detected, unexpected constraint, version incompatibility, undocumented requirement, platform-specific quirk, config option does not work as expected, retry logic hides errors, stale cache returns wrong data, must use exact version pin." \
    "gotcha"

echo ""
echo "=== Seeding complete ==="
