import { describe, expect, it } from "vitest";

import {
  discoverCapturedAgentSession,
  encodeCwd,
  pickFirstSessionAfter,
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
    files: Record<string, { entries: { name: string; mtime: number }[] }>,
  ): AgentSessionFs {
    return {
      async exists(path) {
        return path in files;
      },
      async readDir(path) {
        const dir = files[path];
        if (!dir) throw new Error(`no such dir: ${path}`);
        return dir.entries;
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
      }),
    });

    expect(id).toBe("after");
  });
});
