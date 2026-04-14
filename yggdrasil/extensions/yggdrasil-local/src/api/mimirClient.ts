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

/** SecretStorage key for the Mimir vault bearer token (Sprint 064 P3). */
const VAULT_TOKEN_KEY = "yggdrasil.mimirVaultToken";

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
  /**
   * Sprint 064 P3 — extension-wide SecretStorage handle. Set once at
   * extension activation via `MimirClient.useSecretStorage(context.secrets)`.
   * When `undefined`, vault calls go through with no Authorization header
   * (compatible with older Mimir builds that have no `MIMIR_VAULT_CLIENT_TOKEN`).
   */
  private static secretStorage: vscode.SecretStorage | undefined;

  static useSecretStorage(secrets: vscode.SecretStorage): void {
    MimirClient.secretStorage = secrets;
  }

  get mimirUrl(): string {
    return vscode.workspace
      .getConfiguration("yggdrasil")
      .get<string>("mimirUrl", "http://10.0.65.8:9090");
  }

  /**
   * Fetch the cached bearer token, prompting the user once if absent.
   * Returns `undefined` when SecretStorage is not wired (test contexts) or
   * when the user dismisses the prompt — callers then send no auth header
   * and let Mimir respond 401 if it requires one.
   */
  private async getToken(): Promise<string | undefined> {
    const storage = MimirClient.secretStorage;
    if (!storage) return undefined;

    const cached = await storage.get(VAULT_TOKEN_KEY);
    if (cached) return cached;

    const entered = await vscode.window.showInputBox({
      title: "Yggdrasil Mimir vault token",
      prompt:
        "Enter the MIMIR_VAULT_CLIENT_TOKEN (set on Munin in /etc/systemd/system/yggdrasil-mimir.service.d/vault.conf). Stored securely in VSCode SecretStorage.",
      password: true,
      ignoreFocusOut: true,
      placeHolder: "paste token",
    });

    if (!entered) return undefined;
    await storage.store(VAULT_TOKEN_KEY, entered);
    return entered;
  }

  /** Clear the cached vault token — call after a 401 to force re-prompt. */
  private async forgetToken(): Promise<void> {
    if (MimirClient.secretStorage) {
      await MimirClient.secretStorage.delete(VAULT_TOKEN_KEY);
    }
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

  /**
   * POST JSON to Mimir. Injects `Authorization: Bearer <token>` from
   * SecretStorage when available; on 401, clears the cached token and
   * re-prompts (handles token rotation), retrying the request once.
   */
  private async postJson(
    urlStr: string,
    body: unknown
  ): Promise<Record<string, unknown>> {
    const token = await this.getToken();
    try {
      return await this.postJsonRaw(urlStr, body, token);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      // 401 → token bad/missing. Clear cache, re-prompt, retry exactly once.
      if (message.startsWith("HTTP 401")) {
        await this.forgetToken();
        const fresh = await this.getToken();
        if (fresh) {
          return this.postJsonRaw(urlStr, body, fresh);
        }
      }
      throw err;
    }
  }

  private postJsonRaw(
    urlStr: string,
    body: unknown,
    token: string | undefined
  ): Promise<Record<string, unknown>> {
    const url = new URL(urlStr);
    const payload = JSON.stringify(body ?? {});

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        reject(new Error(`POST ${urlStr} timed out after ${VAULT_TIMEOUT_MS}ms`));
      }, VAULT_TIMEOUT_MS);

      const client = url.protocol === "https:" ? https : http;

      const headers: Record<string, string | number> = {
        "Content-Type": "application/json",
        "Content-Length": Buffer.byteLength(payload),
      };
      if (token) headers["Authorization"] = `Bearer ${token}`;

      const req = client.request(
        {
          method: "POST",
          hostname: url.hostname,
          port: url.port || (url.protocol === "https:" ? 443 : 80),
          path: url.pathname + url.search,
          headers,
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
