/**
 * Slash command registry — preprocesses chat input before submission.
 *
 * Sprint 068 Phase 3 rework: `/flow <name>` and `/model <id>` are gone.
 * Instead, ANY slash command whose first token matches a user-invocable
 * flow name is treated as `flowOverride`. Cron-only flows are filtered
 * out at the call site so the UI cannot pin one (preventing the
 * "HTTP 400: flow '<name>' is cron-only" path on Odin).
 *
 *   /<flow-name> <msg>  — pin that flow for this turn
 *   /memory <q>         — inject Mimir recall results as system context
 *   /clear              — wipe current thread (handled by chatPanel)
 *   /help               — list flows + builtins
 *
 * Unknown slash commands pass through to the model unchanged.
 */

import { OdinClient } from "../api/odinClient";

export interface SlashResult {
  cleanedText: string;
  flowOverride?: string;
  systemContextPrefix?: string;
  notice?: string;
}

/**
 * Minimum projection of a Flow needed to decide whether a name is pinnable.
 * `chatPanel.ts` passes the result of `OdinClient.listFlows()` directly.
 */
export interface KnownFlow {
  name: string;
  /** `trigger` shape from Odin's flow config — used to filter cron-only flows. */
  trigger?: unknown;
}

const STATIC_COMMANDS = new Set(["memory", "clear", "help"]);

/**
 * Return true if the flow's trigger is cron-only (not user-invocable).
 * Manual and Intent triggers are invocable; cron-only is the single reject case.
 */
export function isCronOnlyFlow(f: KnownFlow): boolean {
  const t = f.trigger;
  if (t === null || t === undefined) return false;
  if (typeof t !== "object") return false;
  const keys = Object.keys(t as Record<string, unknown>);
  return keys.length > 0 && keys.every((k) => k === "Cron");
}

export async function preprocess(
  raw: string,
  odin: OdinClient,
  knownFlows: KnownFlow[] = [],
): Promise<SlashResult> {
  const text = raw.trim();
  if (!text.startsWith("/")) {
    return { cleanedText: raw };
  }

  const match = text.match(/^\/([A-Za-z0-9_\-]+)(?:\s+(.*))?$/s);
  if (!match) {
    return { cleanedText: raw };
  }
  const cmd = match[1];
  const arg = (match[2] ?? "").trim();

  // Builtins first — /memory, /clear, /help.
  const cmdLower = cmd.toLowerCase();
  if (cmdLower === "memory") {
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
          `[${i + 1}] (sim ${(h.similarity * 100).toFixed(0)}%) ${h.cause}\n    → ${h.effect}`,
      )
      .join("\n");
    const prefix = `Relevant engram memories (use as context if applicable):\n${lines}`;
    return {
      cleanedText: arg,
      systemContextPrefix: prefix,
      notice: `Injected ${hits.length} memory hits`,
    };
  }

  if (cmdLower === "help") {
    const flowLines = knownFlows
      .filter((f) => !isCronOnlyFlow(f))
      .map((f) => `  /${f.name}`)
      .sort();
    const helpText = [
      "Available slash commands:",
      ...(flowLines.length > 0
        ? ["Flows (type one followed by your message):", ...flowLines, ""]
        : []),
      "Builtins:",
      "  /memory <query>      Inject Mimir recall results as context",
      "  /clear               Wipe current thread",
      "  /help                Show this help",
    ].join("\n");
    return { cleanedText: "", notice: helpText };
  }

  if (cmdLower === "clear") {
    // Handled by chatPanel via the `clearThread` webview message.
    // Reaching here means the user typed /clear into the input rather than
    // hitting the header button — tell them, so the input doesn't just
    // vanish.
    return {
      cleanedText: "",
      notice: "Use the header × button (or press Alt+Shift+Backspace) to clear the thread.",
    };
  }

  // Flow-name dispatcher — case-sensitive match against the live registry.
  const flowMatch = knownFlows.find((f) => f.name === cmd);
  if (flowMatch) {
    if (isCronOnlyFlow(flowMatch)) {
      return {
        cleanedText: raw,
        notice: `Flow '${cmd}' is cron-only and cannot be pinned from chat.`,
      };
    }
    return {
      cleanedText: arg || "Run this flow with no additional context.",
      flowOverride: cmd,
      notice: `Flow pinned: ${cmd}`,
    };
  }

  // Unknown slash command — pass through unchanged. Keeps literal-slash use
  // cases working (e.g. referencing filesystem paths starting with `/`).
  if (STATIC_COMMANDS.has(cmdLower)) {
    // Defensive: if a future builtin is added above but not here, don't
    // silently pass through. No-op today since all builtins are handled.
    return { cleanedText: raw };
  }
  return { cleanedText: raw };
}
