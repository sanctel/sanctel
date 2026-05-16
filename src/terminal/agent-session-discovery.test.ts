import { describe, expect, it } from "vitest";

import {
  discoverAgentSession,
  encodeCwd,
  pickNewestSessionId,
  type AgentSessionFs,
} from "./agent-session-discovery";

describe("encodeCwd", () => {
  it("replaces forward slashes with hyphens (Claude's scheme)", () => {
    expect(encodeCwd("/home/agent/workspace")).toBe("-home-agent-workspace");
  });

  it("handles a worktree path with trailing slash by ignoring it", () => {
    expect(encodeCwd("/home/agent/workspace/")).toBe("-home-agent-workspace");
  });

  it("handles a single-segment absolute path", () => {
    expect(encodeCwd("/tmp")).toBe("-tmp");
  });

  it("handles a deeply nested branch worktree", () => {
    expect(encodeCwd("/Users/me/code/sanctel/.worktrees/feat-x")).toBe(
      "-Users-me-code-sanctel-.worktrees-feat-x",
    );
  });
});

describe("pickNewestSessionId", () => {
  it("returns null for an empty list", () => {
    expect(pickNewestSessionId([])).toBeNull();
  });

  it("picks the newest by mtime and strips the .jsonl suffix", () => {
    const entries = [
      { name: "aaa.jsonl", mtime: 100 },
      { name: "bbb.jsonl", mtime: 300 },
      { name: "ccc.jsonl", mtime: 200 },
    ];
    expect(pickNewestSessionId(entries)).toBe("bbb");
  });

  it("ignores non-jsonl entries", () => {
    const entries = [
      { name: "aaa.jsonl", mtime: 100 },
      { name: "bbb.txt", mtime: 999 },
      { name: "subdir", mtime: 800 },
    ];
    expect(pickNewestSessionId(entries)).toBe("aaa");
  });

  it("returns null when no jsonl files are present", () => {
    expect(pickNewestSessionId([{ name: "notes.md", mtime: 1 }])).toBeNull();
  });
});

describe("discoverAgentSession", () => {
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

  it("returns the newest session id for a worktree cwd that has prior sessions", async () => {
    const fs = fixtureFs({
      "/home/me/.claude/projects/-home-agent-workspace": {
        entries: [
          { name: "old-session.jsonl", mtime: 100 },
          { name: "newer-session.jsonl", mtime: 500 },
          { name: "stale-session.jsonl", mtime: 200 },
        ],
      },
    });

    const id = await discoverAgentSession({
      cwd: "/home/agent/workspace",
      home: "/home/me",
      fs,
    });

    expect(id).toBe("newer-session");
  });

  it("returns null when the projects dir does not exist (first-ever chat in this cwd)", async () => {
    const fs = fixtureFs({});
    const id = await discoverAgentSession({
      cwd: "/home/agent/workspace",
      home: "/home/me",
      fs,
    });
    expect(id).toBeNull();
  });

  it("returns null when the projects dir exists but has no jsonl files", async () => {
    const fs = fixtureFs({
      "/home/me/.claude/projects/-home-agent-workspace": {
        entries: [{ name: "logs", mtime: 1 }],
      },
    });
    const id = await discoverAgentSession({
      cwd: "/home/agent/workspace",
      home: "/home/me",
      fs,
    });
    expect(id).toBeNull();
  });

  it("returns null on a filesystem read error rather than throwing", async () => {
    const fs: AgentSessionFs = {
      async exists() {
        return true;
      },
      async readDir() {
        throw new Error("permission denied");
      },
    };
    const id = await discoverAgentSession({
      cwd: "/home/agent/workspace",
      home: "/home/me",
      fs,
    });
    expect(id).toBeNull();
  });
});
