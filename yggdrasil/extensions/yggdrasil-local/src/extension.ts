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
import { RepoTreeProvider } from "./views/repoTreeProvider";
import { getEditorContext, formatContextBlock } from "./editorContext";
import { SelfImprovementChecker } from "./selfImprovement";

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
  const repoTree = new RepoTreeProvider();
  context.subscriptions.push(
    vscode.window.registerTreeDataProvider("yggdrasil.flowsTree", flowsTree),
    vscode.window.registerTreeDataProvider("yggdrasil.modelsTree", modelsTree),
    vscode.window.registerTreeDataProvider("yggdrasil.repo", repoTree),
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

    // P3 — Repo awareness commands
    vscode.commands.registerCommand("yggdrasil.attachFile", (uri: vscode.Uri) => {
      const panel = ChatPanel.show(context, odin, chatHistory);
      if (uri) {
        vscode.workspace.fs.readFile(uri).then((data) => {
          const content = new TextDecoder().decode(data);
          const label = vscode.workspace.asRelativePath(uri);
          const langId = label.split(".").pop() ?? "text";
          // Post attachment to webview
          panel; // panel is used for side-effect (show)
          // The attachment is posted via the webview — trigger via seed
          ChatPanel.show(context, odin, chatHistory, {
            userText: "",
            contextBlock: `File: ${label}\n\`\`\`${langId}\n${content}\n\`\`\``,
            run: false,
          });
        });
      }
    }),

    vscode.commands.registerCommand("yggdrasil.pickFile", async () => {
      const files = await vscode.workspace.findFiles("**/*", "**/node_modules/**", 50);
      const items = files.map((f) => vscode.workspace.asRelativePath(f));
      const picked = await vscode.window.showQuickPick(items, { placeHolder: "Attach file to chat" });
      if (picked) {
        const uri = vscode.workspace.workspaceFolders?.[0]
          ? vscode.Uri.joinPath(vscode.workspace.workspaceFolders[0].uri, picked)
          : vscode.Uri.file(picked);
        vscode.commands.executeCommand("yggdrasil.attachFile", uri);
      }
    }),

    vscode.commands.registerCommand("yggdrasil.previewEdit", async (filePath: string, proposed: string) => {
      if (!filePath || !proposed) return;
      const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
      if (!workspaceFolder) return;
      const fileUri = vscode.Uri.joinPath(workspaceFolder.uri, filePath);
      try {
        const we = new vscode.WorkspaceEdit();
        const doc = await vscode.workspace.openTextDocument(fileUri);
        we.replace(fileUri, new vscode.Range(0, 0, doc.lineCount, 0), proposed);
        const applied = await vscode.workspace.applyEdit(we);
        if (applied) {
          vscode.window.showInformationMessage(`Applied edit to ${filePath}`);
        }
      } catch (err) {
        vscode.window.showErrorMessage(`Preview edit failed: ${err instanceof Error ? err.message : String(err)}`);
      }
    }),

    // P4 — Voice toggle command
    vscode.commands.registerCommand("yggdrasil.voice.toggle", () => {
      ChatPanel.instance?.postMessage({ type: "voice.toggle" });
    }),

    // P3 — Active editor watcher: send current file context to chat panel
    vscode.window.onDidChangeActiveTextEditor((editor) => {
      const cfg = vscode.workspace.getConfiguration("yggdrasil.chat");
      if (!cfg.get<boolean>("autoInjectActiveEditor", true)) return;
      if (!editor) return;
      const ctx = getEditorContext();
      if (!ctx) return;
      const panel = ChatPanel.instance;
      if (panel) {
        panel.postCurrentEditor({
          filename: ctx.filename,
          language: ctx.language,
          uri: ctx.uri.toString(),
        });
      }
    }),

    // Disposables
    statusBar,
    eventWatcher,
    outputChannel,
    hookManager,
    autoUpdater
  );

  // P3b — Self-improvement session-init hook (after TreeDataProvider registration)
  const selfImprovement = new SelfImprovementChecker(context, odin);
  try {
    await selfImprovement.check((payload) => {
      ChatPanel.instance?.postMessage(payload as unknown as Record<string, unknown>);
    });
  } catch (err) {
    outputChannel.append(
      `Self-improvement check error: ${err instanceof Error ? err.message : String(err)}`
    );
  }

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
