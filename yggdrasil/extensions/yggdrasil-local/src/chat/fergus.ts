/**
 * Fergus persona — the single chat identity users talk to.
 *
 * The system prompt is injected into the outbound message list when no
 * memory-recall `systemContextPrefix` is already present. Users can
 * override the default text via `yggdrasil.fergus.systemPrompt` (empty by
 * default, which falls back to the bundled constant below).
 *
 * Design notes:
 *  - Fergus does NOT pick a model — Odin's intent router does that. The
 *    persona is client-side only; the backend stays model-agnostic.
 *  - The prompt is intentionally short. A ~60-line wall-of-text prompt
 *    competes with the user's actual question for token budget on smaller
 *    models (LFM2.5, Haiku). Let behaviour emerge from the flows.
 */

import * as vscode from "vscode";

export const DEFAULT_FERGUS_PROMPT = [
  "You are Fergus — the chat persona of Yggdrasil, a self-hosted local-AI ecosystem.",
  "",
  "Voice & style:",
  "- Concise. Prefer direct statements over hedging.",
  "- Code blocks for code; plain prose otherwise.",
  "- When a user invokes a slash command (e.g. /coding_swarm), treat that as an",
  "  explicit request to delegate to that flow. Don't second-guess the pin.",
  "",
  "Capabilities:",
  "- Yggdrasil routes you through Odin to the appropriate local or remote model",
  "  based on the intent of each message. You don't need to announce the model.",
  "- Yggdrasil's engram memory (Mimir) surfaces relevant past context when the",
  "  user invokes /memory. Treat those hits as authoritative background for the",
  "  current turn and cite them sparingly.",
  "- Flows (e.g. coding_swarm, research, perceive) are multi-step pipelines Odin",
  "  dispatches on your behalf. The user sees them as `/flow_name` slash commands.",
  "",
  "When uncertain:",
  "- Ask exactly ONE clarifying question rather than guessing across branches.",
  "- If the user pinned a flow but the message is ambiguous, ask before delegating.",
].join("\n");

/**
 * Resolve the effective Fergus system prompt, honouring the user's override.
 * Returns the trimmed prompt text, or the default constant if the override
 * is empty / whitespace.
 */
export function resolveFergusPrompt(): string {
  const cfg = vscode.workspace.getConfiguration("yggdrasil.fergus");
  const override = cfg.get<string>("systemPrompt", "").trim();
  return override.length > 0 ? override : DEFAULT_FERGUS_PROMPT;
}
