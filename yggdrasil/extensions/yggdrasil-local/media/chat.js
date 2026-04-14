/* Yggdrasil chat — client-side, Sprint 062.
   Terminal-log aesthetic: monospace throughout, role glyphs, statusline.
   Slash-command autocomplete on '/' at caret 0.
   '@' at caret 0 → requestFilePicker postMessage.
   Prism highlighting loaded lazily in P2a pass.
   Virtual scrolling activated for threads >= 50 messages (P2c). */

(function () {
  const vscode = acquireVsCodeApi();

  // ─────────────────────────────────────────────────────────────
  // State
  // ─────────────────────────────────────────────────────────────
  let state = {
    threads: [],
    currentThreadId: null,
    messages: [],
    models: [],
    flows: [],
    defaultModel: null,
  };

  let attachments = []; // {kind, label, content}[]
  let streamBuf = "";
  let streamingEl = null;
  let generating = false;
  let currentModel = null;

  // Sprint 061 swarm-flow UI state
  let thinkingFoldEl = null;
  let currentThinkStep = null;
  let assistantHasStreamed = false;

  // P2c virtual scroll state
  const VS_THRESHOLD = 50;
  const VS_BUFFER = 30;
  let vsEnabled = false;
  let vsMessages = [];
  let vsWindowStart = 0;
  let vsWindowEnd = 0;
  let vsHeights = [];
  let vsAvgHeight = 80;
  let vsObserver = null;

  // P2 slash autocomplete state
  let slashMenuActive = false;
  let slashMenuIdx = 0;
  const SLASH_COMMANDS = [
    { cmd: "/flow",   desc: "Run a named flow: /flow <name> <message>" },
    { cmd: "/clear",  desc: "Clear current thread messages" },
    { cmd: "/new",    desc: "Create a new thread" },
    { cmd: "/reload", desc: "Reload models and flows" },
    { cmd: "/voice",  desc: "Toggle voice push-to-talk" },
    { cmd: "/model",  desc: "Override model: /model <id> <message>" },
    { cmd: "/memory", desc: "Inject memory context: /memory <query>" },
    { cmd: "/help",   desc: "List all slash commands" },
  ];

  // ─────────────────────────────────────────────────────────────
  // DOM refs
  // ─────────────────────────────────────────────────────────────
  const messagesEl   = document.getElementById("messages");
  const inputEl      = document.getElementById("input");
  const sendBtn      = document.getElementById("send");
  const stopBtn      = document.getElementById("stop");
  const threadSelect = document.getElementById("thread-select");
  const modelSelect  = document.getElementById("model-select");
  const flowSelect   = document.getElementById("flow-select");
  const chipsEl      = document.getElementById("chips");
  const errorEl      = document.getElementById("error-banner");
  const noticeEl     = document.getElementById("notice-banner");
  const slashMenuEl  = document.getElementById("slash-menu");
  const statusMode   = document.getElementById("statusline-mode");
  const statusModel  = document.getElementById("statusline-model");
  const statusThread = document.getElementById("statusline-thread");
  const notifCard    = document.getElementById("notification-card");
  const notifText    = document.getElementById("notification-card-text");

  // ─────────────────────────────────────────────────────────────
  // Incoming messages from extension
  // ─────────────────────────────────────────────────────────────
  window.addEventListener("message", (e) => {
    const msg = e.data;
    switch (msg.type) {
      case "state":
        state = { ...state, ...msg.state };
        currentModel = state.defaultModel;
        render();
        break;
      case "messages":
        state.messages = msg.messages ?? [];
        renderMessages();
        break;
      case "streamStart":
        beginStreamedMessage(msg);
        break;
      case "streamDelta":
        appendDelta(msg.delta);
        break;
      case "swarmEvent":
        handleSwarmEvent(msg.event);
        break;
      case "streamEnd":
        finishStream(msg);
        break;
      case "streamError":
        showError(msg.error ?? "Stream failed");
        finishStream({ model: streamingEl?.dataset.model, failed: true });
        break;
      case "notice":
        showNotice(msg.text ?? "");
        break;
      case "themeChange": {
        const theme = String(msg.theme ?? "classic");
        const font  = String(msg.font ?? "system");
        const crt   = msg.crtEffects ? "on" : "off";
        document.body.dataset.theme = theme;
        document.body.dataset.font  = font;
        document.body.dataset.crt   = crt;
        let overlay = document.getElementById("crt-overlay");
        if (msg.crtEffects && !overlay) {
          overlay = document.createElement("div");
          overlay.className = "crt-overlay";
          overlay.id = "crt-overlay";
          document.body.insertBefore(overlay, document.body.firstChild);
        } else if (!msg.crtEffects && overlay) {
          overlay.remove();
        }
        break;
      }
      case "seed": {
        const seed = msg.seed ?? {};
        if (seed.contextBlock) {
          attachments.push({ kind: "context", label: "Editor selection", content: seed.contextBlock });
          renderChips();
        }
        if (seed.userText) {
          inputEl.value = seed.userText;
          inputEl.dispatchEvent(new Event("input"));
        }
        if (seed.flowHint && flowSelect) {
          flowSelect.value = seed.flowHint;
        }
        if (seed.run) {
          submit();
        } else {
          inputEl.focus();
        }
        break;
      }
      case "attachment":
        attachments.push(e.data.attachment);
        renderChips();
        break;
      case "showNotificationCard": {
        const count = msg.count ?? 0;
        const titles = Array.isArray(msg.summaryTitles) ? msg.summaryTitles : [];
        if (notifCard && notifText) {
          notifText.textContent = `${count} self-improvement suggestion${count !== 1 ? "s" : ""} pending: ${titles.slice(0, 2).join(", ")}${titles.length > 2 ? "..." : ""}`;
          notifCard.style.display = "flex";
          notifCard.dataset.count = String(count);
        }
        break;
      }
      case "filePicked": {
        const label = msg.label ?? msg.path ?? "file";
        const content = msg.content ?? "";
        attachments.push({ kind: "file", label, content });
        renderChips();
        break;
      }
    }
  });

  // ─────────────────────────────────────────────────────────────
  // Render
  // ─────────────────────────────────────────────────────────────
  function render() {
    renderThreadPicker();
    renderModelPicker();
    renderFlowPicker();
    renderMessages();
    updateStatusline();
  }

  function renderThreadPicker() {
    if (!threadSelect) return;
    const options = state.threads.map(
      (t) => `<option value="${esc(t.id)}">${esc((t.title || "New chat").slice(0, 40))}</option>`
    );
    threadSelect.innerHTML = options.join("");
    if (state.currentThreadId) threadSelect.value = state.currentThreadId;
    if (statusThread) {
      const cur = state.threads.find((t) => t.id === state.currentThreadId);
      statusThread.textContent = cur ? (cur.title || "New chat").slice(0, 20) : "-";
    }
  }

  function renderModelPicker() {
    if (!modelSelect) return;
    if (state.models.length === 0) {
      modelSelect.innerHTML = '<option value="">— no models —</option>';
      return;
    }
    const byBackend = new Map();
    for (const m of state.models) {
      const b = m.backend ?? "default";
      (byBackend.get(b) ?? byBackend.set(b, []).get(b)).push(m);
    }
    let html = "";
    for (const [backend, list] of byBackend.entries()) {
      html += `<optgroup label="${esc(backend)}">`;
      for (const m of list) {
        const flag = m.loaded ? "\u25cf " : "";
        html += `<option value="${esc(m.id)}">${flag}${esc(m.id)}</option>`;
      }
      html += "</optgroup>";
    }
    modelSelect.innerHTML = html;
    if (state.defaultModel) modelSelect.value = state.defaultModel;
    if (statusModel) statusModel.textContent = state.defaultModel ? state.defaultModel.split(":")[0].slice(0, 20) : "-";
  }

  function renderFlowPicker() {
    if (!flowSelect) return;
    const options = state.flows.map((f) => `<option value="${esc(f.name)}">${esc(f.name)}</option>`);
    flowSelect.innerHTML = '<option value="">raw</option>' + options.join("");
  }

  function updateStatusline() {
    if (!statusMode) return;
    if (generating) {
      statusMode.textContent = "GENERATING";
      statusMode.classList.add("generating");
    } else {
      statusMode.textContent = "IDLE";
      statusMode.classList.remove("generating");
    }
  }

  // ─────────────────────────────────────────────────────────────
  // Message rendering (P2 terminal-log style)
  // ─────────────────────────────────────────────────────────────
  function renderMessages() {
    if (!messagesEl) return;

    if (state.messages.length === 0) {
      vsEnabled = false;
      messagesEl.innerHTML = buildEmptyState();
      return;
    }

    // P2c: enable virtual scrolling for large threads
    if (state.messages.length >= VS_THRESHOLD) {
      renderVirtual(state.messages);
    } else {
      vsEnabled = false;
      disconnectVsObserver();
      messagesEl.innerHTML = state.messages.map(renderMessage).join("");
      attachCopyHandlers();
      scrollToBottom();
    }
  }

  function buildEmptyState() {
    return `<div class="empty-state">
      <h2>\u25b6 Yggdrasil Chat</h2>
      <p>Ask anything \u2014 powered by your local AI fleet.</p>
      <div class="shortcuts">
<code>/flow coding_swarm &lt;msg&gt;</code>  run a flow
<code>/model &lt;id&gt; &lt;msg&gt;</code>    override model
<code>/memory &lt;query&gt;</code>   inject memory context
<code>@</code>                 attach a file
<code>/help</code>             list all commands
      </div>
    </div>`;
  }

  function renderMessage(m) {
    const glyph = m.role === "user" ? "\u203a" : m.role === "assistant" ? "\u2039" : "\u2699";
    const meta = buildMetaRow(m);
    const content = renderMarkdown(m.content || "");
    return `<div class="msg ${esc(m.role)}" data-role="${esc(m.role)}">` +
      `<div class="msg-avatar" aria-hidden="true">${glyph}</div>` +
      `<div class="msg-body">` +
      (meta ? `<div class="msg-meta">${meta}</div>` : "") +
      `<div class="msg-content">${content}</div>` +
      `</div></div>`;
  }

  function buildMetaRow(m) {
    const parts = [];
    if (m.model)  parts.push(`<span class="tag model">${esc(m.model.split(":")[0])}</span>`);
    if (m.flow)   parts.push(`<span class="tag flow">${esc(m.flow)}</span>`);
    if (m.ts)     parts.push(`<span>${new Date(m.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</span>`);
    return parts.join("");
  }

  // ─────────────────────────────────────────────────────────────
  // Markdown renderer (P2 + P2a Prism integration)
  // ─────────────────────────────────────────────────────────────
  function renderMarkdown(text) {
    if (!text) return "";

    // Strip correction markers inserted by swarm refiner
    text = text.replace(/\n\n<!--correction-->\n\n/g, "\n\n");

    const parts = [];
    // Match yggdrasil-edit fences before standard fences
    const editFenceRe = /```yggdrasil-edit path=([^\s`]+)\n([\s\S]*?)```/g;
    const codeFenceRe = /```([a-zA-Z0-9_+\-]*)\n([\s\S]*?)```/g;

    // Unified fence pass — find all fences (edit first, then code)
    const allFences = [];
    let em;
    while ((em = editFenceRe.exec(text)) !== null) {
      allFences.push({ start: em.index, end: em.index + em[0].length, kind: "edit", path: em[1], code: em[2] });
    }
    while ((em = codeFenceRe.exec(text)) !== null) {
      // Skip if inside an already-captured edit fence
      const inside = allFences.some((f) => em.index >= f.start && em.index < f.end);
      if (!inside) {
        allFences.push({ start: em.index, end: em.index + em[0].length, kind: "code", lang: em[1], code: em[2] });
      }
    }
    allFences.sort((a, b) => a.start - b.start);

    let last = 0;
    for (const fence of allFences) {
      if (fence.start > last) {
        parts.push({ kind: "text", v: text.slice(last, fence.start) });
      }
      parts.push(fence);
      last = fence.end;
    }
    if (last < text.length) parts.push({ kind: "text", v: text.slice(last) });

    return parts.map((p) => {
      if (p.kind === "edit") {
        const highlighted = highlightCode(esc(p.code), pathToLang(p.path));
        return `<div class="codefence prism" data-path="${esc(p.path)}">` +
          `<button class="copy" data-copy>Copy</button>` +
          `<button class="apply-diff" data-apply-diff data-path="${esc(p.path)}">Apply diff</button>` +
          `<code class="language-${esc(pathToLang(p.path))}">${highlighted}</code>` +
          `</div>`;
      }
      if (p.kind === "code") {
        const lang = (p.lang || "").toLowerCase();
        const highlighted = highlightCode(esc(p.code), lang);
        return `<pre class="codefence${lang ? " prism" : ""}">` +
          `<button class="copy" data-copy>Copy</button>` +
          `<code class="language-${esc(lang)}">${highlighted}</code>` +
          `</pre>`;
      }
      return inlineMarkdown(p.v);
    }).join("");
  }

  // Prism highlighting — lazy per-language loading (P2a)
  const prismLangsLoaded = new Set();
  function highlightCode(escapedCode, lang) {
    // Prism not yet loaded — return escaped plain text
    if (typeof window.Prism === "undefined") return escapedCode;
    const g = window.Prism.languages[lang];
    if (!g) return escapedCode;
    try {
      // Prism.highlight expects unescaped source — we must unescape first
      const raw = unescHtml(escapedCode);
      return window.Prism.highlight(raw, g, lang);
    } catch (_e) {
      return escapedCode;
    }
  }

  function unescHtml(s) {
    return s.replace(/&amp;/g, "&").replace(/&lt;/g, "<").replace(/&gt;/g, ">")
            .replace(/&quot;/g, '"').replace(/&#39;/g, "'");
  }

  function pathToLang(path) {
    const ext = (path || "").split(".").pop().toLowerCase();
    const map = {
      rs: "rust", go: "go", py: "python", ts: "typescript", js: "javascript",
      json: "json", toml: "toml", yaml: "yaml", yml: "yaml", sh: "bash",
      sql: "sql", md: "markdown",
    };
    return map[ext] || ext || "plaintext";
  }

  function inlineMarkdown(text) {
    const lines = text.split("\n");
    const out = [];
    let inList = false;
    for (const raw of lines) {
      const bullet = raw.match(/^\s*[-*]\s+(.*)$/);
      if (bullet) {
        if (!inList) { out.push("<ul>"); inList = true; }
        out.push(`<li>${formatInline(bullet[1])}</li>`);
        continue;
      }
      if (inList) { out.push("</ul>"); inList = false; }
      if (raw.trim() === "") {
        out.push("");
      } else {
        out.push(`<p>${formatInline(raw)}</p>`);
      }
    }
    if (inList) out.push("</ul>");
    return out.join("");
  }

  function formatInline(s) {
    let x = esc(s);
    x = x.replace(/`([^`]+)`/g, (_m, c) => `<code class="inline">${c}</code>`);
    x = x.replace(/\*\*([^*]+)\*\*/g, (_m, c) => `<strong>${c}</strong>`);
    x = x.replace(/(^|[^*])\*([^*\n]+)\*/g, (_m, pre, c) => `${pre}<em>${c}</em>`);
    return x;
  }

  function attachCopyHandlers() {
    document.querySelectorAll("[data-copy]").forEach((btn) => {
      btn.addEventListener("click", (e) => {
        const fence = e.currentTarget.closest(".codefence, pre");
        const code = fence?.querySelector("code")?.textContent ?? "";
        vscode.postMessage({ type: "copy", text: code });
        btn.textContent = "Copied";
        setTimeout(() => (btn.textContent = "Copy"), 1200);
      });
    });
    document.querySelectorAll("[data-apply-diff]").forEach((btn) => {
      btn.addEventListener("click", (e) => {
        const fence = e.currentTarget.closest(".codefence");
        const path = fence?.dataset.path ?? btn.dataset.path ?? "";
        const proposed = fence?.querySelector("code")?.textContent ?? "";
        vscode.postMessage({ type: "previewDiff", path, proposed });
      });
    });
  }

  // ─────────────────────────────────────────────────────────────
  // P2c — Virtual scrolling
  // ─────────────────────────────────────────────────────────────
  function renderVirtual(messages) {
    vsEnabled = true;
    vsMessages = messages;

    if (vsHeights.length !== messages.length) {
      vsHeights = new Array(messages.length).fill(vsAvgHeight);
    }

    // Always render tail (streaming message must be visible)
    vsWindowEnd = messages.length;
    vsWindowStart = Math.max(0, vsWindowEnd - VS_BUFFER * 2);

    buildVirtualDom();
    scrollToBottom();
    setupVsObserver();
  }

  function buildVirtualDom() {
    if (!messagesEl) return;
    const topHeight = vsHeights.slice(0, vsWindowStart).reduce((s, h) => s + h, 0);
    const botHeight = vsHeights.slice(vsWindowEnd).reduce((s, h) => s + h, 0);

    let html = "";
    if (topHeight > 0) {
      html += `<div class="msg-placeholder" id="vs-top-sentinel" style="height:${topHeight}px"></div>`;
    } else {
      html += `<div id="vs-top-sentinel" style="height:1px"></div>`;
    }

    for (let i = vsWindowStart; i < vsWindowEnd && i < vsMessages.length; i++) {
      html += `<div data-vs-idx="${i}">${renderMessage(vsMessages[i])}</div>`;
    }

    if (botHeight > 0) {
      html += `<div class="msg-placeholder" id="vs-bot-sentinel" style="height:${botHeight}px"></div>`;
    } else {
      html += `<div id="vs-bot-sentinel" style="height:1px"></div>`;
    }

    messagesEl.innerHTML = html;
    attachCopyHandlers();

    // Sample rendered heights for estimation
    messagesEl.querySelectorAll("[data-vs-idx]").forEach((el) => {
      const idx = parseInt(el.dataset.vsIdx, 10);
      if (!isNaN(idx)) {
        vsHeights[idx] = el.offsetHeight || vsAvgHeight;
      }
    });
    updateVsAvgHeight();
  }

  function updateVsAvgHeight() {
    const sample = vsHeights.filter((h) => h > 0);
    if (sample.length > 0) vsAvgHeight = sample.reduce((a, b) => a + b, 0) / sample.length;
  }

  function setupVsObserver() {
    disconnectVsObserver();
    vsObserver = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        if (entry.target.id === "vs-top-sentinel" && vsWindowStart > 0) {
          vsWindowStart = Math.max(0, vsWindowStart - VS_BUFFER);
          buildVirtualDom();
          setupVsObserver();
        }
        if (entry.target.id === "vs-bot-sentinel" && vsWindowEnd < vsMessages.length) {
          vsWindowEnd = Math.min(vsMessages.length, vsWindowEnd + VS_BUFFER);
          buildVirtualDom();
          setupVsObserver();
        }
      }
    }, { root: messagesEl, rootMargin: "200px" });

    const top = document.getElementById("vs-top-sentinel");
    const bot = document.getElementById("vs-bot-sentinel");
    if (top) vsObserver.observe(top);
    if (bot) vsObserver.observe(bot);
  }

  function disconnectVsObserver() {
    if (vsObserver) { vsObserver.disconnect(); vsObserver = null; }
  }

  // ─────────────────────────────────────────────────────────────
  // Streaming (P2 terminal-log DOM)
  // ─────────────────────────────────────────────────────────────
  function beginStreamedMessage(meta) {
    generating = true;
    updateButtons();
    updateStatusline();
    clearError();
    streamBuf = "";
    thinkingFoldEl = null;
    currentThinkStep = null;
    assistantHasStreamed = false;

    const emptyState = messagesEl.querySelector(".empty-state");
    if (emptyState) emptyState.remove();

    const wrapper = document.createElement("div");
    wrapper.className = "msg assistant streaming";
    wrapper.dataset.model = meta.model ?? "";
    const glyph = "\u2039"; // ‹
    const metaHtml = buildMetaRow({ model: meta.model, flow: meta.flow, ts: Date.now() });
    wrapper.innerHTML =
      `<div class="msg-avatar" aria-hidden="true">${glyph}</div>` +
      `<div class="msg-body">` +
      (metaHtml ? `<div class="msg-meta">${metaHtml}</div>` : "") +
      `<div class="msg-content"></div>` +
      `</div>`;
    messagesEl.appendChild(wrapper);
    streamingEl = wrapper;
    scrollToBottom();
  }

  function appendDelta(delta) {
    if (!streamingEl) return;
    streamBuf += delta;
    assistantHasStreamed = true;
    const content = streamingEl.querySelector(".msg-content");
    if (content) content.innerHTML = renderMarkdown(streamBuf);
    scrollToBottom();
  }

  // Sprint 061 swarm-flow thinking fold — re-skinned for terminal aesthetic.
  // Logic preserved exactly; only glyph/class changes.
  function handleSwarmEvent(ev) {
    if (!streamingEl || !ev || typeof ev.phase !== "string") return;

    if (ev.phase === "step_start") {
      if (ev.role === "swarm_thinking") {
        ensureThinkingFold();
        const section = document.createElement("div");
        section.className = "thinking-step active";
        section.dataset.step = String(ev.step || "");
        section.innerHTML =
          `<div class="thinking-label">\u25b6 ${esc(ev.label || ev.step || "\u2026")}</div>` +
          `<div class="thinking-body"></div>`;
        thinkingFoldEl.querySelector(".fold-body").appendChild(section);
        currentThinkStep = section;
      } else if (ev.role === "assistant") {
        if (assistantHasStreamed) {
          streamBuf += "\n\n<!--correction-->\n\n";
          const content = streamingEl.querySelector(".msg-content");
          if (content) {
            const divider = document.createElement("div");
            divider.className = "correction-divider";
            divider.textContent = `\u2500\u2500 ${ev.label || "correction"} \u2500\u2500`;
            content.appendChild(divider);
          }
        }
      }
      scrollToBottom();
      return;
    }

    if (ev.phase === "step_delta" && ev.role === "swarm_thinking") {
      if (!currentThinkStep) {
        ensureThinkingFold();
        const section = document.createElement("div");
        section.className = "thinking-step active";
        section.dataset.step = String(ev.step || "");
        section.innerHTML =
          `<div class="thinking-label">\u25b6 ${esc(ev.step || "\u2026")}</div>` +
          `<div class="thinking-body"></div>`;
        thinkingFoldEl.querySelector(".fold-body").appendChild(section);
        currentThinkStep = section;
      }
      const body = currentThinkStep.querySelector(".thinking-body");
      if (body) body.textContent += String(ev.content || "");
      scrollToBottom();
      return;
    }

    if (ev.phase === "step_end") {
      if (currentThinkStep && currentThinkStep.dataset.step === String(ev.step || "")) {
        currentThinkStep.classList.remove("active");
        currentThinkStep = null;
      }
      return;
    }

    if (ev.phase === "error") {
      if (thinkingFoldEl) {
        const err = document.createElement("div");
        err.className = "thinking-error";
        err.textContent = `error in ${ev.step || "flow"}: ${ev.message}`;
        thinkingFoldEl.querySelector(".fold-body").appendChild(err);
      }
      showError(ev.message || "swarm flow error");
      return;
    }

    if (ev.phase === "done") {
      if (currentThinkStep) {
        currentThinkStep.classList.remove("active");
        currentThinkStep = null;
      }
    }
  }

  function ensureThinkingFold() {
    if (thinkingFoldEl || !streamingEl) return;
    const body = streamingEl.querySelector(".msg-body");
    if (!body) return;
    const fold = document.createElement("details");
    fold.className = "thinking-fold";
    fold.innerHTML = `<summary>thinking</summary><div class="fold-body"></div>`;
    body.insertBefore(fold, body.querySelector(".msg-content"));
    thinkingFoldEl = fold;
  }

  function finishStream(meta) {
    if (streamingEl) {
      streamingEl.classList.remove("streaming");
      const content = streamingEl.querySelector(".msg-content");
      if (content) content.innerHTML = renderMarkdown(streamBuf);
      attachCopyHandlers();
      streamingEl = null;
    }
    streamBuf = "";
    thinkingFoldEl = null;
    currentThinkStep = null;
    assistantHasStreamed = false;
    generating = false;
    updateButtons();
    updateStatusline();
  }

  // ─────────────────────────────────────────────────────────────
  // Slash-command autocomplete (P2)
  // ─────────────────────────────────────────────────────────────
  function openSlashMenu(prefix) {
    if (!slashMenuEl) return;
    const q = prefix.toLowerCase();
    const matches = SLASH_COMMANDS.filter((c) => c.cmd.startsWith(q));
    if (matches.length === 0) { closeSlashMenu(); return; }

    slashMenuActive = true;
    slashMenuIdx = 0;
    slashMenuEl.innerHTML = matches.map((c, i) =>
      `<div class="slash-menu-item" role="option" data-cmd="${esc(c.cmd)}" aria-selected="${i === 0 ? 'true' : 'false'}">` +
      `<span class="cmd">${esc(c.cmd)}</span><span class="desc">${esc(c.desc)}</span></div>`
    ).join("");
    slashMenuEl.style.display = "block";

    slashMenuEl.querySelectorAll(".slash-menu-item").forEach((item, i) => {
      item.addEventListener("click", () => {
        applySlashItem(matches[i].cmd);
      });
    });
  }

  function closeSlashMenu() {
    slashMenuActive = false;
    slashMenuIdx = 0;
    if (slashMenuEl) slashMenuEl.style.display = "none";
  }

  function navigateSlashMenu(dir) {
    if (!slashMenuEl || !slashMenuActive) return;
    const items = slashMenuEl.querySelectorAll(".slash-menu-item");
    if (items.length === 0) return;
    items[slashMenuIdx]?.setAttribute("aria-selected", "false");
    slashMenuIdx = (slashMenuIdx + dir + items.length) % items.length;
    items[slashMenuIdx]?.setAttribute("aria-selected", "true");
  }

  function applySlashItem(cmd) {
    if (!inputEl) return;
    inputEl.value = cmd + " ";
    closeSlashMenu();
    inputEl.focus();
  }

  // ─────────────────────────────────────────────────────────────
  // Input + submission
  // ─────────────────────────────────────────────────────────────
  inputEl?.addEventListener("input", () => {
    // Auto-grow textarea
    inputEl.style.height = "auto";
    inputEl.style.height = Math.min(inputEl.scrollHeight, 200) + "px";

    const val = inputEl.value;
    const pos = inputEl.selectionStart ?? 0;

    // Slash autocomplete: '/' at caret position 0 (or beginning of value)
    if (val.startsWith("/") && pos <= val.length) {
      openSlashMenu(val);
    } else {
      closeSlashMenu();
    }

    // '@' detection: at caret 0 on a fresh '@' token → request file picker
    if (val === "@" || (val.endsWith("@") && pos === val.length && (pos === 1 || val[pos - 2] === " "))) {
      vscode.postMessage({ type: "requestFilePicker", prefix: val.slice(0, pos) });
    }
  });

  inputEl?.addEventListener("keydown", (e) => {
    if (slashMenuActive) {
      if (e.key === "ArrowDown")  { e.preventDefault(); navigateSlashMenu(1); return; }
      if (e.key === "ArrowUp")    { e.preventDefault(); navigateSlashMenu(-1); return; }
      if (e.key === "Tab" || e.key === "Enter") {
        e.preventDefault();
        const items = slashMenuEl?.querySelectorAll(".slash-menu-item");
        const active = items?.[slashMenuIdx];
        if (active) applySlashItem(active.dataset.cmd ?? "");
        return;
      }
      if (e.key === "Escape") { closeSlashMenu(); return; }
    }

    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      submit();
    }
  });

  sendBtn?.addEventListener("click", () => submit());
  stopBtn?.addEventListener("click", () => {
    vscode.postMessage({ type: "stop" });
  });

  function submit() {
    if (generating) return;
    const text = inputEl.value.trim();
    if (!text) return;

    closeSlashMenu();

    vscode.postMessage({
      type: "send",
      text,
      model: modelSelect?.value || null,
      flow: flowSelect?.value || null,
      attachments,
    });

    inputEl.value = "";
    inputEl.style.height = "auto";
    attachments = [];
    renderChips();
  }

  // ─────────────────────────────────────────────────────────────
  // Thread + tool buttons
  // ─────────────────────────────────────────────────────────────
  threadSelect?.addEventListener("change", () => {
    const val = threadSelect.value;
    vscode.postMessage({ type: "switchThread", id: val });
  });

  document.getElementById("new-thread")?.addEventListener("click", () => {
    vscode.postMessage({ type: "newThread" });
  });

  document.getElementById("clear-thread")?.addEventListener("click", () => {
    if (confirm("Clear messages in this thread?")) {
      vscode.postMessage({ type: "clearThread" });
    }
  });

  document.getElementById("delete-thread")?.addEventListener("click", () => {
    if (confirm("Delete this thread permanently?")) {
      vscode.postMessage({ type: "deleteThread" });
    }
  });

  document.getElementById("attach-file")?.addEventListener("click", () => {
    vscode.postMessage({ type: "attachFile" });
  });

  document.getElementById("attach-selection")?.addEventListener("click", () => {
    vscode.postMessage({ type: "attachSelection" });
  });

  // ─────────────────────────────────────────────────────────────
  // Notification card actions (P3b)
  // ─────────────────────────────────────────────────────────────
  document.getElementById("notif-view")?.addEventListener("click", () => {
    vscode.postMessage({ type: "notifView" });
    if (notifCard) notifCard.style.display = "none";
  });

  document.getElementById("notif-snooze")?.addEventListener("click", () => {
    vscode.postMessage({ type: "notifSnooze" });
    if (notifCard) notifCard.style.display = "none";
  });

  document.getElementById("notif-dismiss")?.addEventListener("click", () => {
    vscode.postMessage({ type: "notifDismiss" });
    if (notifCard) notifCard.style.display = "none";
  });

  // ─────────────────────────────────────────────────────────────
  // Chips
  // ─────────────────────────────────────────────────────────────
  function renderChips() {
    if (!chipsEl) return;
    chipsEl.innerHTML = attachments
      .map((a, i) =>
        `<span class="chip">${esc(a.label)} <span class="close" data-rm="${i}" role="button" aria-label="Remove attachment" tabindex="0">\u00d7</span></span>`
      )
      .join("");
    chipsEl.querySelectorAll("[data-rm]").forEach((x) => {
      const handleRemove = () => {
        const idx = Number(x.dataset.rm);
        attachments.splice(idx, 1);
        renderChips();
      };
      x.addEventListener("click", handleRemove);
      x.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") handleRemove(); });
    });
  }

  function updateButtons() {
    if (sendBtn) sendBtn.style.display = generating ? "none" : "";
    if (stopBtn) stopBtn.style.display = generating ? "" : "none";
  }

  function showError(msg) {
    if (!errorEl) return;
    errorEl.textContent = msg;
    errorEl.classList.add("show");
    setTimeout(() => errorEl.classList.remove("show"), 8000);
  }

  function clearError() {
    errorEl?.classList.remove("show");
  }

  function showNotice(text) {
    if (!noticeEl) return;
    noticeEl.textContent = text;
    noticeEl.classList.add("show");
    setTimeout(() => noticeEl.classList.remove("show"), 6000);
  }

  function scrollToBottom() {
    if (messagesEl) messagesEl.scrollTop = messagesEl.scrollHeight;
  }

  function esc(s) {
    return String(s ?? "").replace(/[&<>"']/g, (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
    );
  }

  // ─────────────────────────────────────────────────────────────
  // Init
  // ─────────────────────────────────────────────────────────────
  updateButtons();
  updateStatusline();
  vscode.postMessage({ type: "ready" });
})();
