/**
 * Mimir HTTP client — vault CRUD.
 *
 * All vault operations POST to `${mimirUrl}/api/v1/vault` with
 * `{action, key, scope, value?, tags?}`.
 *
 * Response shapes (Sprint 063 P4):
 *   set    → { id, key, scope }
 *   get    → { key, value, scope, tags, updated_at }
 *   list   → { secrets: [{ key, scope, tags, updated_at }], count }
 *   delete → { deleted, scope }
 */

import * as http from "http";
import * as https from "https";
import { URL } from "url";
import * as vscode from "vscode";

const VAULT_TIMEOUT_MS = 8000;

export interface VaultSecret {
  key: string;
  scope: string;
  tags: string[];
  updated_at: string;
}

export interface VaultGetResult {
  key: string;
  value: string;
  scope: string;
  tags: string[];
  updated_at: string;
}

export interface VaultSetResult {
  id: string;
  key: string;
  scope: string;
}

export interface VaultListResult {
  secrets: VaultSecret[];
  count: number;
}

export interface VaultDeleteResult {
  deleted: string;
  scope: string;
}

export class MimirClient {
  get mimirUrl(): string {
    return vscode.workspace
      .getConfiguration("yggdrasil")
      .get<string>("mimirUrl", "http://10.0.65.8:9090");
  }

  async listVault(scope?: string): Promise<VaultListResult> {
    const body: Record<string, unknown> = { action: "list" };
    if (scope) body.scope = scope;
    const raw = await this.postJson(`${this.mimirUrl}/api/v1/vault`, body);
    return {
      secrets: Array.isArray(raw?.secrets)
        ? (raw.secrets as VaultSecret[])
        : [],
      count: typeof raw?.count === "number" ? (raw.count as number) : 0,
    };
  }

  async getVault(key: string, scope: string): Promise<VaultGetResult> {
    const raw = await this.postJson(`${this.mimirUrl}/api/v1/vault`, {
      action: "get",
      key,
      scope,
    });
    return {
      key: String(raw?.key ?? key),
      value: String(raw?.value ?? ""),
      scope: String(raw?.scope ?? scope),
      tags: Array.isArray(raw?.tags) ? (raw.tags as string[]) : [],
      updated_at: String(raw?.updated_at ?? ""),
    };
  }

  async setVault(
    key: string,
    value: string,
    scope: string,
    tags: string[]
  ): Promise<VaultSetResult> {
    const raw = await this.postJson(`${this.mimirUrl}/api/v1/vault`, {
      action: "set",
      key,
      value,
      scope,
      tags,
    });
    return {
      id: String(raw?.id ?? ""),
      key: String(raw?.key ?? key),
      scope: String(raw?.scope ?? scope),
    };
  }

  async deleteVault(key: string, scope: string): Promise<VaultDeleteResult> {
    const raw = await this.postJson(`${this.mimirUrl}/api/v1/vault`, {
      action: "delete",
      key,
      scope,
    });
    return {
      deleted: String(raw?.deleted ?? key),
      scope: String(raw?.scope ?? scope),
    };
  }

  // ─────────────────────────────────────────────────────────────
  // Internal HTTP helpers (mirrors odinClient pattern)
  // ─────────────────────────────────────────────────────────────

  private postJson(
    urlStr: string,
    body: unknown
  ): Promise<Record<string, unknown>> {
    const url = new URL(urlStr);
    const payload = JSON.stringify(body ?? {});

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        reject(new Error(`POST ${urlStr} timed out after ${VAULT_TIMEOUT_MS}ms`));
      }, VAULT_TIMEOUT_MS);

      const client = url.protocol === "https:" ? https : http;

      const req = client.request(
        {
          method: "POST",
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
          res.on("data", (chunk: Buffer) => (data += chunk.toString()));
          res.on("end", () => {
            clearTimeout(timeout);
            if (
              res.statusCode !== undefined &&
              res.statusCode >= 200 &&
              res.statusCode < 300
            ) {
              try {
                resolve(data.length > 0 ? JSON.parse(data) : {});
              } catch {
                resolve({});
              }
            } else {
              reject(
                new Error(`HTTP ${res.statusCode}: ${data.slice(0, 300)}`)
              );
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
}
