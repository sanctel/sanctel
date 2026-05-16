import { describe, expect, it } from "vitest";
import { parseAttachError } from "./attach-error";

describe("parseAttachError", () => {
  // The Rust side emits "worktree-missing: <path>" via AttachError::Display
  // (src-tauri/src/terminal_runtime.rs). The prefix is the wire contract —
  // the broken-tab UI routes off it.
  it("routes `worktree-missing:` prefix to the broken-tab path", () => {
    const got = parseAttachError("worktree-missing: /Users/me/wt/deleted");
    expect(got).toEqual({
      kind: "worktree-missing",
      path: "/Users/me/wt/deleted",
    });
  });

  it("treats anything else as a generic attach failure", () => {
    const got = parseAttachError("tmux new-window failed: no server");
    expect(got).toEqual({
      kind: "other",
      message: "tmux new-window failed: no server",
    });
  });

  it("accepts Error instances and plain values", () => {
    expect(parseAttachError(new Error("worktree-missing: /p")).kind).toBe(
      "worktree-missing",
    );
    expect(parseAttachError(undefined).kind).toBe("other");
  });
});
