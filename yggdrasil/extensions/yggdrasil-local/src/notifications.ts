/**
 * Configurable toast notifications for memory operations.
 *
 * Reads yggdrasil.notifications.enabled and yggdrasil.notifications.events
 * from VS Code settings to decide which events trigger a toast.
 */

import * as vscode from "vscode";
import type { YggEvent } from "./eventWatcher";

export class NotificationManager {
  onEvent(event: YggEvent): void {
    const config = vscode.workspace.getConfiguration("yggdrasil");
    if (!config.get<boolean>("notifications.enabled", true)) return;

    const enabledEvents = config.get<string[]>("notifications.events", [
      "ingest",
      "error",
    ]);
    if (!enabledEvents.includes(event.event)) return;

    switch (event.event) {
      case "init": {
        const count = event.data.count ?? 0;
        vscode.window.showInformationMessage(
          `Yggdrasil: ${count} engrams recalled from prior session`
        );
        break;
      }

      case "recall": {
        const count = event.data.count ?? 0;
        const file = event.data.file ?? "unknown";
        vscode.window.showInformationMessage(
          `Yggdrasil: ${count} memories recalled for ${file}`
        );
        break;
      }

      case "ingest": {
        if (event.data.stored) {
          const file = event.data.file ?? "unknown";
          const cause =
            typeof event.data.cause === "string"
              ? event.data.cause.slice(0, 80)
              : "";
          vscode.window.showInformationMessage(
            `Yggdrasil: Stored memory for ${file}${cause ? ` \u2014 ${cause}` : ""}`
          );
        }
        break;
      }

      case "sleep": {
        const summary = event.data.summary ?? "session ended";
        vscode.window.showInformationMessage(`Yggdrasil: ${summary}`);
        break;
      }

      case "error": {
        const stage = event.data.stage ?? "unknown";
        const message = event.data.message ?? "unknown error";
        vscode.window.showWarningMessage(
          `Yggdrasil error (${stage}): ${message}`
        );
        break;
      }
    }
  }
}
