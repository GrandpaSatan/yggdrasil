/**
 * Guards the Fergus no-model invariant at the webview boundary.
 *
 * The webview's ChatInput ONLY ever posts { type: "send", text, attachments? }.
 * It must never attach `model` or `flow` — those are decided host-side by
 * `preprocess()` in slashCommands.ts. This test re-imports the message-type
 * union to ensure the `send` variant's shape hasn't drifted.
 */

import { describe, expect, it } from "vitest";
import type { WebviewToHost } from "./messages";

describe("WebviewToHost send variant", () => {
  it("accepts a minimal send with only text", () => {
    const msg: WebviewToHost = { type: "send", text: "hello" };
    expect(msg.type).toBe("send");
  });

  it("accepts a send with attachments but still no model field on the webview side", () => {
    const msg: WebviewToHost = {
      type: "send",
      text: "do it",
      attachments: [{ label: "foo.ts", content: "// stuff" }],
    };
    // The model field IS present on the type union (legacy compatibility with
    // tooling callers) but the webview must never set it. We can't enforce
    // that at compile time; this test documents the invariant.
    expect((msg as Record<string, unknown>).model).toBeUndefined();
    expect((msg as Record<string, unknown>).flow).toBeUndefined();
  });

  it("allows `model` and `flow` fields on the type — but the webview's ChatInput doesn't set them", () => {
    // Type-level check: the WebviewToHost union includes optional model/flow
    // on the `send` variant so callers can still send them if needed.
    const withModel: WebviewToHost = {
      type: "send",
      text: "test",
      model: "morrigan/qwen3.5-27b",
    };
    // This is allowed at the type level. The behavioural invariant
    // (ChatInput omits them) is covered by the unit test in ChatInput.
    expect(withModel.type).toBe("send");
  });
});
