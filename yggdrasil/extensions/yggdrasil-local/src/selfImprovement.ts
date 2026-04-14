/**
 * SelfImprovementChecker — Sprint 062 P3b.
 *
 * On extension activate(), if Yggdrasil workspace detected:
 *  1. Fetches pending self-improvement suggestions from Odin memory API.
 *  2. If ≥1 result AND not snoozed AND not dismissed → calls onResult(payload).
 *  3. Uses a 10-min TTL cache in globalState to avoid hammering the server.
 *
 * The caller (extension.ts) wires onResult to ChatPanel.postMessage.
 */

import * as vscode from "vscode";
import { OdinClient } from "./api/odinClient";

const CACHE_KEY   = "selfImprovement.cache";
const SNOOZE_KEY  = "selfImprovement.snoozedUntil";
const DISMISS_KEY = "selfImprovement.dismissed";
const TTL_MS      = 10 * 60 * 1000; // 10 minutes

interface MemoryCacheEntry {
  ts: number;
  results: Array<{ cause: string; effect: string; similarity: number }>;
}

interface NotificationPayload {
  type: "showNotificationCard";
  count: number;
  summaryTitles: string[];
}

export class SelfImprovementChecker {
  constructor(
    private context: vscode.ExtensionContext,
    private odin: OdinClient
  ) {}

  async check(onResult: (payload: NotificationPayload) => void): Promise<void> {
    if (!this.isYggdrasilWorkspace()) return;

    // Check snooze / dismiss gates
    const snoozedUntil = this.context.globalState.get<number>(SNOOZE_KEY, 0);
    if (snoozedUntil > Date.now()) return;

    const dismissed = this.context.globalState.get<boolean>(DISMISS_KEY, false);
    if (dismissed) return;

    const results = await this.fetchPending();
    if (!results || results.length === 0) return;

    const summaryTitles = results.slice(0, 3).map((r) =>
      r.cause.slice(0, 60).replace(/\s+/g, " ")
    );

    onResult({ type: "showNotificationCard", count: results.length, summaryTitles });
  }

  private isYggdrasilWorkspace(): boolean {
    const folders = vscode.workspace.workspaceFolders ?? [];
    for (const f of folders) {
      const fsPath = f.uri.fsPath;
      // Match workspace path containing "Yggdrasil"
      if (/Yggdrasil/i.test(fsPath)) return true;
    }
    return false;
  }

  private async fetchPending(): Promise<Array<{ cause: string; effect: string; similarity: number }> | null> {
    // Check cache
    const cached = this.context.globalState.get<MemoryCacheEntry>(CACHE_KEY);
    if (cached && Date.now() - cached.ts < TTL_MS) {
      return cached.results;
    }

    try {
      const cfg = vscode.workspace.getConfiguration("yggdrasil");
      const odinUrl = cfg.get<string>("odinUrl", "http://localhost:8080");
      const url = `${odinUrl}/api/memory/query?text=self_improvement+pending&limit=10&tags=self_improvement,pending`;

      const resp = await fetch(url, {
        signal: AbortSignal.timeout(5000),
      });
      if (!resp.ok) return null;

      const data = await resp.json() as { results?: Array<{ cause: string; effect: string; similarity: number }> };
      const results = Array.isArray(data.results) ? data.results : [];

      // Cache result
      await this.context.globalState.update(CACHE_KEY, { ts: Date.now(), results });
      return results;
    } catch {
      return null;
    }
  }
}
