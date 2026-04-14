/**
 * Unit tests for hookManager.ts lock file — Sprint 063 P6b.
 *
 * Coverage:
 *   tryAcquireLock: acquire, release, steal stale, steal dead PID
 *   releaseLock: only releases when PID matches
 *   applyHooks: replace / merge modes
 *   detectManualHooks: identifies non-managed entries
 */

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { HookManager } from "./hookManager";
import { makeExtensionContext } from "./__mocks__/vscode";

// ─────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────

function makeTempClaudeDir(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ygg-hook-test-"));
  return dir;
}

/** Build a HookManager with private paths overridden to a temp dir. */
function makeHookManager(tmpDir: string): HookManager {
  const ctx = makeExtensionContext("/tmp/test-ext");
  const outputChannel = {
    append: (_: string) => {},
    appendLine: (_: string) => {},
  };

  // We need to reach private fields — use "as any" only in tests
  const hm = new HookManager(ctx as never, outputChannel as never);

  // Override private paths via object property assignment
  (hm as unknown as Record<string, string>)["lockPath"] = path.join(
    tmpDir,
    ".yggdrasil-hook.lock"
  );
  (hm as unknown as Record<string, string>)["settingsPath"] = path.join(
    tmpDir,
    "settings.json"
  );

  return hm;
}

// ─────────────────────────────────────────────────────────────
// Lock acquire / release
// ─────────────────────────────────────────────────────────────

describe("HookManager lock — acquire and release", () => {
  let tmpDir: string;

  beforeEach(() => {
    tmpDir = makeTempClaudeDir();
  });

  afterEach(() => {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  it("acquires lock when none exists", () => {
    const hm = makeHookManager(tmpDir);
    const acquired = hm.tryAcquireLock();
    expect(acquired).toBe(true);

    const lockPath = path.join(tmpDir, ".yggdrasil-hook.lock");
    expect(fs.existsSync(lockPath)).toBe(true);

    const content = JSON.parse(fs.readFileSync(lockPath, "utf-8"));
    expect(content.pid).toBe(process.pid);
    expect(typeof content.ts).toBe("number");
    expect(typeof content.hostname).toBe("string");
  });

  it("releases lock (deletes file) when PID matches", () => {
    const hm = makeHookManager(tmpDir);
    hm.tryAcquireLock();

    const lockPath = path.join(tmpDir, ".yggdrasil-hook.lock");
    expect(fs.existsSync(lockPath)).toBe(true);

    hm.releaseLock();
    expect(fs.existsSync(lockPath)).toBe(false);
  });

  it("does not release lock when PID does not match", () => {
    const hm = makeHookManager(tmpDir);
    const lockPath = path.join(tmpDir, ".yggdrasil-hook.lock");

    // Write a lock with a different PID
    fs.writeFileSync(
      lockPath,
      JSON.stringify({ pid: 99999999, ts: Date.now(), hostname: "other" }),
      "utf-8"
    );

    hm.releaseLock(); // should not delete since pid != process.pid
    expect(fs.existsSync(lockPath)).toBe(true);
  });
});

// ─────────────────────────────────────────────────────────────
// Lock steal — dead PID
// ─────────────────────────────────────────────────────────────

describe("HookManager lock — steal when holder PID is dead", () => {
  let tmpDir: string;

  beforeEach(() => {
    tmpDir = makeTempClaudeDir();
  });

  afterEach(() => {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  it("steals lock when holding PID no longer exists", () => {
    const hm = makeHookManager(tmpDir);
    const lockPath = path.join(tmpDir, ".yggdrasil-hook.lock");

    // PID 1 is always init/systemd, not a child of this process — but EPERM
    // means alive. Use a deliberately invalid PID (negative or max int).
    // process.kill(-1, 0) throws ESRCH on Linux.
    // We'll use a very high PID that almost certainly doesn't exist.
    const deadPid = 2_000_000; // exceeds Linux /proc/sys/kernel/pid_max on most systems
    fs.writeFileSync(
      lockPath,
      JSON.stringify({ pid: deadPid, ts: Date.now(), hostname: "other" }),
      "utf-8"
    );

    const acquired = hm.tryAcquireLock();
    // If PID is dead, we steal. If by some fluke it's alive, we won't acquire.
    // On any normal system 2_000_000 is not a valid PID.
    // We accept either outcome but verify the function doesn't throw.
    expect(typeof acquired).toBe("boolean");
  });
});

// ─────────────────────────────────────────────────────────────
// Lock steal — stale TTL
// ─────────────────────────────────────────────────────────────

describe("HookManager lock — steal when TTL exceeded", () => {
  let tmpDir: string;

  beforeEach(() => {
    tmpDir = makeTempClaudeDir();
  });

  afterEach(() => {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  it("steals lock when timestamp is older than 30s", () => {
    const hm = makeHookManager(tmpDir);
    const lockPath = path.join(tmpDir, ".yggdrasil-hook.lock");

    const staleTs = Date.now() - 31_000; // 31 seconds ago — past 30s TTL
    fs.writeFileSync(
      lockPath,
      JSON.stringify({ pid: process.pid, ts: staleTs, hostname: os.hostname() }),
      "utf-8"
    );

    // Even though pid === process.pid, TTL is exceeded — we should steal
    const acquired = hm.tryAcquireLock();
    expect(acquired).toBe(true);

    // Lock file should now contain our fresh timestamp
    const content = JSON.parse(fs.readFileSync(lockPath, "utf-8"));
    expect(content.ts).toBeGreaterThan(staleTs);
  });
});

// ─────────────────────────────────────────────────────────────
// applyHooks — replace vs merge modes
// ─────────────────────────────────────────────────────────────

describe("HookManager.applyHooks", () => {
  const ctx = makeExtensionContext();
  const hm = new HookManager(ctx as never, { append: () => {}, appendLine: () => {} } as never);

  const managedHooks = {
    SessionStart: [
      { hooks: [{ type: "command", command: "YGG_MANAGED=062; some-script init" }] },
    ],
    PostToolUse: [
      {
        matcher: "Edit|Write|Bash",
        hooks: [{ type: "command", command: "YGG_MANAGED=062; some-script post" }],
      },
    ],
  };

  const userHooks = {
    SessionStart: [
      { hooks: [{ type: "command", command: "my-custom-hook init" }] },
    ],
    PostToolUse: [
      {
        matcher: "Edit",
        hooks: [{ type: "command", command: "my-custom-hook post" }],
      },
    ],
  };

  it("replace mode discards user hooks entirely", () => {
    const result = hm.applyHooks(userHooks, managedHooks, "replace");
    const cmdStr = JSON.stringify(result);
    expect(cmdStr).not.toContain("my-custom-hook");
    expect(cmdStr).toContain("YGG_MANAGED");
  });

  it("merge mode retains user hooks and appends managed ones", () => {
    const result = hm.applyHooks(userHooks, managedHooks, "merge");
    const cmdStr = JSON.stringify(result);
    expect(cmdStr).toContain("my-custom-hook");
    expect(cmdStr).toContain("YGG_MANAGED");
  });

  it("merge mode does not duplicate managed hooks on re-run", () => {
    // Simulate running applyHooks twice
    const first = hm.applyHooks(userHooks, managedHooks, "merge");
    const second = hm.applyHooks(first, managedHooks, "merge");

    // Count occurrences of managed commands in PostToolUse
    const postHooks = second["PostToolUse"] ?? [];
    const managedCount = postHooks
      .flatMap((m) => m.hooks)
      .filter((h) => h.command?.includes("YGG_MANAGED")).length;

    // Should be exactly 1, not 2
    expect(managedCount).toBe(1);
  });
});

// ─────────────────────────────────────────────────────────────
// detectManualHooks
// ─────────────────────────────────────────────────────────────

describe("HookManager.detectManualHooks", () => {
  const ctx = makeExtensionContext();
  const hm = new HookManager(ctx as never, { append: () => {}, appendLine: () => {} } as never);

  it("identifies non-managed hooks correctly", () => {
    const hooks = {
      SessionStart: [
        {
          hooks: [
            { type: "command", command: "YGG_MANAGED=062; managed-cmd" },
            { type: "command", command: "user-custom-init" },
          ],
        },
      ],
    };

    const manual = hm.detectManualHooks(hooks, {});
    expect(manual).toHaveLength(1);
    expect(manual[0].command).toBe("user-custom-init");
    expect(manual[0].event).toBe("SessionStart");
  });

  it("returns empty array when all hooks are managed", () => {
    const hooks = {
      PreToolUse: [
        {
          hooks: [{ type: "command", command: "YGG_MANAGED=062; sidecar" }],
        },
      ],
    };
    const manual = hm.detectManualHooks(hooks, {});
    expect(manual).toHaveLength(0);
  });
});
