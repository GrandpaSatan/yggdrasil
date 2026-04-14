/**
 * RepoTreeProvider — workspace file tree for the Yggdrasil sidebar.
 * Sprint 062 P3. Implements TreeDataProvider<RepoNode> with lazy children.
 * Excludes node_modules, .git, out, dist, target by default.
 */

import * as vscode from "vscode";
import * as path from "path";

export interface RepoNode {
  resourceUri: vscode.Uri;
  isDirectory: boolean;
  label: string;
}

const EXCLUDE_PATTERNS = [
  "node_modules",
  ".git",
  "out",
  "dist",
  "target",
  ".vscode",
  "__pycache__",
  ".mypy_cache",
  ".pytest_cache",
  "*.lock",
];

export class RepoTreeProvider implements vscode.TreeDataProvider<RepoNode> {
  private _onDidChangeTreeData = new vscode.EventEmitter<RepoNode | undefined | void>();
  readonly onDidChangeTreeData = this._onDidChangeTreeData.event;

  refresh(): void {
    this._onDidChangeTreeData.fire();
  }

  getTreeItem(element: RepoNode): vscode.TreeItem {
    const item = new vscode.TreeItem(
      element.resourceUri,
      element.isDirectory
        ? vscode.TreeItemCollapsibleState.Collapsed
        : vscode.TreeItemCollapsibleState.None
    );
    item.label = element.label;
    if (!element.isDirectory) {
      item.command = {
        command: "vscode.open",
        title: "Open File",
        arguments: [element.resourceUri],
      };
      item.contextValue = "repoFile";
      item.tooltip = element.resourceUri.fsPath;
    } else {
      item.contextValue = "repoDirectory";
    }
    return item;
  }

  async getChildren(element?: RepoNode): Promise<RepoNode[]> {
    const root = element?.resourceUri ?? this.workspaceRoot();
    if (!root) return [];

    try {
      const entries = await vscode.workspace.fs.readDirectory(root);
      const nodes: RepoNode[] = [];

      for (const [name, fileType] of entries) {
        if (this.shouldExclude(name)) continue;
        const uri = vscode.Uri.joinPath(root, name);
        const isDir = (fileType & vscode.FileType.Directory) !== 0;
        nodes.push({ resourceUri: uri, isDirectory: isDir, label: name });
      }

      // Directories first, then files, both alphabetical
      nodes.sort((a, b) => {
        if (a.isDirectory !== b.isDirectory) return a.isDirectory ? -1 : 1;
        return a.label.localeCompare(b.label);
      });

      return nodes;
    } catch {
      return [];
    }
  }

  private workspaceRoot(): vscode.Uri | undefined {
    return vscode.workspace.workspaceFolders?.[0]?.uri;
  }

  private shouldExclude(name: string): boolean {
    for (const pat of EXCLUDE_PATTERNS) {
      if (pat.includes("*")) {
        const ext = pat.slice(1);
        if (name.endsWith(ext)) return true;
      } else if (name === pat) {
        return true;
      }
    }
    return false;
  }
}
