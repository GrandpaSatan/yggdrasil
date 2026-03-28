/**
 * JSONL event file watcher.
 *
 * Watches /tmp/ygg-hooks/memory-events.jsonl for new lines using polling
 * (FileSystemWatcher doesn't work reliably for /tmp on all platforms).
 * Dispatches parsed events to a callback.
 */

import * as fs from "fs";
import * as vscode from "vscode";

/** A single event from the JSONL file. */
export interface YggEvent {
  ts: string;
  event: string;
  data: Record<string, unknown>;
}

export class EventWatcher implements vscode.Disposable {
  private filePath: string;
  private callback: (event: YggEvent) => void;
  private byteOffset = 0;
  private pollInterval: ReturnType<typeof setInterval> | null = null;
  private fsWatcher: fs.FSWatcher | null = null;

  constructor(filePath: string, callback: (event: YggEvent) => void) {
    this.filePath = filePath;
    this.callback = callback;
  }

  start(): void {
    // Read any existing content (in case extension started after hooks)
    this.readNewLines();

    // Use fs.watch for instant detection + polling fallback
    try {
      this.fsWatcher = fs.watch(this.filePath, () => {
        this.readNewLines();
      });
      this.fsWatcher.on("error", () => {
        // File may not exist yet — polling will catch it
      });
    } catch {
      // File doesn't exist yet — that's fine
    }

    // Poll every 2s as fallback (fs.watch misses some writes on Linux /tmp)
    this.pollInterval = setInterval(() => this.readNewLines(), 2000);
  }

  private readNewLines(): void {
    try {
      if (!fs.existsSync(this.filePath)) return;

      const stat = fs.statSync(this.filePath);

      // File was truncated (new session) — reset
      if (stat.size < this.byteOffset) {
        this.byteOffset = 0;
      }

      // No new data
      if (stat.size <= this.byteOffset) return;

      // Read only the new bytes
      const fd = fs.openSync(this.filePath, "r");
      const buffer = Buffer.alloc(stat.size - this.byteOffset);
      fs.readSync(fd, buffer, 0, buffer.length, this.byteOffset);
      fs.closeSync(fd);

      this.byteOffset = stat.size;

      // Parse each line
      const lines = buffer.toString("utf-8").split("\n");
      for (const line of lines) {
        const trimmed = line.trim();
        if (!trimmed) continue;
        try {
          const event = JSON.parse(trimmed) as YggEvent;
          if (event.event && event.ts) {
            this.callback(event);
          }
        } catch {
          // Malformed line — skip
        }
      }
    } catch {
      // File access error — will retry on next poll
    }
  }

  dispose(): void {
    if (this.pollInterval) {
      clearInterval(this.pollInterval);
      this.pollInterval = null;
    }
    if (this.fsWatcher) {
      this.fsWatcher.close();
      this.fsWatcher = null;
    }
  }
}
