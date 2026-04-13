/**
 * Code actions — editor context menu entries for Yggdrasil chat.
 *
 * Commands registered:
 *   yggdrasil.chat.explainSelection  — opens chat with pre-filled explain prompt
 *   yggdrasil.chat.askAboutFile      — opens chat with current-file context
 *   yggdrasil.chat.editWithModel     — asks the model to rewrite the selection
 *
 * These commands open the ChatPanel (if not already open), seed a new
 * user turn with the selection or file content as context, and focus
 * the input so the user can complete the prompt.
 */

import * as vscode from "vscode";

export interface ChatSeed {
  userText: string;
  contextBlock?: string;
  flowHint?: string;
  run?: boolean;
}

export function getSelectionContext(): { text: string; language: string; filename: string } | null {
  const editor = vscode.window.activeTextEditor;
  if (!editor) return null;
  const sel = editor.selection;
  const text = sel.isEmpty ? editor.document.getText() : editor.document.getText(sel);
  if (!text.trim()) return null;
  return {
    text,
    language: editor.document.languageId,
    filename: editor.document.fileName.split(/[\\/]/).pop() ?? "untitled",
  };
}

export function registerCodeActions(
  context: vscode.ExtensionContext,
  openChat: (seed: ChatSeed) => void
): void {
  context.subscriptions.push(
    vscode.commands.registerCommand("yggdrasil.chat.explainSelection", () => {
      const ctx = getSelectionContext();
      if (!ctx) {
        vscode.window.showInformationMessage("Select code or open a file first.");
        return;
      }
      openChat({
        userText: `Explain what this ${ctx.language} code does and point out anything non-obvious.`,
        contextBlock: `File: ${ctx.filename}\n\`\`\`${ctx.language}\n${ctx.text}\n\`\`\``,
        run: true,
      });
    }),

    vscode.commands.registerCommand("yggdrasil.chat.askAboutFile", () => {
      const ctx = getSelectionContext();
      if (!ctx) {
        vscode.window.showInformationMessage("Open a file first.");
        return;
      }
      openChat({
        userText: "",
        contextBlock: `File: ${ctx.filename}\n\`\`\`${ctx.language}\n${ctx.text}\n\`\`\``,
        run: false,
      });
    }),

    vscode.commands.registerCommand("yggdrasil.chat.editWithModel", async () => {
      const ctx = getSelectionContext();
      if (!ctx) {
        vscode.window.showInformationMessage("Select code to edit first.");
        return;
      }
      const instruction = await vscode.window.showInputBox({
        prompt: "How should the selection be modified?",
        placeHolder: "e.g. add error handling, convert to async, add tests…",
      });
      if (!instruction) return;

      openChat({
        userText: `Rewrite the following ${ctx.language} code. ${instruction}\n\nOutput ONLY the corrected code, no explanations.`,
        contextBlock: `File: ${ctx.filename}\n\`\`\`${ctx.language}\n${ctx.text}\n\`\`\``,
        flowHint: "coding_swarm",
        run: true,
      });
    })
  );
}
