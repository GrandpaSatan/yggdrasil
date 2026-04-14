/**
 * Unit tests for odinClient.ts — Sprint 063 P7b.
 *
 * Coverage:
 *   streamChat SSE parsing:
 *     - Standard OpenAI chunks (default event)
 *     - Swarm ygg_step events (Sprint 061): step_start, step_delta, step_end, done, error
 *     - Error HTTP response
 *     - Timeout
 *   listModels: normal + empty response
 *   queryMemory: normal + empty + malformed
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import * as http from "http";
import * as net from "net";
import { AddressInfo } from "net";
import { OdinClient } from "./odinClient";
import type { SwarmEvent } from "./odinClient";

// ─────────────────────────────────────────────────────────────
// Helpers: tiny in-process HTTP server for SSE testing
// ─────────────────────────────────────────────────────────────

function buildSseResponse(frames: string[]): string {
  return frames.join("") + "\n";
}

function openChunk(content: string, model = "test-model", finishReason?: string): string {
  const choice: Record<string, unknown> = {
    delta: { role: "assistant", content },
    index: 0,
    finish_reason: finishReason ?? null,
  };
  return `data: ${JSON.stringify({ id: "c1", object: "chat.completion.chunk", model, choices: [choice] })}\n\n`;
}

function yggStepFrame(payload: object): string {
  return `event: ygg_step\ndata: ${JSON.stringify(payload)}\n\n`;
}

function doneFrame(): string {
  return "data: [DONE]\n\n";
}

/** Spin up a one-shot HTTP server that sends the given raw SSE body then closes. */
function createSseServer(
  statusCode: number,
  body: string
): Promise<{ url: string; close: () => void }> {
  return new Promise((resolve) => {
    const server = http.createServer((_req, res) => {
      res.writeHead(statusCode, { "Content-Type": "text/event-stream" });
      res.end(body);
    });
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address() as AddressInfo;
      resolve({
        url: `http://127.0.0.1:${port}`,
        close: () => server.close(),
      });
    });
  });
}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

describe("OdinClient.streamChat — standard OpenAI SSE", () => {
  it("collects content deltas and returns aggregated text", async () => {
    const body =
      openChunk("Hello") +
      openChunk(", ") +
      openChunk("world") +
      openChunk("", "test-model", "stop") +
      doneFrame();

    const { url, close } = await createSseServer(200, body);

    try {
      const odin = new OdinClient();
      // Override mimirUrl/odinUrl getters via prototype
      vi.spyOn(odin, "odinUrl", "get").mockReturnValue(url);

      const tokens: string[] = [];
      const result = await odin.streamChat(
        { model: "test-model", messages: [{ role: "user", content: "hi" }] },
        (delta) => tokens.push(delta)
      );

      expect(tokens).toEqual(["Hello", ", ", "world"]);
      expect(result).toBe("Hello, world");
    } finally {
      close();
    }
  });

  it("fires onMeta with model and finish_reason", async () => {
    const body =
      openChunk("OK", "my-model", "stop") +
      doneFrame();

    const { url, close } = await createSseServer(200, body);

    try {
      const odin = new OdinClient();
      vi.spyOn(odin, "odinUrl", "get").mockReturnValue(url);

      const metas: Array<{ model: string; finish_reason?: string }> = [];
      await odin.streamChat(
        { model: "test-model", messages: [{ role: "user", content: "go" }] },
        () => {},
        (meta) => metas.push(meta)
      );

      expect(metas.length).toBeGreaterThan(0);
      expect(metas[0].finish_reason).toBe("stop");
    } finally {
      close();
    }
  });
});

describe("OdinClient.streamChat — Sprint 061 swarm ygg_step events", () => {
  it("routes ygg_step frames to onSwarmEvent callback", async () => {
    const body =
      yggStepFrame({ phase: "step_start", step: "s1", label: "Plan", role: "planner" }) +
      yggStepFrame({ phase: "step_delta", step: "s1", role: "planner", content: "thinking..." }) +
      yggStepFrame({ phase: "step_end", step: "s1" }) +
      openChunk("Final answer") +
      yggStepFrame({ phase: "done" }) +
      doneFrame();

    const { url, close } = await createSseServer(200, body);

    try {
      const odin = new OdinClient();
      vi.spyOn(odin, "odinUrl", "get").mockReturnValue(url);

      const swarmEvents: SwarmEvent[] = [];
      const result = await odin.streamChat(
        { model: "test-model", messages: [{ role: "user", content: "run flow" }] },
        () => {},
        undefined,
        (ev) => swarmEvents.push(ev)
      );

      expect(swarmEvents[0]).toMatchObject({ phase: "step_start", step: "s1", label: "Plan" });
      expect(swarmEvents[1]).toMatchObject({ phase: "step_delta", content: "thinking..." });
      expect(swarmEvents[2]).toMatchObject({ phase: "step_end", step: "s1" });
      expect(swarmEvents[3]).toMatchObject({ phase: "done" });
      expect(result).toBe("Final answer");
    } finally {
      close();
    }
  });

  it("ygg_step error event is surfaced correctly", async () => {
    const body =
      yggStepFrame({ phase: "error", step: "s1", message: "model overloaded" }) +
      doneFrame();

    const { url, close } = await createSseServer(200, body);

    try {
      const odin = new OdinClient();
      vi.spyOn(odin, "odinUrl", "get").mockReturnValue(url);

      const swarmEvents: SwarmEvent[] = [];
      await odin.streamChat(
        { model: "test-model", messages: [{ role: "user", content: "run" }] },
        () => {},
        undefined,
        (ev) => swarmEvents.push(ev)
      );

      expect(swarmEvents[0]).toMatchObject({ phase: "error", message: "model overloaded" });
    } finally {
      close();
    }
  });

  it("does NOT surface ygg_step events to onToken callback", async () => {
    const body =
      yggStepFrame({ phase: "step_delta", step: "s1", role: "r", content: "hidden" }) +
      openChunk("visible") +
      doneFrame();

    const { url, close } = await createSseServer(200, body);

    try {
      const odin = new OdinClient();
      vi.spyOn(odin, "odinUrl", "get").mockReturnValue(url);

      const tokens: string[] = [];
      await odin.streamChat(
        { model: "test-model", messages: [{ role: "user", content: "go" }] },
        (d) => tokens.push(d)
      );

      // "hidden" must NOT appear — it was in a ygg_step frame
      expect(tokens).not.toContain("hidden");
      expect(tokens).toContain("visible");
    } finally {
      close();
    }
  });
});

describe("OdinClient.streamChat — error responses", () => {
  it("rejects on HTTP 4xx", async () => {
    const { url, close } = await createSseServer(400, '{"error":"bad request"}');

    try {
      const odin = new OdinClient();
      vi.spyOn(odin, "odinUrl", "get").mockReturnValue(url);

      await expect(
        odin.streamChat(
          { model: "test-model", messages: [{ role: "user", content: "hi" }] },
          () => {}
        )
      ).rejects.toThrow(/HTTP 400/);
    } finally {
      close();
    }
  });

  it("rejects on HTTP 5xx", async () => {
    const { url, close } = await createSseServer(503, "service unavailable");

    try {
      const odin = new OdinClient();
      vi.spyOn(odin, "odinUrl", "get").mockReturnValue(url);

      await expect(
        odin.streamChat(
          { model: "test-model", messages: [{ role: "user", content: "hi" }] },
          () => {}
        )
      ).rejects.toThrow(/HTTP 503/);
    } finally {
      close();
    }
  });
});

describe("OdinClient.listModels", () => {
  it("parses model list from /v1/models", async () => {
    const payload = {
      data: [
        { id: "qwen3:30b-a3b", owned_by: "hugin-ollama", loaded: true },
        { id: "lfm-1.2b:latest", owned_by: "munin-ollama", loaded: false },
      ],
    };

    const { url, close } = await createSseServer(200, JSON.stringify(payload));

    try {
      const odin = new OdinClient();
      vi.spyOn(odin, "odinUrl", "get").mockReturnValue(url);

      const models = await odin.listModels();
      expect(models).toHaveLength(2);
      expect(models[0]).toMatchObject({ id: "qwen3:30b-a3b", backend: "hugin-ollama", loaded: true });
      expect(models[1]).toMatchObject({ id: "lfm-1.2b:latest", loaded: false });
    } finally {
      close();
    }
  });

  it("returns empty array when server unreachable", async () => {
    const odin = new OdinClient();
    vi.spyOn(odin, "odinUrl", "get").mockReturnValue("http://127.0.0.1:1"); // port 1 = always refused

    const models = await odin.listModels();
    expect(models).toEqual([]);
  });
});

describe("OdinClient.queryMemory", () => {
  it("returns hits from /api/memory/query", async () => {
    const payload = {
      results: [
        { cause: "sprint 063", effect: "vault ui built", similarity: 0.92 },
      ],
    };

    const { url, close } = await createSseServer(200, JSON.stringify(payload));

    try {
      const odin = new OdinClient();
      vi.spyOn(odin, "mimirUrl", "get").mockReturnValue(url);

      const hits = await odin.queryMemory("sprint 063 vault");
      expect(hits).toHaveLength(1);
      expect(hits[0].cause).toBe("sprint 063");
      expect(hits[0].similarity).toBeCloseTo(0.92);
    } finally {
      close();
    }
  });

  it("returns empty array when server returns no results", async () => {
    const { url, close } = await createSseServer(200, JSON.stringify({ results: [] }));

    try {
      const odin = new OdinClient();
      vi.spyOn(odin, "mimirUrl", "get").mockReturnValue(url);

      const hits = await odin.queryMemory("nothing");
      expect(hits).toEqual([]);
    } finally {
      close();
    }
  });

  it("returns empty array on malformed JSON response", async () => {
    const { url, close } = await createSseServer(200, "not json");

    try {
      const odin = new OdinClient();
      vi.spyOn(odin, "mimirUrl", "get").mockReturnValue(url);

      const hits = await odin.queryMemory("test");
      expect(hits).toEqual([]);
    } finally {
      close();
    }
  });
});
