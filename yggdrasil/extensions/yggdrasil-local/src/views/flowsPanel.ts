/**
 * Flows Panel — full-width WebviewPanel showing Yggdrasil topology,
 * AI distribution, and per-flow step tables with expandable system prompts.
 *
 * Ported from yggdrasil/docs/sprint-058-flows.html. Opened via the
 * `yggdrasil.openFlows` command or by clicking a flow in the sidebar tree.
 */

import * as vscode from "vscode";

export class FlowsPanel {
  private static panel: vscode.WebviewPanel | undefined;
  private static readonly viewType = "yggdrasil.flowsPanel";

  static createOrShow(context: vscode.ExtensionContext, focusFlowId?: string): void {
    const column = vscode.window.activeTextEditor?.viewColumn ?? vscode.ViewColumn.One;

    if (FlowsPanel.panel) {
      FlowsPanel.panel.reveal(column);
      if (focusFlowId) {
        FlowsPanel.panel.webview.postMessage({ type: "focus", tab: focusFlowId });
      }
      return;
    }

    const mediaRoot = vscode.Uri.joinPath(context.extensionUri, "media");
    const panel = vscode.window.createWebviewPanel(
      FlowsPanel.viewType,
      "Yggdrasil Flows",
      column,
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [mediaRoot],
      }
    );

    FlowsPanel.panel = panel;
    panel.webview.html = FlowsPanel.getHtml(panel.webview, context.extensionUri);

    panel.onDidDispose(() => {
      FlowsPanel.panel = undefined;
    });

    panel.webview.onDidReceiveMessage((msg) => {
      if (msg?.type === "ready" && focusFlowId) {
        panel.webview.postMessage({ type: "focus", tab: focusFlowId });
      }
    });
  }

  private static getHtml(webview: vscode.Webview, extensionUri: vscode.Uri): string {
    const mediaRoot = vscode.Uri.joinPath(extensionUri, "media");
    const cssUri = webview.asWebviewUri(vscode.Uri.joinPath(mediaRoot, "flows.css"));
    const jsUri = webview.asWebviewUri(vscode.Uri.joinPath(mediaRoot, "flows.js"));
    const nonce = getNonce();
    const csp = [
      `default-src 'none'`,
      `img-src ${webview.cspSource} https: data:`,
      `style-src ${webview.cspSource} 'unsafe-inline'`,
      `font-src ${webview.cspSource}`,
      `script-src 'nonce-${nonce}'`,
    ].join("; ");

    return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="${csp}">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Yggdrasil Flows</title>
<link rel="stylesheet" href="${cssUri}">
</head>
<body>

<div class="sidebar">
  <div class="sidebar-header">
    <h1>Yggdrasil</h1>
    <p>Coding Swarm Architecture</p>
    <div class="stats">
      <div class="stat live">● 3 nodes</div>
      <div class="stat">Sprint 058</div>
    </div>
  </div>

  <div class="nav-section">
    <div class="nav-section-title">Architecture</div>
    <div class="nav-item active" data-tab="overview">Topology</div>
    <div class="nav-item" data-tab="distribution">AI Distribution</div>
  </div>

  <div class="nav-section">
    <div class="nav-section-title">Coding Flows</div>
    <div class="nav-item" data-tab="coding_swarm">coding_swarm <span class="status status-new">NEW</span></div>
    <div class="nav-item" data-tab="code_qa">code_qa <span class="status status-new">NEW</span></div>
    <div class="nav-item" data-tab="code_docs">code_docs <span class="status status-new">NEW</span></div>
    <div class="nav-item" data-tab="devops">devops <span class="status status-new">NEW</span></div>
    <div class="nav-item" data-tab="ui_design">ui_design <span class="status status-new">NEW</span></div>
    <div class="nav-item" data-tab="dba">dba <span class="status status-new">NEW</span></div>
    <div class="nav-item" data-tab="full_stack">full_stack <span class="status status-partial">META</span></div>
  </div>

  <div class="nav-section">
    <div class="nav-section-title">Existing Flows</div>
    <div class="nav-item" data-tab="research">research <span class="status status-done">LIVE</span></div>
    <div class="nav-item" data-tab="perceive">perceive <span class="status status-done">LIVE</span></div>
    <div class="nav-item" data-tab="saga">saga_classify_distill <span class="status status-done">LIVE</span></div>
    <div class="nav-item" data-tab="home_assistant">home_assistant <span class="status status-empty">EMPTY</span></div>
    <div class="nav-item" data-tab="complex_reasoning">complex_reasoning <span class="status status-new">S59</span></div>
    <div class="nav-item" data-tab="dream">dream_* flows <span class="status status-empty">EMPTY</span></div>
  </div>

  <div class="nav-section">
    <div class="nav-section-title">Legend</div>
    <div style="padding: 0 20px; font-size: 10px; color: #71717a; line-height: 1.8;">
      <div><span class="ai-badge ai-assigned">ASSIGNED</span> Running</div>
      <div><span class="ai-badge ai-empty">EMPTY</span> No model</div>
      <div><span class="ai-badge ai-distilled">DISTILLED</span> Custom-trained</div>
      <div><span class="ai-badge ai-ondemand">ON-DEMAND</span> Morrigan only</div>
      <div><span class="ai-badge ai-nonllm">NON-LLM</span> Tool/static</div>
    </div>
  </div>
</div>

<div class="main">

<div class="tab active" id="tab-overview">
  <div class="tab-header">
    <div class="hleft">
      <span class="badge">ARCHITECTURE</span>
      <h2>Network Topology</h2>
      <p>Three-node fleet: Hugin + Munin are always on (primary swarm), Morrigan is on-demand. Flows route through Odin on Munin, which dispatches to models across the fleet.</p>
    </div>
  </div>

  <div class="flow-container">
    <svg class="flow-svg" viewBox="0 0 1200 400" preserveAspectRatio="xMidYMid meet">
      <defs>
        <marker id="arrowhead" markerWidth="10" markerHeight="10" refX="9" refY="3" orient="auto">
          <polygon points="0 0, 9 3, 0 6" fill="#52525b" />
        </marker>
      </defs>

      <rect class="node-rect user" x="20" y="170" width="120" height="60" />
      <text class="node-title" x="80" y="195">User Request</text>
      <text class="node-host" x="80" y="215">chat / voice / code</text>

      <rect class="node-rect assigned" x="220" y="160" width="160" height="80" />
      <text class="node-title" x="300" y="185">Odin Router</text>
      <text class="node-model" x="300" y="205">Flow Engine</text>
      <text class="node-host" x="300" y="225">Munin :8080</text>

      <rect class="node-rect" x="460" y="60" width="180" height="110" fill="#1e3a8a" stroke="#3b82f6" />
      <text class="node-title" x="550" y="88" fill="#dbeafe">Hugin — Reviewer</text>
      <text class="node-model" x="550" y="108" fill="#93c5fd">eGPU: gemma4:e4b</text>
      <text class="node-model" x="550" y="123" fill="#93c5fd">eGPU: code-cleaner-350m</text>
      <text class="node-model" x="550" y="138" fill="#93c5fd">eGPU: rwkv-7</text>
      <text class="node-host" x="550" y="158" fill="#93c5fd">10.0.65.9 :11434</text>

      <rect class="node-rect" x="460" y="185" width="180" height="130" fill="#14532d" stroke="#22c55e" />
      <text class="node-title" x="550" y="213" fill="#dcfce7">Munin — Coder + Reasoner</text>
      <text class="node-model" x="550" y="233" fill="#86efac">iGPU: nemotron-3-nano:4b</text>
      <text class="node-model" x="550" y="248" fill="#86efac">iGPU: glm-4.7-flash</text>
      <text class="node-model" x="550" y="263" fill="#86efac">odin / mimir / mcp</text>
      <text class="node-model" x="550" y="278" fill="#86efac">huginn (code indexer)</text>
      <text class="node-host" x="550" y="298" fill="#86efac">10.0.65.8 :8080 :9090 :9093</text>

      <rect class="node-rect" x="460" y="310" width="180" height="80" fill="#713f12" stroke="#eab308" stroke-dasharray="4" />
      <text class="node-title" x="550" y="338" fill="#fef3c7">Morrigan — On Demand</text>
      <text class="node-model" x="550" y="358" fill="#fde047">Idle (10 models available)</text>
      <text class="node-host" x="550" y="378" fill="#fde047">10.0.65.20 2x RTX 3060</text>

      <path class="flow-arrow" d="M 140 200 L 220 200" />
      <path class="flow-arrow" d="M 380 180 L 460 110" />
      <path class="flow-arrow" d="M 380 200 L 460 240" />
      <path class="flow-arrow" d="M 380 220 L 460 350" stroke-dasharray="4" />

      <rect class="node-rect response" x="780" y="170" width="120" height="60" />
      <text class="node-title" x="840" y="195" fill="#bbf7d0">Response</text>
      <text class="node-host" x="840" y="215" fill="#86efac">code / text / voice</text>

      <path class="flow-arrow" d="M 640 110 L 780 190" />
      <path class="flow-arrow" d="M 640 240 L 780 210" />
      <path class="flow-arrow" d="M 640 350 L 780 215" stroke-dasharray="4" />
    </svg>
  </div>

  <div class="section">
    <div class="section-title">Design Principles</div>
    <div class="grid grid-4">
      <div class="card">
        <h3>Cross-Architecture Review</h3>
        <p>Coder (Qwen MoE) and reviewer (Gemma dense) are different architectures — different blind spots, better bug catching.</p>
      </div>
      <div class="card">
        <h3>Graceful Degradation</h3>
        <p>Flows work on Hugin+Munin alone. Morrigan is overflow for hard cases only.</p>
      </div>
      <div class="card">
        <h3>LGTM Convergence</h3>
        <p>Review loops iterate until reviewer says LGTM (max 3). Quality gated in architecture.</p>
      </div>
      <div class="card">
        <h3>Code Cleaner</h3>
        <p>Fine-tuned LFM2-350M normalizes messy thinking-model outputs into clean code before downstream steps.</p>
      </div>
    </div>
  </div>
</div>

<div class="tab" id="tab-distribution">
  <div class="tab-header">
    <div class="hleft">
      <span class="badge">AI Distribution</span>
      <h2>AI Distribution Map</h2>
      <p>Which models are loaded, on which host, and what role they play. Green border = currently loaded in VRAM.</p>
    </div>
  </div>

  <div class="hardware-map">
    <div class="host-card live">
      <div class="host-header">
        <div>
          <h3>Hugin <span class="verified">live</span></h3>
          <div class="ip">10.0.65.9</div>
        </div>
      </div>

      <div class="host-section">
        <div class="host-section-title">Services</div>
        <div class="svc-row"><span class="svc-dot"></span>ollama.service :11434</div>
        <div class="svc-row"><span class="svc-dot"></span>yggdrasil-huginn.service</div>
        <div class="svc-row"><span class="svc-dot"></span>yggdrasil-muninn.service</div>
        <div class="svc-row"><span class="svc-dot"></span>yggdrasil-vision.service</div>
        <div class="svc-row"><span class="svc-dot"></span>yggdrasil-llama-omni2.service</div>
      </div>

      <div class="host-section">
        <div class="host-section-title">Role: Reviewer + Vision + Voice</div>
        <p style="font-size: 11px; color: #a1a1aa; line-height: 1.5;">Code review, vision (perceive), agentic tool calling, voice I/O, code extraction. Does NOT generate code — maintains cross-architecture review independence.</p>
      </div>

      <div class="host-section">
        <div class="host-section-title">Models Loaded</div>
        <div class="model-row loaded"><span>gemma4:e4b</span><span class="size">16.2 GB</span></div>
        <div class="model-row loaded"><span>code-cleaner-350m</span><span class="size">3.0 GB</span></div>
        <div class="model-row loaded"><span>all-minilm</span><span class="size">0.1 GB</span></div>
      </div>

      <div class="host-section">
        <div class="host-section-title">Available</div>
        <div class="model-row"><span>rwkv-7-g1e:2.9b</span><span class="size">2.6 GB</span></div>
        <div class="model-row"><span>LFM2.5-1.2B-Instruct</span><span class="size">730 MB</span></div>
      </div>
    </div>

    <div class="host-card live">
      <div class="host-header">
        <div>
          <h3>Munin <span class="verified">live</span></h3>
          <div class="ip">10.0.65.8</div>
        </div>
      </div>

      <div class="host-section">
        <div class="host-section-title">Services</div>
        <div class="svc-row"><span class="svc-dot"></span>yggdrasil-odin.service :8080</div>
        <div class="svc-row"><span class="svc-dot"></span>yggdrasil-mimir.service :9090</div>
        <div class="svc-row"><span class="svc-dot"></span>yggdrasil-mcp-remote.service :9093</div>
        <div class="svc-row"><span class="svc-dot"></span>ollama.service :11434</div>
      </div>

      <div class="host-section">
        <div class="host-section-title">Role: Coder + Orchestrator</div>
        <p style="font-size: 11px; color: #a1a1aa; line-height: 1.5;">Primary code generator (Nemotron) + Odin flow engine + Mimir memory. Does NOT review its own output.</p>
      </div>

      <div class="host-section">
        <div class="host-section-title">Models Loaded</div>
        <div class="model-row loaded"><span>nemotron-3-nano:4b</span><span class="size">5.1 GB</span></div>
        <div class="model-row loaded"><span>glm-4.7-flash</span><span class="size">~18 GB</span></div>
        <div class="model-row loaded"><span>all-minilm</span><span class="size">0.1 GB</span></div>
      </div>

      <div class="host-section">
        <div class="host-section-title">Available</div>
        <div class="model-row"><span>gemma4:e2b</span><span class="size">7.2 GB</span></div>
        <div class="model-row"><span>review-1.2b</span><span class="size">1.2 GB</span></div>
        <div class="model-row"><span>saga-350m</span><span class="size">711 MB</span></div>
        <div class="model-row"><span>LFM2.5-1.2B-Instruct</span><span class="size">730 MB</span></div>
      </div>
    </div>

    <div class="host-card">
      <div class="host-header">
        <div>
          <h3>Morrigan <span style="color:#fde047;font-size:10px;margin-left:8px;">idle</span></h3>
          <div class="ip">10.0.65.20 (on-demand)</div>
        </div>
      </div>

      <div class="host-section">
        <div class="host-section-title">Services</div>
        <div class="svc-row"><span class="svc-dot"></span>ollama.service :11434 (GPU0)</div>
        <div class="svc-row"><span class="svc-dot"></span>ollama-gpu1.service :11435 (GPU1)</div>
      </div>

      <div class="host-section">
        <div class="host-section-title">Models Loaded</div>
        <div class="model-row"><span>— none loaded —</span><span class="size">0 MB</span></div>
      </div>

      <div class="host-section">
        <div class="host-section-title">Available for on-demand load</div>
        <div class="model-row"><span>qwen3:30b-a3b</span><span class="size">18.0 GB</span></div>
        <div class="model-row"><span>qwen3.5:9b</span><span class="size">6.6 GB</span></div>
        <div class="model-row"><span>nemotron-3-nano:4b</span><span class="size">2.8 GB</span></div>
        <div class="model-row"><span>lfm-code-v2</span><span class="size">1.2 GB</span></div>
        <div class="model-row"><span>lfm-review-v2</span><span class="size">1.2 GB</span></div>
      </div>
    </div>
  </div>
</div>

<div class="tab" id="tab-coding_swarm"></div>
<div class="tab" id="tab-code_qa"></div>
<div class="tab" id="tab-code_docs"></div>
<div class="tab" id="tab-devops"></div>
<div class="tab" id="tab-ui_design"></div>
<div class="tab" id="tab-dba"></div>
<div class="tab" id="tab-full_stack"></div>
<div class="tab" id="tab-research"></div>
<div class="tab" id="tab-perceive"></div>
<div class="tab" id="tab-saga"></div>
<div class="tab" id="tab-home_assistant"></div>
<div class="tab" id="tab-complex_reasoning"></div>
<div class="tab" id="tab-dream"></div>

<div class="tooltip" id="tooltip"></div>

</div>

<script nonce="${nonce}" src="${jsUri}"></script>
<script nonce="${nonce}">
  // Focus handler — receives {type: "focus", tab: "<flow-id>"} from extension
  window.addEventListener("message", (e) => {
    const msg = e.data;
    if (msg?.type === "focus" && typeof msg.tab === "string") {
      const item = document.querySelector('.nav-item[data-tab="' + msg.tab + '"]');
      if (item) item.click();
    }
  });
</script>

</body>
</html>`;
  }
}

function getNonce(): string {
  let text = "";
  const possible = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  for (let i = 0; i < 32; i++) {
    text += possible.charAt(Math.floor(Math.random() * possible.length));
  }
  return text;
}
