/**
 * Unit tests for mimirClient.ts — Sprint 063 P7b.
 *
 * Coverage:
 *   listVault: empty, with entries
 *   setVault: success, 400 error
 *   getVault: normal
 *   deleteVault: normal
 *   invalid JSON response
 */

import { describe, it, expect, vi } from "vitest";
import * as http from "http";
import { AddressInfo } from "net";
import { MimirClient } from "./mimirClient";

// ─────────────────────────────────────────────────────────────
// Test server helper
// ─────────────────────────────────────────────────────────────

function createJsonServer(
  statusCode: number,
  body: unknown
): Promise<{ url: string; close: () => void }> {
  const raw = typeof body === "string" ? body : JSON.stringify(body);
  return new Promise((resolve) => {
    const server = http.createServer((_req, res) => {
      res.writeHead(statusCode, { "Content-Type": "application/json" });
      res.end(raw);
    });
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address() as AddressInfo;
      resolve({
        url: `http://127.0.0.1:${port}`,
        close: () => server.close(),
      });
    });
  });
}

// ─────────────────────────────────────────────────────────────
// listVault
// ─────────────────────────────────────────────────────────────

describe("MimirClient.listVault", () => {
  it("returns empty list when vault has no secrets", async () => {
    const { url, close } = await createJsonServer(200, { secrets: [], count: 0 });

    try {
      const mimir = new MimirClient();
      vi.spyOn(mimir, "mimirUrl", "get").mockReturnValue(url);

      const result = await mimir.listVault();
      expect(result.secrets).toEqual([]);
      expect(result.count).toBe(0);
    } finally {
      close();
    }
  });

  it("returns secret metadata (not values) for each entry", async () => {
    const { url, close } = await createJsonServer(200, {
      secrets: [
        { key: "api_key", scope: "global", tags: ["env:prod"], updated_at: "2026-04-13T00:00:00Z" },
        { key: "db_pass", scope: "project:yggdrasil", tags: [], updated_at: "2026-04-12T00:00:00Z" },
      ],
      count: 2,
    });

    try {
      const mimir = new MimirClient();
      vi.spyOn(mimir, "mimirUrl", "get").mockReturnValue(url);

      const result = await mimir.listVault();
      expect(result.count).toBe(2);
      expect(result.secrets[0].key).toBe("api_key");
      expect(result.secrets[0].scope).toBe("global");
      expect(result.secrets[1].key).toBe("db_pass");
      // No value field in list response — this is intentional (security invariant)
      expect("value" in result.secrets[0]).toBe(false);
    } finally {
      close();
    }
  });
});

// ─────────────────────────────────────────────────────────────
// setVault
// ─────────────────────────────────────────────────────────────

describe("MimirClient.setVault", () => {
  it("returns id, key, scope on success", async () => {
    const { url, close } = await createJsonServer(200, {
      id: "aaaaaaaa-0000-0000-0000-000000000001",
      key: "my_secret",
      scope: "global",
    });

    try {
      const mimir = new MimirClient();
      vi.spyOn(mimir, "mimirUrl", "get").mockReturnValue(url);

      const result = await mimir.setVault("my_secret", "super-secret-value", "global", ["tag1"]);
      expect(result.key).toBe("my_secret");
      expect(result.scope).toBe("global");
      expect(result.id).toBeTruthy();
    } finally {
      close();
    }
  });

  it("rejects with error message on HTTP 400", async () => {
    const { url, close } = await createJsonServer(400, '{"error":"invalid key name"}');

    try {
      const mimir = new MimirClient();
      vi.spyOn(mimir, "mimirUrl", "get").mockReturnValue(url);

      await expect(
        mimir.setVault("bad key!", "value", "global", [])
      ).rejects.toThrow(/HTTP 400/);
    } finally {
      close();
    }
  });

  it("rejects with error message on HTTP 500", async () => {
    const { url, close } = await createJsonServer(500, '{"error":"internal error"}');

    try {
      const mimir = new MimirClient();
      vi.spyOn(mimir, "mimirUrl", "get").mockReturnValue(url);

      await expect(
        mimir.setVault("key", "value", "global", [])
      ).rejects.toThrow(/HTTP 500/);
    } finally {
      close();
    }
  });
});

// ─────────────────────────────────────────────────────────────
// getVault
// ─────────────────────────────────────────────────────────────

describe("MimirClient.getVault", () => {
  it("returns key, value, scope, tags, updated_at", async () => {
    const { url, close } = await createJsonServer(200, {
      key: "api_key",
      value: "sk-1234567890",
      scope: "global",
      tags: ["env:prod"],
      updated_at: "2026-04-13T10:00:00Z",
    });

    try {
      const mimir = new MimirClient();
      vi.spyOn(mimir, "mimirUrl", "get").mockReturnValue(url);

      const result = await mimir.getVault("api_key", "global");
      expect(result.key).toBe("api_key");
      expect(result.value).toBe("sk-1234567890");
      expect(result.scope).toBe("global");
      expect(result.tags).toContain("env:prod");
      expect(result.updated_at).toBeTruthy();
    } finally {
      close();
    }
  });

  it("rejects on HTTP 404", async () => {
    const { url, close } = await createJsonServer(404, '{"error":"not found"}');

    try {
      const mimir = new MimirClient();
      vi.spyOn(mimir, "mimirUrl", "get").mockReturnValue(url);

      await expect(
        mimir.getVault("missing_key", "global")
      ).rejects.toThrow(/HTTP 404/);
    } finally {
      close();
    }
  });
});

// ─────────────────────────────────────────────────────────────
// deleteVault
// ─────────────────────────────────────────────────────────────

describe("MimirClient.deleteVault", () => {
  it("returns deleted key and scope on success", async () => {
    const { url, close } = await createJsonServer(200, {
      deleted: "old_key",
      scope: "global",
    });

    try {
      const mimir = new MimirClient();
      vi.spyOn(mimir, "mimirUrl", "get").mockReturnValue(url);

      const result = await mimir.deleteVault("old_key", "global");
      expect(result.deleted).toBe("old_key");
      expect(result.scope).toBe("global");
    } finally {
      close();
    }
  });

  it("rejects on HTTP 404 when key not found", async () => {
    const { url, close } = await createJsonServer(404, '{"error":"key not found"}');

    try {
      const mimir = new MimirClient();
      vi.spyOn(mimir, "mimirUrl", "get").mockReturnValue(url);

      await expect(
        mimir.deleteVault("ghost_key", "global")
      ).rejects.toThrow(/HTTP 404/);
    } finally {
      close();
    }
  });
});

// ─────────────────────────────────────────────────────────────
// Invalid JSON response handling
// ─────────────────────────────────────────────────────────────

describe("MimirClient — invalid JSON response", () => {
  it("listVault returns empty defaults on malformed response", async () => {
    const { url, close } = await createJsonServer(200, "not valid json {");

    try {
      const mimir = new MimirClient();
      vi.spyOn(mimir, "mimirUrl", "get").mockReturnValue(url);

      // postJson resolves {} on parse failure
      const result = await mimir.listVault();
      expect(result.secrets).toEqual([]);
      expect(result.count).toBe(0);
    } finally {
      close();
    }
  });
});

// ─────────────────────────────────────────────────────────────
// Connection refused (Mimir unreachable)
// ─────────────────────────────────────────────────────────────

describe("MimirClient — unreachable server", () => {
  it("rejects with network error", async () => {
    const mimir = new MimirClient();
    vi.spyOn(mimir, "mimirUrl", "get").mockReturnValue("http://127.0.0.1:1");

    await expect(mimir.listVault()).rejects.toThrow();
  });
});
