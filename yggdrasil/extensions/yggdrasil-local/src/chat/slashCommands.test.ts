/**
 * Unit tests for slashCommands.ts — Sprint 068 Phase 3.
 *
 * Coverage of the Fergus slash contract:
 *   - Builtins: /memory, /help, /clear
 *   - Flow-name dispatch against a live `knownFlows` list
 *   - Cron-only flows rejected at the client boundary (guards 400 path)
 *   - Unknown slashes pass through verbatim
 *   - `/model` and `/flow` are gone (no dedicated cases exist)
 */

import { describe, it, expect, vi } from "vitest";
import { preprocess, isCronOnlyFlow, type KnownFlow } from "./slashCommands";
import type { OdinClient } from "../api/odinClient";

// ── OdinClient mock ──────────────────────────────────────────
function makeOdin(memoryHits: Array<{ cause: string; effect: string; similarity: number }> = []) {
  return {
    queryMemory: vi.fn().mockResolvedValue(memoryHits),
  } as unknown as OdinClient;
}

// ── Fixtures ──
const FLOW_CODING: KnownFlow = { name: "coding_swarm", trigger: { Manual: {} } };
const FLOW_RESEARCH: KnownFlow = { name: "research", trigger: { Intent: "research" } };
const FLOW_NIGHTLY: KnownFlow = { name: "nightly_dream", trigger: { Cron: "0 2 * * *" } };
const KNOWN = [FLOW_CODING, FLOW_RESEARCH, FLOW_NIGHTLY];

describe("isCronOnlyFlow", () => {
  it("returns true for cron-only triggers", () => {
    expect(isCronOnlyFlow(FLOW_NIGHTLY)).toBe(true);
  });

  it("returns false for Manual and Intent triggers", () => {
    expect(isCronOnlyFlow(FLOW_CODING)).toBe(false);
    expect(isCronOnlyFlow(FLOW_RESEARCH)).toBe(false);
  });

  it("returns false when trigger is absent", () => {
    expect(isCronOnlyFlow({ name: "loose" })).toBe(false);
  });
});

describe("preprocess — non-slash passthrough", () => {
  it("returns raw text unchanged when input has no leading slash", async () => {
    const result = await preprocess("hello world", makeOdin(), KNOWN);
    expect(result.cleanedText).toBe("hello world");
    expect(result.flowOverride).toBeUndefined();
  });

  it("empty string passes through", async () => {
    const result = await preprocess("", makeOdin(), KNOWN);
    expect(result.cleanedText).toBe("");
  });

  it("whitespace-only string passes through", async () => {
    const result = await preprocess("   ", makeOdin(), KNOWN);
    expect(result.cleanedText).toBe("   ");
  });
});

describe("flow-name dispatcher", () => {
  it("pins a known Manual flow and strips the name from cleanedText", async () => {
    const result = await preprocess("/coding_swarm write a cache module", makeOdin(), KNOWN);
    expect(result.flowOverride).toBe("coding_swarm");
    expect(result.cleanedText).toBe("write a cache module");
    expect(result.notice).toContain("coding_swarm");
  });

  it("pins a known Intent flow", async () => {
    const result = await preprocess("/research DOM diffing algorithms", makeOdin(), KNOWN);
    expect(result.flowOverride).toBe("research");
    expect(result.cleanedText).toBe("DOM diffing algorithms");
  });

  it("flow name only (no message) produces a fallback cleanedText", async () => {
    const result = await preprocess("/coding_swarm", makeOdin(), KNOWN);
    expect(result.flowOverride).toBe("coding_swarm");
    expect(result.cleanedText).toBeTruthy();
  });

  it("rejects cron-only flows with a notice (guards the 400 unknown-flow path)", async () => {
    const result = await preprocess("/nightly_dream trigger me", makeOdin(), KNOWN);
    expect(result.flowOverride).toBeUndefined();
    expect(result.notice).toMatch(/cron-only/i);
    // Passes raw through so the user sees what they typed.
    expect(result.cleanedText).toBe("/nightly_dream trigger me");
  });

  it("unknown slash passes through verbatim", async () => {
    const result = await preprocess("/wibble xyz", makeOdin(), KNOWN);
    expect(result.flowOverride).toBeUndefined();
    expect(result.cleanedText).toBe("/wibble xyz");
  });

  it("empty knownFlows falls back to builtins-only + passthrough", async () => {
    const result = await preprocess("/coding_swarm hello", makeOdin(), []);
    expect(result.flowOverride).toBeUndefined();
    expect(result.cleanedText).toBe("/coding_swarm hello");
  });
});

describe("/memory command", () => {
  it("injects memory hits as systemContextPrefix", async () => {
    const odin = makeOdin([
      { cause: "sprint 063 vault UI", effect: "built mimirClient", similarity: 0.9 },
    ]);
    const result = await preprocess("/memory vault UI sprint 063", odin, KNOWN);
    expect(odin.queryMemory).toHaveBeenCalledWith("vault UI sprint 063", 5);
    expect(result.systemContextPrefix).toContain("sprint 063 vault UI");
    expect(result.notice).toContain("1 memory hit");
    expect(result.cleanedText).toBe("vault UI sprint 063");
  });

  it("returns notice when no hits found", async () => {
    const odin = makeOdin([]);
    const result = await preprocess("/memory nonexistent topic", odin, KNOWN);
    expect(result.notice).toMatch(/no matching/i);
    expect(result.systemContextPrefix).toBeUndefined();
  });

  it("handles queryMemory rejection gracefully", async () => {
    const odin = {
      queryMemory: vi.fn().mockRejectedValue(new Error("ECONNREFUSED")),
    } as unknown as OdinClient;
    const result = await preprocess("/memory test query", odin, KNOWN);
    expect(result.notice).toMatch(/failed/i);
    expect(result.cleanedText).toBe("test query");
  });

  it("empty arg returns usage notice", async () => {
    const result = await preprocess("/memory", makeOdin(), KNOWN);
    expect(result.notice).toMatch(/usage/i);
  });
});

describe("/help command", () => {
  it("lists user-invocable flows and all three builtins", async () => {
    const result = await preprocess("/help", makeOdin(), KNOWN);
    expect(result.notice).toContain("/coding_swarm");
    expect(result.notice).toContain("/research");
    expect(result.notice).toContain("/memory");
    expect(result.notice).toContain("/clear");
    expect(result.notice).toContain("/help");
    expect(result.cleanedText).toBe("");
  });

  it("omits cron-only flows from /help output", async () => {
    const result = await preprocess("/help", makeOdin(), KNOWN);
    expect(result.notice).not.toContain("/nightly_dream");
  });

  it("still works with no flows known", async () => {
    const result = await preprocess("/help", makeOdin(), []);
    expect(result.notice).toContain("/memory");
    expect(result.notice).toContain("/help");
  });
});

describe("/clear command", () => {
  it("returns a directive notice pointing at the header button", async () => {
    const result = await preprocess("/clear", makeOdin(), KNOWN);
    expect(result.notice).toMatch(/header/i);
    expect(result.cleanedText).toBe("");
  });
});

describe("whitespace edge cases", () => {
  it("leading whitespace is trimmed and the slash is still parsed", async () => {
    const result = await preprocess("  /coding_swarm msg", makeOdin(), KNOWN);
    expect(result.flowOverride).toBe("coding_swarm");
  });

  it("extra spaces between name and arg are collapsed via arg trim", async () => {
    const result = await preprocess("/coding_swarm    some message here", makeOdin(), KNOWN);
    expect(result.flowOverride).toBe("coding_swarm");
    expect(result.cleanedText).toBe("some message here");
  });
});
