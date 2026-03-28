#!/usr/bin/env node
/**
 * Yggdrasil Local MCP Server — stdio transport.
 *
 * Replaces the Rust ygg-mcp-server binary. Spawned by Claude Code via .mcp.json.
 * Serves: sync_docs_tool, screenshot_tool.
 * Writes events to /tmp/ygg-hooks/memory-events.jsonl for the VS Code extension.
 */

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import * as fs from "fs";
import * as path from "path";
import { handleSyncDocs } from "./syncDocs";
import { handleScreenshot } from "./screenshot";

// Read version from package.json — single source of truth.
const PKG_VERSION = (() => {
  try {
    const pkg = JSON.parse(
      fs.readFileSync(path.join(__dirname, "../../package.json"), "utf-8")
    );
    return pkg.version ?? "0.0.0";
  } catch {
    return "0.0.0";
  }
})();

// ── Config ─────────────────────────────────────────────────────────

interface Config {
  odin_url: string;
  muninn_url?: string;
  timeout_secs: number;
  generate_tok_per_sec?: number;
  prefetch_query?: string;
  project?: string;
  workspace_path?: string;
  remote_url?: string;
}

function loadConfig(): Config {
  const configArg = process.argv.indexOf("--config");
  const configPath =
    configArg >= 0 && process.argv[configArg + 1]
      ? process.argv[configArg + 1]
      : path.join(
          process.env.HOME || "~",
          ".config/yggdrasil/local-mcp.yaml"
        );

  try {
    const raw = fs.readFileSync(configPath, "utf-8");
    return JSON.parse(raw) as Config;
  } catch (e) {
    console.error(`[ygg-mcp] Failed to load config from ${configPath}: ${e}`);
    return {
      odin_url: process.env.ODIN_URL ?? "http://localhost:8080",
      timeout_secs: 300,
    };
  }
}

// ── Event emitter ──────────────────────────────────────────────────

const EVENTS_FILE = "/tmp/ygg-hooks/memory-events.jsonl";

export function emitEvent(
  event: string,
  data: Record<string, unknown>
): void {
  try {
    fs.mkdirSync("/tmp/ygg-hooks", { recursive: true });
    const line = JSON.stringify({
      ts: new Date().toISOString(),
      event,
      data,
    });
    fs.appendFileSync(EVENTS_FILE, line + "\n");
  } catch {
    // Never fail — fire and forget
  }
}

// ── Main ───────────────────────────────────────────────────────────

async function main() {
  const config = loadConfig();
  const sessionId = crypto.randomUUID();

  const server = new McpServer({
    name: "yggdrasil-local",
    version: PKG_VERSION,
  });

  // ── sync_docs_tool ────────────────────────────────────────────
  server.tool(
    "sync_docs_tool",
    `Sprint lifecycle doc agent. Supports three events:
event='setup': Initialize a new workspace — creates /docs/ and /sprints/, scaffolds required docs.
event='sprint_start': Updates USAGE.md via LLM, checks /docs/ + /sprints/ invariants.
event='sprint_end': Archives sprint to Mimir, updates ARCHITECTURE.md, deletes sprint file.
workspace_path: Pass the current project root to override the config default.`,
    {
      event: z
        .string()
        .describe('Lifecycle event: "setup", "sprint_start", or "sprint_end"'),
      sprint_id: z
        .string()
        .optional()
        .describe("Sprint identifier, e.g. '049'"),
      sprint_content: z
        .string()
        .optional()
        .describe("Full sprint document content"),
      workspace_path: z
        .string()
        .optional()
        .describe("Workspace root path override"),
    },
    async (params) => {
      const start = Date.now();
      try {
        const result = await handleSyncDocs(config, params, sessionId);
        emitEvent("tool", {
          name: "sync_docs",
          status: "ok",
          duration_ms: Date.now() - start,
        });
        return { content: [{ type: "text" as const, text: result }] };
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        emitEvent("tool", {
          name: "sync_docs",
          status: "error",
          error: msg,
          duration_ms: Date.now() - start,
        });
        return {
          content: [{ type: "text" as const, text: `Error: ${msg}` }],
          isError: true,
        };
      }
    }
  );

  // ── screenshot_tool ───────────────────────────────────────────
  server.tool(
    "screenshot_tool",
    `Capture a screenshot of a web page via headless Chromium.
Returns the file path to the saved PNG image. Use the Read tool to view it.
Screenshots are saved to /tmp/ygg-screenshots/.`,
    {
      url: z.string().describe("URL to capture"),
      selector: z
        .string()
        .optional()
        .describe("CSS selector to wait for before capture"),
      full_page: z
        .boolean()
        .optional()
        .describe("Capture full scrollable page (default: false)"),
      viewport_width: z
        .number()
        .optional()
        .describe("Viewport width in pixels (default: 1280)"),
      viewport_height: z
        .number()
        .optional()
        .describe("Viewport height in pixels (default: 720)"),
    },
    async (params) => {
      const start = Date.now();
      try {
        const result = await handleScreenshot(params);
        emitEvent("tool", {
          name: "screenshot",
          status: "ok",
          path: result,
          duration_ms: Date.now() - start,
        });
        return { content: [{ type: "text" as const, text: result }] };
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        emitEvent("tool", {
          name: "screenshot",
          status: "error",
          error: msg,
          duration_ms: Date.now() - start,
        });
        return {
          content: [{ type: "text" as const, text: `Error: ${msg}` }],
          isError: true,
        };
      }
    }
  );

  // ── Connect stdio transport ───────────────────────────────────
  const transport = new StdioServerTransport();
  await server.connect(transport);
}

main().catch((e) => {
  console.error(`[ygg-mcp] Fatal: ${e}`);
  process.exit(1);
});
