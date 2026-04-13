/**
 * Flows TreeView provider — renders the flows list in the activity-bar sidebar.
 *
 * Tree structure:
 *   Architecture
 *     Topology
 *     AI Distribution
 *   Coding Flows
 *     coding_swarm, code_qa, code_docs, devops, ui_design, dba, full_stack
 *   Existing Flows
 *     research, perceive, saga_classify_distill, home_assistant, complex_reasoning, dream_*
 *
 * Clicking a leaf runs the `yggdrasil.openFlows` command with the flow id,
 * which opens the full-width FlowsPanel focused on that tab.
 */

import * as vscode from "vscode";

type FlowStatus = "new" | "live" | "empty" | "partial" | "architecture";

interface FlowLeaf {
  id: string;
  label: string;
  status: FlowStatus;
  tooltip: string;
}

interface FlowGroup {
  label: string;
  children: FlowLeaf[];
}

const GROUPS: FlowGroup[] = [
  {
    label: "Architecture",
    children: [
      { id: "overview", label: "Topology", status: "architecture", tooltip: "Three-node fleet overview — Hugin, Munin, Morrigan" },
      { id: "distribution", label: "AI Distribution", status: "architecture", tooltip: "Live map of models loaded per host" },
    ],
  },
  {
    label: "Coding Flows",
    children: [
      { id: "coding_swarm", label: "coding_swarm", status: "new", tooltip: "Cross-model generate → review → refine with LGTM loop" },
      { id: "code_qa", label: "code_qa", status: "new", tooltip: "Test generation with coverage analysis" },
      { id: "code_docs", label: "code_docs", status: "new", tooltip: "Docs with accuracy cross-check" },
      { id: "devops", label: "devops", status: "new", tooltip: "Infra config with safety review" },
      { id: "ui_design", label: "ui_design", status: "new", tooltip: "Frontend components with visual review loop" },
      { id: "dba", label: "dba", status: "new", tooltip: "Schema migrations with safety review" },
      { id: "full_stack", label: "full_stack", status: "partial", tooltip: "Meta-flow — orchestrates 4 others" },
    ],
  },
  {
    label: "Existing Flows",
    children: [
      { id: "research", label: "research", status: "live", tooltip: "7-step research pipeline (Sprint 056)" },
      { id: "perceive", label: "perceive", status: "live", tooltip: "Voice + vision understanding (Sprint 057)" },
      { id: "saga", label: "saga_classify_distill", status: "live", tooltip: "Memory classification + engram extraction" },
      { id: "home_assistant", label: "home_assistant", status: "empty", tooltip: "HA device control — needs model reassignment" },
      { id: "complex_reasoning", label: "complex_reasoning", status: "new", tooltip: "Fast plan → deep verify (Sprint 059)" },
      { id: "dream", label: "dream_* flows", status: "empty", tooltip: "Memory consolidation / exploration — empty" },
    ],
  },
];

type TreeNode = { kind: "group"; group: FlowGroup } | { kind: "leaf"; leaf: FlowLeaf };

export class FlowsTreeProvider implements vscode.TreeDataProvider<TreeNode> {
  private _onDidChangeTreeData = new vscode.EventEmitter<TreeNode | undefined>();
  readonly onDidChangeTreeData = this._onDidChangeTreeData.event;

  refresh(): void {
    this._onDidChangeTreeData.fire(undefined);
  }

  getTreeItem(node: TreeNode): vscode.TreeItem {
    if (node.kind === "group") {
      const item = new vscode.TreeItem(node.group.label, vscode.TreeItemCollapsibleState.Expanded);
      item.contextValue = "flowGroup";
      item.iconPath = new vscode.ThemeIcon("folder");
      return item;
    }

    const item = new vscode.TreeItem(node.leaf.label, vscode.TreeItemCollapsibleState.None);
    item.tooltip = node.leaf.tooltip;
    item.contextValue = "flow";
    item.iconPath = iconForStatus(node.leaf.status);
    item.description = descriptionForStatus(node.leaf.status);
    item.command = {
      command: "yggdrasil.openFlows",
      title: "Open Flows",
      arguments: [node.leaf.id],
    };
    return item;
  }

  getChildren(node?: TreeNode): TreeNode[] {
    if (!node) {
      return GROUPS.map((group) => ({ kind: "group", group }));
    }
    if (node.kind === "group") {
      return node.group.children.map((leaf) => ({ kind: "leaf", leaf }));
    }
    return [];
  }
}

function iconForStatus(status: FlowStatus): vscode.ThemeIcon {
  switch (status) {
    case "live":
      return new vscode.ThemeIcon("pass-filled", new vscode.ThemeColor("testing.iconPassed"));
    case "new":
      return new vscode.ThemeIcon("sparkle", new vscode.ThemeColor("charts.blue"));
    case "empty":
      return new vscode.ThemeIcon("circle-slash", new vscode.ThemeColor("charts.red"));
    case "partial":
      return new vscode.ThemeIcon("circle-large-outline", new vscode.ThemeColor("charts.yellow"));
    case "architecture":
      return new vscode.ThemeIcon("symbol-structure");
  }
}

function descriptionForStatus(status: FlowStatus): string {
  switch (status) {
    case "live":
      return "live";
    case "new":
      return "new";
    case "empty":
      return "empty";
    case "partial":
      return "meta";
    default:
      return "";
  }
}
