/* Yggdrasil chat — client-side.
   Streams assistant tokens from the extension host, renders a compact
   markdown subset (code fences, inline code, bold, italic, lists),
   manages attachments, thread pickers, and model/flow overrides. */

(function () {
  const vscode = acquireVsCodeApi();

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

  // ─────────────────────────────────────────────────────────────
  // DOM refs
  // ─────────────────────────────────────────────────────────────
  const messagesEl = document.getElementById("messages");
  const inputEl = document.getElementById("input");
  const sendBtn = document.getElementById("send");
  const stopBtn = document.getElementById("stop");
  const threadSelect = document.getElementById("thread-select");
  const modelSelect = document.getElementById("model-select");
  const flowSelect = document.getElementById("flow-select");
  const chipsEl = document.getElementById("chips");
  const errorEl = document.getElementById("error-banner");
  const noticeEl = document.getElementById("notice-banner");

  // ─────────────────────────────────────────────────────────────
  // Incoming messages from extension
  // ─────────────────────────────────────────────────────────────
  window.addEventListener("message", (e) => {
    const msg = e.data;
    switch (msg.type) {
      case "state":
        state = { ...state, ...msg.state };
        render();
        break;
      case "messages":
        state.messages = msg.messages ?? [];
        render();
        break;
      case "streamStart":
        beginStreamedMessage(msg);
        break;
      case "streamDelta":
        appendDelta(msg.delta);
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
  }

  function renderThreadPicker() {
    if (!threadSelect) return;
    const options = state.threads.map(
      (t) => `<option value="${t.id}">${esc(t.title || "New chat")}</option>`
    );
    threadSelect.innerHTML = '<option value="__new">+ New thread</option>' + options.join("");
    if (state.currentThreadId) threadSelect.value = state.currentThreadId;
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
        const flag = m.loaded ? "● " : "";
        html += `<option value="${esc(m.id)}">${flag}${esc(m.id)}</option>`;
      }
      html += "</optgroup>";
    }
    modelSelect.innerHTML = html;
    if (state.defaultModel) modelSelect.value = state.defaultModel;
  }

  function renderFlowPicker() {
    if (!flowSelect) return;
    const options = state.flows.map((f) => `<option value="${esc(f.name)}">${esc(f.name)}</option>`);
    flowSelect.innerHTML = '<option value="">raw chat</option>' + options.join("");
  }

  function renderMessages() {
    if (!messagesEl) return;
    if (state.messages.length === 0) {
      messagesEl.innerHTML = `
        <div class="empty-state">
          <h2>Yggdrasil Chat</h2>
          <p>Ask anything — powered by your local AI fleet. Pick a model above, or run a flow for multi-step workflows.</p>
          <div class="shortcuts">
<code>/flow coding_swarm &lt;msg&gt;</code> — run a flow
<code>/model &lt;id&gt; &lt;msg&gt;</code> — override model
<code>/memory &lt;query&gt;</code> — inject memory
<code>/help</code> — list commands
          </div>
        </div>`;
      return;
    }
    messagesEl.innerHTML = state.messages.map(renderMessage).join("");
    attachCopyHandlers();
    scrollToBottom();
  }

  function renderMessage(m) {
    const avatar = m.role === "user" ? "U" : m.role === "assistant" ? "AI" : "S";
    const meta = [
      m.model ? `<span class="tag model">${esc(m.model)}</span>` : "",
      m.flow ? `<span class="tag flow">flow: ${esc(m.flow)}</span>` : "",
      m.ts ? `<span>${new Date(m.ts).toLocaleTimeString()}</span>` : "",
    ]
      .filter(Boolean)
      .join("");
    return `<div class="msg ${m.role}">
      <div class="msg-avatar">${avatar}</div>
      <div class="msg-body">
        ${meta ? `<div class="msg-meta">${meta}</div>` : ""}
        <div class="msg-content">${renderMarkdown(m.content)}</div>
      </div>
    </div>`;
  }

  // Minimal markdown — fences, inline code, bold, italic, bullets.
  function renderMarkdown(text) {
    if (!text) return "";
    const parts = [];
    const fenceRe = /```([a-zA-Z0-9_+\-]*)\n([\s\S]*?)```/g;
    let last = 0;
    let m;
    while ((m = fenceRe.exec(text)) !== null) {
      if (m.index > last) parts.push({ kind: "text", v: text.slice(last, m.index) });
      parts.push({ kind: "code", lang: m[1], v: m[2] });
      last = m.index + m[0].length;
    }
    if (last < text.length) parts.push({ kind: "text", v: text.slice(last) });

    return parts
      .map((p) => {
        if (p.kind === "code") {
          return `<pre><button class="copy" data-copy>Copy</button><code data-lang="${esc(p.lang)}">${esc(p.v)}</code></pre>`;
        }
        return inlineMarkdown(p.v);
      })
      .join("");
  }

  function inlineMarkdown(text) {
    const lines = text.split("\n");
    const out = [];
    let inList = false;
    for (const raw of lines) {
      const line = raw;
      const bullet = line.match(/^\s*[-*]\s+(.*)$/);
      if (bullet) {
        if (!inList) {
          out.push("<ul>");
          inList = true;
        }
        out.push(`<li>${formatInline(bullet[1])}</li>`);
        continue;
      }
      if (inList) {
        out.push("</ul>");
        inList = false;
      }
      if (line.trim() === "") {
        out.push("");
      } else {
        out.push(`<p>${formatInline(line)}</p>`);
      }
    }
    if (inList) out.push("</ul>");
    return out.join("");
  }

  function formatInline(s) {
    // escape first, then apply inline tokens on the escaped string
    let x = esc(s);
    x = x.replace(/`([^`]+)`/g, (_m, c) => `<code class="inline">${c}</code>`);
    x = x.replace(/\*\*([^*]+)\*\*/g, (_m, c) => `<strong>${c}</strong>`);
    x = x.replace(/(^|[^*])\*([^*\n]+)\*/g, (_m, pre, c) => `${pre}<em>${c}</em>`);
    return x;
  }

  function attachCopyHandlers() {
    document.querySelectorAll("[data-copy]").forEach((btn) => {
      btn.addEventListener("click", (e) => {
        const pre = e.currentTarget.closest("pre");
        const code = pre?.querySelector("code")?.textContent ?? "";
        vscode.postMessage({ type: "copy", text: code });
        btn.textContent = "Copied";
        setTimeout(() => (btn.textContent = "Copy"), 1200);
      });
    });
  }

  // ─────────────────────────────────────────────────────────────
  // Streaming
  // ─────────────────────────────────────────────────────────────
  function beginStreamedMessage(meta) {
    generating = true;
    updateButtons();
    clearError();
    streamBuf = "";

    const emptyState = messagesEl.querySelector(".empty-state");
    if (emptyState) emptyState.remove();

    const wrapper = document.createElement("div");
    wrapper.className = "msg assistant streaming";
    wrapper.dataset.model = meta.model ?? "";
    wrapper.innerHTML = `<div class="msg-avatar">AI</div>
      <div class="msg-body">
        <div class="msg-meta">
          <span class="tag model">${esc(meta.model ?? "?")}</span>
          ${meta.flow ? `<span class="tag flow">flow: ${esc(meta.flow)}</span>` : ""}
        </div>
        <div class="msg-content"></div>
      </div>`;
    messagesEl.appendChild(wrapper);
    streamingEl = wrapper;
    scrollToBottom();
  }

  function appendDelta(delta) {
    if (!streamingEl) return;
    streamBuf += delta;
    const content = streamingEl.querySelector(".msg-content");
    if (content) content.innerHTML = renderMarkdown(streamBuf);
    scrollToBottom();
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
    generating = false;
    updateButtons();
  }

  // ─────────────────────────────────────────────────────────────
  // Input + submission
  // ─────────────────────────────────────────────────────────────
  inputEl?.addEventListener("input", () => {
    // Auto-grow textarea
    inputEl.style.height = "auto";
    inputEl.style.height = Math.min(inputEl.scrollHeight, 240) + "px";
  });

  inputEl?.addEventListener("keydown", (e) => {
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
    if (val === "__new") {
      vscode.postMessage({ type: "newThread" });
    } else {
      vscode.postMessage({ type: "switchThread", id: val });
    }
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

  function renderChips() {
    if (!chipsEl) return;
    chipsEl.innerHTML = attachments
      .map(
        (a, i) =>
          `<span class="chip">${esc(a.label)} <span class="close" data-rm="${i}">×</span></span>`
      )
      .join("");
    chipsEl.querySelectorAll("[data-rm]").forEach((x) => {
      x.addEventListener("click", () => {
        const idx = Number(x.dataset.rm);
        attachments.splice(idx, 1);
        renderChips();
      });
    });
  }

  // Listen for seed/attachments from the extension
  window.addEventListener("message", (e) => {
    if (e.data?.type === "attachment") {
      attachments.push(e.data.attachment);
      renderChips();
    }
  });

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

  // Initial render + ready signal
  updateButtons();
  vscode.postMessage({ type: "ready" });
})();
