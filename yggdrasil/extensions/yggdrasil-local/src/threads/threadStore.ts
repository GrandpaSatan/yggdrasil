/**
 * ThreadStore — FS-backed thread history, Sprint 062 P2b.
 *
 * Threads stored as one JSONL file per thread at:
 *   <globalStorageUri>/threads/<threadId>.jsonl
 *
 * Index at: <globalStorageUri>/threads/_index.json
 *   Format: ThreadIndex[]  { id, title, mtime, messageCount }
 *
 * On first run: detects Sprint 061 globalState-based storage, migrates
 * silently, backs up originals to threads/_backup-pre062/.
 *
 * All I/O is synchronous-style wrapped in async for VSCode compat.
 */

import * as vscode from "vscode";
import * as path from "path";
import { randomUUID } from "crypto";

export interface StoredMessage {
  role: "system" | "user" | "assistant";
  content: string;
  ts: number;
  model?: string;
  flow?: string;
}

export interface ThreadIndex {
  id: string;
  title: string;
  mtime: number;
  messageCount: number;
}

export interface StoredThread {
  index: ThreadIndex;
  messages: StoredMessage[];
}

const INDEX_FILE = "_index.json";
const BACKUP_DIR = "_backup-pre062";
const SPRINT061_STATE_KEY = "yggdrasil.chat.threads";

export class ThreadStore {
  private threadsDir: vscode.Uri;
  private indexUri: vscode.Uri;
  private initialized = false;

  constructor(private context: vscode.ExtensionContext) {
    this.threadsDir = vscode.Uri.joinPath(context.globalStorageUri, "threads");
    this.indexUri   = vscode.Uri.joinPath(this.threadsDir, INDEX_FILE);
  }

  async init(): Promise<void> {
    if (this.initialized) return;
    await this.ensureDir(this.threadsDir);
    await this.maybeMigrate();
    this.initialized = true;
  }

  // ── Public API ────────────────────────────────────────────────

  async list(): Promise<ThreadIndex[]> {
    await this.init();
    const index = await this.loadIndex();
    return index.sort((a, b) => b.mtime - a.mtime);
  }

  async load(id: string): Promise<StoredThread | undefined> {
    await this.init();
    const index = await this.loadIndex();
    const entry = index.find((t) => t.id === id);
    if (!entry) return undefined;
    const messages = await this.loadMessages(id);
    return { index: entry, messages };
  }

  async append(id: string, message: StoredMessage): Promise<void> {
    await this.init();
    const fileUri = this.messageFileUri(id);
    const line = JSON.stringify(message) + "\n";
    const enc = new TextEncoder().encode(line);
    try {
      // Append by reading existing + writing combined
      let existing: Uint8Array;
      try { existing = await vscode.workspace.fs.readFile(fileUri); }
      catch { existing = new Uint8Array(0); }
      const combined = new Uint8Array(existing.length + enc.length);
      combined.set(existing);
      combined.set(enc, existing.length);
      await vscode.workspace.fs.writeFile(fileUri, combined);
    } catch (e) {
      await vscode.workspace.fs.writeFile(fileUri, enc);
    }
    await this.updateIndex(id, message);
  }

  async rename(id: string, title: string): Promise<void> {
    await this.init();
    const index = await this.loadIndex();
    const entry = index.find((t) => t.id === id);
    if (!entry) return;
    entry.title = title;
    await this.saveIndex(index);
  }

  async delete(id: string): Promise<void> {
    await this.init();
    try {
      await vscode.workspace.fs.delete(this.messageFileUri(id));
    } catch { /* file may not exist */ }
    const index = (await this.loadIndex()).filter((t) => t.id !== id);
    await this.saveIndex(index);
  }

  async search(query: string): Promise<ThreadIndex[]> {
    await this.init();
    const index = await this.loadIndex();
    if (!query.trim()) return index.sort((a, b) => b.mtime - a.mtime);
    const q = query.toLowerCase();
    const scored: Array<{ entry: ThreadIndex; score: number }> = [];
    for (const entry of index) {
      let score = 0;
      if (entry.title.toLowerCase().includes(q)) score += 2;
      scored.push({ entry, score });
    }
    return scored
      .filter((s) => s.score > 0 || query.length < 2)
      .sort((a, b) => b.score - a.score || b.entry.mtime - a.entry.mtime)
      .map((s) => s.entry);
  }

  async exportAsMarkdown(id: string): Promise<string> {
    await this.init();
    const thread = await this.load(id);
    if (!thread) return "";
    const title = thread.index.title || id;
    const lines = [`# ${title}`, ``, `*Exported ${new Date().toISOString()}*`, ``];
    for (const m of thread.messages) {
      const glyph = m.role === "user" ? ">" : m.role === "assistant" ? "" : "**System**";
      const ts = new Date(m.ts).toLocaleString();
      if (m.role === "user") {
        lines.push(`> **User** (${ts})`);
        lines.push(`> ${m.content.replace(/\n/g, "\n> ")}`);
      } else if (m.role === "assistant") {
        lines.push(`**Assistant** (${m.model ?? "?"}, ${ts})`);
        lines.push(m.content);
      } else {
        lines.push(`*System: ${m.content}*`);
      }
      lines.push(``);
    }
    return lines.join("\n");
  }

  async createThread(title?: string): Promise<ThreadIndex> {
    await this.init();
    const entry: ThreadIndex = {
      id: randomUUID(),
      title: title ?? "New chat",
      mtime: Date.now(),
      messageCount: 0,
    };
    const index = await this.loadIndex();
    index.unshift(entry);
    await this.saveIndex(index);
    return entry;
  }

  // ── Private helpers ───────────────────────────────────────────

  private messageFileUri(id: string): vscode.Uri {
    return vscode.Uri.joinPath(this.threadsDir, `${id}.jsonl`);
  }

  private async loadMessages(id: string): Promise<StoredMessage[]> {
    try {
      const data = await vscode.workspace.fs.readFile(this.messageFileUri(id));
      const text = new TextDecoder().decode(data);
      return text
        .split("\n")
        .filter((l) => l.trim())
        .map((l) => {
          try { return JSON.parse(l) as StoredMessage; }
          catch { return null; }
        })
        .filter((m): m is StoredMessage => m !== null);
    } catch {
      return [];
    }
  }

  private async loadIndex(): Promise<ThreadIndex[]> {
    try {
      const data = await vscode.workspace.fs.readFile(this.indexUri);
      const text = new TextDecoder().decode(data);
      const parsed = JSON.parse(text);
      return Array.isArray(parsed) ? parsed : [];
    } catch {
      return [];
    }
  }

  private async saveIndex(index: ThreadIndex[]): Promise<void> {
    const text = JSON.stringify(index.slice(0, 200), null, 2);
    await vscode.workspace.fs.writeFile(this.indexUri, new TextEncoder().encode(text));
  }

  private async updateIndex(id: string, lastMessage: StoredMessage): Promise<void> {
    const index = await this.loadIndex();
    let entry = index.find((t) => t.id === id);
    if (!entry) {
      entry = { id, title: "New chat", mtime: lastMessage.ts, messageCount: 1 };
      index.unshift(entry);
    } else {
      entry.mtime = lastMessage.ts;
      entry.messageCount = (entry.messageCount || 0) + 1;
      // Auto-title from first user message
      if (entry.title === "New chat" && lastMessage.role === "user" && lastMessage.content.trim()) {
        entry.title = lastMessage.content.trim().slice(0, 48).replace(/\s+/g, " ");
      }
    }
    await this.saveIndex(index);
  }

  private async ensureDir(uri: vscode.Uri): Promise<void> {
    try { await vscode.workspace.fs.createDirectory(uri); } catch { /* already exists */ }
  }

  // ── Sprint 061 migration ──────────────────────────────────────

  private async maybeMigrate(): Promise<void> {
    const existingIndex = await this.loadIndex();
    if (existingIndex.length > 0) return; // Already migrated or empty

    // Check for Sprint 061 globalState storage
    const oldThreads = this.context.globalState.get<Array<{
      id: string;
      title: string;
      createdAt: number;
      updatedAt: number;
      messages: StoredMessage[];
    }>>(SPRINT061_STATE_KEY);

    if (!Array.isArray(oldThreads) || oldThreads.length === 0) return;

    // Back up old data
    const backupDir = vscode.Uri.joinPath(this.threadsDir, BACKUP_DIR);
    await this.ensureDir(backupDir);
    const backupUri = vscode.Uri.joinPath(backupDir, "sprint061-threads.json");
    await vscode.workspace.fs.writeFile(
      backupUri,
      new TextEncoder().encode(JSON.stringify(oldThreads, null, 2))
    );

    // Migrate each thread
    const newIndex: ThreadIndex[] = [];
    for (const t of oldThreads) {
      const entry: ThreadIndex = {
        id: t.id,
        title: t.title || "New chat",
        mtime: t.updatedAt || Date.now(),
        messageCount: t.messages?.length ?? 0,
      };
      newIndex.push(entry);

      if (Array.isArray(t.messages) && t.messages.length > 0) {
        const lines = t.messages.map((m) => JSON.stringify(m)).join("\n") + "\n";
        await vscode.workspace.fs.writeFile(
          this.messageFileUri(t.id),
          new TextEncoder().encode(lines)
        );
      }
    }

    await this.saveIndex(newIndex);
    // Note: do NOT clear the old globalState entry — keep backup in case user downgrades
  }
}
