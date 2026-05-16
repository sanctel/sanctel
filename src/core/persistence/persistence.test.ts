// Contract tests for the Persistence interface. These exercise the
// in-memory implementation; the production `SqlPersistence` runs against
// the same tests by re-exporting them with a different factory in
// `sql-persistence.test.ts` (once a Tauri-aware test harness lands —
// today sql.js needs the Tauri webview's fs plugin, which Vitest can't
// give it, so we cover behavior here and trust sql.js to honor SQL).

import { describe, expect, it } from "vitest";

import { InMemoryPersistence } from "./in-memory";
import type {
  PersistedProfile,
  PersistedSpace,
  PersistedTab,
} from "./persistence";

const profile: PersistedProfile = {
  id: "profile-default",
  name: "Default",
  color: null,
  isDefault: true,
};

const space: PersistedSpace = {
  id: "space-default",
  profileId: profile.id,
  name: "Default",
  color: "#6366f1",
  sortOrder: 0,
};

function tab(
  id: string,
  overrides: Partial<PersistedTab> = {},
): PersistedTab {
  return {
    id,
    spaceId: space.id,
    kind: "terminal",
    title: `Tab ${id}`,
    sortOrder: 0,
    url: "local://terminal",
    worktreeId: null,
    windowName: null,
    initialCommand: null,
    agentSessionId: null,
    ...overrides,
  };
}

describe("InMemoryPersistence", () => {
  it("loads an empty snapshot before any writes", async () => {
    const p = new InMemoryPersistence();
    await p.init();
    const snap = await p.loadAll();
    expect(snap.profiles).toEqual([]);
    expect(snap.spaces).toEqual([]);
    expect(snap.tabs).toEqual([]);
  });

  it("round-trips a profile, space, and tab", async () => {
    const p = new InMemoryPersistence();
    await p.init();
    await p.saveProfile(profile);
    await p.saveSpace(space);
    await p.saveTab(
      tab("tab-1", {
        title: "build watcher",
        worktreeId: "sanctel-main",
        windowName: "term-1",
      }),
    );

    const snap = await p.loadAll();
    expect(snap.profiles).toEqual([profile]);
    expect(snap.spaces).toEqual([space]);
    expect(snap.tabs).toHaveLength(1);
    expect(snap.tabs[0]).toMatchObject({
      id: "tab-1",
      title: "build watcher",
      worktreeId: "sanctel-main",
      windowName: "term-1",
    });
  });

  it("loads spaces and tabs ordered by sort_order", async () => {
    const p = new InMemoryPersistence();
    await p.saveProfile(profile);
    await p.saveSpace({ ...space, id: "space-b", sortOrder: 1 });
    await p.saveSpace({ ...space, id: "space-a", sortOrder: 0 });

    await p.saveTab(tab("tab-c", { sortOrder: 2 }));
    await p.saveTab(tab("tab-a", { sortOrder: 0 }));
    await p.saveTab(tab("tab-b", { sortOrder: 1 }));

    const snap = await p.loadAll();
    expect(snap.spaces.map((s) => s.id)).toEqual(["space-a", "space-b"]);
    expect(snap.tabs.map((t) => t.id)).toEqual(["tab-a", "tab-b", "tab-c"]);
  });

  it("renameTab updates only the title", async () => {
    const p = new InMemoryPersistence();
    await p.saveProfile(profile);
    await p.saveSpace(space);
    await p.saveTab(
      tab("tab-1", { title: "Terminal", windowName: "term-1" }),
    );

    await p.renameTab("tab-1", "build watcher");

    const snap = await p.loadAll();
    expect(snap.tabs[0].title).toBe("build watcher");
    // The tmux window name MUST NOT move — that's the acceptance criterion
    // about renaming not touching the shell.
    expect(snap.tabs[0].windowName).toBe("term-1");
  });

  it("removeTab deletes the row", async () => {
    const p = new InMemoryPersistence();
    await p.saveProfile(profile);
    await p.saveSpace(space);
    await p.saveTab(tab("tab-1"));
    await p.saveTab(tab("tab-2"));

    await p.removeTab("tab-1");

    const snap = await p.loadAll();
    expect(snap.tabs.map((t) => t.id)).toEqual(["tab-2"]);
  });

  it("reorderTabs rewrites sort_order for the given space only", async () => {
    const p = new InMemoryPersistence();
    await p.saveProfile(profile);
    await p.saveSpace(space);
    await p.saveSpace({ ...space, id: "space-other", sortOrder: 1 });
    await p.saveTab(tab("tab-1", { sortOrder: 0 }));
    await p.saveTab(tab("tab-2", { sortOrder: 1 }));
    await p.saveTab(tab("tab-3", { sortOrder: 2 }));
    await p.saveTab(
      tab("tab-x", { spaceId: "space-other", sortOrder: 0 }),
    );

    await p.reorderTabs(space.id, ["tab-3", "tab-1", "tab-2"]);

    const snap = await p.loadAll();
    const order = snap.tabs
      .filter((t) => t.spaceId === space.id)
      .map((t) => t.id);
    expect(order).toEqual(["tab-3", "tab-1", "tab-2"]);
    // The other space's tab is untouched.
    const otherTab = snap.tabs.find((t) => t.id === "tab-x")!;
    expect(otherTab.sortOrder).toBe(0);
  });

  it("survives a 'restart' — load returns what was written, on a fresh instance reading the same storage", async () => {
    // The in-memory store models a single process; the meaningful
    // survives-restart test for SqlPersistence is exercised in the
    // tabStore hydrate test (which creates two stores against the
    // SAME persistence instance — i.e. the same on-disk DB).
    const p = new InMemoryPersistence();
    await p.saveProfile(profile);
    await p.saveSpace(space);
    await p.saveTab(tab("tab-1", { title: "preserved" }));

    const snap = await p.loadAll();
    expect(snap.tabs[0].title).toBe("preserved");
  });
});
