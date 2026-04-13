/**
 * Slash command registry — preprocesses chat input before submission.
 *
 *   /flow <name>   — pin a flow to use for this turn (overrides model picker)
 *   /model <id>    — override model for this turn
 *   /memory <q>    — inject Mimir recall results as system context
 *   /clear         — wipe current thread (handled by chatPanel, not here)
 *   /help          — list available commands
 *
 * Unknown slash commands pass through to the model unchanged so the
 * chat can still be used with arbitrary leading slashes if needed.
 */

import { OdinClient } from "../api/odinClient";

export interface SlashResult {
  cleanedText: string;
  modelOverride?: string;
  flowOverride?: string;
  systemContextPrefix?: string;
  notice?: string;
}

export async function preprocess(raw: string, odin: OdinClient): Promise<SlashResult> {
  const text = raw.trim();
  if (!text.startsWith("/")) {
    return { cleanedText: raw };
  }

  const match = text.match(/^\/(\w+)(?:\s+(.*))?$/s);
  if (!match) {
    return { cleanedText: raw };
  }
  const cmd = match[1].toLowerCase();
  const arg = (match[2] ?? "").trim();

  switch (cmd) {
    case "flow": {
      if (!arg) {
        return { cleanedText: "", notice: "Usage: /flow <flow-name> <your message>" };
      }
      const parts = arg.split(/\s+/);
      const flowName = parts[0];
      const rest = arg.slice(flowName.length).trim();
      return {
        cleanedText: rest || "Run this flow with no additional context.",
        flowOverride: flowName,
        notice: `Flow pinned: ${flowName}`,
      };
    }

    case "model": {
      if (!arg) {
        return { cleanedText: "", notice: "Usage: /model <model-id> <your message>" };
      }
      const parts = arg.split(/\s+/);
      const modelId = parts[0];
      const rest = arg.slice(modelId.length).trim();
      return {
        cleanedText: rest || "Respond with the chosen model.",
        modelOverride: modelId,
        notice: `Model override: ${modelId}`,
      };
    }

    case "memory": {
      if (!arg) {
        return { cleanedText: "", notice: "Usage: /memory <query>" };
      }
      let hits;
      try {
        hits = await odin.queryMemory(arg, 5);
      } catch (err) {
        return {
          cleanedText: arg,
          notice: `Memory query failed: ${err instanceof Error ? err.message : String(err)}`,
        };
      }
      if (hits.length === 0) {
        return { cleanedText: arg, notice: "No matching memories found." };
      }
      const lines = hits
        .map(
          (h, i) =>
            `[${i + 1}] (sim ${(h.similarity * 100).toFixed(0)}%) ${h.cause}\n    → ${h.effect}`
        )
        .join("\n");
      const prefix = `Relevant engram memories (use as context if applicable):\n${lines}`;
      return {
        cleanedText: arg,
        systemContextPrefix: prefix,
        notice: `Injected ${hits.length} memory hits`,
      };
    }

    case "help": {
      const helpText = [
        "Available slash commands:",
        "  /flow <name> <msg>   Pin a flow (e.g. /flow coding_swarm write a cache)",
        "  /model <id> <msg>    Override model for this turn",
        "  /memory <query>      Inject Mimir recall results as context",
        "  /clear               Wipe current thread",
        "  /help                Show this help",
      ].join("\n");
      return { cleanedText: "", notice: helpText };
    }

    default:
      return { cleanedText: raw };
  }
}
