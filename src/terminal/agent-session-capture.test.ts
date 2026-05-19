import { describe, expect, it, vi } from "vitest";

import {
  discoverCapturedAgentSession,
  encodeCwd,
  pickFirstSessionAfter,
  startAgentSessionCapture,
  type AgentSessionFs,
} from "./agent-session-capture";

describe("encodeCwd", () => {
  it("replaces forward slashes with hyphens using Claude's project-dir scheme", () => {
    expect(encodeCwd("/home/agent/workspace")).toBe("-home-agent-workspace");
  });
});

describe("pickFirstSessionAfter", () => {
  it("ignores older jsonl files and picks the first post-start session", () => {
    expect(
      pickFirstSessionAfter(
        [
          { name: "old.jsonl", mtime: 100 },
          { name: "first.jsonl", mtime: 300 },
          { name: "second.jsonl", mtime: 500 },
          { name: "notes.txt", mtime: 250 },
        ],
        200,
      ),
    ).toBe("first");
  });

  it("returns null when no jsonl was modified after start", () => {
    expect(pickFirstSessionAfter([{ name: "old.jsonl", mtime: 100 }], 200))
      .toBeNull();
  });
});

describe("discoverCapturedAgentSession", () => {
  function fixtureFs(
    files: Record<
      string,
      {
        entries?: { name: string; mtime: number }[];
        text?: string;
      }
    >,
  ): AgentSessionFs {
    return {
      async exists(path) {
        return path in files;
      },
      async readDir(path) {
        const dir = files[path];
        if (!dir?.entries) throw new Error(`no such dir: ${path}`);
        return dir.entries;
      },
      async readHeader(path) {
        const file = files[path];
        if (!file?.text) throw new Error(`no such file: ${path}`);
        return file.text;
      },
    };
  }

  // Claude jsonl files don't carry `cwd` on line 1 — the first records
  // are header-y (permission-mode, file-history-snapshot) and lack cwd.
  // The cwd field first appears on the first message record (typically
  // line 3). This helper builds a realistic jsonl header so the tests
  // exercise the multi-line scan rather than a synthetic single-record
  // shape that would mask the bug fixed alongside this rewrite.
  function jsonlHeaderWithCwd(cwd: string, sessionId = "test-session"): string {
    return [
      JSON.stringify({ type: "permission-mode", permissionMode: "auto", sessionId }),
      JSON.stringify({ type: "file-history-snapshot", messageId: "m1" }),
      JSON.stringify({ type: "user", cwd, sessionId, message: { role: "user", content: "hi" } }),
    ].join("\n");
  }

  it("scans the Claude project dir for a post-start transcript", async () => {
    const id = await discoverCapturedAgentSession({
      worktreePath: "/home/agent/workspace",
      startedAt: 200,
      home: "/home/me",
      fs: fixtureFs({
        "/home/me/.claude/projects/-home-agent-workspace": {
          entries: [
            { name: "before.jsonl", mtime: 100 },
            { name: "after.jsonl", mtime: 300 },
          ],
        },
        "/home/me/.claude/projects/-home-agent-workspace/after.jsonl": {
          text: jsonlHeaderWithCwd("/home/agent/workspace"),
        },
      }),
    });

    expect(id).toBe("after");
  });

  it("skips an earlier post-start jsonl from a different worktree cwd", async () => {
    const id = await discoverCapturedAgentSession({
      worktreePath: "/home/agent/workspace",
      startedAt: 200,
      home: "/home/me",
      fs: fixtureFs({
        "/home/me/.claude/projects/-home-agent-workspace": {
          entries: [
            { name: "other-worktree.jsonl", mtime: 300 },
            { name: "chat-tab.jsonl", mtime: 500 },
          ],
        },
        "/home/me/.claude/projects/-home-agent-workspace/other-worktree.jsonl": {
          text: jsonlHeaderWithCwd("/home/agent/other"),
        },
        "/home/me/.claude/projects/-home-agent-workspace/chat-tab.jsonl": {
          text: jsonlHeaderWithCwd("/HOME/AGENT/WORKSPACE/"),
        },
      }),
    });

    expect(id).toBe("chat-tab");
  });

  // Regression pin: when the first line is a record without `cwd` (which
  // is claude's actual format), the scan must keep going until it finds a
  // record that has one. Pre-fix, the discovery rejected every jsonl
  // because it only checked line 1.
  it("finds cwd on a later line when the first record lacks it", async () => {
    const id = await discoverCapturedAgentSession({
      worktreePath: "/home/agent/workspace",
      startedAt: 200,
      home: "/home/me",
      fs: fixtureFs({
        "/home/me/.claude/projects/-home-agent-workspace": {
          entries: [{ name: "real.jsonl", mtime: 300 }],
        },
        "/home/me/.claude/projects/-home-agent-workspace/real.jsonl": {
          text: jsonlHeaderWithCwd("/home/agent/workspace"),
        },
      }),
    });

    expect(id).toBe("real");
  });
});

describe("startAgentSessionCapture", () => {
  it("stops polling after maxDurationMs and remains idempotent to stop", async () => {
    vi.useFakeTimers();
    try {
      let existsCalls = 0;
      const fs: AgentSessionFs = {
        async exists() {
          existsCalls += 1;
          return false;
        },
        async readDir() {
          return [];
        },
        async readHeader() {
          return "";
        },
      };

      const capture = startAgentSessionCapture({
        tabId: "tab-chat",
        worktreePath: "/home/agent/workspace",
        startedAt: 1000,
        home: "/home/me",
        fs,
        intervalMs: 10,
        maxDurationMs: 25,
        onSession: vi.fn(),
      });

      await vi.advanceTimersByTimeAsync(100);

      expect(existsCalls).toBe(2);
      capture.stop();
      capture.stop();
    } finally {
      vi.useRealTimers();
    }
  });
});
