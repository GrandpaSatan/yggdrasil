/**
 * Hook Manager — deploys ygg-memory.sh and manages ~/.claude/settings.json hooks.
 *
 * On activation:
 * 1. Copies bundled scripts/ygg-memory.sh → ~/.yggdrasil/ygg-memory.sh
 * 2. Ensures ~/.claude/settings.json hooks point to the deployed script
 * 3. Reports health status (green/yellow/red)
 */

import * as fs from "fs";
import * as path from "path";
import * as os from "os";
import * as http from "http";
import * as vscode from "vscode";
import type { OutputChannelManager } from "./outputChannel";

export type HealthStatus = "green" | "yellow" | "red";

export class HookManager implements vscode.Disposable {
  private deployDir: string;
  private scriptTarget: string;
  private settingsPath: string;

  constructor(
    private context: vscode.ExtensionContext,
    private outputChannel: OutputChannelManager
  ) {
    this.deployDir = path.join(os.homedir(), ".yggdrasil");
    this.scriptTarget = path.join(this.deployDir, "ygg-memory.sh");
    this.settingsPath = path.join(os.homedir(), ".claude", "settings.json");
  }

  async initialize(): Promise<void> {
    this.deployScript();
    this.ensureHooks();
  }

  /** Copy bundled script to ~/.yggdrasil/ygg-memory.sh if changed. */
  private deployScript(): void {
    const scriptSource = path.join(
      this.context.extensionPath,
      "scripts",
      "ygg-memory.sh"
    );

    if (!fs.existsSync(scriptSource)) {
      this.outputChannel.append(
        `WARN: bundled script not found at ${scriptSource}`
      );
      return;
    }

    // Create deploy dir
    if (!fs.existsSync(this.deployDir)) {
      fs.mkdirSync(this.deployDir, { recursive: true });
    }

    // Compare contents — skip if identical
    const sourceContent = fs.readFileSync(scriptSource);
    if (fs.existsSync(this.scriptTarget)) {
      const targetContent = fs.readFileSync(this.scriptTarget);
      if (sourceContent.equals(targetContent)) {
        return; // Already up to date
      }
    }

    fs.copyFileSync(scriptSource, this.scriptTarget);
    fs.chmodSync(this.scriptTarget, 0o755);
    this.outputChannel.append(
      `Deployed sidecar script to ${this.scriptTarget}`
    );
  }

  /** Ensure ~/.claude/settings.json hooks point to the deployed script. */
  private ensureHooks(): void {
    const config = vscode.workspace.getConfiguration("yggdrasil");
    if (!config.get<boolean>("hooks.managed", true)) {
      return; // User disabled hook management
    }

    const mimirUrl = config.get<string>("mimirUrl", "http://10.0.65.8:9090");
    const huginUrl = config.get<string>(
      "huginUrl",
      "http://10.0.65.9:11434"
    );

    // Extract IPs from URLs for env vars
    const mimirIp = this.extractHost(mimirUrl);
    const huginIp = this.extractHost(huginUrl);

    const envPrefix = `MUNIN_IP=${mimirIp} HUGIN_IP=${huginIp}`;
    const script = this.scriptTarget;

    // Sprint 061: hook timeouts are GENEROUS — never tuned to p95. Tight
    // timeouts cause silent fallbacks to degraded paths (e.g. RWKV-7
    // classifier killed at 1.5s when its real p95 is ~3.4s). Rule of thumb:
    // set 3-10× observed p95. See engram "stop tuning timeouts to measured p95".
    const expectedHooks = {
      SessionStart: [
        {
          hooks: [
            {
              type: "command",
              command: `${envPrefix} ${script} init`,
              timeout: 30000,
            },
          ],
        },
      ],
      PreToolUse: [
        {
          matcher: "Edit|Write|Read|Bash|Grep|Agent",
          hooks: [
            {
              type: "command",
              command: `${envPrefix} ${script} sidecar`,
              timeout: 30000,
            },
          ],
        },
        {
          matcher: "mcp__yggdrasil__ha_call_service_tool",
          hooks: [
            {
              type: "command",
              command:
                'if [ ! -f /tmp/ygg-hooks/ha_verified ]; then echo "BLOCKED: Must call ha_get_states_tool or ha_list_entities_tool before controlling devices." >&2; exit 2; fi',
            },
          ],
        },
      ],
      PostToolUse: [
        {
          matcher: "Edit|Write|Bash",
          hooks: [
            {
              type: "command",
              command: `${envPrefix} ${script} post`,
              timeout: 30000,
            },
          ],
        },
        {
          matcher:
            "mcp__yggdrasil__ha_get_states_tool|mcp__yggdrasil__ha_list_entities_tool",
          hooks: [
            {
              type: "command",
              command:
                "mkdir -p /tmp/ygg-hooks && touch /tmp/ygg-hooks/ha_verified",
            },
          ],
        },
      ],
      Stop: [
        {
          hooks: [
            {
              type: "command",
              command: `${envPrefix} ${script} sleep`,
              timeout: 60000,
            },
          ],
        },
      ],
    };

    // Read existing settings
    const claudeDir = path.dirname(this.settingsPath);
    if (!fs.existsSync(claudeDir)) {
      fs.mkdirSync(claudeDir, { recursive: true });
    }

    let existing: Record<string, unknown> = {};
    if (fs.existsSync(this.settingsPath)) {
      try {
        existing = JSON.parse(fs.readFileSync(this.settingsPath, "utf-8"));
      } catch {
        this.outputChannel.append(
          `WARN: could not parse ${this.settingsPath}, starting fresh`
        );
      }
    }

    // Deep-compare hooks — only write if different
    const existingHooks = JSON.stringify(existing.hooks || {});
    const newHooks = JSON.stringify(expectedHooks);
    if (existingHooks === newHooks) {
      return; // Already correct
    }

    // Backup before writing
    if (fs.existsSync(this.settingsPath)) {
      const backupPath = `${this.settingsPath}.bak.${Date.now()}`;
      fs.copyFileSync(this.settingsPath, backupPath);
    }

    // Merge: replace hooks, preserve everything else
    const merged = { ...existing, hooks: expectedHooks };
    fs.writeFileSync(
      this.settingsPath,
      JSON.stringify(merged, null, 2) + "\n",
      "utf-8"
    );

    this.outputChannel.append(
      `Updated Claude Code hooks in ${this.settingsPath}`
    );
    this.outputChannel.append(`  Script: ${script}`);
    this.outputChannel.append(`  Mimir: ${mimirIp}, Hugin: ${huginIp}`);
  }

  /** Check sidecar health: script deployed + hooks correct + Mimir reachable. */
  async checkHealth(): Promise<HealthStatus> {
    // Check script exists
    if (!fs.existsSync(this.scriptTarget)) {
      return "red";
    }

    // Check hooks reference our script
    if (fs.existsSync(this.settingsPath)) {
      try {
        const settings = JSON.parse(
          fs.readFileSync(this.settingsPath, "utf-8")
        );
        const hooksStr = JSON.stringify(settings.hooks || {});
        if (!hooksStr.includes(this.scriptTarget)) {
          return "red";
        }
      } catch {
        return "red";
      }
    } else {
      return "red";
    }

    // Check Mimir reachability
    const config = vscode.workspace.getConfiguration("yggdrasil");
    const mimirUrl = config.get<string>("mimirUrl", "http://10.0.65.8:9090");
    const reachable = await this.checkUrl(`${mimirUrl}/health`);
    return reachable ? "green" : "yellow";
  }

  /** HTTP GET with timeout — returns true if 2xx response. */
  private checkUrl(url: string): Promise<boolean> {
    return new Promise((resolve) => {
      const timeout = setTimeout(() => resolve(false), 2000);
      try {
        const req = http.get(url, (res) => {
          clearTimeout(timeout);
          resolve(
            res.statusCode !== undefined &&
              res.statusCode >= 200 &&
              res.statusCode < 300
          );
          res.resume(); // Drain response
        });
        req.on("error", () => {
          clearTimeout(timeout);
          resolve(false);
        });
      } catch {
        clearTimeout(timeout);
        resolve(false);
      }
    });
  }

  /** Extract hostname/IP from a URL string. */
  private extractHost(url: string): string {
    try {
      const u = new URL(url);
      return u.hostname;
    } catch {
      return url;
    }
  }

  dispose(): void {
    // Nothing to clean up
  }
}
