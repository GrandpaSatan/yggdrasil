/**
 * Dynamic status bar item for Yggdrasil memory operations.
 *
 * Shows: "$(database) Ygg: N recalled · N stored"
 * Click opens the memory dashboard.
 */

import * as vscode from "vscode";
import type { YggEvent } from "./eventWatcher";

export interface SessionStats {
  recallCount: number;
  storeCount: number;
  errorCount: number;
  sidecarCount: number;
  lastCategory: string | null;
  sessionStart: string | null;
  lastEvent: string | null;
  events: YggEvent[];
}

export class StatusBarManager implements vscode.Disposable {
  private item: vscode.StatusBarItem;
  private healthStatus: "green" | "yellow" | "red" = "green";
  private stats: SessionStats = {
    recallCount: 0,
    storeCount: 0,
    errorCount: 0,
    sidecarCount: 0,
    lastCategory: null,
    sessionStart: null,
    lastEvent: null,
    events: [],
  };

  constructor() {
    this.item = vscode.window.createStatusBarItem(
      vscode.StatusBarAlignment.Right,
      100
    );
    this.item.command = "yggdrasil.openDashboard";
    this.updateText();
  }

  show(): void {
    this.item.show();
  }

  setHealthStatus(status: "green" | "yellow" | "red"): void {
    this.healthStatus = status;
    this.updateText();
  }

  getStats(): SessionStats {
    return { ...this.stats, events: [...this.stats.events] };
  }

  onEvent(event: YggEvent): void {
    // Keep last 50 events for dashboard
    this.stats.events.push(event);
    if (this.stats.events.length > 50) {
      this.stats.events.shift();
    }
    this.stats.lastEvent = event.event;

    switch (event.event) {
      case "init":
        // New session — reset counters
        this.stats.recallCount = (event.data.count as number) || 0;
        this.stats.storeCount = 0;
        this.stats.errorCount = 0;
        this.stats.sidecarCount = 0;
        this.stats.lastCategory = null;
        this.stats.sessionStart = event.ts;
        break;

      case "recall":
        this.stats.recallCount += (event.data.count as number) || 0;
        break;

      case "ingest":
        if (event.data.stored) {
          this.stats.storeCount++;
        }
        break;

      case "error":
        this.stats.errorCount++;
        break;

      case "sleep":
        // Session end — keep stats visible
        break;

      case "sidecar":
        this.stats.sidecarCount++;
        this.stats.lastCategory = (event.data.category as string) || null;
        break;

      case "error_recall":
        this.stats.recallCount += (event.data.count as number) || 0;
        break;

      case "update":
        // Extension auto-update — no counter change
        break;

      case "tool":
        // MCP tool execution — no counter change
        break;
    }

    this.updateText();
  }

  private updateText(): void {
    const parts: string[] = [];

    if (this.stats.lastCategory) {
      parts.push(this.stats.lastCategory);
    }
    if (this.stats.recallCount > 0) {
      parts.push(`${this.stats.recallCount} recalled`);
    }
    if (this.stats.storeCount > 0) {
      parts.push(`${this.stats.storeCount} stored`);
    }
    if (this.stats.errorCount > 0) {
      parts.push(`${this.stats.errorCount} errors`);
    }

    const summary = parts.length > 0 ? parts.join(" \u00b7 ") : "idle";
    this.item.text = `$(database) Ygg: ${summary}`;

    // Tooltip with more detail
    const lines = ["Yggdrasil Memory Monitor"];
    if (this.stats.sessionStart) {
      const start = new Date(this.stats.sessionStart);
      lines.push(`Session: ${start.toLocaleTimeString()}`);
    }
    lines.push(`Recalled: ${this.stats.recallCount}`);
    lines.push(`Stored: ${this.stats.storeCount}`);
    lines.push(`Errors: ${this.stats.errorCount}`);
    if (this.stats.sidecarCount > 0) {
      lines.push(`Sidecar: ${this.stats.sidecarCount} classifications`);
    }
    if (this.stats.lastCategory) {
      lines.push(`Category: ${this.stats.lastCategory}`);
    }
    if (this.stats.lastEvent) {
      lines.push(`Last: ${this.stats.lastEvent}`);
    }
    // Health info
    const healthLabels = {
      green: "Hooks: configured \u2713 | Mimir: reachable \u2713",
      yellow: "Hooks: configured \u2713 | Mimir: unreachable \u2717",
      red: "Hooks: NOT CONFIGURED \u2717",
    };
    lines.push(healthLabels[this.healthStatus]);
    lines.push("", "Click to open dashboard");
    this.item.tooltip = lines.join("\n");

    // Color priority: red health > errors > yellow health > green (default)
    if (this.healthStatus === "red") {
      this.item.backgroundColor = new vscode.ThemeColor(
        "statusBarItem.errorBackground"
      );
    } else if (this.stats.errorCount > 0) {
      this.item.backgroundColor = new vscode.ThemeColor(
        "statusBarItem.warningBackground"
      );
    } else if (this.healthStatus === "yellow") {
      this.item.backgroundColor = new vscode.ThemeColor(
        "statusBarItem.warningBackground"
      );
    } else {
      this.item.backgroundColor = undefined;
    }
  }

  dispose(): void {
    this.item.dispose();
  }
}
