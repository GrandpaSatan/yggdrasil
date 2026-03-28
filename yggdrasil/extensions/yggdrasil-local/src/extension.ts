/**
 * Yggdrasil Local — VS Code Extension entry point.
 *
 * Activates on startup, watches /tmp/ygg-hooks/memory-events.jsonl for
 * real-time memory operation events, and provides status bar, output channel,
 * notifications, and a dashboard webview.
 */

import * as vscode from "vscode";
import { StatusBarManager } from "./statusBar";
import { EventWatcher } from "./eventWatcher";
import { OutputChannelManager } from "./outputChannel";
import { NotificationManager } from "./notifications";
import { AudioPlayer } from "./audioPlayer";
import { DashboardPanel } from "./dashboard";

let statusBar: StatusBarManager;
let eventWatcher: EventWatcher;
let outputChannel: OutputChannelManager;
let notifications: NotificationManager;
let audioPlayer: AudioPlayer;

export function activate(context: vscode.ExtensionContext) {
  // Initialize components
  outputChannel = new OutputChannelManager();
  statusBar = new StatusBarManager();
  notifications = new NotificationManager();
  audioPlayer = new AudioPlayer();

  // Get events file path from config
  const config = vscode.workspace.getConfiguration("yggdrasil");
  const eventsFile = config.get<string>(
    "eventsFile",
    "/tmp/ygg-hooks/memory-events.jsonl"
  );

  // Wire event watcher to all consumers
  eventWatcher = new EventWatcher(eventsFile, (event) => {
    statusBar.onEvent(event);
    outputChannel.onEvent(event);
    notifications.onEvent(event);
    audioPlayer.onEvent(event);
  });

  // Register commands
  context.subscriptions.push(
    vscode.commands.registerCommand("yggdrasil.openDashboard", () => {
      DashboardPanel.createOrShow(context, statusBar.getStats());
    }),

    vscode.commands.registerCommand("yggdrasil.showLog", () => {
      outputChannel.show();
    }),

    vscode.commands.registerCommand("yggdrasil.toggleNotifications", () => {
      const current = config.get<boolean>("notifications.enabled", true);
      config.update(
        "notifications.enabled",
        !current,
        vscode.ConfigurationTarget.Global
      );
      vscode.window.showInformationMessage(
        `Yggdrasil notifications ${!current ? "enabled" : "disabled"}`
      );
    }),

    // Disposables
    statusBar,
    eventWatcher,
    outputChannel
  );

  // Start watching
  eventWatcher.start();
  statusBar.show();

  outputChannel.append("Yggdrasil Local extension activated");
}

export function deactivate() {
  eventWatcher?.dispose();
  statusBar?.dispose();
  outputChannel?.dispose();
}
