/**
 * Hook Manager — deploys ygg-memory.sh and manages ~/.claude/settings.json hooks.
 *
 * On activation:
 * 1. Copies bundled scripts/ygg-memory.sh → ~/.yggdrasil/ygg-memory.sh
 * 2. Ensures ~/.claude/settings.json hooks point to the deployed script
 * 3. Reports health status (green/yellow/red)
 *
 * Sprint 062: introduces a write-mode split ("replace" | "merge") so users
 * with hand-crafted hooks in ~/.claude/settings.json don't lose them on
 * extension upgrade. Yggdrasil-owned hook commands are tagged with
 * `YGG_MANAGED=062;` so later updates can identify-and-replace just the
 * managed entries without touching the user's own.
 */

import * as fs from "fs";
import * as path from "path";
import * as os from "os";
import * as http from "http";
import * as vscode from "vscode";
import type { OutputChannelManager } from "./outputChannel";

// ─── Lock file types ──────────────────────────────────────────
interface LockFileContent {
  pid: number;
  ts: number;   // unix ms
  hostname: string;
}

/** How long a lock is considered fresh (ms). After this, any writer steals it. */
const LOCK_TTL_MS = 30_000;

export type HealthStatus = "green" | "yellow" | "red";
export type WriteMode = "replace" | "merge";

/** Sentinel prepended to every Yggdrasil-managed hook command. */
const MANAGED_TAG = "YGG_MANAGED=062;";

/** GlobalState key that records the user acknowledged the divergence prompt. */
const DIVERGE_ACK_KEY = "hookManager.divergeAckedV062";

/** Minimal structural types we rely on from ~/.claude/settings.json. */
interface HookSpec {
  type: string;
  command: string;
  timeout?: number;
}
interface HookMatcher {
  matcher?: string;
  hooks: HookSpec[];
}
type HookMap = Record<string, HookMatcher[]>;

/** Thrown when the user chose "Leave alone" to abort the write path. */
class HookManagerOptOut extends Error {
  constructor() {
    super("user opted out of hook management");
    this.name = "HookManagerOptOut";
  }
}

export class HookManager implements vscode.Disposable {
  private deployDir: string;
  private scriptTarget: string;
  private settingsPath: string;
  private lockPath: string;
  private hookChannel: vscode.OutputChannel;

  constructor(
    private context: vscode.ExtensionContext,
    private outputChannel: OutputChannelManager
  ) {
    this.deployDir = path.join(os.homedir(), ".yggdrasil");
    this.scriptTarget = path.join(this.deployDir, "ygg-memory.sh");
    this.settingsPath = path.join(os.homedir(), ".claude", "settings.json");
    this.lockPath = path.join(os.homedir(), ".claude", ".yggdrasil-hook.lock");
    this.hookChannel = vscode.window.createOutputChannel("yggdrasil.hookManager");
  }

  // ─── Lock file helpers ────────────────────────────────────────

  /**
   * Try to acquire the PID-stamped lock.
   * Returns true if the lock was acquired; false if another live process holds it.
   *
   * Strategy:
   *  1. Atomic create with "wx" flag — fails if file exists.
   *  2. If exists: read it. If PID is dead (kill(pid, 0) throws) OR ts is older
   *     than LOCK_TTL_MS → steal the lock (overwrite).
   *  3. Else: skip write, log the holder PID.
   */
  public tryAcquireLock(): boolean {
    const payload = JSON.stringify({
      pid: process.pid,
      ts: Date.now(),
      hostname: os.hostname(),
    } satisfies LockFileContent);

    // Attempt atomic create
    try {
      fs.writeFileSync(this.lockPath, payload, { flag: "wx", encoding: "utf-8" });
      return true; // created — we own the lock
    } catch (err: unknown) {
      if ((err as NodeJS.ErrnoException).code !== "EEXIST") {
        // Unexpected error — fail safe, allow write
        this.hookChannel.appendLine(
          `hookManager: unexpected lock error (${String(err)}), proceeding without lock`
        );
        return true;
      }
    }

    // File exists — read it and decide
    let existing: LockFileContent | null = null;
    try {
      existing = JSON.parse(fs.readFileSync(this.lockPath, "utf-8")) as LockFileContent;
    } catch {
      // Corrupt/empty lock — steal it
      this.writeLock(payload);
      return true;
    }

    // Check TTL
    if (Date.now() - existing.ts > LOCK_TTL_MS) {
      this.hookChannel.appendLine(
        `hookManager: lock held by PID ${existing.pid} is stale (${LOCK_TTL_MS}ms TTL exceeded), stealing`
      );
      this.writeLock(payload);
      return true;
    }

    // Check if holding PID is still alive
    const pidAlive = isPidAlive(existing.pid);
    if (!pidAlive) {
      this.hookChannel.appendLine(
        `hookManager: lock held by PID ${existing.pid} (dead), stealing`
      );
      this.writeLock(payload);
      return true;
    }

    // Live process holds the lock
    this.hookChannel.appendLine(
      `hookManager: lock held by PID ${existing.pid} on ${existing.hostname}, skipping this window`
    );
    return false;
  }

  /** Release the lock (only if we still own it). */
  public releaseLock(): void {
    try {
      const content = JSON.parse(fs.readFileSync(this.lockPath, "utf-8")) as LockFileContent;
      if (content.pid === process.pid) {
        fs.unlinkSync(this.lockPath);
      }
    } catch {
      // Lock already gone or unreadable — no-op
    }
  }

  private writeLock(payload: string): void {
    try {
      fs.writeFileSync(this.lockPath, payload, { encoding: "utf-8" });
    } catch {
      // Non-fatal — worst case two windows race but we've tried our best
    }
  }

  async initialize(): Promise<void> {
    this.deployScript();
    try {
      await this.ensureHooks();
    } catch (err) {
      if (err instanceof HookManagerOptOut) {
        return; // user chose "Leave alone" — already logged
      }
      throw err;
    }
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
  private async ensureHooks(): Promise<void> {
    const config = vscode.workspace.getConfiguration("yggdrasil");
    if (!config.get<boolean>("hooks.managed", true)) {
      return; // User disabled hook management
    }

    const expected = this.computeExpectedHooks(config);

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

    const existingHookMap: HookMap =
      (existing.hooks as HookMap | undefined) ?? {};

    // Detect user-authored hooks we would collide with.
    const manual = this.detectManualHooks(existingHookMap, expected);

    // Resolve the write mode — prompting the user once on first divergence.
    const mode = await this.resolveWriteMode(config, manual);

    const applied = this.applyHooks(existingHookMap, expected, mode);

    // No-op if the resulting tree is byte-identical to what's already there.
    if (JSON.stringify(existingHookMap) === JSON.stringify(applied)) {
      return;
    }

    // Acquire lock before writing — prevents multi-window race.
    const lockAcquired = this.tryAcquireLock();
    if (!lockAcquired) {
      return; // Another VSCode window is writing; skip — it will write consistent state.
    }

    try {
      // Backup before writing.
      if (fs.existsSync(this.settingsPath)) {
        const backupPath = `${this.settingsPath}.bak.${Date.now()}`;
        fs.copyFileSync(this.settingsPath, backupPath);
      }

      const merged = { ...existing, hooks: applied };
      fs.writeFileSync(
        this.settingsPath,
        JSON.stringify(merged, null, 2) + "\n",
        "utf-8"
      );

      const hookCount = Object.values(applied).reduce(
        (sum, arr) => sum + arr.reduce((n, m) => n + m.hooks.length, 0),
        0
      );
      const msg = `hookManager: wrote ${hookCount} hooks (managed=true, mode=${mode})`;
      this.hookChannel.appendLine(msg);
      this.outputChannel.append(msg);
      this.outputChannel.append(`  Path: ${this.settingsPath}`);
      this.outputChannel.append(`  Script: ${this.scriptTarget}`);
    } finally {
      this.releaseLock();
    }
  }

  /**
   * Build the Yggdrasil-owned hook tree. Every command is prefixed with
   * `YGG_MANAGED=062;` so `detectManualHooks` can tell user hooks apart
   * from ours later on.
   *
   * Sprint 061: hook timeouts are GENEROUS — never tuned to p95. Tight
   * timeouts cause silent fallbacks to degraded paths (e.g. RWKV-7
   * classifier killed at 1.5s when its real p95 is ~3.4s). Rule of thumb:
   * set 3-10× observed p95. See engram "stop tuning timeouts to measured p95".
   */
  public computeExpectedHooks(
    config: vscode.WorkspaceConfiguration
  ): HookMap {
    const mimirUrl = config.get<string>("mimirUrl", "http://10.0.65.8:9090");
    const huginUrl = config.get<string>(
      "huginUrl",
      "http://10.0.65.9:11434"
    );

    const mimirIp = this.extractHost(mimirUrl);
    const huginIp = this.extractHost(huginUrl);

    const envPrefix = `MUNIN_IP=${mimirIp} HUGIN_IP=${huginIp}`;
    const script = this.scriptTarget;
    const tag = MANAGED_TAG;

    return {
      SessionStart: [
        {
          hooks: [
            {
              type: "command",
              command: `${tag} ${envPrefix} ${script} init`,
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
              command: `${tag} ${envPrefix} ${script} sidecar`,
              timeout: 30000,
            },
          ],
        },
        {
          matcher: "mcp__yggdrasil__ha_call_service_tool",
          hooks: [
            {
              type: "command",
              command: `${tag} if [ ! -f /tmp/ygg-hooks/ha_verified ]; then echo "BLOCKED: Must call ha_get_states_tool or ha_list_entities_tool before controlling devices." >&2; exit 2; fi`,
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
              command: `${tag} ${envPrefix} ${script} post`,
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
              command: `${tag} mkdir -p /tmp/ygg-hooks && touch /tmp/ygg-hooks/ha_verified`,
            },
          ],
        },
      ],
      Stop: [
        {
          hooks: [
            {
              type: "command",
              command: `${tag} ${envPrefix} ${script} sleep`,
              timeout: 60000,
            },
          ],
        },
      ],
    };
  }

  /**
   * Return the list of hook entries already in `existing` that are NOT
   * owned by Yggdrasil (i.e. their command does not start with
   * `YGG_MANAGED=`). Used to drive the divergence prompt.
   */
  public detectManualHooks(
    existing: HookMap,
    _expected: HookMap
  ): { event: string; matcher?: string; command: string }[] {
    const manual: { event: string; matcher?: string; command: string }[] = [];
    for (const [event, matchers] of Object.entries(existing)) {
      if (!Array.isArray(matchers)) continue;
      for (const m of matchers) {
        if (!m || !Array.isArray(m.hooks)) continue;
        for (const h of m.hooks) {
          const cmd = typeof h.command === "string" ? h.command : "";
          if (!this.isManagedCommand(cmd)) {
            manual.push({ event, matcher: m.matcher, command: cmd });
          }
        }
      }
    }
    return manual;
  }

  /**
   * Produce the hook tree to persist to disk, combining existing user
   * entries with Yggdrasil-owned entries according to the write mode.
   *
   * - `replace`: user entries are discarded; only Yggdrasil hooks remain.
   *   (Pre-062 behaviour.)
   * - `merge`: per event type, retain existing non-managed entries and
   *   append Yggdrasil-managed ones. Any pre-existing Yggdrasil-managed
   *   entries in the same event are dropped first so we don't duplicate
   *   on upgrade.
   */
  public applyHooks(
    existing: HookMap,
    expected: HookMap,
    mode: WriteMode
  ): HookMap {
    if (mode === "replace") {
      return JSON.parse(JSON.stringify(expected));
    }

    const out: HookMap = {};
    const allEvents = new Set<string>([
      ...Object.keys(existing),
      ...Object.keys(expected),
    ]);

    for (const event of allEvents) {
      const userMatchers = (existing[event] ?? []).map((m) => ({
        ...m,
        hooks: (m.hooks ?? []).filter(
          (h) => !this.isManagedCommand(h.command)
        ),
      }));
      // Drop empty matcher groups created by filtering.
      const prunedUser = userMatchers.filter((m) => m.hooks.length > 0);

      const managed = expected[event] ?? [];

      out[event] = [...prunedUser, ...JSON.parse(JSON.stringify(managed))];
    }
    return out;
  }

  /**
   * Decide which write mode to use. If `yggdrasil.hooks.writeMode` is
   * already set, use it. Otherwise — and only if the user has non-managed
   * hook entries — prompt once and persist the choice to globalState.
   */
  private async resolveWriteMode(
    config: vscode.WorkspaceConfiguration,
    manual: { event: string; command: string }[]
  ): Promise<WriteMode> {
    const configured = config.get<WriteMode>("hooks.writeMode", "merge");

    if (manual.length === 0) {
      return configured;
    }

    const acked = this.context.globalState.get<boolean>(DIVERGE_ACK_KEY, false);
    if (acked) {
      return configured;
    }

    const choice = await vscode.window.showWarningMessage(
      "Yggdrasil wants to update ~/.claude/settings.json hooks. Your existing hook entries will be handled per the selected mode.",
      { modal: true },
      "Update (replace)",
      "Leave alone",
      "Merge (keep yours)"
    );

    let mode: WriteMode = configured;
    if (choice === "Update (replace)") {
      mode = "replace";
    } else if (choice === "Merge (keep yours)") {
      mode = "merge";
    } else if (choice === "Leave alone") {
      // User opted out — disable hook management entirely and bail.
      await config.update(
        "hooks.managed",
        false,
        vscode.ConfigurationTarget.Global
      );
      await this.context.globalState.update(DIVERGE_ACK_KEY, true);
      this.hookChannel.appendLine(
        "hookManager: user chose 'Leave alone' — hooks.managed=false"
      );
      throw new HookManagerOptOut();
    } else {
      // Dismissed — keep existing setting, do NOT persist ack so we re-prompt.
      return configured;
    }

    await config.update(
      "hooks.writeMode",
      mode,
      vscode.ConfigurationTarget.Global
    );
    await this.context.globalState.update(DIVERGE_ACK_KEY, true);
    this.hookChannel.appendLine(
      `hookManager: divergence acknowledged — writeMode=${mode}`
    );
    return mode;
  }

  private isManagedCommand(cmd: string | undefined): boolean {
    if (typeof cmd !== "string") return false;
    return cmd.trimStart().startsWith("YGG_MANAGED=");
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
    this.releaseLock();
    this.hookChannel.dispose();
  }
}

/**
 * Check if a PID is alive by sending signal 0.
 * Returns false if the process does not exist (ESRCH).
 */
function isPidAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (e: unknown) {
    if ((e as NodeJS.ErrnoException).code === "ESRCH") {
      return false;
    }
    // EPERM means the process exists but we can't signal it — still alive
    return true;
  }
}
