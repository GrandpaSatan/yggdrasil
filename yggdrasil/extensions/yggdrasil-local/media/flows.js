/* Yggdrasil flows webview script.
   Renders flow diagrams + step tables with expandable system-prompt disclosures.
   Communicates with extension host via acquireVsCodeApi() for future refresh/edit actions. */

(function () {
  const vscode = typeof acquireVsCodeApi === "function" ? acquireVsCodeApi() : null;

  // ─────────────────────────────────────────────────────────────
  // Flow definitions — each step now carries system_prompt + input_template
  // so the UI can expose the full context each model receives.
  // ─────────────────────────────────────────────────────────────
  const FLOWS = {
    coding_swarm: {
      badge: "INTENT: coding",
      title: "coding_swarm",
      loop: "LGTM Loop · max 3",
      desc: "Cross-model generate → static check → review → refine. Coder and reviewer must be different models.",
      trigger: 'User: "Write function X"',
      response: "Refined code",
      steps: [
        {
          name: "generate",
          model: "nemotron-3-nano:4b",
          host: "Munin iGPU",
          type: "assigned",
          system_prompt:
            "You are an expert code generator for the Yggdrasil AI homelab project. Write clean, idiomatic code that follows project conventions: axum handlers, serde structs, thiserror enums, tokio async, tracing instrumentation. Never hardcode IPs — use environment variables. Include error handling with Result types. Output ONLY the code, no explanations.",
          input_template: "user_message",
          temperature: 0.2,
        },
        {
          name: "static_check",
          model: "clippy / cargo",
          host: "local",
          type: "nonllm",
        },
        {
          name: "review",
          model: "gemma4:e4b",
          host: "Hugin eGPU",
          type: "assigned",
          system_prompt:
            "You are a code reviewer for the Yggdrasil project. Review the code for: bugs, security issues (hardcoded IPs, unwrap() panics, SQL injection), convention violations, missing error handling, and performance issues. If the code is correct, respond with exactly: LGTM. If issues exist, respond with a numbered list of issues, each with severity (high/medium/low) and a specific fix suggestion. Be concise — only flag real problems, not style preferences.",
          input_template: "{generated_code.output}",
          temperature: 0.1,
        },
        {
          name: "refine",
          model: "nemotron-3-nano:4b",
          host: "Munin iGPU",
          type: "assigned",
          system_prompt:
            "You are a code refiner. Apply the reviewer's feedback to fix the code. Output ONLY the corrected code, no explanations. If the review says LGTM, output the code unchanged.",
          input_template:
            "Original code:\n{generated_code.output}\n\nReview feedback:\n{review.output}\n\nApply the fixes and output the corrected code.",
          temperature: 0.1,
        },
      ],
      loopFrom: 2,
      loopTo: 1,
    },

    code_qa: {
      badge: "MANUAL",
      title: "code_qa",
      desc: "Write tests for existing code. QA uses different model than coder to avoid self-validation bias.",
      trigger: 'User: "Test function X"',
      response: "Test suite",
      steps: [
        {
          name: "fetch_code",
          model: "gemma4:e4b + search_code",
          host: "Hugin eGPU",
          type: "assigned",
          agentic: true,
          system_prompt:
            "You are a code retrieval specialist. Search the codebase for the function, module, or component the user wants tested. Return the relevant source code with file paths and line numbers.",
          input_template: "user_message",
          temperature: 0.1,
        },
        {
          name: "analyze_coverage",
          model: "gemma4:e4b",
          host: "Hugin eGPU",
          type: "assigned",
          system_prompt:
            "You are a test coverage analyst. Given source code, identify: (1) happy path scenarios, (2) edge cases (empty inputs, boundaries, overflow), (3) error conditions (network failures, invalid input, missing data), (4) security-sensitive paths. Output a numbered list of test cases to write.",
          input_template:
            "User request: {user_message}\n\nSource code to test:\n{source_code.output}\n\nAnalyze what test cases are needed.",
          temperature: 0.2,
        },
        {
          name: "write_tests",
          model: "nemotron-3-nano:4b",
          host: "Munin iGPU",
          type: "assigned",
          system_prompt:
            "You are a Rust test generator for the Yggdrasil project. Generate test code following conventions: #[cfg(test)] mod tests, #[test] or #[tokio::test], descriptive snake_case names (test_<what>_<scenario>), assert!/assert_eq!/assert_ne! macros. Output ONLY valid Rust test code, no explanations.",
          input_template:
            "Source code:\n{source_code.output}\n\nTest cases to implement:\n{coverage_analysis.output}\n\nGenerate the test code.",
          temperature: 0.2,
        },
        {
          name: "validate",
          model: "gemma4:e4b",
          host: "Hugin eGPU",
          type: "assigned",
          system_prompt:
            "You are a test validator. Review the generated tests against the coverage analysis. Check: (1) all identified test cases are covered, (2) assertions are meaningful (not just assert!(true)), (3) edge cases are actually tested, (4) test names are descriptive. If all good, respond: LGTM. If gaps exist, list them.",
          input_template:
            "Coverage analysis:\n{coverage_analysis.output}\n\nGenerated tests:\n{test_code.output}\n\nValidate the tests cover all identified cases.",
          temperature: 0.1,
        },
      ],
    },

    code_docs: {
      badge: "MANUAL",
      title: "code_docs",
      desc: "Generate documentation with accuracy cross-check against actual source code.",
      trigger: 'User: "Document X"',
      response: "Accurate docs",
      steps: [
        {
          name: "fetch_code",
          model: "gemma4:e4b + search_code",
          host: "Hugin eGPU",
          type: "assigned",
          agentic: true,
          system_prompt:
            "Search the codebase for the struct, function, module, or API the user wants documented. Return the complete source code with file paths, signatures, and any existing doc comments.",
          input_template: "user_message",
          temperature: 0.1,
        },
        {
          name: "generate_docs",
          model: "nemotron-3-nano:4b",
          host: "Munin iGPU",
          type: "assigned",
          system_prompt:
            "You are a technical documentation writer. Generate clear, accurate documentation for the given code. Include: struct/function description, parameters with types, return values, usage examples, and any important notes about error handling or thread safety. Use Rust doc comment format (/// for items, //! for modules).",
          input_template:
            "User request: {user_message}\n\nSource code:\n{source_code.output}\n\nGenerate documentation.",
          temperature: 0.3,
        },
        {
          name: "review_accuracy",
          model: "gemma4:e4b",
          host: "Hugin eGPU",
          type: "assigned",
          system_prompt:
            "You are a documentation accuracy reviewer. Compare the generated documentation against the actual source code. Check: (1) parameter names and types match, (2) return types are correct, (3) described behavior matches the implementation, (4) examples would actually compile. List any inaccuracies found. If everything is accurate, respond: LGTM.",
          input_template:
            "Source code:\n{source_code.output}\n\nGenerated documentation:\n{docs.output}\n\nVerify accuracy.",
          temperature: 0.1,
        },
        {
          name: "fix",
          model: "nemotron-3-nano:4b",
          host: "Munin iGPU",
          type: "assigned",
          system_prompt:
            "Fix any inaccuracies identified in the review. Output the corrected documentation. If the review says LGTM, output the documentation unchanged.",
          input_template:
            "Documentation:\n{docs.output}\n\nAccuracy review:\n{accuracy_review.output}\n\nOutput the corrected documentation.",
          temperature: 0.1,
        },
      ],
    },

    devops: {
      badge: "INTENT: deployment",
      title: "devops",
      desc: "Infrastructure configuration generation with safety review.",
      trigger: 'User: "Deploy X"',
      response: "Config file",
      steps: [
        {
          name: "analyze_infra",
          model: "gemma4:e4b + ha tools",
          host: "Hugin eGPU",
          type: "assigned",
          agentic: true,
          system_prompt:
            "You are an infrastructure analyst for the Yggdrasil homelab. Gather current system state by checking service health and HA device states. Summarize: which services are running, what ports are in use, which nodes are online, and any relevant device states. Focus on information relevant to the user's deployment request.",
          input_template: "user_message",
          temperature: 0.1,
        },
        {
          name: "generate_config",
          model: "nemotron-3-nano:4b",
          host: "Munin iGPU",
          type: "assigned",
          system_prompt:
            "You are a DevOps engineer for the Yggdrasil homelab. Generate the requested deployment configuration (systemd unit, Docker Compose, deploy script, config file). Follow Yggdrasil conventions: binaries at /opt/yggdrasil/bin/, configs at /etc/yggdrasil/<service>/config.json, services run as yggdrasil user, env from /opt/yggdrasil/.env. NEVER hardcode IPs — use ${VAR} expansion. Deploy via /tmp then sudo cp. Output ONLY the config files, no explanations.",
          input_template:
            "User request: {user_message}\n\nCurrent infrastructure state:\n{infra_state.output}\n\nGenerate the deployment configuration.",
          temperature: 0.2,
        },
        {
          name: "review",
          model: "gemma4:e4b",
          host: "Hugin eGPU",
          type: "assigned",
          system_prompt:
            "You are a deployment safety reviewer. Check the generated config for: (1) hardcoded IPs or secrets (must use env vars), (2) correct file paths (/opt/yggdrasil/bin/, /etc/yggdrasil/), (3) correct user/permissions (yggdrasil:yggdrasil), (4) port conflicts with known services, (5) missing restart policies or health checks. If safe, respond: LGTM. Otherwise list issues.",
          input_template:
            "Infrastructure state:\n{infra_state.output}\n\nGenerated config:\n{config.output}\n\nReview for deployment safety.",
          temperature: 0.1,
        },
      ],
    },

    ui_design: {
      badge: "MANUAL",
      title: "ui_design",
      loop: "APPROVED Loop · max 2",
      desc: "Frontend component generation with visual quality review. Gemma4 E4B is vision-capable.",
      trigger: 'User: "Build UI X"',
      response: "React component",
      steps: [
        {
          name: "design_spec",
          model: "glm-4.7-flash",
          host: "Munin iGPU",
          type: "assigned",
          system_prompt:
            "You are a UI/UX architect. From the user's request, create a structured design specification: (1) component hierarchy, (2) layout approach (CSS Grid, Flexbox), (3) color scheme (dark mode, zinc-950 background), (4) interaction states (hover, active, loading, error), (5) data flow (props, state, events). Yggdrasil style: enterprise SaaS, dark mode, left-side nav rail, no centered-div layouts, no top navbars. Output as a structured spec.",
          input_template: "user_message",
          temperature: 0.3,
        },
        {
          name: "generate_ui",
          model: "nemotron-3-nano:4b",
          host: "Munin iGPU",
          type: "assigned",
          system_prompt:
            "You are a frontend engineer specializing in enterprise SaaS interfaces. Generate production-ready React/TypeScript components with Tailwind CSS. Follow these design rules: dark mode (bg-zinc-950, text-zinc-100), CSS Grid for layouts, left-side nav rails, data-dense tables with sorting/filtering, real-time charts where applicable. Use shadcn/ui patterns. Output ONLY the code — no explanations.",
          input_template:
            "Design specification:\n{design_spec.output}\n\nUser request: {user_message}\n\nGenerate the React/TypeScript component code.",
          temperature: 0.2,
        },
        {
          name: "visual_review",
          model: "gemma4:e4b",
          host: "Hugin eGPU",
          type: "assigned",
          system_prompt:
            "You are a UI quality reviewer. Review the generated frontend code for: (1) accessibility (ARIA labels, keyboard navigation, contrast ratios), (2) responsive design (breakpoints, mobile layout), (3) component structure (proper prop types, error boundaries), (4) dark mode consistency (no white backgrounds, proper contrast), (5) interaction completeness (loading states, empty states, error states). If the UI code meets all criteria, respond: APPROVED. Otherwise list specific issues.",
          input_template:
            "Design spec:\n{design_spec.output}\n\nGenerated UI code:\n{ui_code.output}\n\nReview for quality and completeness.",
          temperature: 0.1,
        },
        {
          name: "refine_ui",
          model: "nemotron-3-nano:4b",
          host: "Munin iGPU",
          type: "assigned",
          system_prompt:
            "Apply the reviewer's feedback to fix the UI code. Output ONLY the corrected code. If the review says APPROVED, output the code unchanged.",
          input_template:
            "UI code:\n{ui_code.output}\n\nReview feedback:\n{visual_review.output}\n\nApply fixes and output corrected code.",
          temperature: 0.1,
        },
      ],
      loopFrom: 3,
      loopTo: 2,
    },

    dba: {
      badge: "MANUAL",
      title: "dba",
      desc: "Database schema design, migration generation, safety review, query optimization.",
      trigger: 'User: "Schema change X"',
      response: "SQL migration",
      steps: [
        {
          name: "analyze_schema",
          model: "gemma4:e4b + search_code",
          host: "Hugin eGPU",
          type: "assigned",
          agentic: true,
          system_prompt:
            "You are a database analyst for the Yggdrasil project. Search the codebase for existing database schemas, migrations, and queries related to the user's request. Return: current table definitions, existing migration files, and any relevant query patterns. The project uses PostgreSQL with pgvector and sqlx.",
          input_template: "user_message",
          temperature: 0.1,
        },
        {
          name: "generate_migration",
          model: "nemotron-3-nano:4b",
          host: "Munin iGPU",
          type: "assigned",
          system_prompt:
            "You are a PostgreSQL DBA for the Yggdrasil project. Generate a versioned, idempotent SQL migration with both UP and DOWN sections. Follow conventions: snake_case table/column names, UUID primary keys, timestamptz for dates, JSONB for flexible fields. For vector columns use pgvector (vector(N)). Include appropriate indexes. Consider backward compatibility — migrations must not break existing queries. Output ONLY the SQL, no explanations.",
          input_template:
            "User request: {user_message}\n\nExisting schema context:\n{schema_analysis.output}\n\nGenerate the SQL migration.",
          temperature: 0.2,
        },
        {
          name: "review_safety",
          model: "gemma4:e4b",
          host: "Hugin eGPU",
          type: "assigned",
          system_prompt:
            "You are a database migration safety reviewer. Check the migration for: (1) backward compatibility — will existing queries break? (2) data loss risk — any DROP TABLE, DROP COLUMN, or type changes without data migration? (3) index impact — will new indexes cause long lock times on large tables? (4) constraint safety — NOT NULL on existing columns needs DEFAULT. (5) pgvector specifics — correct dimension, appropriate index type (HNSW vs IVFFlat). If safe, respond: LGTM. Otherwise list issues with severity.",
          input_template:
            "Schema context:\n{schema_analysis.output}\n\nProposed migration:\n{migration.output}\n\nReview for safety.",
          temperature: 0.1,
        },
        {
          name: "optimize_queries",
          model: "nemotron-3-nano:4b",
          host: "Munin iGPU",
          type: "assigned",
          system_prompt:
            "Given the migration and safety review, suggest query optimizations: (1) indexes that should be added for common query patterns, (2) any queries in the codebase that should be updated, (3) EXPLAIN ANALYZE suggestions for validation. If the safety review flagged issues, also output the corrected migration SQL. Output SQL and recommendations.",
          input_template:
            "Migration:\n{migration.output}\n\nSafety review:\n{safety_review.output}\n\nOptimize queries and fix any flagged issues.",
          temperature: 0.2,
        },
      ],
    },

    full_stack: {
      badge: "META",
      title: "full_stack",
      desc: "Orchestrates 4 other flows. Backend → Frontend → Tests → Deploy.",
      trigger: 'User: "Build feature X"',
      response: "Shipped feature",
      steps: [
        { name: "plan", model: "glm-4.7-flash", host: "Munin iGPU", type: "assigned" },
        { name: "coding_swarm", model: "(invoke flow)", host: "multiple", type: "assigned" },
        { name: "ui_design", model: "(invoke flow)", host: "multiple", type: "assigned" },
        { name: "code_qa", model: "(invoke flow)", host: "multiple", type: "assigned" },
        { name: "devops", model: "(invoke flow)", host: "multiple", type: "assigned" },
      ],
    },

    research: {
      badge: "INTENT: research · Sprint 056",
      title: "research",
      desc: "Live 7-step pipeline: decompose → plan → search internal + external → filter → store → synthesize.",
      trigger: "User: research question",
      response: "Research report",
      steps: [
        { name: "decompose", model: "rwkv-7:2.9b", host: "Hugin (not loaded)", type: "assigned" },
        { name: "plan", model: "rwkv-7:2.9b", host: "Hugin (not loaded)", type: "assigned" },
        { name: "search_internal", model: "gemma4:e4b + tools", host: "Hugin eGPU", type: "assigned", agentic: true },
        { name: "search_external", model: "gemma4:e4b + web", host: "Hugin eGPU", type: "assigned", agentic: true },
        { name: "filter", model: "rwkv-7:2.9b", host: "Hugin (not loaded)", type: "assigned" },
        { name: "store", model: "gemma4:e4b + store_memory", host: "Hugin eGPU", type: "assigned", agentic: true },
        { name: "synthesize", model: "rwkv-7:2.9b", host: "Hugin (not loaded)", type: "assigned" },
      ],
    },

    perceive: {
      badge: "MODALITY: omni · Sprint 057",
      title: "perceive",
      desc: "Unified voice + vision understanding via Gemma4 E4B (vision-capable).",
      trigger: "Image or audio input",
      response: "Text understanding",
      steps: [{ name: "understand", model: "gemma4:e4b", host: "Hugin eGPU", type: "assigned" }],
    },

    saga: {
      badge: "AUTO · Sprint 054",
      title: "saga_classify_distill",
      desc: "Memory classification — should this tool output be stored? Extract cause/effect if yes.",
      trigger: "Tool output (Edit/Bash/etc)",
      response: "Engram or skip",
      steps: [
        { name: "classify", model: "saga-350m", host: "Munin CPU", type: "distilled" },
        { name: "distill", model: "saga-350m", host: "Munin CPU", type: "distilled" },
      ],
    },

    home_assistant: {
      badge: "MANUAL · Sprint 054",
      title: "home_assistant",
      desc: "Extract entity + action from command, execute, confirm. LFM2-24B-A2B removed — needs reassignment.",
      trigger: 'User: "Turn on lights"',
      response: "Action confirmation",
      steps: [
        { name: "extract_action", model: "Empty (was LFM2-24B-A2B)", host: "—", type: "empty" },
        { name: "execute", model: "ha_call_service", host: "local", type: "nonllm" },
        { name: "confirm", model: "ha_get_states", host: "local", type: "nonllm" },
      ],
    },

    complex_reasoning: {
      badge: "MANUAL · Sprint 059",
      title: "complex_reasoning",
      desc: 'Fast plan then deep verification. Powered by GLM-4.7-Flash — the "offline Claude" for hard reasoning. 200K context, preserved thinking across multi-turn.',
      trigger: "Hard question",
      response: "Verified answer",
      steps: [
        {
          name: "fast_plan",
          model: "glm-4.7-flash",
          host: "Munin iGPU",
          type: "assigned",
          system_prompt:
            "You are a reasoning specialist. Given a hard question, produce a concise plan: (1) identify the core question, (2) list sub-problems that must be resolved, (3) for each sub-problem, state the approach. Keep it tight — this is a first-pass plan, not the final answer.",
          input_template: "user_message",
          temperature: 0.2,
        },
        {
          name: "deep_verify",
          model: "glm-4.7-flash",
          host: "Munin iGPU",
          type: "assigned",
          system_prompt:
            "You are a verification specialist. Given a plan from fast_plan, execute it rigorously: resolve each sub-problem with step-by-step reasoning, check for logical gaps, cite evidence where possible, and produce a final verified answer. If the plan is flawed, revise it and explain why.",
          input_template:
            "Original question: {user_message}\n\nInitial plan:\n{plan.output}\n\nVerify, deepen, and answer.",
          temperature: 0.3,
        },
      ],
    },

    dream: {
      badge: "IDLE / MANUAL · Sprint 055",
      title: "dream_* flows (consolidation / exploration / speculation)",
      desc: "Three related background flows for memory consolidation, brainstorming, and deep problem exploration. All currently empty.",
      trigger: "Idle timer or seed topic",
      response: "New engrams",
      steps: [
        { name: "query_recent/brainstorm/deep_reason", model: "Empty", host: "—", type: "empty" },
        { name: "find_patterns/evaluate/summarize", model: "Empty", host: "—", type: "empty" },
        { name: "store_insights/store", model: "Empty", host: "—", type: "empty" },
      ],
    },
  };

  // ─────────────────────────────────────────────────────────────
  // SVG flow chart builder
  // ─────────────────────────────────────────────────────────────
  function buildFlowSVG(flow) {
    const stepCount = flow.steps.length;
    const nodeW = 180;
    const nodeH = 80;
    const gap = 60;
    const startX = 20;
    const centerY = 100;
    const svgH = flow.loop ? 260 : 200;

    const nodes = [];
    const arrows = [];
    const pathId = `path-${flow.title.replace(/[^a-z0-9]/gi, "-")}`;

    nodes.push({
      x: startX,
      y: centerY,
      w: 120,
      h: nodeH,
      class: "user",
      title: "User",
      line2: flow.trigger.length > 24 ? flow.trigger.slice(0, 22) + "…" : flow.trigger,
      line3: "Request",
    });

    let prevX = startX + 120;

    flow.steps.forEach((s) => {
      const x = prevX + gap;
      nodes.push({
        x,
        y: centerY,
        w: nodeW,
        h: nodeH,
        class: s.type,
        title: s.name + (s.agentic ? " 🔧" : ""),
        line2: s.model,
        line3: s.host,
      });
      arrows.push({
        from: [prevX, centerY + nodeH / 2],
        to: [x, centerY + nodeH / 2],
        class: "flow-arrow",
      });
      prevX = x + nodeW;
    });

    const respX = prevX + gap;
    nodes.push({
      x: respX,
      y: centerY,
      w: 120,
      h: nodeH,
      class: "response",
      title: "Response",
      line2: flow.response.length > 24 ? flow.response.slice(0, 22) + "…" : flow.response,
      line3: "To user",
    });
    arrows.push({
      from: [prevX, centerY + nodeH / 2],
      to: [respX, centerY + nodeH / 2],
      class: "flow-arrow",
    });

    if (flow.loop && flow.loopFrom !== undefined && flow.loopTo !== undefined) {
      const fromNode = nodes[1 + flow.loopFrom];
      const toNode = nodes[1 + flow.loopTo];
      const fromX = fromNode.x + fromNode.w / 2;
      const toX = toNode.x + toNode.w / 2;
      const yTop = centerY - 40;
      arrows.push({
        path: `M ${fromX} ${centerY} Q ${fromX} ${yTop}, ${(fromX + toX) / 2} ${yTop} T ${toX} ${centerY}`,
        class: "flow-arrow loop",
        curve: true,
      });
    }

    const totalWFinal = respX + 120 + 40;
    let svg = `<svg class="flow-svg" viewBox="0 0 ${totalWFinal} ${svgH}" preserveAspectRatio="xMidYMid meet">
    <defs>
      <marker id="ah-${pathId}" markerWidth="10" markerHeight="10" refX="9" refY="3" orient="auto">
        <polygon points="0 0, 9 3, 0 6" fill="#52525b" />
      </marker>
      <marker id="ah-loop-${pathId}" markerWidth="10" markerHeight="10" refX="9" refY="3" orient="auto">
        <polygon points="0 0, 9 3, 0 6" fill="#3b82f6" />
      </marker>
      <path id="${pathId}" d="${buildFlowPath(nodes, centerY, nodeH)}" fill="none" />
    </defs>`;

    arrows.forEach((a) => {
      if (a.curve) {
        svg += `<path d="${a.path}" class="${a.class}" marker-end="url(#ah-loop-${pathId})" />`;
        const m = a.path.match(/Q (\d+) (\d+)/);
        if (m) {
          svg += `<text x="${m[1]}" y="${parseInt(m[2]) - 8}" fill="#93c5fd" font-size="10" text-anchor="middle" font-family="JetBrains Mono, monospace">↻ loop</text>`;
        }
      } else {
        svg += `<line x1="${a.from[0]}" y1="${a.from[1]}" x2="${a.to[0]}" y2="${a.to[1]}" class="${a.class}" marker-end="url(#ah-${pathId})" />`;
      }
    });

    nodes.forEach((n, idx) => {
      svg += `<g class="flow-node" data-idx="${idx}">
      <rect class="node-rect ${n.class}" x="${n.x}" y="${n.y}" width="${n.w}" height="${n.h}" />
      <text class="node-title" x="${n.x + n.w / 2}" y="${n.y + 22}">${n.title}</text>
      <text class="node-model" x="${n.x + n.w / 2}" y="${n.y + 42}">${escapeHtml(n.line2)}</text>
      <text class="node-host" x="${n.x + n.w / 2}" y="${n.y + 62}">${escapeHtml(n.line3)}</text>
    </g>`;
    });

    svg += `<circle r="5" fill="#3b82f6" filter="drop-shadow(0 0 6px #3b82f6)" class="flow-packet" data-path="${pathId}">
    <animateMotion dur="${Math.max(4, stepCount * 1.2)}s" repeatCount="indefinite" rotate="auto">
      <mpath href="#${pathId}" />
    </animateMotion>
  </circle>`;

    svg += `</svg>`;
    return svg;
  }

  function buildFlowPath(nodes, centerY, nodeH) {
    const y = centerY + nodeH / 2;
    let d = `M ${nodes[0].x + nodes[0].w} ${y}`;
    nodes.slice(1).forEach((n) => {
      d += ` L ${n.x} ${y}`;
      d += ` L ${n.x + n.w} ${y}`;
    });
    return d;
  }

  function escapeHtml(s) {
    return String(s).replace(/[&<>"']/g, (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
    );
  }

  // ─────────────────────────────────────────────────────────────
  // Render a flow tab — now with a Prompt column showing the
  // system prompt + input template as a collapsible disclosure.
  // ─────────────────────────────────────────────────────────────
  function renderFlow(flowId) {
    const flow = FLOWS[flowId];
    if (!flow) return "";

    let html = `<div class="tab-header">
    <div class="hleft">
      <span class="badge">${flow.badge}</span>
      <h2>${flow.title}${flow.loop ? ` <span class="loop-badge">${flow.loop}</span>` : ""}</h2>
      <p>${flow.desc}</p>
    </div>
  </div>
  <div class="flow-container" style="margin-bottom: 20px;">
    ${buildFlowSVG(flow)}
  </div>
  <div class="section">
    <div class="section-title">Step Assignments</div>
    <table>
      <tr>
        <th style="width: 4%;">#</th>
        <th style="width: 18%;">Step</th>
        <th style="width: 22%;">Model</th>
        <th style="width: 16%;">Host</th>
        <th style="width: 14%;">Type</th>
        <th style="width: 26%;">Prompt</th>
      </tr>`;

    flow.steps.forEach((s, i) => {
      const typeLabel =
        {
          assigned: '<span class="ai-badge ai-assigned">ASSIGNED</span>',
          empty: '<span class="ai-badge ai-empty">EMPTY</span>',
          nonllm: '<span class="ai-badge ai-nonllm">NON-LLM</span>',
          distilled: '<span class="ai-badge ai-distilled">DISTILLED</span>',
          ondemand: '<span class="ai-badge ai-ondemand">ON-DEMAND</span>',
        }[s.type] || "";

      const promptCell = s.system_prompt
        ? `<details class="prompt-detail">
             <summary>view prompt</summary>
             <span class="prompt-label">System Prompt</span>
             <pre>${escapeHtml(s.system_prompt)}</pre>
             ${s.input_template ? `<span class="prompt-label">Input Template</span><pre>${escapeHtml(s.input_template)}</pre>` : ""}
             ${typeof s.temperature === "number" ? `<span class="prompt-label">Temperature</span><pre>${s.temperature}</pre>` : ""}
           </details>`
        : s.type === "nonllm"
          ? '<span style="color:#71717a;font-size:10px;">tool / static</span>'
          : s.type === "empty"
            ? '<span style="color:#fca5a5;font-size:10px;">no prompt — empty step</span>'
            : '<span style="color:#71717a;font-size:10px;">—</span>';

      html += `<tr>
      <td class="mono">${i + 1}</td>
      <td>${s.name}${s.agentic ? ' <span style="color:#93c5fd;font-size:10px;">(agentic)</span>' : ""}</td>
      <td class="mono">${escapeHtml(s.model)}</td>
      <td class="mono">${escapeHtml(s.host)}</td>
      <td>${typeLabel}</td>
      <td>${promptCell}</td>
    </tr>`;
    });
    html += `</table></div>`;

    const hasEmpty = flow.steps.some((s) => s.type === "empty");
    if (hasEmpty) {
      html += `<div class="card" style="background:#2a0f0f; border-color:#991b1b; margin-top: 12px;">
      <h3 style="color:#fca5a5;">⚠ Empty steps need assignment</h3>
      <p style="color:#fca5a5;">This flow has one or more steps without a model assigned. The flow cannot run end-to-end until all steps have models.</p>
    </div>`;
    }

    return html;
  }

  // Populate flow tabs
  Object.keys(FLOWS).forEach((id) => {
    const el = document.getElementById("tab-" + id);
    if (el) el.innerHTML = renderFlow(id);
  });

  // Nav handling
  document.querySelectorAll(".nav-item").forEach((item) => {
    item.addEventListener("click", () => {
      const tab = item.dataset.tab;
      document.querySelectorAll(".nav-item").forEach((n) => n.classList.remove("active"));
      document.querySelectorAll(".tab").forEach((t) => t.classList.remove("active"));
      item.classList.add("active");
      const el = document.getElementById("tab-" + tab);
      if (el) el.classList.add("active");
      if (vscode) vscode.postMessage({ type: "tab", tab });
    });
  });

  // Tooltips on node hover — now includes the first 120 chars of the prompt
  const tooltip = document.getElementById("tooltip");
  document.addEventListener("mousemove", (e) => {
    const node = e.target.closest(".flow-node");
    if (node && tooltip) {
      const rect = node.querySelector("rect");
      const title = node.querySelector(".node-title")?.textContent || "";
      const model = node.querySelector(".node-model")?.textContent || "";
      const host = node.querySelector(".node-host")?.textContent || "";

      // Try to find the prompt for this step by matching the title
      const activeTab = document.querySelector(".tab.active");
      const flowId = activeTab?.id?.replace("tab-", "");
      const flow = flowId ? FLOWS[flowId] : null;
      const stepName = title.replace(/\s*🔧\s*$/, "");
      const step = flow?.steps?.find((s) => s.name === stepName);
      const promptHint = step?.system_prompt
        ? `<br><span style="color:#93c5fd;font-size:10px;">${escapeHtml(step.system_prompt.slice(0, 120))}${step.system_prompt.length > 120 ? "…" : ""}</span>`
        : "";

      tooltip.innerHTML = `<strong style="color:#fafafa">${title}</strong><br><span style="color:#a1a1aa">${model}</span><br><span style="color:#71717a;font-size:10px">${host}</span>${promptHint}`;
      tooltip.style.left = e.pageX + 14 + "px";
      tooltip.style.top = e.pageY + 14 + "px";
      tooltip.classList.add("show");
    } else if (tooltip) {
      tooltip.classList.remove("show");
    }
  });

  // Signal ready to extension
  if (vscode) vscode.postMessage({ type: "ready" });
})();
