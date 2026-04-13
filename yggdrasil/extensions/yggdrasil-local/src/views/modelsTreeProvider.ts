/**
 * Models TreeView provider — sidebar tree of models exposed by Odin.
 *
 * Tree structure (live from GET /v1/models):
 *   <backend>
 *     <model-id>  [loaded|ready]
 *     <model-id>  ...
 *
 * Refreshes every 30 seconds automatically; can be refreshed on demand
 * via the `yggdrasil.refreshModels` command.
 */

import * as vscode from "vscode";
import { OdinClient, Model } from "../api/odinClient";

type ModelsNode =
  | { kind: "backend"; name: string; models: Model[] }
  | { kind: "model"; model: Model }
  | { kind: "empty"; message: string };

const REFRESH_INTERVAL_MS = 30_000;

export class ModelsTreeProvider implements vscode.TreeDataProvider<ModelsNode>, vscode.Disposable {
  private _onDidChangeTreeData = new vscode.EventEmitter<ModelsNode | undefined>();
  readonly onDidChangeTreeData = this._onDidChangeTreeData.event;

  private cache: Model[] = [];
  private lastFetch = 0;
  private fetching = false;
  private timer: NodeJS.Timeout | undefined;

  constructor(private client: OdinClient) {
    this.timer = setInterval(() => this.refresh(), REFRESH_INTERVAL_MS);
  }

  refresh(): void {
    this.lastFetch = 0;
    this._onDidChangeTreeData.fire(undefined);
  }

  getTreeItem(node: ModelsNode): vscode.TreeItem {
    if (node.kind === "empty") {
      const item = new vscode.TreeItem(node.message, vscode.TreeItemCollapsibleState.None);
      item.iconPath = new vscode.ThemeIcon("info");
      return item;
    }

    if (node.kind === "backend") {
      const item = new vscode.TreeItem(node.name, vscode.TreeItemCollapsibleState.Expanded);
      item.description = `${node.models.length} models`;
      item.iconPath = new vscode.ThemeIcon("server");
      item.contextValue = "modelBackend";
      return item;
    }

    const item = new vscode.TreeItem(node.model.id, vscode.TreeItemCollapsibleState.None);
    item.iconPath = new vscode.ThemeIcon(
      node.model.loaded ? "pass-filled" : "circle-outline",
      new vscode.ThemeColor(node.model.loaded ? "testing.iconPassed" : "disabledForeground")
    );
    item.description = node.model.loaded ? "loaded" : "ready";
    item.tooltip = new vscode.MarkdownString(
      [
        `**${node.model.id}**`,
        node.model.backend ? `Backend: \`${node.model.backend}\`` : "",
        node.model.size_bytes ? `Size: ${formatBytes(node.model.size_bytes)}` : "",
        node.model.loaded ? "Status: **loaded in VRAM**" : "Status: available (not loaded)",
      ]
        .filter(Boolean)
        .join("\n\n")
    );
    item.contextValue = "model";
    item.command = {
      command: "yggdrasil.useModelInChat",
      title: "Use model in chat",
      arguments: [node.model.id],
    };
    return item;
  }

  async getChildren(node?: ModelsNode): Promise<ModelsNode[]> {
    if (!node) {
      await this.ensureFresh();
      if (this.cache.length === 0) {
        return [
          {
            kind: "empty",
            message: "No models available — check Odin URL in Yggdrasil settings.",
          },
        ];
      }
      const byBackend = new Map<string, Model[]>();
      for (const m of this.cache) {
        const b = m.backend ?? "default";
        const list = byBackend.get(b) ?? [];
        list.push(m);
        byBackend.set(b, list);
      }
      return Array.from(byBackend.entries())
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([name, models]) => ({ kind: "backend" as const, name, models }));
    }

    if (node.kind === "backend") {
      const sorted = [...node.models].sort((a, b) => {
        if (a.loaded !== b.loaded) return a.loaded ? -1 : 1;
        return a.id.localeCompare(b.id);
      });
      return sorted.map((model) => ({ kind: "model" as const, model }));
    }

    return [];
  }

  private async ensureFresh(): Promise<void> {
    if (this.fetching) return;
    if (Date.now() - this.lastFetch < 5000) return; // coalesce rapid calls
    this.fetching = true;
    try {
      this.cache = await this.client.listModels();
      this.lastFetch = Date.now();
    } catch {
      // Keep stale cache on failure
    } finally {
      this.fetching = false;
    }
  }

  dispose(): void {
    if (this.timer) clearInterval(this.timer);
  }
}

function formatBytes(bytes: number): string {
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(1)} MB`;
  if (bytes >= 1e3) return `${(bytes / 1e3).toFixed(1)} KB`;
  return `${bytes} B`;
}
