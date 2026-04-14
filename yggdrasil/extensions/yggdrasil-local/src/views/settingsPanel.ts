/**
 * Settings Panel — WebviewPanel that consolidates every Yggdrasil config knob.
 *
 * Four tabs:
 *   1. Endpoints     — Odin/Mimir/Hugin/Gitea URLs + test buttons
 *   2. Flows         — pick a flow, edit each step's model/prompt/params
 *   3. Notifications — event filter, sound, hook management
 *   4. Secrets       — VS Code SecretStorage (OS keychain-backed)
 *
 * Flow edits are saved via OdinClient.updateFlow(). When Odin's /api/flows
 * endpoint is not yet deployed, the client gracefully reads local
 * deploy/config-templates/*.json as a fallback so the UI still works.
 */

import * as vscode from "vscode";
import { OdinClient, Flow } from "../api/odinClient";
import { MimirClient } from "../api/mimirClient";
import type { HookManager } from "../hookManager";

/** Clipboard auto-clear timers keyed by `"${scope}:${key}"`. */
const clipboardTimers = new Map<string, ReturnType<typeof setTimeout>>();

export class SettingsPanel {
  private static panel: vscode.WebviewPanel | undefined;
  private static readonly viewType = "yggdrasil.settingsPanel";
  private static mimir = new MimirClient();

  static createOrShow(
    context: vscode.ExtensionContext,
    odin: OdinClient,
    hooks: HookManager
  ): void {
    const column = vscode.window.activeTextEditor?.viewColumn ?? vscode.ViewColumn.One;
    if (SettingsPanel.panel) {
      SettingsPanel.panel.reveal(column);
      return;
    }

    const mediaRoot = vscode.Uri.joinPath(context.extensionUri, "media");
    const panel = vscode.window.createWebviewPanel(
      SettingsPanel.viewType,
      "Yggdrasil Settings",
      column,
      { enableScripts: true, retainContextWhenHidden: true, localResourceRoots: [mediaRoot] }
    );
    SettingsPanel.panel = panel;
    panel.webview.html = SettingsPanel.getHtml(panel.webview, context.extensionUri);

    panel.onDidDispose(() => {
      SettingsPanel.panel = undefined;
    });

    panel.webview.onDidReceiveMessage(async (msg) => {
      try {
        await SettingsPanel.handleMessage(panel, context, odin, hooks, msg);
      } catch (err) {
        const m = err instanceof Error ? err.message : String(err);
        panel.webview.postMessage({ type: "toast", message: `Error: ${m}`, kind: "fail" });
      }
    });
  }

  private static async handleMessage(
    panel: vscode.WebviewPanel,
    context: vscode.ExtensionContext,
    odin: OdinClient,
    hooks: HookManager,
    msg: { type: string } & Record<string, unknown>
  ): Promise<void> {
    switch (msg.type) {
      case "ready":
        await SettingsPanel.pushState(panel, context, odin);
        return;

      case "testEndpoint": {
        const url = String(msg.url ?? "");
        const endpoint = String(msg.endpoint ?? "");
        const ok = await testUrl(url);
        panel.webview.postMessage({ type: "testResult", endpoint, ok, detail: ok ? "reachable" : "unreachable" });
        return;
      }

      case "saveEndpoints": {
        const e = msg.endpoints as Record<string, unknown>;
        const config = vscode.workspace.getConfiguration("yggdrasil");
        const pairs: [string, unknown][] = [
          ["odinUrl", e.odinUrl],
          ["mimirUrl", e.mimirUrl],
          ["huginUrl", e.huginUrl],
          ["giteaUrl", e.giteaUrl],
          ["giteaRepo", e.giteaRepo],
          ["autoUpdate.enabled", e.autoUpdateEnabled],
        ];
        for (const [key, val] of pairs) {
          await config.update(key, val, vscode.ConfigurationTarget.Global);
        }
        panel.webview.postMessage({ type: "toast", message: "Endpoints saved", kind: "ok" });
        // Re-run hooks so the script env vars reflect new URLs
        try {
          await hooks.initialize();
        } catch {
          /* non-fatal */
        }
        return;
      }

      case "loadFlow": {
        const name = String(msg.name ?? "");
        const flow = await odin.getFlow(name);
        panel.webview.postMessage({ type: "flowLoaded", flow });
        return;
      }

      case "saveFlow": {
        const flow = msg.flow as Flow;
        const result = await odin.updateFlow(flow.name, flow);
        if (result.ok) {
          panel.webview.postMessage({
            type: "toast",
            message: `Flow "${flow.name}" saved to Odin`,
            kind: "ok",
          });
        } else {
          panel.webview.postMessage({
            type: "toast",
            message: `Save failed: ${result.error ?? "unknown error"} — Odin /api/flows may not be deployed yet.`,
            kind: "fail",
          });
        }
        return;
      }

      case "saveNotifications": {
        const n = msg.notifications as Record<string, unknown>;
        const h = msg.hooks as Record<string, unknown>;
        const config = vscode.workspace.getConfiguration("yggdrasil");
        await config.update("notifications.enabled", n.enabled, vscode.ConfigurationTarget.Global);
        await config.update("notifications.sound", n.sound, vscode.ConfigurationTarget.Global);
        await config.update("notifications.events", n.events, vscode.ConfigurationTarget.Global);
        await config.update("hooks.managed", h.managed, vscode.ConfigurationTarget.Global);
        panel.webview.postMessage({ type: "toast", message: "Notifications saved", kind: "ok" });
        return;
      }

      case "reinstallHooks": {
        await hooks.initialize();
        panel.webview.postMessage({
          type: "toast",
          message: "Hooks reinstalled — restart Claude Code to activate.",
          kind: "ok",
        });
        return;
      }

      case "setSecret": {
        const key = String(msg.key ?? "");
        const value = String(msg.value ?? "");
        await context.secrets.store(`yggdrasil.${key}`, value);
        panel.webview.postMessage({ type: "secretUpdated", key, set: true });
        panel.webview.postMessage({
          type: "toast",
          message: `Secret "${key}" stored in OS keychain`,
          kind: "ok",
        });
        return;
      }

      case "deleteSecret": {
        const key = String(msg.key ?? "");
        await context.secrets.delete(`yggdrasil.${key}`);
        panel.webview.postMessage({ type: "secretUpdated", key, set: false });
        panel.webview.postMessage({
          type: "toast",
          message: `Secret "${key}" deleted`,
          kind: "ok",
        });
        return;
      }

      // ─── Mimir Vault ───────────────────────────────────────────

      case "vaultList": {
        try {
          const result = await SettingsPanel.mimir.listVault();
          panel.webview.postMessage({ type: "vaultList", secrets: result.secrets, count: result.count });
        } catch (err) {
          const m = err instanceof Error ? err.message : String(err);
          panel.webview.postMessage({ type: "toast", message: `Vault list failed: ${m}`, kind: "fail" });
        }
        return;
      }

      case "vaultSet": {
        const key = String(msg.key ?? "");
        const value = String(msg.value ?? "");
        const scope = String(msg.scope ?? "global");
        const rawTags = String(msg.tags ?? "");
        const tags = rawTags
          .split(",")
          .map((t) => t.trim())
          .filter((t) => t.length > 0);

        if (!key || !value) {
          panel.webview.postMessage({ type: "toast", message: "Key and value are required", kind: "fail" });
          return;
        }

        // Security: never log the value — only key + scope metadata
        try {
          await SettingsPanel.mimir.setVault(key, value, scope, tags);
          panel.webview.postMessage({
            type: "toast",
            message: `Vault: stored "${key}" (scope: ${scope})`,
            kind: "ok",
          });
          // Refresh vault list
          const result = await SettingsPanel.mimir.listVault();
          panel.webview.postMessage({ type: "vaultList", secrets: result.secrets, count: result.count });
        } catch (err) {
          const m = err instanceof Error ? err.message : String(err);
          panel.webview.postMessage({ type: "toast", message: `Vault set failed: ${m}`, kind: "fail" });
        }
        return;
      }

      case "vaultDelete": {
        const key = String(msg.key ?? "");
        const scope = String(msg.scope ?? "global");
        try {
          await SettingsPanel.mimir.deleteVault(key, scope);
          panel.webview.postMessage({
            type: "toast",
            message: `Vault: deleted "${key}" (scope: ${scope})`,
            kind: "ok",
          });
          const result = await SettingsPanel.mimir.listVault();
          panel.webview.postMessage({ type: "vaultList", secrets: result.secrets, count: result.count });
        } catch (err) {
          const m = err instanceof Error ? err.message : String(err);
          panel.webview.postMessage({ type: "toast", message: `Vault delete failed: ${m}`, kind: "fail" });
        }
        return;
      }

      case "vaultCopy": {
        const key = String(msg.key ?? "");
        const scope = String(msg.scope ?? "global");
        try {
          const secret = await SettingsPanel.mimir.getVault(key, scope);
          await vscode.env.clipboard.writeText(secret.value);

          // Show initial toast
          panel.webview.postMessage({
            type: "toast",
            message: `Vault: "${key}" copied — clipboard clears in 30s`,
            kind: "ok",
          });

          // Cancel any previous timer for this key
          const timerKey = `${scope}:${key}`;
          const existing = clipboardTimers.get(timerKey);
          if (existing) clearTimeout(existing);

          // Schedule clipboard clear with progress toasts
          const t10 = setTimeout(() => {
            panel.webview.postMessage({
              type: "toast",
              message: `Vault: clipboard clears in 10s (key: ${key})`,
              kind: "ok",
            });
          }, 20_000);

          const t3 = setTimeout(() => {
            panel.webview.postMessage({
              type: "toast",
              message: `Vault: clipboard clears in 3s (key: ${key})`,
              kind: "ok",
            });
          }, 27_000);

          const tClear = setTimeout(async () => {
            clipboardTimers.delete(timerKey);
            const current = await vscode.env.clipboard.readText();
            if (current === secret.value) {
              await vscode.env.clipboard.writeText("");
              panel.webview.postMessage({
                type: "vaultClipboardCleared",
                scope,
                key,
              });
              panel.webview.postMessage({
                type: "toast",
                message: `Vault: clipboard cleared (key: ${key})`,
                kind: "ok",
              });
            }
          }, 30_000);

          // Store the primary clear timer so we can cancel it
          clipboardTimers.set(timerKey, tClear);

          // Ensure sub-timers also get cleaned up if caller cancels early
          // (they'll fire harmlessly if not cleared, so no strict need)
          void t10; void t3;
        } catch (err) {
          const m = err instanceof Error ? err.message : String(err);
          panel.webview.postMessage({ type: "toast", message: `Vault copy failed: ${m}`, kind: "fail" });
        }
        return;
      }
    }
  }

  private static async pushState(
    panel: vscode.WebviewPanel,
    context: vscode.ExtensionContext,
    odin: OdinClient
  ): Promise<void> {
    const config = vscode.workspace.getConfiguration("yggdrasil");

    const [flows, models] = await Promise.all([odin.listFlows(), odin.listModels()]);

    const secretKeys = ["giteaToken", "githubToken", "haToken", "braveSearchKey"];
    const secrets: Record<string, boolean> = {};
    for (const k of secretKeys) {
      const v = await context.secrets.get(`yggdrasil.${k}`);
      secrets[k] = typeof v === "string" && v.length > 0;
    }

    const backends = collectBackends(flows, models);

    // Load vault secret list (metadata only — never values)
    let vaultSecrets: unknown[] = [];
    let vaultCount = 0;
    try {
      const vaultResult = await SettingsPanel.mimir.listVault();
      vaultSecrets = vaultResult.secrets;
      vaultCount = vaultResult.count;
    } catch {
      // Mimir may not be reachable — fail silently, vault section shows empty
    }

    panel.webview.postMessage({
      type: "state",
      state: {
        endpoints: {
          odinUrl: config.get("odinUrl"),
          mimirUrl: config.get("mimirUrl"),
          huginUrl: config.get("huginUrl"),
          giteaUrl: config.get("giteaUrl"),
          giteaRepo: config.get("giteaRepo"),
          autoUpdateEnabled: config.get("autoUpdate.enabled"),
        },
        notifications: {
          enabled: config.get("notifications.enabled"),
          sound: config.get("notifications.sound"),
          events: config.get("notifications.events") ?? [],
        },
        hooks: { managed: config.get("hooks.managed") },
        flows: flows.map((f) => ({ name: f.name })),
        models,
        backends,
        secrets,
        vault: { secrets: vaultSecrets, count: vaultCount },
      },
    });
  }

  private static getHtml(webview: vscode.Webview, extensionUri: vscode.Uri): string {
    const mediaRoot = vscode.Uri.joinPath(extensionUri, "media");
    const cssUri = webview.asWebviewUri(vscode.Uri.joinPath(mediaRoot, "settings.css"));
    const jsUri = webview.asWebviewUri(vscode.Uri.joinPath(mediaRoot, "settings.js"));
    const nonce = getNonce();
    const csp = [
      `default-src 'none'`,
      `img-src ${webview.cspSource} data:`,
      `style-src ${webview.cspSource} 'unsafe-inline'`,
      `script-src 'nonce-${nonce}'`,
    ].join("; ");

    return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="${csp}">
<title>Yggdrasil Settings</title>
<link rel="stylesheet" href="${cssUri}">
</head>
<body>

<div class="header">
  <h1>Yggdrasil Settings</h1>
  <p>Central control surface for endpoints, flows, notifications, and secrets.</p>
  <div class="tabs">
    <button class="tab-btn active" data-tab="endpoints">Endpoints</button>
    <button class="tab-btn" data-tab="flows">Flows <span id="flow-dirty" class="dirty-indicator">unsaved</span></button>
    <button class="tab-btn" data-tab="notifications">Notifications &amp; Hooks</button>
    <button class="tab-btn" data-tab="secrets">Secrets</button>
  </div>
</div>

<div class="main">

  <!-- ENDPOINTS -->
  <div class="tab active" id="tab-endpoints">
    <div class="section">
      <h2>Service Endpoints</h2>
      <p class="sub">Base URLs for every Yggdrasil service this extension talks to. Test buttons do a live HTTP probe.</p>

      <div class="row">
        <label>Odin URL<span class="hint">Flow engine + chat + models</span></label>
        <input type="url" id="odinUrl" placeholder="http://10.0.65.8:8080">
        <div><button class="btn" data-test="odinUrl">Test</button><span id="test-odinUrl" class="test-result"></span></div>
      </div>
      <div class="row">
        <label>Mimir URL<span class="hint">Engram memory service</span></label>
        <input type="url" id="mimirUrl" placeholder="http://10.0.65.8:9090">
        <div><button class="btn" data-test="mimirUrl">Test</button><span id="test-mimirUrl" class="test-result"></span></div>
      </div>
      <div class="row">
        <label>Hugin URL<span class="hint">Ollama — reviewer + vision</span></label>
        <input type="url" id="huginUrl" placeholder="http://10.0.65.9:11434">
        <div><button class="btn" data-test="huginUrl">Test</button><span id="test-huginUrl" class="test-result"></span></div>
      </div>
      <div class="row">
        <label>Gitea URL<span class="hint">Auto-update source</span></label>
        <input type="url" id="giteaUrl" placeholder="http://10.0.65.11:3000">
        <div><button class="btn" data-test="giteaUrl">Test</button><span id="test-giteaUrl" class="test-result"></span></div>
      </div>
      <div class="row">
        <label>Gitea Repo<span class="hint">owner/name for .vsix releases</span></label>
        <input type="text" id="giteaRepo" placeholder="jesus/Yggdrasil">
        <div></div>
      </div>
      <div class="row">
        <label>Auto-update<span class="hint">Check hourly for new .vsix</span></label>
        <div><input type="checkbox" id="autoUpdate"> <span style="font-size:12px;color:#a1a1aa;">Enabled</span></div>
        <div></div>
      </div>

      <div class="btn-row">
        <button class="btn primary" id="save-endpoints">Save Endpoints</button>
      </div>
    </div>
  </div>

  <!-- FLOWS -->
  <div class="tab" id="tab-flows">
    <div class="section">
      <h2>Flow Configuration</h2>
      <p class="sub">Per-step role assignments. Changes save via <code>PUT /api/flows/:id</code> to Odin (falls back to read-only mode if endpoint not deployed).</p>

      <div class="flow-picker">
        <select id="flow-select">
          <option value="">— pick a flow —</option>
        </select>
      </div>

      <div id="flow-editor">
        <div class="empty-state">Pick a flow above to edit its steps, prompts, and parameters.</div>
      </div>

      <div class="btn-row">
        <button class="btn primary" id="save-flow">Save Flow</button>
        <button class="btn" id="revert-flow">Revert</button>
      </div>
    </div>
  </div>

  <!-- NOTIFICATIONS -->
  <div class="tab" id="tab-notifications">
    <div class="section">
      <h2>Notifications</h2>
      <p class="sub">Toasts + sound cues for memory events.</p>

      <div class="row">
        <label>Enabled<span class="hint">Master toggle</span></label>
        <div><input type="checkbox" id="notif-enabled"></div>
        <div></div>
      </div>
      <div class="row">
        <label>Sound<span class="hint">Play audio cue on store</span></label>
        <div><input type="checkbox" id="notif-sound"></div>
        <div></div>
      </div>

      <div>
        <label style="font-size:12px;color:#a1a1aa;">Event types that trigger notifications</label>
        <div class="checkbox-list" id="event-list"></div>
      </div>

      <div class="btn-row">
        <button class="btn primary" id="save-notifications">Save Notifications</button>
      </div>
    </div>

    <div class="section">
      <h2>Claude Code Hooks</h2>
      <p class="sub">Hooks deploy to <code>~/.claude/settings.json</code> so Claude Code emits memory events to Yggdrasil.</p>

      <div class="row">
        <label>Managed<span class="hint">Auto-install + update hooks</span></label>
        <div><input type="checkbox" id="hooks-managed"></div>
        <div></div>
      </div>

      <div class="btn-row">
        <button class="btn" id="reinstall-hooks">Reinstall hooks now</button>
      </div>
    </div>
  </div>

  <!-- SECRETS -->
  <div class="tab" id="tab-secrets">
    <div class="section">
      <h2>Secrets</h2>
      <p class="sub">Stored via VS Code SecretStorage — persisted to the OS keychain (libsecret / Credential Vault / Keychain). Never written to settings.json.</p>

      <div id="secrets-list"></div>
    </div>

    <div class="section vault-section">
      <h2>Mimir Vault</h2>
      <p class="sub">AES-256-GCM encrypted secrets stored in Mimir. Scoped by global / project / user. Values are never rendered in the UI — copy to clipboard only.</p>

      <div id="vault-list" class="vault-list">
        <div class="vault-empty">Loading vault…</div>
      </div>

      <div class="vault-divider"></div>

      <div class="vault-form">
        <div class="vault-form-title">Add / Update Secret</div>
        <div class="vault-form-grid">
          <div class="vault-form-field full">
            <label>Scope</label>
            <div class="vault-scope-group" id="vault-scope-radios">
              <label class="vault-scope-option">
                <input type="radio" name="vault-scope" value="global" checked> global
              </label>
              <label class="vault-scope-option">
                <input type="radio" name="vault-scope" value="project-auto"> project: <span id="vault-scope-project-auto-label" class="vault-scope-auto-label">auto</span>
              </label>
              <label class="vault-scope-option">
                <input type="radio" name="vault-scope" value="project-custom"> project:
                <input type="text" id="vault-scope-project-custom" class="vault-scope-text" placeholder="my-project" disabled>
              </label>
              <label class="vault-scope-option">
                <input type="radio" name="vault-scope" value="user"> user: <span id="vault-scope-user-label" class="vault-scope-auto-label">os-user</span>
              </label>
            </div>
          </div>
          <div class="vault-form-field">
            <label>Key</label>
            <input type="text" id="vault-key" placeholder="api_key_name" autocomplete="off" spellcheck="false">
          </div>
          <div class="vault-form-field">
            <label>Value</label>
            <input type="password" id="vault-value" placeholder="secret value" autocomplete="new-password">
          </div>
          <div class="vault-form-field full">
            <label>Tags <span style="font-size:9px;color:#52525b;font-weight:400;">(comma-separated, optional)</span></label>
            <input type="text" id="vault-tags" placeholder="env:prod, service:openai" autocomplete="off">
          </div>
        </div>
        <div class="btn-row">
          <button class="btn primary" id="vault-save">Save to Vault</button>
          <button class="btn" id="vault-refresh">Refresh</button>
        </div>
      </div>
    </div>
  </div>

</div>

<div id="toast" class="toast"></div>

<script nonce="${nonce}" src="${jsUri}"></script>

</body>
</html>`;
  }
}

function testUrl(url: string): Promise<boolean> {
  return new Promise((resolve) => {
    if (!url) return resolve(false);
    const timeout = setTimeout(() => resolve(false), 3000);
    try {
      const parsed = new URL(url);
      const client =
        parsed.protocol === "https:" ? require("https") : require("http");
      const probe = parsed.pathname === "/" || parsed.pathname === "" ? "/health" : parsed.pathname;
      const req = client.get(
        {
          hostname: parsed.hostname,
          port: parsed.port || (parsed.protocol === "https:" ? 443 : 80),
          path: probe,
          timeout: 3000,
        },
        (res: { statusCode?: number; resume: () => void }) => {
          clearTimeout(timeout);
          resolve(
            typeof res.statusCode === "number" && res.statusCode >= 200 && res.statusCode < 500
          );
          res.resume();
        }
      );
      req.on("error", () => {
        clearTimeout(timeout);
        resolve(false);
      });
    } catch {
      clearTimeout(timeout);
      resolve(false);
    }
  });
}

function getNonce(): string {
  let text = "";
  const possible = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  for (let i = 0; i < 32; i++) {
    text += possible.charAt(Math.floor(Math.random() * possible.length));
  }
  return text;
}

function collectBackends(flows: Flow[], models: { backend?: string }[]): string[] {
  const set = new Set<string>();
  for (const f of flows) {
    for (const s of f.steps) {
      if (s.backend) set.add(s.backend);
    }
  }
  for (const m of models) {
    if (m.backend) set.add(m.backend);
  }
  return Array.from(set).sort();
}
