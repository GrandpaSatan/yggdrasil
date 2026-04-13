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
import type { HookManager } from "../hookManager";

export class SettingsPanel {
  private static panel: vscode.WebviewPanel | undefined;
  private static readonly viewType = "yggdrasil.settingsPanel";

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
