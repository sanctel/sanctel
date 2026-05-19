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
      async readFirstLine(path) {
        const file = files[path];
        if (!file?.text) throw new Error(`no such file: ${path}`);
        return file.text.split(/\r?\n/, 1)[0] ?? "";
      },
    };
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
          text: JSON.stringify({ cwd: "/home/agent/workspace" }),
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
          text: JSON.stringify({ cwd: "/home/agent/other" }),
        },
        "/home/me/.claude/projects/-home-agent-workspace/chat-tab.jsonl": {
          text: JSON.stringify({ cwd: "/home/agent/workspace/" }),
        },
      }),
    });

    expect(id).toBe("chat-tab");
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
        async readFirstLine() {
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
