# Yggdrasil Extension — Release Smoke Test

Run this runbook against a fresh VS Code window after every VSIX bump, before
tagging the release. Every section is pass/fail; if any section fails, the
release is blocked.

- **Target Odin:** http://10.0.65.8:8080 (Munin)
- **Target Mimir:** http://10.0.65.8:9090 (Munin)
- **Workstation prereqs:** the `code` CLI on `$PATH`, extension source built
  (`npm run compile` in `extensions/yggdrasil-local/`), a VSIX produced by
  `npx @vscode/vsce package --no-dependencies`.

Each `[ ]` is a tick-box. Fill them in, commit the file as the release record.

---

## 0. Build artifact

- [ ] `npx @vscode/vsce package --no-dependencies` produces `yggdrasil-local-<version>.vsix` with **0 warnings**.
- [ ] The version in `package.json` matches the VSIX filename.
- [ ] `CHANGELOG.md` has a section for `[<version>]`.

## 1. Install & activation

- [ ] `code --install-extension yggdrasil-local-<version>.vsix --force` exits 0.
- [ ] Reloading VS Code lights up the Yggdrasil activity-bar icon.
- [ ] The status-bar item reads either green ("yggdrasil ok") or a colour-coded
      health dot — not absent, not error-red.
- [ ] Opening the output channel "Yggdrasil" shows activation log with no stack
      traces.

## 2. Walkthrough (first-run experience)

Open the walkthrough via `Help → Welcome → Walkthroughs → Get Started with Yggdrasil`.

- [ ] Step 1 (welcome) renders markdown correctly.
- [ ] Step 2 (configure Odin) — click-through opens Settings panel at Endpoints
      tab; health probe button succeeds against Munin.
- [ ] Step 3 (configure Mimir) renders and the Mimir URL field persists after
      reload.
- [ ] Step 4 (explore flows) opens the Flows explorer (`Ctrl+Shift+Y`).
- [ ] Step 5 (open chat) opens the Chat panel (`Ctrl+Shift+I`).

## 3. Activity-bar tree views

- [ ] Flows tree lists at minimum: coding_swarm, code_qa, code_docs, devops,
      ui_design, dba, complex_reasoning, perceive (matches Odin `/api/flows`).
- [ ] Models tree lists the live models from Odin `/v1/models`
      (nemotron-3-nano:4b, glm-4.7-flash, gemma4:e4b, fusion-v6:latest, ...).
- [ ] Models tree refreshes every ~30 s (or via refresh command).

## 4. Commands (palette)

Every command under `yggdrasil.*` must fire without throwing.

- [ ] `Yggdrasil: Open Dashboard` (`Ctrl+Shift+M`).
- [ ] `Yggdrasil: Open Flows` (`Ctrl+Shift+Y`).
- [ ] `Yggdrasil: Open Chat` (`Ctrl+Shift+I`).
- [ ] `Yggdrasil: Open Settings`.
- [ ] `Yggdrasil: Refresh Flows`.
- [ ] `Yggdrasil: Refresh Models`.
- [ ] `Yggdrasil: Use Model in Chat` (from Models tree context menu).
- [ ] `Yggdrasil: Show Log`.
- [ ] `Yggdrasil: Toggle Notifications`.
- [ ] `Yggdrasil: Reinstall Hooks` — toasts success, `~/.claude/settings.json`
      gets re-patched.
- [ ] `Yggdrasil: Check for Updates` — either "no newer version" or prompts
      install; fails visibly (not silently) on auth errors.

## 5. Chat panel (streaming)

Open the Chat panel (`Ctrl+Shift+I`).

- [ ] Model picker lists all models from Models tree.
- [ ] Flow picker lists flows from Flows tree.
- [ ] Send "hello" against default flow — streams tokens, renders markdown,
      stop button works mid-stream.
- [ ] History persists across panel close/reopen (max 50 entries).
- [ ] Stop button cancels a long-running response cleanly.

## 6. Chat slash commands

In the Chat panel input:

- [ ] `/help` — renders help card with all commands.
- [ ] `/flow coding_swarm write fibonacci in rust` — routes through coding_swarm
      and returns code.
- [ ] `/model gemma4:e4b hello` — routes to the explicit model.
- [ ] `/memory "sprint 059"` — calls Mimir `/api/v1/query` and renders results.
- [ ] `/clear` — empties the visible thread (does not delete `globalState`).

## 7. Editor code actions

Open any source file, make a selection, right-click → **Yggdrasil**:

- [ ] `Yggdrasil: Explain Selection` (`Ctrl+Shift+E`) opens Chat with the
      selection attached as context.
- [ ] `Yggdrasil: Edit With Model` prompts for an instruction, then rewrites
      the selection in-place (or produces a diff preview).
- [ ] `Yggdrasil: Ask About This File` attaches the full file as context in
      Chat.

## 8. Settings panel — Endpoints tab

- [ ] Odin URL health probe reports green against Munin.
- [ ] Mimir URL health probe reports green (or explains the failure).
- [ ] Saving a URL change persists (reload → same value).

## 9. Settings panel — Flows tab (Odin CRUD, Sprint 059)

Requires a rebuilt Odin with the flow CRUD endpoints deployed.

- [ ] Flows list loads from `GET /api/flows` (not local JSON fallback).
- [ ] Clicking a flow opens a per-step editor.
- [ ] Editing `coding_swarm.generate.system_prompt`, hitting Save, produces
      a "Saved" toast and the new prompt round-trips through a page refresh.
- [ ] Attempting to save a step with an unknown backend returns a 400 error
      surfaced in the UI.
- [ ] Against a legacy Odin (no `/api/flows` endpoint), the editor falls back
      to read-only local-JSON viewing without throwing.

## 10. Settings panel — Notifications & Hooks

- [ ] Event filter toggles persist.
- [ ] Sound toggle persists.
- [ ] "Reinstall hooks" button runs successfully (see Section 4 above).

## 11. Settings panel — Secrets tab

- [ ] Storing `giteaToken` shows "stored in OS keychain" toast.
- [ ] Same for `githubToken`, `haToken`, `braveSearchKey`.
- [ ] Deleting a secret shows "deleted" toast and the row re-renders as empty.
- [ ] Token values are not logged in the output channel.

## 12. Auto-updater (Sprint 059 — v0.8.0)

Scenario A — Gitea provider with `REQUIRE_SIGNIN_VIEW` disabled:

- [ ] With no token set, `Yggdrasil: Check for Updates` succeeds (fetches
      releases anonymously).

Scenario B — Gitea provider with `REQUIRE_SIGNIN_VIEW` enabled:

- [ ] With no token set, the output channel prints "HTTP 401/403 — set a Gitea
      token in Yggdrasil → Settings → Secrets".
- [ ] After saving a `giteaToken`, `Yggdrasil: Check for Updates` succeeds and
      (if a newer version exists) prompts install.

Scenario C — GitHub provider (skip until we actually publish to GitHub):

- [ ] Switching `yggdrasil.autoUpdate.provider` to `github` and setting
      `yggdrasil.githubRepo` + a `githubToken` works the same way.

## 13a. Retro chat themes (Sprint 061 — v0.9.x)

- [ ] `yggdrasil.chat.theme: "classic"` (default) — chat panel unchanged from v0.8.x.
- [ ] `yggdrasil.chat.theme: "pipboy-green"` — phosphor-green-on-black with
      glow text-shadow on every visible element; monospace font; typography
      bumped (body 17px, code 16px, headings 20px).
- [ ] `yggdrasil.chat.theme: "bbs-cyan"` — cyan-on-near-black equivalent.
      Change switches live (no panel reopen) and persists across reloads.
- [ ] `yggdrasil.chat.crtEffects: true` — scanline overlay appears with
      animated sweep line and edge vignette; subtle flicker. Toggling false
      removes the `.crt-overlay` DOM node entirely (no compositing cost).
- [ ] `yggdrasil.chat.font: "vt323"` — bundled pixel font renders; VT323-only
      +2px bump applies (body 19px, code 17px).
- [ ] `yggdrasil.chat.font: "jetbrains-mono"` — JetBrains Mono stack falls
      back gracefully when the system lacks the font.
- [ ] Classic theme with any font setting: typography bumps do NOT apply
      (bumps are gated on retro themes).

## 13b. Swarm chat (Sprint 061 — streaming flows)

**Prereqs:** Odin running a build with Sprint 061 changes; `swarm_chat`
flow PUT to `/api/flows/swarm_chat`; `yggdrasil-ollama-warm.timer` active
on Munin + Hugin.

- [ ] Send "hello, who are you?" in Chat (any theme).
  - [ ] First visible tokens are drafter content in the main bubble
        (assistant stream) within ~800ms TTFT.
  - [ ] A collapsible "thinking" fold appears above the bubble with a
        `Cross-checking…` section streaming reviewer tokens.
  - [ ] Reviewer emits `LGTM` → refiner is skipped; no `── correction ──`
        divider appears; final bubble equals the drafter's output.
  - [ ] `journalctl -u yggdrasil-odin` shows
        `sentinel matched — skipping downstream steps skips=["refine"]`.
- [ ] Send an ambiguous query ("Is X better than Y? Explain with sources.").
  - [ ] Drafter streams; reviewer streams into fold; sentinel does NOT fire.
  - [ ] Refiner starts; `── correction ──` divider inserted into the main
        bubble; refiner's content continues below the divider.
- [ ] SSE contract: `curl -Ns http://$ODIN/v1/chat/completions -d '{...}'`
      against the same prompt shows:
  - [ ] Intermediate `event: ygg_step` frames with `phase:step_start|step_delta|step_end`.
  - [ ] Unnamed `data: {ChatCompletionChunk}` frames only for drafter (and
        refiner when it runs).
  - [ ] Stream terminates with `data: [DONE]`.
- [ ] `stream: false` non-flow request: `curl -X POST
      /v1/chat/completions` with explicit `"model":"nemotron-3-nano:4b"` and
      `"stream":false` still returns a single JSON object (backwards compat).
- [ ] `intent_default: "chat"` in Odin config: low-confidence /
      no-clear-intent queries route to `swarm_chat`; `journalctl` shows
      `intent=chat method=Fallback` for such queries.
- [ ] Prefix cache hit: send two chats in quick succession with the same
      session; drafter's second run Ollama logs show
      `prompt_eval_count: 0` for the system-prompt tokens (cache hit).
- [ ] Retro theme + swarm chat together: thinking fold inherits
      phosphor/cyan color; scanlines pass through the fold; correction
      divider is legible.

## 13c. Ollama warm-up timer

- [ ] `systemctl status yggdrasil-ollama-warm.timer` on Munin and Hugin
      shows `active (waiting)` with a next-fire timestamp.
- [ ] `sudo systemctl start yggdrasil-ollama-warm.service` returns exit 0
      and `journalctl -u yggdrasil-ollama-warm` shows
      `warming <model>...` for each model in `YGGDRASIL_WARM_MODELS`.
- [ ] Offline model in the list does NOT block other models
      (`SuccessExitStatus=0 1` + best-effort curl).

## 14. Regression — existing features

- [ ] Fusion V6 smoke: send "Write a Fusion 360 Python snippet that creates a
      10mm cube." through `/model fusion-v6:latest` → returns valid `adsk.*`
      Python.
- [ ] Perceive flow: attach an image to Chat, gets a vision caption from Hugin
      890M iGPU.
- [ ] Memory push: after a chat, check `tail /tmp/ygg-hooks/memory-events.jsonl`
      → new `sidecar→ingest` pair appears with `stored:true`.

## 15. Voice Push-to-Talk Round-Trip (Sprint 062 feature, Sprint 063 E2E)

Manual only — requires a physical microphone. Not automated in CI.

**Prereqs:** Odin running Sprint 062 build; `yggdrasil.voice.enabled=true` in
settings; Odin voice WebSocket reachable at `ws://10.0.65.8:8080/v1/voice`.

- [ ] Set `yggdrasil.voice.enabled=true` in extension settings (Settings panel →
      Endpoints tab or via VS Code settings JSON).
- [ ] Reload the chat panel — mic button appears in the chat titlebar.
- [ ] Click mic button → browser microphone permission prompt appears on first run.
      Grant the permission.
- [ ] Hold (or toggle) mic button → status indicator shows `●REC` state.
- [ ] Release mic → status transitions to `processing` → transcript text appears
      in the chat input or as a user message within ~2 s.
- [ ] Assistant response generates → TTS audio plays back through system audio
      within ~2 s of the response completing.
- [ ] Keyboard hotkey `Ctrl+Shift+Space` (macOS: `Cmd+Shift+Space`) triggers the
      same record→transcribe→respond flow as the mic button.
- [ ] Simulate network interruption mid-session (e.g.
      `nmcli device disconnect <iface>` then reconnect, or disable/enable Wi-Fi)
      — the voice WebSocket reconnects within 60 s and the same `session_id` is
      reused. Verify in Odin logs: `journalctl -u yggdrasil-odin | grep session_id`.
- [ ] During the entire voice session, the VS Code Developer Tools console
      (`Help → Toggle Developer Tools → Console`) shows **zero** lines beginning
      with `Content Security Policy` or `Refused to load`.

---

## Result

- **Tested by:** \_\_\_\_
- **Date:** \_\_\_\_
- **Extension version:** \_\_\_\_
- **Odin commit SHA:** \_\_\_\_
- **Result:** PASS / FAIL — if FAIL, list failed section IDs.
