/**
 * Odin HTTP client.
 *
 * Odin exposes an OpenAI-compatible API plus Yggdrasil-specific endpoints:
 *   GET  /health                      — liveness
 *   GET  /v1/models                   — list routable models
 *   POST /v1/chat/completions         — chat (supports stream: true via SSE)
 *   GET  /api/flows                   — (future) list flows
 *   GET  /api/flows/:id               — (future) single flow
 *   PUT  /api/flows/:id               — (future) update flow
 *
 * When /api/flows is not yet implemented, listFlows()/getFlow() gracefully
 * fall back to reading deploy/config-templates/*.json from the workspace,
 * so the Settings UI is usable before the Odin PR lands.
 */

import * as fs from "fs";
import * as path from "path";
import * as http from "http";
import * as https from "https";
import { URL } from "url";
import * as vscode from "vscode";

const API_TIMEOUT_MS = 5000;
const STREAM_TIMEOUT_MS = 120_000;

export interface Model {
  id: string;
  backend?: string;
  loaded?: boolean;
  size_bytes?: number;
}

export interface ChatMessage {
  role: "system" | "user" | "assistant" | "tool";
  content: string;
}

export interface ChatRequest {
  /**
   * Sprint 068 Phase 3 (Fergus): `model` is now OPTIONAL. When omitted, Odin
   * runs its intent-based router and picks a backend automatically. Sending
   * an explicit model is supported but not used by the Fergus chat path —
   * it's preserved for tooling callers that want backend pinning.
   */
  model?: string;
  messages: ChatMessage[];
  temperature?: number;
  max_tokens?: number;
  stream?: boolean;
  /**
   * Sprint 063 P1: explicit flow pin. When set, Odin bypasses intent
   * classification and dispatches directly to the named flow. The flow's
   * trigger must be `Manual` or `Intent(_)`; cron-only flows return 400.
   */
  flow?: string;
}

/**
 * Sprint 061 swarm-flow SSE event payload. Emitted on `event: ygg_step`
 * frames when Odin is running a multi-step flow. Default-event data frames
 * (no `event:` line) are standard OpenAI chunks and are NOT surfaced here.
 */
export type SwarmEvent =
  | { phase: "step_start"; step: string; label: string; role: string }
  | { phase: "step_delta"; step: string; role: string; content: string }
  | { phase: "step_end"; step: string }
  | { phase: "done" }
  | { phase: "error"; step?: string; message: string };

export interface FlowStep {
  name: string;
  backend?: string;
  model?: string;
  system_prompt?: string;
  input?: unknown;
  output_key?: string;
  temperature?: number;
  max_tokens?: number;
  think?: boolean;
  tools?: string[];
  agent_config?: Record<string, unknown>;
}

export interface Flow {
  name: string;
  trigger?: unknown;
  timeout_secs?: number;
  max_step_output_chars?: number;
  loop_config?: Record<string, unknown>;
  steps: FlowStep[];
}

export interface FlowFile {
  _comment?: string;
  _design?: string;
  _models?: Record<string, string>;
  flows: Flow[];
}

export interface MemoryHit {
  cause: string;
  effect: string;
  similarity: number;
}

export class OdinClient {
  get odinUrl(): string {
    return vscode.workspace
      .getConfiguration("yggdrasil")
      .get<string>("odinUrl", "http://10.0.65.8:8080");
  }

  get mimirUrl(): string {
    return vscode.workspace
      .getConfiguration("yggdrasil")
      .get<string>("mimirUrl", "http://10.0.65.8:9090");
  }

  async health(): Promise<boolean> {
    const res = await this.get(`${this.odinUrl}/health`, API_TIMEOUT_MS);
    return res !== null;
  }

  async listModels(): Promise<Model[]> {
    const data = await this.getJson(`${this.odinUrl}/v1/models`);
    if (!data || !Array.isArray(data.data)) {
      return [];
    }
    return (data.data as Record<string, unknown>[]).map((m) => ({
      id: String(m.id ?? ""),
      backend: typeof m.owned_by === "string" ? (m.owned_by as string) : undefined,
      loaded: m.loaded === true,
      size_bytes: typeof m.size === "number" ? (m.size as number) : undefined,
    }));
  }

  async listFlows(): Promise<Flow[]> {
    // Try Odin first
    const remote = await this.getJson(`${this.odinUrl}/api/flows`);
    if (remote && Array.isArray(remote.flows)) {
      return remote.flows as Flow[];
    }
    // Fallback: read local flow templates from the workspace
    return this.readLocalFlows();
  }

  async getFlow(id: string): Promise<Flow | null> {
    const remote = await this.getJson(`${this.odinUrl}/api/flows/${encodeURIComponent(id)}`);
    if (remote && typeof remote.name === "string" && Array.isArray(remote.steps)) {
      return remote as unknown as Flow;
    }
    const all = await this.readLocalFlows();
    return all.find((f) => f.name === id) ?? null;
  }

  async updateFlow(id: string, flow: Flow): Promise<{ ok: boolean; error?: string }> {
    try {
      await this.putJson(
        `${this.odinUrl}/api/flows/${encodeURIComponent(id)}`,
        flow,
        API_TIMEOUT_MS
      );
      return { ok: true };
    } catch (err) {
      return { ok: false, error: err instanceof Error ? err.message : String(err) };
    }
  }

  async queryMemory(text: string, limit = 5): Promise<MemoryHit[]> {
    const body = { text, limit };
    const data = await this.postJson(`${this.mimirUrl}/api/memory/query`, body, API_TIMEOUT_MS);
    if (!data || !Array.isArray(data.results)) {
      return [];
    }
    return (data.results as Record<string, unknown>[]).map((h) => ({
      cause: String(h.cause ?? ""),
      effect: String(h.effect ?? ""),
      similarity: typeof h.similarity === "number" ? (h.similarity as number) : 0,
    }));
  }

  /**
   * Stream a chat completion. The onToken callback fires for each assistant
   * content delta. When Odin is running a multi-step swarm flow (Sprint 061),
   * intermediate "thinking" steps arrive as `event: ygg_step` SSE frames —
   * surfaced via the optional onSwarmEvent callback. Default-event data frames
   * remain standard OpenAI chunks (assistant content).
   *
   * Returns the aggregated assistant text when the stream completes.
   */
  async streamChat(
    req: ChatRequest,
    onToken: (delta: string) => void,
    onMeta?: (meta: { model: string; finish_reason?: string }) => void,
    onSwarmEvent?: (ev: SwarmEvent) => void
  ): Promise<string> {
    const url = new URL(`${this.odinUrl}/v1/chat/completions`);
    // Sprint 068 Phase 3: omit `model` entirely when undefined so Odin's
    // intent router picks the backend. Sending `model: ""` or `null` would
    // trip the upstream proxy's validation; omitting the key is what the
    // Fergus chat path intends.
    const { model, ...rest } = req;
    const payload = model ? { ...rest, model, stream: true } : { ...rest, stream: true };
    const body = JSON.stringify(payload);

    return new Promise((resolve, reject) => {
      const client = url.protocol === "https:" ? https : http;
      const request = client.request(
        {
          method: "POST",
          hostname: url.hostname,
          port: url.port || (url.protocol === "https:" ? 443 : 80),
          path: url.pathname + url.search,
          headers: {
            "Content-Type": "application/json",
            "Content-Length": Buffer.byteLength(body),
            Accept: "text/event-stream",
          },
          timeout: STREAM_TIMEOUT_MS,
        },
        (res) => {
          if (res.statusCode && res.statusCode >= 400) {
            let errBody = "";
            res.on("data", (chunk: Buffer) => {
              errBody += chunk.toString();
            });
            res.on("end", () => {
              reject(new Error(`HTTP ${res.statusCode}: ${errBody.slice(0, 500)}`));
            });
            return;
          }

          let buffer = "";
          let full = "";
          res.setEncoding("utf-8");

          res.on("data", (chunk: string) => {
            buffer += chunk;
            // SSE frames are separated by \n\n; each frame has optional "event: <name>"
            // and one or more "data: <payload>" lines.
            let idx;
            while ((idx = buffer.indexOf("\n\n")) !== -1) {
              const frame = buffer.slice(0, idx);
              buffer = buffer.slice(idx + 2);

              let eventName = ""; // default (unnamed) = OpenAI-standard data frame
              const dataLines: string[] = [];
              for (const line of frame.split("\n")) {
                if (line.startsWith("event:")) {
                  eventName = line.slice(6).trim();
                } else if (line.startsWith("data:")) {
                  dataLines.push(line.slice(5).trim());
                }
              }
              const payload = dataLines.join("\n");
              if (!payload) continue;
              if (payload === "[DONE]") {
                continue;
              }

              try {
                const obj = JSON.parse(payload);
                if (eventName === "ygg_step") {
                  // Sprint 061: swarm-flow metadata; route to onSwarmEvent
                  // if the caller opted in. Otherwise silently ignore.
                  if (onSwarmEvent && typeof obj.phase === "string") {
                    onSwarmEvent(obj as SwarmEvent);
                  }
                  continue;
                }
                // Default event = standard OpenAI chunk
                const choice = obj.choices?.[0];
                const delta = choice?.delta?.content;
                if (typeof delta === "string" && delta.length > 0) {
                  full += delta;
                  onToken(delta);
                }
                if (choice?.finish_reason && onMeta) {
                  onMeta({
                    model: String(obj.model ?? req.model),
                    finish_reason: String(choice.finish_reason),
                  });
                }
              } catch {
                // ignore unparseable frame
              }
            }
          });

          res.on("end", () => resolve(full));
          res.on("error", (err) => reject(err));
        }
      );

      request.on("timeout", () => {
        request.destroy(new Error("stream timeout"));
      });
      request.on("error", (err) => reject(err));
      request.write(body);
      request.end();
    });
  }

  // ─────────────────────────────────────────────────────────────
  // Internal HTTP helpers
  // ─────────────────────────────────────────────────────────────

  private get(url: string, timeoutMs: number): Promise<string | null> {
    return new Promise((resolve) => {
      const timeout = setTimeout(() => resolve(null), timeoutMs);
      const client = url.startsWith("https") ? https : http;
      try {
        const req = client.get(url, (res) => {
          let data = "";
          res.on("data", (c: Buffer) => (data += c.toString()));
          res.on("end", () => {
            clearTimeout(timeout);
            if (res.statusCode && res.statusCode >= 200 && res.statusCode < 300) {
              resolve(data);
            } else {
              resolve(null);
            }
          });
        });
        req.on("error", () => {
          clearTimeout(timeout);
          resolve(null);
        });
      } catch {
        clearTimeout(timeout);
        resolve(null);
      }
    });
  }

  private async getJson(url: string): Promise<Record<string, unknown> | null> {
    const text = await this.get(url, API_TIMEOUT_MS);
    if (!text) return null;
    try {
      return JSON.parse(text);
    } catch {
      return null;
    }
  }

  private postJson(
    url: string,
    body: unknown,
    timeoutMs: number
  ): Promise<Record<string, unknown> | null> {
    return this.sendJson("POST", url, body, timeoutMs);
  }

  private putJson(
    url: string,
    body: unknown,
    timeoutMs: number
  ): Promise<Record<string, unknown> | null> {
    return this.sendJson("PUT", url, body, timeoutMs);
  }

  private sendJson(
    method: string,
    urlStr: string,
    body: unknown,
    timeoutMs: number
  ): Promise<Record<string, unknown> | null> {
    const url = new URL(urlStr);
    const payload = JSON.stringify(body ?? {});

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        reject(new Error(`${method} ${urlStr} timed out`));
      }, timeoutMs);
      const client = url.protocol === "https:" ? https : http;

      const req = client.request(
        {
          method,
          hostname: url.hostname,
          port: url.port || (url.protocol === "https:" ? 443 : 80),
          path: url.pathname + url.search,
          headers: {
            "Content-Type": "application/json",
            "Content-Length": Buffer.byteLength(payload),
          },
        },
        (res) => {
          let data = "";
          res.on("data", (c: Buffer) => (data += c.toString()));
          res.on("end", () => {
            clearTimeout(timeout);
            if (res.statusCode && res.statusCode >= 200 && res.statusCode < 300) {
              try {
                resolve(data.length > 0 ? JSON.parse(data) : {});
              } catch {
                resolve({});
              }
            } else {
              reject(new Error(`HTTP ${res.statusCode}: ${data.slice(0, 300)}`));
            }
          });
        }
      );

      req.on("error", (err) => {
        clearTimeout(timeout);
        reject(err);
      });
      req.write(payload);
      req.end();
    });
  }

  /**
   * Read flow templates from the first workspace folder's
   * yggdrasil/deploy/config-templates/ directory as a fallback when
   * Odin's /api/flows endpoint is not available.
   */
  private async readLocalFlows(): Promise<Flow[]> {
    const folders = vscode.workspace.workspaceFolders;
    if (!folders || folders.length === 0) return [];

    const candidates = folders
      .map((f) => path.join(f.uri.fsPath, "yggdrasil", "deploy", "config-templates"))
      .concat(folders.map((f) => path.join(f.uri.fsPath, "deploy", "config-templates")));

    const templateDir = candidates.find((p) => safeExists(p));
    if (!templateDir) return [];

    const flows: Flow[] = [];
    try {
      const files = fs.readdirSync(templateDir).filter((f) => f.endsWith("-flow.json"));
      for (const file of files) {
        try {
          const content = fs.readFileSync(path.join(templateDir, file), "utf-8");
          const parsed = JSON.parse(content) as FlowFile;
          if (Array.isArray(parsed.flows)) {
            flows.push(...parsed.flows);
          }
        } catch {
          // skip malformed file
        }
      }
    } catch {
      // directory read failure
    }
    return flows;
  }
}

function safeExists(p: string): boolean {
  try {
    return fs.existsSync(p);
  } catch {
    return false;
  }
}
