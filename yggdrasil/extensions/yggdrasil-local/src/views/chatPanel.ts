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
import { ThreadStore } from "../threads/threadStore";

export class ChatPanel {
  static instance: ChatPanel | undefined;
  private static readonly viewType = "yggdrasil.chatPanel";

  private panel: vscode.WebviewPanel;
  private thread: ChatThread;
  private abortCurrent: (() => void) | null = null;

  private threadStore: ThreadStore;

  private constructor(
    private context: vscode.ExtensionContext,
    private odin: OdinClient,
    private history: ChatHistory
  ) {
    this.threadStore = new ThreadStore(context);
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

        // P2b — ThreadStore bridge
        case "requestThreads": {
          const threads = await this.threadStore.list();
          this.panel.webview.postMessage({ type: "threadList", threads });
          return;
        }

        case "loadThread": {
          const id = String(msg.id ?? "");
          const stored = await this.threadStore.load(id);
          if (stored) {
            this.panel.webview.postMessage({ type: "threadData", thread: stored });
          }
          return;
        }

        case "renameThread": {
          const id = String(msg.id ?? "");
          const title = String(msg.title ?? "");
          await this.threadStore.rename(id, title);
          return;
        }

        case "searchThreads": {
          const q = String(msg.query ?? "");
          const results = await this.threadStore.search(q);
          this.panel.webview.postMessage({ type: "threadSearch", results });
          return;
        }

        case "exportThread": {
          const id = String(msg.id ?? "");
          const md = await this.threadStore.exportAsMarkdown(id);
          const uri = await vscode.window.showSaveDialog({
            defaultUri: vscode.Uri.file(`thread-${id.slice(0, 8)}.md`),
            filters: { Markdown: ["md"] },
          });
          if (uri && md) {
            await vscode.workspace.fs.writeFile(uri, new TextEncoder().encode(md));
            vscode.window.showInformationMessage("Thread exported.");
          }
          return;
        }

        // P3 — file picker (requestFilePicker from chat.js)
        case "requestFilePicker": {
          const files = await vscode.workspace.findFiles("**/*", "**/node_modules/**", 50);
          const items = files.map((f) => ({
            label: vscode.workspace.asRelativePath(f),
            uri: f.toString(),
          }));
          const picked = await vscode.window.showQuickPick(items.map((i) => i.label), {
            placeHolder: "Select file to attach",
          });
          if (picked) {
            const item = items.find((i) => i.label === picked);
            if (item) {
              const fileUri = vscode.Uri.parse(item.uri);
              const content = new TextDecoder().decode(
                await vscode.workspace.fs.readFile(fileUri)
              );
              const langId = picked.split(".").pop() ?? "text";
              this.panel.webview.postMessage({
                type: "filePicked",
                label: picked,
                path: picked,
                content: `File: ${picked}\n\`\`\`${langId}\n${content}\n\`\`\``,
              });
            }
          }
          return;
        }

        // P3 — preview diff (apply-diff button from P2a)
        case "previewDiff": {
          const filePath = String(msg.path ?? "");
          const proposed = String(msg.proposed ?? "");
          if (!filePath || !proposed) return;
          const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
          if (!workspaceFolder) return;
          const fileUri = vscode.Uri.joinPath(workspaceFolder.uri, filePath);
          const we = new vscode.WorkspaceEdit();
          const doc = await vscode.workspace.openTextDocument(fileUri);
          we.replace(fileUri, new vscode.Range(0, 0, doc.lineCount, 0), proposed);
          const applied = await vscode.workspace.applyEdit(we);
          if (applied) {
            vscode.window.showInformationMessage(`Applied diff to ${filePath}`);
          } else {
            vscode.window.showErrorMessage(`Failed to apply diff to ${filePath}`);
          }
          return;
        }

        // P3b — notification card actions
        case "notifView":
          this.panel.webview.postMessage({ type: "notice", text: "Loading self-improvement suggestions..." });
          return;

        case "notifSnooze":
          await this.context.globalState.update(
            "selfImprovement.snoozedUntil",
            Date.now() + 7 * 86_400_000
          );
          return;

        case "notifDismiss":
          await this.context.globalState.update("selfImprovement.dismissed", true);
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
          flow: effectiveFlow,
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

  /** Post an arbitrary message to the webview — used by extension.ts for P3/P3b. */
  postMessage(msg: Record<string, unknown>): void {
    this.panel.webview.postMessage(msg);
  }

  /** Send active editor metadata to webview for context awareness. */
  postCurrentEditor(info: { filename: string; language: string; uri: string }): void {
    this.panel.webview.postMessage({ type: "activeEditor", ...info });
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

    // P4 — Voice push-to-talk (opt-in)
    const voiceCfg = vscode.workspace.getConfiguration("yggdrasil.voice");
    const voiceEnabled = voiceCfg.get<boolean>("enabled", false);
    const ttsEnabled = voiceCfg.get<boolean>("ttsPlayback", true);
    const voiceWorkletUri = this.panel.webview.asWebviewUri(
      vscode.Uri.joinPath(mediaRoot, "voice-worklet.js")
    );
    const voiceClientUri = this.panel.webview.asWebviewUri(
      vscode.Uri.joinPath(mediaRoot, "voice-client.js")
    );
    const odinUrl = vscode.workspace.getConfiguration("yggdrasil").get<string>("odinUrl", "http://localhost:8080");

    // P2a — Prism syntax highlighting (vendored, no CDN)
    const vendorRoot = vscode.Uri.joinPath(mediaRoot, "vendor");
    const langRoot = vscode.Uri.joinPath(vendorRoot, "prism-languages");
    const prismCoreUri = this.panel.webview.asWebviewUri(vscode.Uri.joinPath(vendorRoot, "prism.js"));
    const prismLangUris: Record<string, vscode.Uri> = {};
    for (const lang of ["clike", "javascript", "typescript", "rust", "go", "python", "json", "toml", "yaml", "bash", "sql", "markdown"]) {
      prismLangUris[lang] = this.panel.webview.asWebviewUri(vscode.Uri.joinPath(langRoot, `${lang}.js`));
    }
    const hlClassicUri = this.panel.webview.asWebviewUri(vscode.Uri.joinPath(mediaRoot, "themes", "highlight-classic.css"));
    const hlPipboyUri  = this.panel.webview.asWebviewUri(vscode.Uri.joinPath(mediaRoot, "themes", "highlight-pipboy.css"));
    const hlBbsUri     = this.panel.webview.asWebviewUri(vscode.Uri.joinPath(mediaRoot, "themes", "highlight-bbs.css"));
    const nonce = getNonce();
    const csp = [
      `default-src 'none'`,
      `img-src ${this.panel.webview.cspSource} data:`,
      `style-src ${this.panel.webview.cspSource} 'unsafe-inline'`,
      `font-src ${this.panel.webview.cspSource}`,
      `script-src 'nonce-${nonce}'`,
      `connect-src ws: wss: http: https:`,
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
<link rel="stylesheet" href="${hlClassicUri}">
<link rel="stylesheet" href="${hlPipboyUri}">
<link rel="stylesheet" href="${hlBbsUri}">
</head>
<body data-theme="${theme}" data-font="${font}" data-crt="${crtEffects ? "on" : "off"}" data-prism-langs='${JSON.stringify(Object.fromEntries(Object.entries(prismLangUris).map(([k,v])=>[k,v.toString()])))}' data-odin-url="${odinUrl}" data-voice-enabled="${voiceEnabled}" data-tt-enabled="${ttsEnabled}" data-voice-worklet-uri="${voiceWorkletUri}">
<script nonce="${nonce}" src="${prismCoreUri}"></script>
<script nonce="${nonce}" src="${prismLangUris["clike"]}"></script>
<script nonce="${nonce}" src="${prismLangUris["javascript"]}"></script>
<script nonce="${nonce}" src="${prismLangUris["typescript"]}"></script>
<script nonce="${nonce}" src="${prismLangUris["rust"]}"></script>
<script nonce="${nonce}" src="${prismLangUris["go"]}"></script>
<script nonce="${nonce}" src="${prismLangUris["python"]}"></script>
<script nonce="${nonce}" src="${prismLangUris["json"]}"></script>
<script nonce="${nonce}" src="${prismLangUris["toml"]}"></script>
<script nonce="${nonce}" src="${prismLangUris["yaml"]}"></script>
<script nonce="${nonce}" src="${prismLangUris["bash"]}"></script>
<script nonce="${nonce}" src="${prismLangUris["sql"]}"></script>
<script nonce="${nonce}" src="${prismLangUris["markdown"]}"></script>

${crtOverlayHtml}

<div class="titlebar">
  <div class="titlebar-left">
    <select id="thread-select" title="Switch thread"></select>
    <button class="icon-btn" id="new-thread" title="New thread">+</button>
  </div>
  <div class="titlebar-center">
    <span class="titlebar-brand">YGG</span>
  </div>
  <div class="titlebar-right">
    <select id="flow-select" title="Pin a flow"></select>
    <select id="model-select" title="Model"></select>
    <button class="icon-btn" id="clear-thread" title="Clear thread">&#8856;</button>
    <button class="icon-btn" id="delete-thread" title="Delete thread">&#10005;</button>
    <button class="icon-btn" id="mic-btn" title="Push to talk (voice disabled)" style="display:none;" aria-label="Push to talk">&#9679;</button>
  </div>
</div>

<div class="notification-card" id="notification-card" style="display:none;" role="alert" aria-live="polite">
  <span class="notification-card-text" id="notification-card-text"></span>
  <div class="notification-card-actions">
    <button class="btn" id="notif-view">View</button>
    <button class="btn" id="notif-snooze">Snooze 7d</button>
    <button class="btn" id="notif-dismiss">Dismiss</button>
  </div>
</div>

<div class="messages" id="messages" role="log" aria-label="Chat messages" aria-live="polite"></div>

<div class="input-area">
  <div class="slash-menu" id="slash-menu" style="display:none;" role="listbox" aria-label="Commands"></div>
  <div class="error-banner" id="error-banner" role="alert"></div>
  <div class="notice-banner" id="notice-banner" role="status"></div>
  <div class="attachment-chips" id="chips" aria-label="Attachments"></div>
  <div class="input-wrap">
    <textarea id="input" placeholder="Ask Yggdrasil\u2026 (Enter=send, Shift+Enter=newline, /=commands, @=attach file)" rows="1" aria-label="Chat input" aria-multiline="true"></textarea>
    <div class="input-bar">
      <button class="btn" id="attach-selection" title="Attach editor selection" aria-label="Attach editor selection">+sel</button>
      <button class="btn" id="attach-file" title="Attach current file" aria-label="Attach current file">+file</button>
      <span class="spacer"></span>
      <span class="hint">Enter&#8629;</span>
      <button class="btn primary" id="send" aria-label="Send message">Send</button>
      <button class="btn danger" id="stop" style="display:none;" aria-label="Stop generation">Stop</button>
    </div>
  </div>
</div>

<div class="statusline" id="statusline" aria-live="polite">
  <span class="statusline-mode" id="statusline-mode">IDLE</span>
  <span class="statusline-sep">|</span>
  <span class="statusline-model" id="statusline-model">-</span>
  <span class="statusline-sep">|</span>
  <span class="statusline-thread" id="statusline-thread">-</span>
</div>

<script nonce="${nonce}" src="${jsUri}"></script>
${voiceEnabled ? `<script nonce="${nonce}" src="${voiceClientUri}"></script>` : ""}

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
