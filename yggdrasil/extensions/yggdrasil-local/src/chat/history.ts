/**
 * Chat history — persistent multi-thread storage backed by globalState.
 *
 * Keeps the last 50 threads across workspace reloads. Each thread is a
 * list of messages with {role, content, ts, model?, flow?}. Messages
 * trim long content (>50KB) on save to avoid bloating globalState.
 */

import * as vscode from "vscode";
import { randomUUID } from "crypto";

const STORAGE_KEY = "yggdrasil.chat.threads";
const MAX_THREADS = 50;
const MAX_MESSAGE_BYTES = 50_000;

export interface ChatMsg {
  role: "system" | "user" | "assistant";
  content: string;
  ts: number;
  model?: string;
  flow?: string;
}

export interface ChatThread {
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  messages: ChatMsg[];
}

export class ChatHistory {
  constructor(private context: vscode.ExtensionContext) {}

  listThreads(): Array<{ id: string; title: string; updatedAt: number }> {
    const threads = this.loadAll();
    return threads
      .map((t) => ({ id: t.id, title: t.title, updatedAt: t.updatedAt }))
      .sort((a, b) => b.updatedAt - a.updatedAt);
  }

  getThread(id: string): ChatThread | undefined {
    return this.loadAll().find((t) => t.id === id);
  }

  createThread(title?: string): ChatThread {
    const thread: ChatThread = {
      id: randomUUID(),
      title: title ?? "New chat",
      createdAt: Date.now(),
      updatedAt: Date.now(),
      messages: [],
    };
    const all = this.loadAll();
    all.unshift(thread);
    this.saveAll(all);
    return thread;
  }

  appendMessage(id: string, msg: ChatMsg): ChatThread | undefined {
    const all = this.loadAll();
    const thread = all.find((t) => t.id === id);
    if (!thread) return undefined;

    const trimmed: ChatMsg =
      msg.content.length > MAX_MESSAGE_BYTES
        ? { ...msg, content: msg.content.slice(0, MAX_MESSAGE_BYTES) + "\n…[truncated]" }
        : msg;

    thread.messages.push(trimmed);
    thread.updatedAt = Date.now();

    // Auto-title from first user message
    if (thread.title === "New chat" && msg.role === "user" && msg.content.trim()) {
      thread.title = msg.content.trim().slice(0, 48).replace(/\s+/g, " ");
    }
    this.saveAll(all);
    return thread;
  }

  replaceLastAssistant(id: string, content: string): void {
    const all = this.loadAll();
    const thread = all.find((t) => t.id === id);
    if (!thread) return;
    for (let i = thread.messages.length - 1; i >= 0; i--) {
      if (thread.messages[i].role === "assistant") {
        thread.messages[i].content = content;
        thread.updatedAt = Date.now();
        break;
      }
    }
    this.saveAll(all);
  }

  deleteThread(id: string): void {
    const all = this.loadAll().filter((t) => t.id !== id);
    this.saveAll(all);
  }

  clearThread(id: string): void {
    const all = this.loadAll();
    const thread = all.find((t) => t.id === id);
    if (!thread) return;
    thread.messages = [];
    thread.updatedAt = Date.now();
    thread.title = "New chat";
    this.saveAll(all);
  }

  private loadAll(): ChatThread[] {
    const raw = this.context.globalState.get<ChatThread[]>(STORAGE_KEY);
    return Array.isArray(raw) ? raw : [];
  }

  private saveAll(threads: ChatThread[]): void {
    const bounded = threads
      .sort((a, b) => b.updatedAt - a.updatedAt)
      .slice(0, MAX_THREADS);
    this.context.globalState.update(STORAGE_KEY, bounded);
  }
}
