/**
 * Chat Panel — Continue/Cline-style streaming chat over Odin.
 *
 * Responsibilities:
 *   - Own a WebviewPanel and serve the chat UI (media/chat.{css,js})
 *   - Persist threads via ChatHistory (globalState, OS-local)
 *   - Preprocess input through slash commands
 *   - Stream completions from Odin's /v1/chat/completions (SSE)
 *   - Inject attachments (editor selection, file content) as context
 *   - Expose public helpers for code actions to seed a new turn
 */

import * as vscode from "vscode";
import { OdinClient, ChatMessage, SwarmEvent } from "../api/odinClient";
import { ChatHistory, ChatMsg, ChatThread } from "../chat/history";
import { preprocess } from "../chat/slashCommands";
import { ChatSeed, getSelectionContext } from "../chat/codeActions";

export class ChatPanel {
  private static instance: ChatPanel | undefined;
  private static readonly viewType = "yggdrasil.chatPanel";

  private panel: vscode.WebviewPanel;
  private thread: ChatThread;
  private abortCurrent: (() => void) | null = null;

  private constructor(
    private context: vscode.ExtensionContext,
    private odin: OdinClient,
    private history: ChatHistory
  ) {
    const mediaRoot = vscode.Uri.joinPath(context.extensionUri, "media");
    this.panel = vscode.window.createWebviewPanel(
      ChatPanel.viewType,
      "Yggdrasil Chat",
      vscode.ViewColumn.Beside,
      { enableScripts: true, retainContextWhenHidden: true, localResourceRoots: [mediaRoot] }
    );
    this.panel.webview.html = this.getHtml();

    // Pick the most recently used thread, or create a new one
    const existing = history.listThreads();
    this.thread =
      (existing[0] && history.getThread(existing[0].id)) ?? history.createThread();

    this.panel.onDidDispose(() => {
      this.abortCurrent?.();
      this.configSub?.dispose();
      ChatPanel.instance = undefined;
    });

    this.panel.webview.onDidReceiveMessage((m) => this.handleMessage(m));

    this.configSub = vscode.workspace.onDidChangeConfiguration((e) => {
      if (
        e.affectsConfiguration("yggdrasil.chat.theme") ||
        e.affectsConfiguration("yggdrasil.chat.crtEffects") ||
        e.affectsConfiguration("yggdrasil.chat.font")
      ) {
        const t = this.readChatTheme();
        this.panel.webview.postMessage({ type: "themeChange", ...t });
      }
    });
  }

  private configSub: vscode.Disposable | undefined;

  private readChatTheme(): { theme: string; crtEffects: boolean; font: string } {
    const cfg = vscode.workspace.getConfiguration("yggdrasil.chat");
    return {
      theme: cfg.get<string>("theme", "classic"),
      crtEffects: cfg.get<boolean>("crtEffects", false),
      font: cfg.get<string>("font", "system"),
    };
  }

  static show(context: vscode.ExtensionContext, odin: OdinClient, history: ChatHistory, seed?: ChatSeed): ChatPanel {
    if (ChatPanel.instance) {
      ChatPanel.instance.panel.reveal(vscode.ViewColumn.Beside);
      if (seed) ChatPanel.instance.applySeed(seed);
      return ChatPanel.instance;
    }
    ChatPanel.instance = new ChatPanel(context, odin, history);
    if (seed) {
      // Seed applies after the webview signals "ready"
      ChatPanel.instance.pendingSeed = seed;
    }
    return ChatPanel.instance;
  }

  private pendingSeed: ChatSeed | undefined;

  private applySeed(seed: ChatSeed): void {
    this.panel.webview.postMessage({ type: "seed", seed });
  }

  private async handleMessage(msg: { type: string } & Record<string, unknown>): Promise<void> {
    try {
      switch (msg.type) {
        case "ready":
          await this.pushState();
          this.pushMessages();
          if (this.pendingSeed) {
            this.applySeed(this.pendingSeed);
            this.pendingSeed = undefined;
          }
          return;

        case "send":
          await this.handleSend(msg);
          return;

        case "stop":
          this.abortCurrent?.();
          return;

        case "newThread":
          this.thread = this.history.createThread();
          await this.pushState();
          this.pushMessages();
          return;

        case "switchThread": {
          const id = String(msg.id ?? "");
          const t = this.history.getThread(id);
          if (t) {
            this.thread = t;
            await this.pushState();
            this.pushMessages();
          }
          return;
        }

        case "clearThread":
          this.history.clearThread(this.thread.id);
          const refreshed = this.history.getThread(this.thread.id);
          if (refreshed) this.thread = refreshed;
          await this.pushState();
          this.pushMessages();
          return;

        case "deleteThread": {
          this.history.deleteThread(this.thread.id);
          const list = this.history.listThreads();
          this.thread = list[0] ? this.history.getThread(list[0].id)! : this.history.createThread();
          await this.pushState();
          this.pushMessages();
          return;
        }

        case "copy":
          await vscode.env.clipboard.writeText(String(msg.text ?? ""));
          return;

        case "attachFile": {
          const editor = vscode.window.activeTextEditor;
          if (!editor) {
            this.panel.webview.postMessage({ type: "notice", text: "Open a file first." });
            return;
          }
          const fname = editor.document.fileName.split(/[\\/]/).pop() ?? "untitled";
          const content = editor.document.getText();
          this.panel.webview.postMessage({
            type: "attachment",
            attachment: {
              kind: "file",
              label: fname,
              content: `File: ${fname}\n\`\`\`${editor.document.languageId}\n${content}\n\`\`\``,
            },
          });
          return;
        }

        case "attachSelection": {
          const ctx = getSelectionContext();
          if (!ctx) {
            this.panel.webview.postMessage({ type: "notice", text: "Select some code first." });
            return;
          }
          this.panel.webview.postMessage({
            type: "attachment",
            attachment: {
              kind: "selection",
              label: `${ctx.filename} selection`,
              content: `File: ${ctx.filename}\n\`\`\`${ctx.language}\n${ctx.text}\n\`\`\``,
            },
          });
          return;
        }
      }
    } catch (err) {
      const m = err instanceof Error ? err.message : String(err);
      this.panel.webview.postMessage({ type: "streamError", error: m });
    }
  }

  private async handleSend(msg: Record<string, unknown>): Promise<void> {
    const rawText = String(msg.text ?? "");
    const modelOverride = typeof msg.model === "string" && msg.model ? (msg.model as string) : undefined;
    const flowOverride = typeof msg.flow === "string" && msg.flow ? (msg.flow as string) : undefined;
    const rawAttachments = Array.isArray(msg.attachments) ? (msg.attachments as { content: string }[]) : [];

    // Preprocess slash commands
    const slash = await preprocess(rawText, this.odin);

    if (slash.notice) {
      this.panel.webview.postMessage({ type: "notice", text: slash.notice });
    }
    if (slash.cleanedText === "" && !slash.flowOverride && !slash.modelOverride) {
      // Pure info command (/help) — no completion request
      return;
    }

    const effectiveModel = slash.modelOverride ?? modelOverride ?? (await this.defaultModel());
    const effectiveFlow = slash.flowOverride ?? flowOverride;

    // Compose user content with attachments + memory prefix
    const attachmentText = rawAttachments
      .map((a) => a.content)
      .filter(Boolean)
      .join("\n\n");
    const systemPrefix = slash.systemContextPrefix ?? "";
    const userContent = [attachmentText, slash.cleanedText].filter(Boolean).join("\n\n");

    if (!effectiveModel) {
      this.panel.webview.postMessage({
        type: "streamError",
        error: "No model available. Configure Odin URL and ensure models are loaded.",
      });
      return;
    }

    // Persist user turn
    const userMsg: ChatMsg = {
      role: "user",
      content: userContent,
      ts: Date.now(),
      model: effectiveModel,
      flow: effectiveFlow,
    };
    const updated = this.history.appendMessage(this.thread.id, userMsg);
    if (updated) this.thread = updated;
    this.pushMessages();
    await this.pushState(); // thread title may have changed

    // Build message list for Odin (include prior history + optional memory prefix)
    const messages: ChatMessage[] = [];
    if (systemPrefix) {
      messages.push({ role: "system", content: systemPrefix });
    }
    for (const m of this.thread.messages) {
      if (m.role === "system" || m.role === "user" || m.role === "assistant") {
        messages.push({ role: m.role, content: m.content });
      }
    }

    // Inject empty assistant placeholder we'll fill as tokens arrive
    const assistantPlaceholder: ChatMsg = {
      role: "assistant",
      content: "",
      ts: Date.now(),
      model: effectiveModel,
      flow: effectiveFlow,
    };
    const afterPlaceholder = this.history.appendMessage(this.thread.id, assistantPlaceholder);
    if (afterPlaceholder) this.thread = afterPlaceholder;

    this.panel.webview.postMessage({
      type: "streamStart",
      model: effectiveModel,
      flow: effectiveFlow,
    });

    // Abort wiring — `stop` message from webview sets aborted
    let aborted = false;
    this.abortCurrent = () => {
      aborted = true;
    };

    try {
      const full = await this.odin.streamChat(
        {
          model: effectiveModel,
          messages,
          temperature: 0.3,
          max_tokens: 4096,
          stream: true,
        },
        (delta) => {
          if (aborted) return;
          this.panel.webview.postMessage({ type: "streamDelta", delta });
        },
        undefined,
        (swarmEvent: SwarmEvent) => {
          if (aborted) return;
          this.panel.webview.postMessage({ type: "swarmEvent", event: swarmEvent });
        }
      );
      if (!aborted) {
        this.history.replaceLastAssistant(this.thread.id, full);
        this.panel.webview.postMessage({
          type: "streamEnd",
          model: effectiveModel,
          flow: effectiveFlow,
        });
      } else {
        this.history.replaceLastAssistant(this.thread.id, full + "\n[stopped]");
        this.panel.webview.postMessage({
          type: "streamEnd",
          model: effectiveModel,
          failed: false,
        });
      }
    } catch (err) {
      const m = err instanceof Error ? err.message : String(err);
      this.panel.webview.postMessage({ type: "streamError", error: m });
    } finally {
      this.abortCurrent = null;
    }
  }

  private async defaultModel(): Promise<string | undefined> {
    const models = await this.odin.listModels();
    const loaded = models.find((m) => m.loaded);
    return (loaded ?? models[0])?.id;
  }

  private async pushState(): Promise<void> {
    const [models, flows] = await Promise.all([
      this.odin.listModels(),
      this.odin.listFlows(),
    ]);
    this.panel.webview.postMessage({
      type: "state",
      state: {
        threads: this.history.listThreads(),
        currentThreadId: this.thread.id,
        models,
        flows: flows.map((f) => ({ name: f.name })),
        defaultModel: models.find((m) => m.loaded)?.id ?? models[0]?.id ?? null,
      },
    });
  }

  private pushMessages(): void {
    this.panel.webview.postMessage({
      type: "messages",
      messages: this.thread.messages,
    });
  }

  private getHtml(): string {
    const mediaRoot = vscode.Uri.joinPath(this.context.extensionUri, "media");
    const cssUri = this.panel.webview.asWebviewUri(vscode.Uri.joinPath(mediaRoot, "chat.css"));
    const jsUri = this.panel.webview.asWebviewUri(vscode.Uri.joinPath(mediaRoot, "chat.js"));
    const themePipboyUri = this.panel.webview.asWebviewUri(
      vscode.Uri.joinPath(mediaRoot, "themes", "pipboy-green.css")
    );
    const themeBbsUri = this.panel.webview.asWebviewUri(
      vscode.Uri.joinPath(mediaRoot, "themes", "bbs-cyan.css")
    );
    const crtUri = this.panel.webview.asWebviewUri(
      vscode.Uri.joinPath(mediaRoot, "themes", "crt-effects.css")
    );
    const retroTypographyUri = this.panel.webview.asWebviewUri(
      vscode.Uri.joinPath(mediaRoot, "themes", "retro-typography.css")
    );
    const nonce = getNonce();
    const csp = [
      `default-src 'none'`,
      `img-src ${this.panel.webview.cspSource} data:`,
      `style-src ${this.panel.webview.cspSource} 'unsafe-inline'`,
      `font-src ${this.panel.webview.cspSource}`,
      `script-src 'nonce-${nonce}'`,
    ].join("; ");

    const { theme, crtEffects, font } = this.readChatTheme();
    const crtOverlayHtml = crtEffects ? `<div class="crt-overlay" id="crt-overlay"></div>` : "";

    return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="${csp}">
<title>Yggdrasil Chat</title>
<link rel="stylesheet" href="${cssUri}">
<link rel="stylesheet" href="${crtUri}">
<link rel="stylesheet" href="${themePipboyUri}">
<link rel="stylesheet" href="${themeBbsUri}">
<link rel="stylesheet" href="${retroTypographyUri}">
</head>
<body data-theme="${theme}" data-font="${font}" data-crt="${crtEffects ? "on" : "off"}">

${crtOverlayHtml}

<div class="header">
  <div class="thread-picker">
    <select id="thread-select"></select>
  </div>
  <div class="right">
    <select id="flow-select" title="Pin a flow"></select>
    <select id="model-select" title="Model"></select>
    <button class="icon-btn" id="clear-thread" title="Clear thread">⊘</button>
    <button class="icon-btn" id="delete-thread" title="Delete thread">✕</button>
  </div>
</div>

<div class="messages" id="messages"></div>

<div class="input-area">
  <div class="error-banner" id="error-banner"></div>
  <div class="notice-banner" id="notice-banner"></div>
  <div class="attachment-chips" id="chips"></div>
  <div class="input-wrap">
    <textarea id="input" placeholder="Ask Yggdrasil… (Enter to send, Shift+Enter for newline, / for commands)" rows="1"></textarea>
    <div class="input-bar">
      <button class="btn" id="attach-selection" title="Attach editor selection">+ selection</button>
      <button class="btn" id="attach-file" title="Attach current file">+ file</button>
      <span class="spacer"></span>
      <span class="hint">Enter to send</span>
      <button class="btn primary" id="send">Send</button>
      <button class="btn" id="stop" style="display:none;">Stop</button>
    </div>
  </div>
</div>

<script nonce="${nonce}" src="${jsUri}"></script>

</body>
</html>`;
  }
}

function getNonce(): string {
  let text = "";
  const possible = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  for (let i = 0; i < 32; i++) {
    text += possible.charAt(Math.floor(Math.random() * possible.length));
  }
  return text;
}
