/**
 * sync_docs tool — Sprint lifecycle documentation agent.
 *
 * Port of ygg-mcp/src/tools.rs sync_docs function to TypeScript.
 * Handles setup, sprint_start, and sprint_end events.
 */

import * as fs from "fs";
import * as path from "path";

interface Config {
  odin_url: string;
  timeout_secs: number;
  workspace_path?: string;
  project?: string;
}

interface SyncDocsParams {
  event: string;
  sprint_id?: string;
  sprint_content?: string;
  workspace_path?: string;
}

const REQUIRED_DOCS = ["ARCHITECTURE.md", "NAMING_CONVENTIONS.md", "USAGE.md"];
const MAX_PROMPT_BYTES = 100_000;

/** Call Odin's chat completion for LLM generation. */
async function generate(
  config: Config,
  prompt: string,
  sessionId: string,
  maxTokens = 4096
): Promise<string> {
  const controller = new AbortController();
  const timeout = setTimeout(
    () => controller.abort(),
    config.timeout_secs * 1000
  );

  try {
    const resp = await fetch(`${config.odin_url}/v1/chat/completions`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        messages: [{ role: "user", content: prompt }],
        max_tokens: maxTokens,
        session_id: sessionId,
        stream: false,
      }),
      signal: controller.signal,
    });

    if (!resp.ok) {
      const text = await resp.text();
      throw new Error(`Odin returned ${resp.status}: ${text}`);
    }

    const body = (await resp.json()) as {
      choices?: Array<{ message?: { content?: string } }>;
    };
    return body.choices?.[0]?.message?.content ?? "";
  } finally {
    clearTimeout(timeout);
  }
}

/** Store sprint engram in Mimir via Odin proxy. */
async function storeEngram(
  config: Config,
  cause: string,
  effect: string,
  tags: string[]
): Promise<void> {
  try {
    await fetch(`${config.odin_url}/api/v1/store`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ cause, effect, tags }),
    });
  } catch {
    // Best-effort — don't fail the tool
  }
}

export async function handleSyncDocs(
  config: Config,
  params: SyncDocsParams,
  sessionId: string
): Promise<string> {
  // Resolve workspace
  const workspace =
    params.workspace_path?.replace(/\/+$/, "") ||
    config.workspace_path?.replace(/\/+$/, "");
  if (!workspace) {
    throw new Error(
      "No workspace_path provided. Pass it as a parameter or set it in config."
    );
  }

  if ((params.sprint_content?.length ?? 0) > MAX_PROMPT_BYTES) {
    throw new Error(
      `sprint_content exceeds maximum size of ${MAX_PROMPT_BYTES} bytes`
    );
  }

  switch (params.event) {
    case "setup":
      return handleSetup(config, workspace, params, sessionId);
    case "sprint_start":
      return handleSprintStart(config, workspace, params, sessionId);
    case "sprint_end":
      return handleSprintEnd(config, workspace, params, sessionId);
    default:
      throw new Error(
        `Unknown event '${params.event}'. Use 'setup', 'sprint_start', or 'sprint_end'.`
      );
  }
}

/** Initialize /docs/ and /sprints/ for a workspace. */
async function handleSetup(
  config: Config,
  workspace: string,
  params: SyncDocsParams,
  sessionId: string
): Promise<string> {
  const docsDir = path.join(workspace, "docs");
  const sprintsDir = path.join(workspace, "sprints");
  const actions: string[] = [];

  // Create directories
  for (const dir of [docsDir, sprintsDir]) {
    if (!fs.existsSync(dir)) {
      fs.mkdirSync(dir, { recursive: true });
      actions.push(`Created ${dir}`);
    }
  }

  // Scaffold missing required docs
  for (const doc of REQUIRED_DOCS) {
    const docPath = path.join(docsDir, doc);
    if (!fs.existsSync(docPath)) {
      const projectContext = params.sprint_content || config.project || "";
      const prompt = `Generate a ${doc} for a software project.
Project context: ${projectContext}
Output ONLY the markdown content, no code fences.`;

      try {
        const content = await generate(config, prompt, sessionId, 2048);
        fs.writeFileSync(docPath, content.trim() + "\n");
        actions.push(`Scaffolded ${doc}`);
      } catch (e) {
        actions.push(
          `Failed to scaffold ${doc}: ${e instanceof Error ? e.message : e}`
        );
      }
    }
  }

  if (actions.length === 0) {
    return "Setup: workspace already initialized — /docs/ and /sprints/ exist with all required docs.";
  }

  return `## Setup Complete\n\n${actions.map((a) => `- ${a}`).join("\n")}`;
}

/** Sprint start: update USAGE.md, validate invariants. */
async function handleSprintStart(
  config: Config,
  workspace: string,
  params: SyncDocsParams,
  sessionId: string
): Promise<string> {
  const docsDir = path.join(workspace, "docs");
  const sprintsDir = path.join(workspace, "sprints");
  const actions: string[] = [];

  // Auto-setup if needed
  if (!fs.existsSync(docsDir)) {
    const setupResult = await handleSetup(config, workspace, params, sessionId);
    actions.push(setupResult);
  }

  // Write sprint file
  if (params.sprint_id && params.sprint_content) {
    const sprintFile = path.join(
      sprintsDir,
      `sprint-${params.sprint_id}.md`
    );
    fs.writeFileSync(sprintFile, params.sprint_content);
    actions.push(`Created ${sprintFile}`);
  }

  // Update USAGE.md with sprint context
  const usagePath = path.join(docsDir, "USAGE.md");
  if (fs.existsSync(usagePath) && params.sprint_content) {
    try {
      const currentUsage = fs.readFileSync(usagePath, "utf-8");
      const prompt = `Given this sprint document:
---
${params.sprint_content.slice(0, 8000)}
---

And the current USAGE.md:
---
${currentUsage.slice(0, 4000)}
---

Update the USAGE.md to reflect any new API endpoints, startup commands, or configuration changes from this sprint.
Output the COMPLETE updated USAGE.md content. Keep existing sections, only add/modify what changed.`;

      const updated = await generate(config, prompt, sessionId, 4096);
      if (updated.trim().length > 100) {
        fs.writeFileSync(usagePath, updated.trim() + "\n");
        actions.push("Updated USAGE.md");
      }
    } catch (e) {
      actions.push(
        `Warning: USAGE.md update failed: ${e instanceof Error ? e.message : e}`
      );
    }
  }

  // Validate invariants
  const missing = REQUIRED_DOCS.filter(
    (d) => !fs.existsSync(path.join(docsDir, d))
  );
  if (missing.length > 0) {
    actions.push(`Warning: missing docs: ${missing.join(", ")}`);
  }

  return `## Sprint ${params.sprint_id || "?"} Started\n\n${actions.map((a) => `- ${a}`).join("\n")}`;
}

/** Sprint end: archive to Mimir, update ARCHITECTURE.md, delete sprint file. */
async function handleSprintEnd(
  config: Config,
  workspace: string,
  params: SyncDocsParams,
  sessionId: string
): Promise<string> {
  const docsDir = path.join(workspace, "docs");
  const sprintsDir = path.join(workspace, "sprints");
  const actions: string[] = [];

  // Archive sprint to Mimir
  if (params.sprint_id && params.sprint_content) {
    await storeEngram(
      config,
      `Sprint ${params.sprint_id} summary`,
      params.sprint_content.slice(0, 8000),
      ["sprint", `sprint-${params.sprint_id}`]
    );
    actions.push(`Archived sprint ${params.sprint_id} to Mimir`);
  }

  // Update ARCHITECTURE.md with sprint delta
  const archPath = path.join(docsDir, "ARCHITECTURE.md");
  if (fs.existsSync(archPath) && params.sprint_content) {
    try {
      const currentArch = fs.readFileSync(archPath, "utf-8");
      const prompt = `Given this completed sprint document:
---
${params.sprint_content.slice(0, 8000)}
---

And the current ARCHITECTURE.md:
---
${currentArch.slice(0, 6000)}
---

If the sprint introduced architectural changes (new crates, API changes, data flow changes), append a brief delta section to ARCHITECTURE.md.
If no architectural changes, return the document unchanged.
Output the COMPLETE updated ARCHITECTURE.md.`;

      const updated = await generate(config, prompt, sessionId, 4096);
      if (updated.trim().length > 100) {
        fs.writeFileSync(archPath, updated.trim() + "\n");
        actions.push("Updated ARCHITECTURE.md");
      }
    } catch (e) {
      actions.push(
        `Warning: ARCHITECTURE.md update failed: ${e instanceof Error ? e.message : e}`
      );
    }
  }

  // Delete sprint file
  if (params.sprint_id) {
    const sprintFile = path.join(
      sprintsDir,
      `sprint-${params.sprint_id}.md`
    );
    if (fs.existsSync(sprintFile)) {
      fs.unlinkSync(sprintFile);
      actions.push(`Deleted ${sprintFile}`);
    }
  }

  return `## Sprint ${params.sprint_id || "?"} Ended\n\n${actions.map((a) => `- ${a}`).join("\n")}`;
}
