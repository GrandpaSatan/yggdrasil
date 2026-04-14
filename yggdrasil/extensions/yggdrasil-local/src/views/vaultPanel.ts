/**
 * Sprint 064 P4 — full Vault panel.
 *
 * Activity-bar webview view (id: yggdrasil.vaultPanel) that surfaces every
 * Mimir vault secret with scope filter, search, add dialog, and copy/delete
 * actions. The existing Settings tab keeps its compact subsection as a
 * quick-access fallback.
 *
 * IPC shape (extension ↔ webview):
 *   refresh                                            → reload list
 *   add { key, scope, value, tags[] }                  → POST set
 *   delete { key, scope }                              → POST delete
 *   copy { key, scope }                                → fetch value + clipboard (30s clear)
 *
 * On the way back:
 *   list { secrets[], count }                          → render
 *   error { message }                                  → status row
 *   clipboardCleared { key }                           → row hint
 */

import * as vscode from "vscode";
import { MimirClient, VaultSecret } from "../api/mimirClient";

const CLIPBOARD_CLEAR_MS = 30_000;

export class VaultPanelProvider implements vscode.WebviewViewProvider {
  public static readonly viewType = "yggdrasil.vaultPanel";

  private view: vscode.WebviewView | undefined;
  private readonly mimir = new MimirClient();
  private readonly clearTimers = new Map<string, NodeJS.Timeout>();

  constructor(private readonly extensionUri: vscode.Uri) {}

  public resolveWebviewView(view: vscode.WebviewView): void {
    this.view = view;
    view.webview.options = { enableScripts: true };
    view.webview.html = this.renderHtml();

    view.webview.onDidReceiveMessage(async (msg) => {
      try {
        switch (msg?.type) {
          case "refresh":
            await this.sendList();
            break;
          case "add":
            await this.handleAdd(msg);
            break;
          case "delete":
            await this.handleDelete(msg);
            break;
          case "copy":
            await this.handleCopy(msg);
            break;
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        view.webview.postMessage({ type: "error", message });
      }
    });

    void this.sendList();
  }

  /** Public command hook — `yggdrasil.refreshVault`. */
  public async refresh(): Promise<void> {
    await this.sendList();
  }

  // ── IPC handlers ─────────────────────────────────────────────

  private async sendList(): Promise<void> {
    if (!this.view) return;
    try {
      const result = await this.mimir.listVault();
      const grouped = groupByScope(result.secrets);
      this.view.webview.postMessage({
        type: "list",
        secrets: result.secrets,
        count: result.count,
        grouped,
      });
    } catch (err) {
      this.view.webview.postMessage({
        type: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }

  private async handleAdd(msg: {
    key?: unknown;
    scope?: unknown;
    value?: unknown;
    tags?: unknown;
  }): Promise<void> {
    const key = String(msg.key ?? "").trim();
    const scope = String(msg.scope ?? "global").trim() || "global";
    const value = String(msg.value ?? "");
    const tags = Array.isArray(msg.tags) ? (msg.tags as unknown[]).map(String) : [];

    if (!key) throw new Error("key is required");
    if (!value) throw new Error("value is required");

    await this.mimir.setVault(key, value, scope, tags);
    await this.sendList();
  }

  private async handleDelete(msg: {
    key?: unknown;
    scope?: unknown;
  }): Promise<void> {
    const key = String(msg.key ?? "").trim();
    const scope = String(msg.scope ?? "global").trim() || "global";
    if (!key) throw new Error("key is required");

    const confirm = await vscode.window.showWarningMessage(
      `Delete vault key '${key}' (scope=${scope})?`,
      { modal: true },
      "Delete",
    );
    if (confirm !== "Delete") return;

    await this.mimir.deleteVault(key, scope);
    await this.sendList();
  }

  private async handleCopy(msg: {
    key?: unknown;
    scope?: unknown;
  }): Promise<void> {
    const key = String(msg.key ?? "").trim();
    const scope = String(msg.scope ?? "global").trim() || "global";
    if (!key) throw new Error("key is required");

    const got = await this.mimir.getVault(key, scope);
    await vscode.env.clipboard.writeText(got.value);

    const cacheKey = `${scope}:${key}`;
    const existing = this.clearTimers.get(cacheKey);
    if (existing) clearTimeout(existing);

    const timer = setTimeout(async () => {
      // Only clear if the clipboard still holds our value (don't trample user edits).
      const current = await vscode.env.clipboard.readText();
      if (current === got.value) {
        await vscode.env.clipboard.writeText("");
      }
      this.clearTimers.delete(cacheKey);
      this.view?.webview.postMessage({
        type: "clipboardCleared",
        key,
        scope,
      });
    }, CLIPBOARD_CLEAR_MS);
    this.clearTimers.set(cacheKey, timer);

    this.view?.webview.postMessage({
      type: "copied",
      key,
      scope,
      ttlMs: CLIPBOARD_CLEAR_MS,
    });
  }

  // ── HTML ─────────────────────────────────────────────────────

  private renderHtml(): string {
    return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline';">
<style>
  :root {
    --bg: var(--vscode-sideBar-background);
    --fg: var(--vscode-foreground);
    --muted: var(--vscode-descriptionForeground);
    --border: var(--vscode-panel-border);
    --accent: var(--vscode-textLink-foreground);
    --row-hover: var(--vscode-list-hoverBackground);
    --input-bg: var(--vscode-input-background);
    --input-border: var(--vscode-input-border);
    --button-bg: var(--vscode-button-background);
    --button-fg: var(--vscode-button-foreground);
    --button-hover: var(--vscode-button-hoverBackground);
    --error: var(--vscode-errorForeground);
  }
  * { box-sizing: border-box; }
  body { background: var(--bg); color: var(--fg); font-family: var(--vscode-font-family); font-size: 12px; padding: 8px; margin: 0; }
  .toolbar { display: grid; grid-template-columns: 1fr auto auto; gap: 6px; margin-bottom: 8px; align-items: center; }
  input, select, button { font: inherit; color: var(--fg); }
  input[type="text"], input[type="password"], select {
    background: var(--input-bg); border: 1px solid var(--input-border); padding: 4px 6px; border-radius: 2px; min-width: 0;
  }
  button { background: var(--button-bg); color: var(--button-fg); border: 0; padding: 4px 8px; border-radius: 2px; cursor: pointer; }
  button:hover { background: var(--button-hover); }
  button.secondary { background: transparent; color: var(--accent); border: 1px solid var(--input-border); }
  .scope-list { margin: 8px 0; padding: 0; list-style: none; max-height: 110px; overflow-y: auto; border: 1px solid var(--border); border-radius: 2px; }
  .scope-list li { padding: 4px 8px; cursor: pointer; display: flex; justify-content: space-between; }
  .scope-list li:hover { background: var(--row-hover); }
  .scope-list li.active { background: var(--row-hover); border-left: 2px solid var(--accent); padding-left: 6px; }
  .secrets { border-top: 1px solid var(--border); }
  .secret-row { padding: 6px 4px; border-bottom: 1px solid var(--border); display: grid; grid-template-columns: 1fr auto; gap: 4px; align-items: start; }
  .secret-row:hover { background: var(--row-hover); }
  .secret-meta { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .secret-key { font-weight: 600; word-break: break-all; }
  .secret-tags { color: var(--muted); font-size: 11px; }
  .secret-actions { display: flex; gap: 4px; }
  .secret-actions button { padding: 2px 6px; font-size: 11px; }
  .empty { color: var(--muted); padding: 12px; text-align: center; }
  .status { color: var(--muted); font-size: 11px; padding: 4px 0; min-height: 16px; }
  .status.error { color: var(--error); }
  dialog { background: var(--bg); color: var(--fg); border: 1px solid var(--border); padding: 12px; min-width: 280px; }
  dialog::backdrop { background: rgba(0,0,0,0.4); }
  .dialog-row { display: grid; grid-template-columns: 80px 1fr; gap: 6px; margin-bottom: 6px; align-items: center; }
  .dialog-row input { width: 100%; }
  .dialog-actions { display: flex; justify-content: flex-end; gap: 6px; margin-top: 8px; }
</style>
</head>
<body>
<div class="toolbar">
  <input type="text" id="search" placeholder="filter keys / tags…">
  <button id="addBtn">+ Add</button>
  <button class="secondary" id="refreshBtn" title="Refresh">↻</button>
</div>

<ul class="scope-list" id="scopeList"></ul>
<div class="status" id="status"></div>
<div class="secrets" id="secrets"></div>

<dialog id="addDialog">
  <h3 style="margin:0 0 8px 0;">Add secret</h3>
  <div class="dialog-row"><label>Key</label><input id="d_key" type="text" placeholder="e.g. github_token"></div>
  <div class="dialog-row"><label>Scope</label><input id="d_scope" type="text" placeholder="global | project:foo | user:alice" value="global"></div>
  <div class="dialog-row"><label>Value</label><input id="d_value" type="password" placeholder="secret value (won't be shown)"></div>
  <div class="dialog-row"><label>Tags</label><input id="d_tags" type="text" placeholder="comma,separated"></div>
  <div class="dialog-actions">
    <button class="secondary" id="d_cancel">Cancel</button>
    <button id="d_save">Save</button>
  </div>
</dialog>

<script>
  const vscode = acquireVsCodeApi();
  let allSecrets = [];
  let activeScope = "ALL";
  let filterText = "";

  const $ = (id) => document.getElementById(id);
  const escape = (s) => String(s).replace(/[&<>"']/g, (c) => ({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));

  function renderScopes() {
    const counts = new Map();
    counts.set("ALL", allSecrets.length);
    for (const s of allSecrets) counts.set(s.scope, (counts.get(s.scope) || 0) + 1);
    const ul = $("scopeList");
    const entries = Array.from(counts.entries()).sort(([a],[b]) => a === "ALL" ? -1 : b === "ALL" ? 1 : a.localeCompare(b));
    ul.innerHTML = entries.map(([scope, n]) => {
      const cls = scope === activeScope ? "active" : "";
      const label = scope === "ALL" ? "(all scopes)" : escape(scope);
      return '<li class="' + cls + '" data-scope="' + escape(scope) + '"><span>' + label + '</span><span style="color:var(--muted);">' + n + '</span></li>';
    }).join("");
    ul.querySelectorAll("li").forEach(li => li.addEventListener("click", () => {
      activeScope = li.getAttribute("data-scope");
      renderScopes();
      renderSecrets();
    }));
  }

  function renderSecrets() {
    const filtered = allSecrets.filter(s => {
      if (activeScope !== "ALL" && s.scope !== activeScope) return false;
      if (filterText) {
        const q = filterText.toLowerCase();
        if (!s.key.toLowerCase().includes(q) && !(s.tags || []).some(t => t.toLowerCase().includes(q))) return false;
      }
      return true;
    });
    const div = $("secrets");
    if (filtered.length === 0) {
      div.innerHTML = '<div class="empty">No secrets in this view.</div>';
      return;
    }
    div.innerHTML = filtered.map(s => {
      const tags = (s.tags || []).map(escape).join(", ");
      const updated = s.updated_at ? escape(s.updated_at.slice(0, 10)) : "";
      return '' +
        '<div class="secret-row">' +
          '<div class="secret-meta">' +
            '<span class="secret-key">' + escape(s.key) + '</span>' +
            '<span class="secret-tags">' + escape(s.scope) + (tags ? " · " + tags : "") + (updated ? " · " + updated : "") + '</span>' +
          '</div>' +
          '<div class="secret-actions">' +
            '<button data-act="copy" data-key="' + escape(s.key) + '" data-scope="' + escape(s.scope) + '">Copy</button>' +
            '<button class="secondary" data-act="delete" data-key="' + escape(s.key) + '" data-scope="' + escape(s.scope) + '">Del</button>' +
          '</div>' +
        '</div>';
    }).join("");
    div.querySelectorAll("button[data-act]").forEach(btn => {
      btn.addEventListener("click", () => {
        const act = btn.getAttribute("data-act");
        const key = btn.getAttribute("data-key");
        const scope = btn.getAttribute("data-scope");
        vscode.postMessage({ type: act, key, scope });
      });
    });
  }

  function setStatus(msg, isError) {
    const s = $("status");
    s.textContent = msg || "";
    s.className = "status" + (isError ? " error" : "");
  }

  $("search").addEventListener("input", (e) => { filterText = e.target.value; renderSecrets(); });
  $("refreshBtn").addEventListener("click", () => { setStatus("Refreshing…"); vscode.postMessage({ type: "refresh" }); });

  // Add dialog
  const dlg = $("addDialog");
  $("addBtn").addEventListener("click", () => {
    $("d_key").value = ""; $("d_scope").value = activeScope === "ALL" ? "global" : activeScope;
    $("d_value").value = ""; $("d_tags").value = "";
    dlg.showModal(); $("d_key").focus();
  });
  $("d_cancel").addEventListener("click", () => dlg.close());
  $("d_save").addEventListener("click", () => {
    const key = $("d_key").value.trim();
    const scope = $("d_scope").value.trim() || "global";
    const value = $("d_value").value;
    const tags = $("d_tags").value.split(",").map(t => t.trim()).filter(Boolean);
    if (!key || !value) { setStatus("Key and value are required.", true); return; }
    vscode.postMessage({ type: "add", key, scope, value, tags });
    dlg.close();
  });

  window.addEventListener("message", (event) => {
    const msg = event.data;
    if (msg.type === "list") {
      allSecrets = msg.secrets || [];
      setStatus(msg.count + " secret" + (msg.count === 1 ? "" : "s"));
      renderScopes(); renderSecrets();
    } else if (msg.type === "error") {
      setStatus("Error: " + msg.message, true);
    } else if (msg.type === "copied") {
      setStatus("Copied " + msg.key + " — clears in " + Math.round(msg.ttlMs / 1000) + "s");
    } else if (msg.type === "clipboardCleared") {
      setStatus("Clipboard cleared (" + msg.key + ")");
    }
  });

  // Initial load
  vscode.postMessage({ type: "refresh" });
</script>
</body>
</html>`;
  }
}

/** Group secrets by scope for the scope-tree rendering. */
export function groupByScope(secrets: VaultSecret[]): Record<string, VaultSecret[]> {
  const out: Record<string, VaultSecret[]> = {};
  for (const s of secrets) {
    const k = s.scope || "global";
    (out[k] ||= []).push(s);
  }
  return out;
}
