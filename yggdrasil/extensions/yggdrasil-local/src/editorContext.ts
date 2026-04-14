/**
 * editorContext — shared editor context helpers, Sprint 062 P3.
 * Extracted from codeActions.ts; shared between codeActions and auto-inject path.
 * Returns the active selection or first 400 lines of the active document.
 */

import * as vscode from "vscode";

const MAX_LINES = 400;
const MAX_BYTES = 50_000;

export interface EditorContext {
  text: string;
  language: string;
  filename: string;
  uri: vscode.Uri;
  isSelection: boolean;
  lineStart: number;
  lineEnd: number;
}

/**
 * Returns the current editor selection, or the first MAX_LINES lines of the
 * active document if nothing is selected.  Returns null if no editor is open.
 */
export function getEditorContext(): EditorContext | null {
  const editor = vscode.window.activeTextEditor;
  if (!editor) return null;

  const doc = editor.document;
  const sel = editor.selection;
  const hasSelection = !sel.isEmpty;

  let text: string;
  let lineStart: number;
  let lineEnd: number;

  if (hasSelection) {
    text = doc.getText(sel);
    lineStart = sel.start.line + 1;
    lineEnd = sel.end.line + 1;
  } else {
    // First MAX_LINES lines
    const lastLine = Math.min(doc.lineCount, MAX_LINES) - 1;
    const range = new vscode.Range(0, 0, lastLine, doc.lineAt(lastLine).text.length);
    text = doc.getText(range);
    lineStart = 1;
    lineEnd = lastLine + 1;
  }

  // Trim to byte budget
  if (text.length > MAX_BYTES) {
    text = text.slice(0, MAX_BYTES) + "\n…[truncated]";
  }

  if (!text.trim()) return null;

  return {
    text,
    language: doc.languageId,
    filename: doc.fileName.split(/[\\/]/).pop() ?? "untitled",
    uri: doc.uri,
    isSelection: hasSelection,
    lineStart,
    lineEnd,
  };
}

/**
 * Formats an EditorContext as a markdown code fence for injection into chat.
 */
export function formatContextBlock(ctx: EditorContext): string {
  const lineInfo = ctx.isSelection ? ` (lines ${ctx.lineStart}-${ctx.lineEnd})` : "";
  return `File: ${ctx.filename}${lineInfo}\n\`\`\`${ctx.language}\n${ctx.text}\n\`\`\``;
}
