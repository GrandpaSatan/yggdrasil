/**
 * Chat Panel — Fergus persona, React + Vite + Tailwind webview.
 *
 * Responsibilities:
 *   - Own a WebviewPanel and serve the Fergus chat UI
 *     (built bundle at `dist/chat-react/assets/chat.<hash>.js`)
 *   - Persist threads via ChatHistory (globalState, OS-local)
 *   - Preprocess input through slash commands (flows, /memory, /clear, /help)
 *   - Stream completions from Odin's /v1/chat/completions (SSE)
 *   - Inject attachments (editor selection, file content) as context
 *   - Expose public helpers for code actions to seed a new turn
 */

import * as vscode from "vscode";
import * as fs from "node:fs";
import * as path from "node:path";
import { OdinClient, ChatMessage, SwarmEvent } from "../api/odinClient";
import { ChatHistory, ChatMsg, ChatThread } from "../chat/history";
import { preprocess } from "../chat/slashCommands";
import { ChatSeed, getSelectionContext } from "../chat/codeActions";
import { ThreadStore } from "../threads/threadStore";
import { resolveFergusPrompt } from "../chat/fergus";

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
    const bundleRoot = vscode.Uri.joinPath(context.extensionUri, "dist", "chat-react");
    this.panel = vscode.window.createWebviewPanel(
      ChatPanel.viewType,
      "Yggdrasil Chat",
      vscode.ViewColumn.Beside,
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [mediaRoot, bundleRoot],
      },
    );
    this.panel.webview.html = this.getHtml();

    // Pick the most recently used thread, or create a new one
    const existing = history.listThreads();
    this.thread =
      (existing[0] && history.getThread(existing[0].id)) ?? history.createThread();

    this.panel.onDidDispose(() => {
      this.abortCurrent?.();
      ChatPanel.instance = undefined;
    });

    this.panel.webview.onDidReceiveMessage((m) => this.handleMessage(m));
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
    const rawAttachments = Array.isArray(msg.attachments) ? (msg.attachments as { content: string }[]) : [];

    // Sprint 068 Phase 3: fetch the live flow registry so `preprocess` can
    // dispatch `/flow_name` slashes and the UI's SlashMenu contract is
    // enforced server-side of the webview. A lookup failure falls back to
    // no flows known — unknown slashes pass through to Odin, which would
    // reject with a 400 only for genuinely wrong names.
    let knownFlows: Array<{ name: string; trigger?: unknown }> = [];
    try {
      const flows = await this.odin.listFlows();
      knownFlows = flows.map((f) => ({ name: f.name, trigger: f.trigger }));
    } catch {
      // Leave knownFlows empty; preprocess passes the raw slash through.
    }

    const slash = await preprocess(rawText, this.odin, knownFlows);

    if (slash.notice) {
      this.panel.webview.postMessage({ type: "notice", text: slash.notice });
    }
    if (slash.cleanedText === "" && !slash.flowOverride) {
      // Pure info command (/help, /clear directive, empty arg to /memory) —
      // no completion request.
      return;
    }

    const effectiveFlow = slash.flowOverride;

    // Compose user content with attachments + memory prefix
    const attachmentText = rawAttachments
      .map((a) => a.content)
      .filter(Boolean)
      .join("\n\n");
    const memoryPrefix = slash.systemContextPrefix ?? "";
    const userContent = [attachmentText, slash.cleanedText].filter(Boolean).join("\n\n");

    // Persist user turn
    const userMsg: ChatMsg = {
      role: "user",
      content: userContent,
      ts: Date.now(),
      flow: effectiveFlow,
    };
    const updated = this.history.appendMessage(this.thread.id, userMsg);
    if (updated) this.thread = updated;
    this.pushMessages();
    await this.pushState(); // thread title may have changed

    // Build message list for Odin:
    //   [0] Fergus persona (unless /memory injected its own context prefix, in
    //       which case Fergus prepends the memory block).
    //   [1..] full prior thread history (system / user / assistant).
    const fergusPrompt = resolveFergusPrompt();
    const systemContent = memoryPrefix
      ? `${fergusPrompt}\n\n${memoryPrefix}`
      : fergusPrompt;
    const messages: ChatMessage[] = [{ role: "system", content: systemContent }];
    for (const m of this.thread.messages) {
      if (m.role === "user" || m.role === "assistant") {
        messages.push({ role: m.role, content: m.content });
      }
      // Historical system messages are intentionally dropped — Fergus is
      // re-injected fresh each turn. Stored system turns from pre-068
      // threads are therefore inert (they don't override Fergus).
    }

    // Inject empty assistant placeholder we'll fill as tokens arrive
    const assistantPlaceholder: ChatMsg = {
      role: "assistant",
      content: "",
      ts: Date.now(),
      flow: effectiveFlow,
    };
    const afterPlaceholder = this.history.appendMessage(this.thread.id, assistantPlaceholder);
    if (afterPlaceholder) this.thread = afterPlaceholder;

    this.panel.webview.postMessage({
      type: "streamStart",
      flow: effectiveFlow,
    });

    // Abort wiring — `stop` message from webview sets aborted
    let aborted = false;
    this.abortCurrent = () => {
      aborted = true;
    };

    try {
      // Sprint 068 Phase 3: `model` intentionally omitted. Odin's intent
      // router picks the backend; Fergus is one persona, no client-side
      // selection. `streamChat()` strips the undefined key from the body
      // (odinClient.ts), so the outbound JSON has no `"model"` field at all.
      const full = await this.odin.streamChat(
        {
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
        },
      );
      if (!aborted) {
        this.history.replaceLastAssistant(this.thread.id, full);
        this.panel.webview.postMessage({
          type: "streamEnd",
          flow: effectiveFlow,
        });
      } else {
        this.history.replaceLastAssistant(this.thread.id, full + "\n[stopped]");
        this.panel.webview.postMessage({
          type: "streamEnd",
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

  private async pushState(): Promise<void> {
    // Sprint 068 Phase 3: no `models` or `defaultModel` in the state payload.
    // Fergus doesn't let users pick; the SlashMenu needs `flows` (with trigger
    // metadata so it can filter cron-only) and the header needs `threads`.
    let flows: Array<{ name: string; description?: string; trigger?: unknown }> = [];
    try {
      const raw = await this.odin.listFlows();
      flows = raw.map((f) => ({
        name: f.name,
        trigger: f.trigger,
        // Use the flow's `_comment` if any as a lightweight description.
        description:
          (f as unknown as { _comment?: string })._comment ??
          (f.steps?.[0]?.system_prompt?.split("\n")[0] ?? undefined),
      }));
    } catch {
      // Odin unreachable — ship empty flows; SlashMenu falls back to
      // builtins only.
    }
    this.panel.webview.postMessage({
      type: "state",
      state: {
        threads: this.history.listThreads(),
        currentThreadId: this.thread.id,
        flows,
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
    const extUri = this.context.extensionUri;
    const mediaRoot = vscode.Uri.joinPath(extUri, "media");
    const bundleRoot = vscode.Uri.joinPath(extUri, "dist", "chat-react");

    // Resolve the hashed chat bundle filenames via Vite's manifest.
    // Fallback: glob-scan the assets dir if the manifest file is missing
    // (e.g. first-run dev builds before the manifest is emitted).
    const bundle = resolveChatBundle(bundleRoot.fsPath);
    const chatJsUri = this.panel.webview.asWebviewUri(
      vscode.Uri.joinPath(bundleRoot, bundle.js),
    );
    const chatCssUri = bundle.css
      ? this.panel.webview.asWebviewUri(vscode.Uri.joinPath(bundleRoot, bundle.css))
      : undefined;

    // Voice push-to-talk (opt-in) — kept verbatim from Sprint 062.
    // The React chat mounts `voice-client.js` from a useEffect by reading
    // `data-voice-*` attributes off <body>.
    const voiceCfg = vscode.workspace.getConfiguration("yggdrasil.voice");
    const voiceEnabled = voiceCfg.get<boolean>("enabled", false);
    const ttsEnabled = voiceCfg.get<boolean>("ttsPlayback", true);
    const voiceWorkletUri = this.panel.webview.asWebviewUri(
      vscode.Uri.joinPath(mediaRoot, "voice-worklet.js"),
    );
    const voiceClientUri = this.panel.webview.asWebviewUri(
      vscode.Uri.joinPath(mediaRoot, "voice-client.js"),
    );
    const odinUrl = vscode.workspace
      .getConfiguration("yggdrasil")
      .get<string>("odinUrl", "http://localhost:8080");

    const nonce = getNonce();
    const csp = [
      `default-src 'none'`,
      `img-src ${this.panel.webview.cspSource} data:`,
      // Tailwind's production build inlines `@layer` rules in a single CSS
      // asset loaded via <link>. 'unsafe-inline' is retained for now to tolerate
      // any component-injected <style> tags (e.g. TipTap). Tightening is
      // tracked in Sprint 068 Risk #1.
      `style-src ${this.panel.webview.cspSource} 'unsafe-inline'`,
      `font-src ${this.panel.webview.cspSource}`,
      `script-src 'nonce-${nonce}'`,
      `connect-src ws: wss: http: https:`,
    ].join("; ");

    const cssTag = chatCssUri ? `<link rel="stylesheet" href="${chatCssUri}">` : "";

    return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="${csp}">
<title>Yggdrasil Chat</title>
${cssTag}
</head>
<body data-odin-url="${odinUrl}" data-voice-enabled="${voiceEnabled}" data-tts-enabled="${ttsEnabled}" data-voice-worklet-uri="${voiceWorkletUri}" data-voice-client-uri="${voiceClientUri}">
<div id="root"></div>
<script nonce="${nonce}" type="module" src="${chatJsUri}"></script>
</body>
</html>`;
  }
}

/**
 * Resolve the hashed chat bundle from Vite's manifest. Falls back to a glob
 * scan of `assets/chat.*.js` so the webview still loads when the manifest
 * is absent (partial build). Throws if neither the manifest nor the assets
 * directory exists — callers run `npm run build:webview` to fix.
 */
function resolveChatBundle(bundleRoot: string): { js: string; css?: string } {
  const manifestPath = path.join(bundleRoot, ".vite", "manifest.json");
  try {
    const raw = fs.readFileSync(manifestPath, "utf-8");
    const manifest = JSON.parse(raw) as Record<string, { file: string; css?: string[] }>;
    // Vite keys the manifest by the entry-chunk source path.
    const entry =
      manifest["src/main.tsx"] ??
      Object.values(manifest).find((e) => e.file?.startsWith("assets/chat."));
    if (entry?.file) {
      const css = entry.css?.find((c) => c.startsWith("assets/chat."));
      return { js: entry.file, css };
    }
  } catch {
    // Manifest missing or malformed — fall through to glob scan.
  }

  const assetsDir = path.join(bundleRoot, "assets");
  if (!fs.existsSync(assetsDir)) {
    throw new Error(
      `Chat bundle not found at ${assetsDir}. Run \`npm run build:webview\` in the extension root.`,
    );
  }
  const files = fs.readdirSync(assetsDir);
  const js = files.find((f) => /^chat\..*\.js$/.test(f) && !f.endsWith(".chunk.js"));
  const css = files.find((f) => /^chat\..*\.css$/.test(f));
  if (!js) {
    throw new Error(`Chat bundle JS missing in ${assetsDir}. Expected assets/chat.<hash>.js`);
  }
  return {
    js: path.join("assets", js),
    css: css ? path.join("assets", css) : undefined,
  };
}

function getNonce(): string {
  let text = "";
  const possible = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  for (let i = 0; i < 32; i++) {
    text += possible.charAt(Math.floor(Math.random() * possible.length));
  }
  return text;
}
