/**
 * Memory Dashboard — Webview panel showing session statistics.
 *
 * Opened via Ctrl+Shift+M or clicking the status bar item.
 * Uses VS Code CSS variables for native theme integration.
 */

import * as vscode from "vscode";
import * as path from "path";
import * as fs from "fs";
import type { SessionStats } from "./statusBar";

export class DashboardPanel {
  private static panel: vscode.WebviewPanel | undefined;

  static createOrShow(
    context: vscode.ExtensionContext,
    stats: SessionStats
  ): void {
    if (DashboardPanel.panel) {
      DashboardPanel.panel.reveal();
      DashboardPanel.update(stats);
      return;
    }

    DashboardPanel.panel = vscode.window.createWebviewPanel(
      "yggdrasil-dashboard",
      "Yggdrasil Memory Dashboard",
      vscode.ViewColumn.Two,
      { enableScripts: true, retainContextWhenHidden: true }
    );

    DashboardPanel.panel.onDidDispose(() => {
      DashboardPanel.panel = undefined;
    });

    DashboardPanel.update(stats);
  }

  static update(stats: SessionStats): void {
    if (!DashboardPanel.panel) return;
    DashboardPanel.panel.webview.html = getHtml(stats);
  }
}

function getHtml(stats: SessionStats): string {
  const sessionTime = stats.sessionStart
    ? new Date(stats.sessionStart).toLocaleTimeString()
    : "No active session";

  const eventsHtml = stats.events
    .slice(-20)
    .reverse()
    .map((e) => {
      const ts = new Date(e.ts).toLocaleTimeString();
      const icon = eventIcon(e.event);
      const detail = eventDetail(e);
      return `<tr><td class="ts">${ts}</td><td>${icon}</td><td>${e.event}</td><td>${detail}</td></tr>`;
    })
    .join("\n");

  return `<!DOCTYPE html>
<html>
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<style>
  body {
    font-family: var(--vscode-font-family);
    font-size: var(--vscode-font-size);
    color: var(--vscode-foreground);
    background: var(--vscode-editor-background);
    padding: 20px;
    margin: 0;
  }
  h1 {
    font-size: 1.4em;
    margin: 0 0 16px 0;
    color: var(--vscode-foreground);
    border-bottom: 1px solid var(--vscode-widget-border);
    padding-bottom: 8px;
  }
  .stats-grid {
    display: grid;
    grid-template-columns: repeat(5, 1fr);
    gap: 12px;
    margin-bottom: 24px;
  }
  .stat-card {
    background: var(--vscode-editorWidget-background);
    border: 1px solid var(--vscode-widget-border);
    border-radius: 6px;
    padding: 16px;
    text-align: center;
  }
  .stat-value {
    font-size: 2em;
    font-weight: bold;
    color: var(--vscode-charts-blue);
    line-height: 1.2;
  }
  .stat-value.errors { color: var(--vscode-charts-red); }
  .stat-value.stored { color: var(--vscode-charts-green); }
  .stat-value.recalled { color: var(--vscode-charts-purple); }
  .stat-value.sidecar { color: var(--vscode-charts-orange); }
  .stat-label {
    font-size: 0.85em;
    color: var(--vscode-descriptionForeground);
    margin-top: 4px;
  }
  .session-info {
    font-size: 0.9em;
    color: var(--vscode-descriptionForeground);
    margin-bottom: 16px;
  }
  h2 {
    font-size: 1.1em;
    margin: 0 0 8px 0;
    color: var(--vscode-foreground);
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.9em;
  }
  th {
    text-align: left;
    padding: 6px 8px;
    border-bottom: 2px solid var(--vscode-widget-border);
    color: var(--vscode-descriptionForeground);
    font-weight: 600;
  }
  td {
    padding: 5px 8px;
    border-bottom: 1px solid var(--vscode-widget-border);
    vertical-align: top;
  }
  .ts {
    color: var(--vscode-descriptionForeground);
    white-space: nowrap;
    font-family: var(--vscode-editor-font-family);
  }
  tr:hover {
    background: var(--vscode-list-hoverBackground);
  }
  .empty {
    color: var(--vscode-descriptionForeground);
    font-style: italic;
    padding: 20px;
    text-align: center;
  }
</style>
</head>
<body>
  <h1>Yggdrasil Memory Dashboard</h1>

  <div class="session-info">Session started: ${sessionTime}</div>

  <div class="stats-grid">
    <div class="stat-card">
      <div class="stat-value recalled">${stats.recallCount}</div>
      <div class="stat-label">Recalled</div>
    </div>
    <div class="stat-card">
      <div class="stat-value stored">${stats.storeCount}</div>
      <div class="stat-label">Stored</div>
    </div>
    <div class="stat-card">
      <div class="stat-value errors">${stats.errorCount}</div>
      <div class="stat-label">Errors</div>
    </div>
    <div class="stat-card">
      <div class="stat-value sidecar">${stats.sidecarCount}</div>
      <div class="stat-label">Sidecar${stats.lastCategory ? ` (${stats.lastCategory})` : ""}</div>
    </div>
    <div class="stat-card">
      <div class="stat-value">${stats.events.length}</div>
      <div class="stat-label">Total Events</div>
    </div>
  </div>

  <h2>Recent Events</h2>
  ${
    stats.events.length > 0
      ? `<table>
    <thead><tr><th>Time</th><th></th><th>Event</th><th>Details</th></tr></thead>
    <tbody>${eventsHtml}</tbody>
  </table>`
      : '<div class="empty">No events yet. Start a Claude Code session to see memory operations.</div>'
  }
</body>
</html>`;
}

function eventIcon(event: string): string {
  switch (event) {
    case "init": return "\u{1F9E0}";
    case "recall": return "\u{1F50D}";
    case "ingest": return "\u{1F4BE}";
    case "sleep": return "\u{1F634}";
    case "error": return "\u274C";
    case "tool": return "\u{1F527}";
    case "sidecar": return "\u{1F916}";
    case "error_recall": return "\u{1F504}";
    case "update": return "\u2B06\uFE0F";
    default: return "\u2022";
  }
}

function eventDetail(e: { event: string; data: Record<string, unknown> }): string {
  switch (e.event) {
    case "init":
      return `${e.data.count ?? 0} engrams from prior session`;
    case "recall": {
      const query = e.data.query ?? e.data.file ?? "?";
      const mode = e.data.mode ? ` (${e.data.mode})` : "";
      return `${e.data.count ?? 0} memories for "${query}"${mode}`;
    }
    case "ingest":
      return e.data.stored
        ? `${e.data.file ?? "?"} \u2014 ${(e.data.cause as string)?.slice(0, 60) ?? ""}`
        : "skipped (not novel)";
    case "sleep":
      return String(e.data.summary ?? "session ended");
    case "error":
      return `${e.data.stage ?? "?"}: ${e.data.message ?? "unknown"}`;
    case "sidecar": {
      const cat = e.data.category ?? "?";
      const eng = e.data.engrams ?? e.data.queries ?? 0;
      const worthy = e.data.store_worthy ? " \u2714" : "";
      return `${cat} \u2014 ${eng} engrams${worthy}`;
    }
    case "error_recall":
      return `${e.data.count ?? 0} past error encounters`;
    case "update":
      return `${e.data.from ?? "?"} \u2192 ${e.data.to ?? "?"} (${e.data.status ?? "?"})`;
    case "tool":
      return `${e.data.name ?? "?"} (${e.data.status}, ${e.data.duration_ms ?? 0}ms)`;
    default:
      return JSON.stringify(e.data).slice(0, 80);
  }
}
