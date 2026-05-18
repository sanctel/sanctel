// Tests for the tabStore ↔ Persistence wire. We mock `@tauri-apps/api/core`
// `invoke` so the store's IPC calls are observable without a Tauri runtime,
// and use `InMemoryPersistence` as the storage backend.
//
// The "survives a quit" criterion is exercised by hydrating a second store
// against the SAME persistence instance — the same shape as a real restart
// where two processes share the on-disk SQLite file.

import { beforeEach, describe, expect, it, vi } from "vitest";

// `vi.mock` is hoisted to the top of the file; the factory can't close
// over file-scope variables. `vi.hoisted` runs *before* the hoisted mocks
// so the spy is available inside the factory and to the test body.
const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

// tmuxStatusStore.hydrate uses listen/invoke too; stub both so importing
// tabStore (which transitively imports tmuxStatusStore) doesn't try to
// talk to a non-existent Tauri host.
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

import { createTabStore } from "./tabStore";
import { InMemoryPersistence } from "../persistence/in-memory";
import { useTmuxStatus } from "./tmuxStatusStore";

function markTmuxAvailable() {
  useTmuxStatus.setState({
    status: { backend: "tmux", available: true, version: "tmux 3.4", error: null },
    loaded: true,
  });
}

// Counter used by the default `create_tab` mock so each auto-allocated tab
// in a test gets a distinct `term-N`, modelling Rust's server-side
// allocator. Reset between tests.
let nextTermN = 1;

function mockCreateTabAutoAllocate(cmd: string, args: unknown): unknown {
  if (cmd !== "create_tab") return Promise.resolve(undefined);
  const req = (args as { req?: { kind?: string; windowName?: string | null } })
    .req;
  if (!req) return Promise.resolve({ windowName: null });
  const isTerminalLike = req.kind === "terminal" || req.kind === "chat";
  const askedForAuto =
    req.windowName === "auto" || req.windowName == null;
  if (isTerminalLike && askedForAuto) {
    const name = `term-${nextTermN++}`;
    return Promise.resolve({ windowName: name });
  }
  return Promise.resolve({ windowName: null });
}

beforeEach(() => {
  invokeMock.mockReset();
  nextTermN = 1;
  // Default mock: `create_tab` for an auto-allocated terminal/chat returns
  // a CreateTabResp with the next `term-N`; everything else (non-terminal
  // create_tab, close_tab, show_tab …) resolves to undefined.
  invokeMock.mockImplementation(mockCreateTabAutoAllocate);
  markTmuxAvailable();
});

describe("tabStore hydrate", () => {
  it("on first launch, seeds the DB with the default profile + space", async () => {
    const persistence = new InMemoryPersistence();
    const useStore = createTabStore();

    await useStore.getState().hydrate(persistence);

    const snap = await persistence.loadAll();
    expect(snap.profiles).toHaveLength(1);
    expect(snap.profiles[0].id).toBe("profile-default");
    expect(snap.spaces).toHaveLength(1);
    expect(snap.spaces[0].id).toBe("space-default");
    expect(snap.tabs).toEqual([]);
  });

  it("replays create_tab for each persisted tab in sort order", async () => {
    const persistence = new InMemoryPersistence();
    await persistence.saveProfile({
      id: "profile-default",
      name: "Default",
      color: null,
      isDefault: true,
    });
    await persistence.saveSpace({
      id: "space-default",
      profileId: "profile-default",
      name: "Default",
      color: "#6366f1",
      sortOrder: 0,
    });
    await persistence.saveTab({
      id: "tab-2",
      spaceId: "space-default",
      kind: "terminal",
      title: "build watcher",
      sortOrder: 1,
      url: "local://terminal",
      worktreeId: "sanctel-main",
      windowName: "term-2",
      initialCommand: null,
      agentSessionId: null,
    });
    await persistence.saveTab({
      id: "tab-1",
      spaceId: "space-default",
      kind: "browser",
      title: "duckduckgo",
      sortOrder: 0,
      url: "https://duckduckgo.com",
      worktreeId: null,
      windowName: null,
      initialCommand: null,
      agentSessionId: null,
    });

    const useStore = createTabStore();
    await useStore.getState().hydrate(persistence);

    // Sidebar state reflects the rows in sort order.
    const tabs = useStore.getState().tabs;
    expect(tabs.map((t) => t.id)).toEqual(["tab-1", "tab-2"]);
    expect(tabs[1].title).toBe("build watcher");
    expect(tabs[1].windowName).toBe("term-2");
    expect(tabs[1].worktreeId).toBe("sanctel-main");

    // create_tab was invoked per row, in sort order.
    const createTabCalls = invokeMock.mock.calls.filter(
      ([cmd]) => cmd === "create_tab",
    );
    expect(createTabCalls).toHaveLength(2);
    expect(createTabCalls[0][1].req.id).toBe("tab-1");
    expect(createTabCalls[1][1].req.id).toBe("tab-2");
    // Worktree-keyed terminal payload carries the full identity.
    expect(createTabCalls[1][1].req).toMatchObject({
      id: "tab-2",
      kind: "terminal",
      profileId: "profile-default",
      worktreeId: "sanctel-main",
      windowName: "term-2",
    });
  });

  it("two stores against the same persistence end up with the same tabs (the quit/relaunch shape)", async () => {
    const persistence = new InMemoryPersistence();

    const useStoreA = createTabStore();
    await useStoreA.getState().hydrate(persistence);

    // Create a browser tab + a terminal tab in store A.
    await useStoreA.getState().newTab("browser", "https://example.com");
    await useStoreA.getState().newTerminalTab("sanctel-main");
    await useStoreA
      .getState()
      .renameTab(useStoreA.getState().tabs[1].id, "build watcher");

    // "Quit + relaunch": throw away store A, create store B against the
    // same persistence, hydrate.
    invokeMock.mockClear();
    const useStoreB = createTabStore();
    await useStoreB.getState().hydrate(persistence);

    const tabs = useStoreB.getState().tabs;
    expect(tabs).toHaveLength(2);
    expect(tabs[0].kind).toBe("browser");
    expect(tabs[0].url).toBe("https://example.com");
    expect(tabs[1].kind).toBe("terminal");
    expect(tabs[1].title).toBe("build watcher");
    expect(tabs[1].worktreeId).toBe("sanctel-main");
    // create_tab was replayed for both tabs.
    const createTabCalls = invokeMock.mock.calls.filter(
      ([cmd]) => cmd === "create_tab",
    );
    expect(createTabCalls).toHaveLength(2);
  });
});

describe("tabStore mutations persist", () => {
  it("newTab writes a tab row before invoking create_tab", async () => {
    const persistence = new InMemoryPersistence();
    const useStore = createTabStore();
    await useStore.getState().hydrate(persistence);

    invokeMock.mockClear();
    await useStore.getState().newTab("browser", "https://example.com");

    const snap = await persistence.loadAll();
    expect(snap.tabs).toHaveLength(1);
    expect(snap.tabs[0].kind).toBe("browser");
    expect(snap.tabs[0].url).toBe("https://example.com");

    // And create_tab was invoked with the new tab's id.
    const createTabCall = invokeMock.mock.calls.find(
      ([cmd]) => cmd === "create_tab",
    );
    expect(createTabCall?.[1].req.id).toBe(snap.tabs[0].id);
  });

  it("newTerminalTab persists the windowName returned by create_tab", async () => {
    const persistence = new InMemoryPersistence();
    const useStore = createTabStore();
    await useStore.getState().hydrate(persistence);

    await useStore.getState().newTerminalTab("sanctel-main");

    const snap = await persistence.loadAll();
    expect(snap.tabs).toHaveLength(1);
    expect(snap.tabs[0]).toMatchObject({
      kind: "terminal",
      worktreeId: "sanctel-main",
      // The default mock simulates server-side allocation by returning
      // sequential term-N values from CreateTabResp.windowName.
      windowName: "term-1",
    });
  });

  it("newTerminalTab passes windowName: 'auto' to create_tab (no client-side listing)", async () => {
    const persistence = new InMemoryPersistence();
    const useStore = createTabStore();
    await useStore.getState().hydrate(persistence);

    invokeMock.mockClear();
    await useStore.getState().newTerminalTab("sanctel-main");

    // No client-side listing call exists anymore; the issue removed that
    // command. The "auto" sentinel is the only thing on the wire.
    const listCalls = invokeMock.mock.calls.filter(
      ([cmd]) => cmd === "terminal_list_window_names",
    );
    expect(listCalls).toHaveLength(0);

    const createCall = invokeMock.mock.calls.find(
      ([cmd]) => cmd === "create_tab",
    );
    expect(createCall?.[1].req).toMatchObject({
      kind: "terminal",
      worktreeId: "sanctel-main",
      windowName: "auto",
    });
  });

  it("two newTerminalTab calls receive distinct term-N names from create_tab", async () => {
    const persistence = new InMemoryPersistence();
    const useStore = createTabStore();
    await useStore.getState().hydrate(persistence);

    const a = await useStore.getState().newTerminalTab("sanctel-main");
    const b = await useStore.getState().newTerminalTab("sanctel-main");

    expect(a.windowName).toBe("term-1");
    expect(b.windowName).toBe("term-2");
    const snap = await persistence.loadAll();
    expect(snap.tabs.map((t) => t.windowName)).toEqual(["term-1", "term-2"]);
  });

  it("renameTab persists the new title but leaves windowName alone", async () => {
    const persistence = new InMemoryPersistence();
    const useStore = createTabStore();
    await useStore.getState().hydrate(persistence);
    await useStore.getState().newTerminalTab("sanctel-main");
    const tabId = useStore.getState().tabs[0].id;

    await useStore.getState().renameTab(tabId, "build watcher");

    const snap = await persistence.loadAll();
    expect(snap.tabs[0].title).toBe("build watcher");
    expect(snap.tabs[0].windowName).toBe("term-1");
  });

  it("newChatTab ignores a create-time agentSessionId candidate", async () => {
    const persistence = new InMemoryPersistence();
    const useStore = createTabStore();
    await useStore.getState().hydrate(persistence);

    await (
      useStore.getState().newChatTab as unknown as (
        worktreeId: string,
        createTimeAgentSessionId: string,
      ) => Promise<unknown>
    )("sanctel-main", "fresh-unrelated");

    const snap = await persistence.loadAll();
    expect(snap.tabs).toHaveLength(1);
    expect(snap.tabs[0]).toMatchObject({
      kind: "chat",
      worktreeId: "sanctel-main",
      windowName: "term-1",
      initialCommand: "claude",
      agentSessionId: null,
    });

    // The create_tab payload must not capture a create-time transcript
    // candidate that could belong to another Claude process.
    const createTabCall = invokeMock.mock.calls.find(
      ([cmd]) => cmd === "create_tab",
    );
    expect(createTabCall?.[1].req).toMatchObject({
      kind: "chat",
      worktreeId: "sanctel-main",
      windowName: "auto",
      initialCommand: "claude",
      agentSessionId: null,
    });
  });

  it("newChatTab with no prior session persists plain claude (no --resume)", async () => {
    const persistence = new InMemoryPersistence();
    const useStore = createTabStore();
    await useStore.getState().hydrate(persistence);

    await useStore.getState().newChatTab("sanctel-main");

    const snap = await persistence.loadAll();
    expect(snap.tabs[0]).toMatchObject({
      kind: "chat",
      initialCommand: "claude",
      agentSessionId: null,
    });
  });

  it("newChatTab throws tmux-missing when the probe says tmux is unavailable", async () => {
    useTmuxStatus.setState({
      status: { backend: "tmux", available: false, version: null, error: "missing" },
      loaded: true,
    });

    const persistence = new InMemoryPersistence();
    const useStore = createTabStore();
    await useStore.getState().hydrate(persistence);

    await expect(
      useStore.getState().newChatTab("sanctel-main"),
    ).rejects.toThrow(/tmux-missing/);
  });

  it("closeTab removes the row", async () => {
    const persistence = new InMemoryPersistence();
    const useStore = createTabStore();
    await useStore.getState().hydrate(persistence);

    const tab = await useStore.getState().newTab("browser", "https://x");
    expect((await persistence.loadAll()).tabs).toHaveLength(1);

    await useStore.getState().closeTab(tab.id);
    expect((await persistence.loadAll()).tabs).toEqual([]);
  });
});
