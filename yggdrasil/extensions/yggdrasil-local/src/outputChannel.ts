/**
 * Output channel for Yggdrasil memory operation logs.
 *
 * Provides a "Yggdrasil Memory" channel in VS Code's Output panel
 * with human-readable timestamped entries.
 */

import * as vscode from "vscode";
import type { YggEvent } from "./eventWatcher";

export class OutputChannelManager implements vscode.Disposable {
  private channel: vscode.OutputChannel;

  constructor() {
    this.channel = vscode.window.createOutputChannel("Yggdrasil Memory");
  }

  append(message: string): void {
    const ts = new Date().toLocaleTimeString();
    this.channel.appendLine(`[${ts}] ${message}`);
  }

  show(): void {
    this.channel.show(true); // preserveFocus
  }

  onEvent(event: YggEvent): void {
    const ts = new Date(event.ts).toLocaleTimeString();

    switch (event.event) {
      case "init": {
        const count = event.data.count ?? 0;
        this.channel.appendLine(
          `[${ts}] \u{1F9E0} Session started \u2014 ${count} engrams recalled from prior session`
        );
        break;
      }

      case "recall": {
        const count = event.data.count ?? 0;
        const query = event.data.query ?? event.data.file ?? "unknown";
        const mode = event.data.mode ? ` (${event.data.mode})` : "";
        this.channel.appendLine(
          `[${ts}] \u{1F50D} Recalled ${count} memories for "${query}"${mode}`
        );
        break;
      }

      case "ingest": {
        const file = event.data.file ?? "unknown";
        const cause = event.data.cause ?? "";
        if (event.data.stored) {
          this.channel.appendLine(
            `[${ts}] \u{1F4BE} Stored: ${file} \u2014 "${cause}"`
          );
        }
        break;
      }

      case "sleep": {
        const summary = event.data.summary ?? "session ended";
        this.channel.appendLine(`[${ts}] \u{1F634} ${summary}`);
        break;
      }

      case "error": {
        const stage = event.data.stage ?? "unknown";
        const message = event.data.message ?? "unknown error";
        this.channel.appendLine(
          `[${ts}] \u274C Error (${stage}): ${message}`
        );
        // Auto-show on errors
        this.channel.show(true);
        break;
      }

      case "sidecar": {
        const category = event.data.category ?? "unknown";
        const engrams = event.data.engrams ?? event.data.queries ?? 0;
        const worthy = event.data.store_worthy ? " (store-worthy)" : "";
        this.channel.appendLine(
          `[${ts}] \u{1F916} Sidecar: ${category} \u2014 ${engrams} engrams injected${worthy}`
        );
        break;
      }

      case "error_recall": {
        const count = event.data.count ?? 0;
        this.channel.appendLine(
          `[${ts}] \u{1F504} Error recall: found ${count} past encounters`
        );
        break;
      }

      case "update": {
        const from = event.data.from ?? "?";
        const to = event.data.to ?? "?";
        const status = event.data.status ?? "?";
        const icon = status === "complete" ? "\u2705" : "\u{2B06}\u{FE0F}";
        this.channel.appendLine(
          `[${ts}] ${icon} Extension update: ${from} \u2192 ${to} (${status})`
        );
        break;
      }

      case "tool": {
        const name = event.data.name ?? "unknown";
        const status = event.data.status ?? "?";
        const duration = event.data.duration_ms ?? 0;
        const icon = status === "ok" ? "\u2705" : "\u274C";
        this.channel.appendLine(
          `[${ts}] ${icon} Tool: ${name} (${status}, ${duration}ms)`
        );
        break;
      }

      default:
        this.channel.appendLine(`[${ts}] ${event.event}: ${JSON.stringify(event.data)}`);
    }
  }

  dispose(): void {
    this.channel.dispose();
  }
}
