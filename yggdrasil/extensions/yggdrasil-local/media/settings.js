/* Yggdrasil settings panel — client-side.
   Communicates with the extension host for: endpoints config, flow CRUD,
   notifications toggles, secrets (via SecretStorage). */

(function () {
  const vscode = acquireVsCodeApi();

  // ─────────────────────────────────────────────────────────────
  // State (kept minimal — source of truth is the extension host)
  // ─────────────────────────────────────────────────────────────
  let state = {
    endpoints: {},
    notifications: { enabled: true, sound: false, events: [] },
    hooks: { managed: true },
    flows: [],
    models: [],
    backends: [],
    secrets: {},
  };
  let currentFlowName = null;
  let currentFlowDraft = null;
  let dirty = false;

  // ─────────────────────────────────────────────────────────────
  // Tab switching
  // ─────────────────────────────────────────────────────────────
  document.querySelectorAll(".tab-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      const target = btn.dataset.tab;
      document.querySelectorAll(".tab-btn").forEach((b) => b.classList.remove("active"));
      document.querySelectorAll(".tab").forEach((t) => t.classList.remove("active"));
      btn.classList.add("active");
      document.getElementById("tab-" + target)?.classList.add("active");
    });
  });

  // ─────────────────────────────────────────────────────────────
  // Incoming messages
  // ─────────────────────────────────────────────────────────────
  window.addEventListener("message", (e) => {
    const msg = e.data;
    switch (msg.type) {
      case "state":
        state = { ...state, ...msg.state };
        renderAll();
        break;
      case "flowLoaded":
        currentFlowName = msg.flow?.name ?? null;
        currentFlowDraft = msg.flow ? JSON.parse(JSON.stringify(msg.flow)) : null;
        dirty = false;
        renderFlowEditor();
        updateDirtyIndicator();
        break;
      case "testResult":
        setTestResult(msg.endpoint, msg.ok, msg.detail);
        break;
      case "toast":
        showToast(msg.message, msg.kind ?? "ok");
        break;
      case "secretUpdated":
        state.secrets[msg.key] = msg.set === true;
        renderSecrets();
        break;
    }
  });

  // ─────────────────────────────────────────────────────────────
  // ENDPOINTS tab
  // ─────────────────────────────────────────────────────────────
  function renderEndpoints() {
    const e = state.endpoints;
    setVal("odinUrl", e.odinUrl);
    setVal("mimirUrl", e.mimirUrl);
    setVal("huginUrl", e.huginUrl);
    setVal("giteaUrl", e.giteaUrl);
    setVal("giteaRepo", e.giteaRepo);
    setChecked("autoUpdate", e.autoUpdateEnabled);
  }

  document.getElementById("save-endpoints")?.addEventListener("click", () => {
    vscode.postMessage({
      type: "saveEndpoints",
      endpoints: {
        odinUrl: getVal("odinUrl"),
        mimirUrl: getVal("mimirUrl"),
        huginUrl: getVal("huginUrl"),
        giteaUrl: getVal("giteaUrl"),
        giteaRepo: getVal("giteaRepo"),
        autoUpdateEnabled: getChecked("autoUpdate"),
      },
    });
  });

  document.querySelectorAll("[data-test]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const endpoint = btn.dataset.test;
      setTestResult(endpoint, null, "testing…");
      vscode.postMessage({ type: "testEndpoint", endpoint, url: getVal(endpoint) });
    });
  });

  function setTestResult(endpoint, ok, detail) {
    const el = document.getElementById("test-" + endpoint);
    if (!el) return;
    el.className = "test-result " + (ok === null ? "pending" : ok ? "ok" : "fail");
    el.textContent = detail ?? (ok ? "reachable" : "unreachable");
  }

  // ─────────────────────────────────────────────────────────────
  // FLOWS tab
  // ─────────────────────────────────────────────────────────────
  function renderFlowList() {
    const select = document.getElementById("flow-select");
    if (!select) return;
    select.innerHTML = '<option value="">— pick a flow —</option>';
    for (const f of state.flows) {
      const opt = document.createElement("option");
      opt.value = f.name;
      opt.textContent = f.name;
      if (f.name === currentFlowName) opt.selected = true;
      select.appendChild(opt);
    }
  }

  document.getElementById("flow-select")?.addEventListener("change", (e) => {
    if (dirty && !confirm("Discard unsaved changes to " + currentFlowName + "?")) {
      e.target.value = currentFlowName ?? "";
      return;
    }
    const name = e.target.value;
    if (!name) {
      currentFlowName = null;
      currentFlowDraft = null;
      renderFlowEditor();
      return;
    }
    vscode.postMessage({ type: "loadFlow", name });
  });

  function renderFlowEditor() {
    const container = document.getElementById("flow-editor");
    if (!container) return;
    if (!currentFlowDraft) {
      container.innerHTML =
        '<div class="empty-state">Pick a flow above to edit its steps, prompts, and parameters.</div>';
      return;
    }

    const flow = currentFlowDraft;
    const modelOptions = buildModelOptions();
    const backendOptions = state.backends.length > 0
      ? state.backends.map((b) => `<option value="${esc(b)}">${esc(b)}</option>`).join("")
      : ["munin-ollama", "hugin-ollama", "morrigan"]
          .map((b) => `<option value="${b}">${b}</option>`)
          .join("");

    let html = "";

    if (flow.loop_config) {
      const lc = flow.loop_config;
      html += `<div class="step-card">
        <div class="step-head">
          <span class="step-name">Loop Configuration</span>
        </div>
        <div class="step-grid">
          <div>
            <label>max_iterations</label>
            <input type="number" data-loop-field="max_iterations" value="${esc(lc.max_iterations ?? 3)}">
          </div>
          <div>
            <label>convergence_pattern</label>
            <input type="text" data-loop-field="convergence_pattern" value="${esc(lc.convergence_pattern ?? '')}">
          </div>
          <div>
            <label>check_step</label>
            <input type="text" data-loop-field="check_step" value="${esc(lc.check_step ?? '')}">
          </div>
          <div>
            <label>restart_from_step</label>
            <input type="text" data-loop-field="restart_from_step" value="${esc(lc.restart_from_step ?? '')}">
          </div>
        </div>
      </div>`;
    }

    flow.steps.forEach((step, i) => {
      const inputTemplate = stepInputTemplate(step);
      html += `<div class="step-card" data-step-idx="${i}">
        <div class="step-head">
          <div>
            <span class="step-num">${i + 1}</span>
            <span class="step-name">${esc(step.name)}</span>
          </div>
        </div>
        <div class="step-grid">
          <div>
            <label>Backend</label>
            <select data-field="backend">${backendOptions.replace(
              `value="${esc(step.backend ?? '')}"`,
              `value="${esc(step.backend ?? '')}" selected`
            )}</select>
          </div>
          <div>
            <label>Model</label>
            <select data-field="model">${modelOptions}</select>
          </div>
          <div class="full">
            <label>System Prompt</label>
            <textarea data-field="system_prompt" rows="5">${esc(step.system_prompt ?? '')}</textarea>
          </div>
          <div class="full">
            <label>Input Template</label>
            <textarea data-field="input_template" rows="3">${esc(inputTemplate)}</textarea>
          </div>
          <div>
            <label>Temperature</label>
            <input type="number" step="0.05" min="0" max="2" data-field="temperature" value="${esc(step.temperature ?? 0.2)}">
          </div>
          <div>
            <label>Max Tokens</label>
            <input type="number" step="256" min="64" data-field="max_tokens" value="${esc(step.max_tokens ?? 2048)}">
          </div>
          <div>
            <label>Think</label>
            <input type="checkbox" data-field="think" ${step.think ? "checked" : ""}>
          </div>
          <div>
            <label>Output Key</label>
            <input type="text" data-field="output_key" value="${esc(step.output_key ?? '')}">
          </div>
        </div>
      </div>`;
    });

    container.innerHTML = html;

    // Set selected model per step (after innerHTML, because option values are dynamic)
    flow.steps.forEach((step, i) => {
      const card = container.querySelector(`[data-step-idx="${i}"]`);
      if (!card) return;
      const sel = card.querySelector('select[data-field="model"]');
      if (sel && step.model) {
        sel.value = step.model;
        if (sel.value !== step.model) {
          const opt = document.createElement("option");
          opt.value = step.model;
          opt.textContent = step.model + " (manual)";
          sel.appendChild(opt);
          sel.value = step.model;
        }
      }
    });

    // Wire change handlers
    container.querySelectorAll("[data-field]").forEach((el) => {
      el.addEventListener("input", () => markDirty());
      el.addEventListener("change", () => markDirty());
    });
    container.querySelectorAll("[data-loop-field]").forEach((el) => {
      el.addEventListener("input", () => markDirty());
      el.addEventListener("change", () => markDirty());
    });
  }

  function buildModelOptions() {
    const models = state.models ?? [];
    if (models.length === 0) {
      return '<option value="">— no models available —</option>';
    }
    const grouped = new Map();
    for (const m of models) {
      const b = m.backend ?? "default";
      const list = grouped.get(b) ?? [];
      list.push(m);
      grouped.set(b, list);
    }
    let html = '<option value="">— select model —</option>';
    for (const [backend, list] of grouped.entries()) {
      html += `<optgroup label="${esc(backend)}">`;
      for (const m of list) {
        const flag = m.loaded ? "● " : "";
        html += `<option value="${esc(m.id)}">${flag}${esc(m.id)}</option>`;
      }
      html += "</optgroup>";
    }
    return html;
  }

  function stepInputTemplate(step) {
    if (!step.input) return "";
    if (typeof step.input === "string") return step.input;
    if (typeof step.input === "object" && step.input.template?.template) {
      return step.input.template.template;
    }
    try {
      return JSON.stringify(step.input, null, 2);
    } catch {
      return "";
    }
  }

  function collectFlowDraft() {
    if (!currentFlowDraft) return null;
    const container = document.getElementById("flow-editor");
    if (!container) return null;

    const result = JSON.parse(JSON.stringify(currentFlowDraft));

    if (result.loop_config) {
      container.querySelectorAll("[data-loop-field]").forEach((el) => {
        const field = el.dataset.loopField;
        const val = el.type === "number" ? Number(el.value) : el.value;
        result.loop_config[field] = val;
      });
    }

    container.querySelectorAll("[data-step-idx]").forEach((card) => {
      const idx = Number(card.dataset.stepIdx);
      const step = result.steps[idx];
      card.querySelectorAll("[data-field]").forEach((el) => {
        const field = el.dataset.field;
        let val;
        if (el.type === "checkbox") val = el.checked;
        else if (el.type === "number") val = el.value === "" ? undefined : Number(el.value);
        else val = el.value;

        if (field === "input_template") {
          step.input = val ? { template: { template: val } } : undefined;
        } else {
          step[field] = val === "" ? undefined : val;
        }
      });
    });

    return result;
  }

  document.getElementById("save-flow")?.addEventListener("click", () => {
    const draft = collectFlowDraft();
    if (!draft) return;
    vscode.postMessage({ type: "saveFlow", flow: draft });
    currentFlowDraft = draft;
    dirty = false;
    updateDirtyIndicator();
  });

  document.getElementById("revert-flow")?.addEventListener("click", () => {
    if (!currentFlowName) return;
    if (!dirty || confirm("Discard unsaved changes?")) {
      vscode.postMessage({ type: "loadFlow", name: currentFlowName });
    }
  });

  function markDirty() {
    dirty = true;
    updateDirtyIndicator();
  }

  function updateDirtyIndicator() {
    const ind = document.getElementById("flow-dirty");
    if (ind) ind.classList.toggle("show", dirty);
  }

  // ─────────────────────────────────────────────────────────────
  // NOTIFICATIONS tab
  // ─────────────────────────────────────────────────────────────
  function renderNotifications() {
    setChecked("notif-enabled", state.notifications.enabled);
    setChecked("notif-sound", state.notifications.sound);
    setChecked("hooks-managed", state.hooks.managed);

    const events = ["init", "recall", "ingest", "sleep", "error", "sidecar", "error_recall", "update"];
    const container = document.getElementById("event-list");
    if (!container) return;
    container.innerHTML = events
      .map((ev) => {
        const checked = state.notifications.events?.includes(ev) ? "checked" : "";
        return `<label class="checkbox-row">
          <input type="checkbox" data-event="${ev}" ${checked}>
          <span>${ev}</span>
        </label>`;
      })
      .join("");
  }

  document.getElementById("save-notifications")?.addEventListener("click", () => {
    const events = Array.from(document.querySelectorAll('[data-event]'))
      .filter((el) => el.checked)
      .map((el) => el.dataset.event);
    vscode.postMessage({
      type: "saveNotifications",
      notifications: {
        enabled: getChecked("notif-enabled"),
        sound: getChecked("notif-sound"),
        events,
      },
      hooks: { managed: getChecked("hooks-managed") },
    });
  });

  document.getElementById("reinstall-hooks")?.addEventListener("click", () => {
    vscode.postMessage({ type: "reinstallHooks" });
  });

  // ─────────────────────────────────────────────────────────────
  // SECRETS tab
  // ─────────────────────────────────────────────────────────────
  const SECRETS = [
    { key: "giteaToken", label: "Gitea Token", hint: "Auto-update from private Gitea releases" },
    { key: "githubToken", label: "GitHub Token", hint: "Auto-update from GitHub releases (if provider is github)" },
    { key: "haToken", label: "Home Assistant Token", hint: "Long-lived access token" },
    { key: "braveSearchKey", label: "Brave Search API Key", hint: "Web search in flows" },
  ];

  function renderSecrets() {
    const container = document.getElementById("secrets-list");
    if (!container) return;
    container.innerHTML = SECRETS.map((s) => {
      const isSet = state.secrets[s.key] === true;
      return `<div class="secret-row">
        <label>${esc(s.label)}<span class="hint" style="display:block;font-size:10px;color:#52525b;">${esc(s.hint)}</span></label>
        <input type="password" data-secret-input="${s.key}" placeholder="${isSet ? '•••••• (stored)' : 'not set'}">
        <span class="secret-status ${isSet ? 'set' : 'unset'}">${isSet ? 'stored' : 'empty'}</span>
        <div style="display:flex;gap:4px;">
          <button class="btn" data-save-secret="${s.key}">Save</button>
          <button class="btn danger" data-delete-secret="${s.key}">Delete</button>
        </div>
      </div>`;
    }).join("");

    container.querySelectorAll("[data-save-secret]").forEach((btn) => {
      btn.addEventListener("click", () => {
        const key = btn.dataset.saveSecret;
        const input = container.querySelector(`[data-secret-input="${key}"]`);
        if (!input || !input.value) {
          showToast("Enter a value to store", "fail");
          return;
        }
        vscode.postMessage({ type: "setSecret", key, value: input.value });
        input.value = "";
      });
    });
    container.querySelectorAll("[data-delete-secret]").forEach((btn) => {
      btn.addEventListener("click", () => {
        const key = btn.dataset.deleteSecret;
        vscode.postMessage({ type: "deleteSecret", key });
      });
    });
  }

  // ─────────────────────────────────────────────────────────────
  // Helpers
  // ─────────────────────────────────────────────────────────────
  function setVal(id, v) {
    const el = document.getElementById(id);
    if (el) el.value = v ?? "";
  }
  function getVal(id) {
    return document.getElementById(id)?.value ?? "";
  }
  function setChecked(id, v) {
    const el = document.getElementById(id);
    if (el) el.checked = v === true;
  }
  function getChecked(id) {
    return document.getElementById(id)?.checked === true;
  }
  function esc(s) {
    return String(s ?? "").replace(/[&<>"']/g, (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
    );
  }

  function showToast(msg, kind) {
    const t = document.getElementById("toast");
    if (!t) return;
    t.textContent = msg;
    t.className = "toast " + kind + " show";
    setTimeout(() => t.classList.remove("show"), 2600);
  }

  function renderAll() {
    renderEndpoints();
    renderFlowList();
    renderFlowEditor();
    renderNotifications();
    renderSecrets();
  }

  // Ready
  vscode.postMessage({ type: "ready" });
})();
