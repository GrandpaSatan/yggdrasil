/**
 * Yggdrasil Local — VS Code Extension entry point.
 *
 * Self-managing: on activation, deploys the sidecar script,
 * configures Claude Code hooks, checks for updates, and monitors
 * memory operations via the event watcher.
 */

import * as vscode from "vscode";
import { StatusBarManager } from "./statusBar";
import { EventWatcher } from "./eventWatcher";
import { OutputChannelManager } from "./outputChannel";
import { NotificationManager } from "./notifications";
import { AudioPlayer } from "./audioPlayer";
import { DashboardPanel } from "./dashboard";
import { HookManager } from "./hookManager";
import { AutoUpdater } from "./autoUpdater";
import { FlowsPanel } from "./views/flowsPanel";
import { FlowsTreeProvider } from "./views/flowsTreeProvider";
import { ModelsTreeProvider } from "./views/modelsTreeProvider";
import { SettingsPanel } from "./views/settingsPanel";
import { ChatPanel } from "./views/chatPanel";
import { OdinClient } from "./api/odinClient";
import { ChatHistory } from "./chat/history";
import { registerCodeActions } from "./chat/codeActions";

let statusBar: StatusBarManager;
let eventWatcher: EventWatcher;
let outputChannel: OutputChannelManager;
let notifications: NotificationManager;
let audioPlayer: AudioPlayer;
let hookManager: HookManager;
let autoUpdater: AutoUpdater;

export async function activate(context: vscode.ExtensionContext) {
  // Initialize components
  outputChannel = new OutputChannelManager();
  statusBar = new StatusBarManager();
  notifications = new NotificationManager();
  audioPlayer = new AudioPlayer();

  // ── Self-management: deploy script + configure hooks ──────────
  hookManager = new HookManager(context, outputChannel);
  try {
    await hookManager.initialize();
  } catch (err) {
    outputChannel.append(
      `Hook setup error: ${err instanceof Error ? err.message : String(err)}`
    );
  }

  // ── Auto-update: check Gitea for newer extension version ──────
  autoUpdater = new AutoUpdater(context, outputChannel);
  try {
    await autoUpdater.checkAndUpdate();
  } catch (err) {
    outputChannel.append(
      `Auto-update error: ${err instanceof Error ? err.message : String(err)}`
    );
  }

  // ── Health check: update status bar color ─────────────────────
  try {
    const health = await hookManager.checkHealth();
    statusBar.setHealthStatus(health);
  } catch {
    statusBar.setHealthStatus("red");
  }

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

  // Shared Odin HTTP client + chat history
  const odin = new OdinClient();
  const chatHistory = new ChatHistory(context);

  // Sidebar trees
  const flowsTree = new FlowsTreeProvider();
  const modelsTree = new ModelsTreeProvider(odin);
  context.subscriptions.push(
    vscode.window.registerTreeDataProvider("yggdrasil.flowsTree", flowsTree),
    vscode.window.registerTreeDataProvider("yggdrasil.modelsTree", modelsTree),
    modelsTree
  );

  // Editor context-menu actions (explain selection, edit with model, ask about file)
  registerCodeActions(context, (seed) => {
    ChatPanel.show(context, odin, chatHistory, seed);
  });

  // Register commands
  context.subscriptions.push(
    vscode.commands.registerCommand("yggdrasil.openDashboard", () => {
      DashboardPanel.createOrShow(context, statusBar.getStats());
    }),

    vscode.commands.registerCommand(
      "yggdrasil.openFlows",
      (flowId?: string) => {
        FlowsPanel.createOrShow(context, flowId);
      }
    ),

    vscode.commands.registerCommand("yggdrasil.refreshFlows", () => {
      flowsTree.refresh();
      vscode.window.showInformationMessage("Yggdrasil flows refreshed.");
    }),

    vscode.commands.registerCommand("yggdrasil.openSettings", () => {
      SettingsPanel.createOrShow(context, odin, hookManager);
    }),

    vscode.commands.registerCommand("yggdrasil.openChat", () => {
      ChatPanel.show(context, odin, chatHistory);
    }),

    vscode.commands.registerCommand("yggdrasil.refreshModels", () => {
      modelsTree.refresh();
    }),

    vscode.commands.registerCommand(
      "yggdrasil.useModelInChat",
      (modelId: string) => {
        const panel = ChatPanel.show(context, odin, chatHistory);
        // The chat panel UI reads model via the picker — post a seed that
        // pre-pends /model <id> so the user just adds their question.
        panel; // silence unused if minor
        vscode.window.showInformationMessage(
          `Model "${modelId}" — use /model ${modelId} <your prompt> or select it in the chat picker.`
        );
      }
    ),

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

    vscode.commands.registerCommand("yggdrasil.reinstallHooks", async () => {
      try {
        await hookManager.initialize();
        const health = await hookManager.checkHealth();
        statusBar.setHealthStatus(health);
        vscode.window.showInformationMessage(
          "Yggdrasil hooks reinstalled. Restart Claude Code to activate."
        );
      } catch (err) {
        vscode.window.showErrorMessage(
          `Hook reinstall failed: ${err instanceof Error ? err.message : String(err)}`
        );
      }
    }),

    vscode.commands.registerCommand(
      "yggdrasil.checkForUpdates",
      async () => {
        // Reset rate limit to force check
        await context.globalState.update("autoUpdate.lastCheck", 0);
        try {
          await autoUpdater.checkAndUpdate();
          vscode.window.showInformationMessage(
            "Yggdrasil update check complete."
          );
        } catch (err) {
          vscode.window.showErrorMessage(
            `Update check failed: ${err instanceof Error ? err.message : String(err)}`
          );
        }
      }
    ),

    // Disposables
    statusBar,
    eventWatcher,
    outputChannel,
    hookManager,
    autoUpdater
  );

  // Start watching
  eventWatcher.start();
  statusBar.show();

  outputChannel.append("Yggdrasil Local extension activated (self-managed)");
}

export function deactivate() {
  eventWatcher?.dispose();
  statusBar?.dispose();
  outputChannel?.dispose();
  hookManager?.dispose();
  autoUpdater?.dispose();
}
