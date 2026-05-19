import { beforeEach, describe, expect, it, vi } from "vitest";

type EventHandler = (event: { payload: unknown }) => void;

const { invokeMock, listenMock, handlers } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listenMock: vi.fn(),
  handlers: new Map<string, EventHandler>(),
}));
const { captureStops, startAgentSessionCaptureMock } = vi.hoisted(() => {
  const captureStops: ReturnType<typeof vi.fn>[] = [];
  return {
    captureStops,
    startAgentSessionCaptureMock: vi.fn(() => {
      const stop = vi.fn();
      captureStops.push(stop);
      return { stop };
    }),
  };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));
vi.mock("./terminal/agent-session-capture-tauri", () => ({
  startAgentSessionCapture: startAgentSessionCaptureMock,
}));

import { listenForTabLifecycleClose } from "./App";
import { InMemoryPersistence } from "./core/persistence/in-memory";
import { useTabStore } from "./core/store/tabStore";
import { useTmuxStatus } from "./core/store/tmuxStatusStore";

function flushPromises() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

beforeEach(() => {
  handlers.clear();
  captureStops.length = 0;
  startAgentSessionCaptureMock.mockClear();
  listenMock.mockReset();
  listenMock.mockImplementation((eventName: string, handler: EventHandler) => {
    handlers.set(eventName, handler);
    return Promise.resolve(() => {});
  });
  invokeMock.mockReset();
  invokeMock.mockImplementation((cmd, args) => {
    if (
      cmd === "create_tab" &&
      ((args as { req?: { kind?: string } }).req?.kind === "terminal" ||
        (args as { req?: { kind?: string } }).req?.kind === "chat")
    ) {
      return Promise.resolve({ windowName: "term-1" });
    }
    return Promise.resolve(undefined);
  });
  useTabStore.setState({
    profiles: [{ id: "profile-default", name: "Default", isDefault: true }],
    spaces: [
      {
        id: "space-default",
        name: "Default",
        color: "#6366f1",
        profileId: "profile-default",
        activeTabId: null,
      },
    ],
    tabs: [],
    activeSpaceId: "space-default",
  });
  useTmuxStatus.setState({
    status: { backend: "tmux", available: true, version: "tmux 3.4", error: null },
    loaded: true,
  });
});

describe("App tab lifecycle events", () => {
  it("routes tab-exited through Core closeTab and ignores stale ids", async () => {
    const persistence = new InMemoryPersistence();
    await useTabStore.getState().hydrate(persistence);
    const active = await useTabStore
      .getState()
      .newTab("browser", "https://active.example");
    const exited = await useTabStore
      .getState()
      .newTab("browser", "https://exited.example");
    await useTabStore.getState().activateTab(active.id);

    invokeMock.mockClear();
    const stopListening = listenForTabLifecycleClose("sanctel://tab-exited");

    const handler = handlers.get("sanctel://tab-exited");
    expect(handler).toBeDefined();

    handler?.({ payload: { id: "already-removed" } });
    await flushPromises();
    expect(invokeMock.mock.calls).toEqual([]);
    expect(useTabStore.getState().activeTab()?.id).toBe(active.id);

    handler?.({ payload: { id: exited.id } });
    await flushPromises();

    expect(useTabStore.getState().tabs.map((t) => t.id)).toEqual([active.id]);
    expect(useTabStore.getState().activeTab()?.id).toBe(active.id);
    expect((await persistence.loadAll()).tabs.map((t) => t.id)).toEqual([
      active.id,
    ]);
    expect(invokeMock.mock.calls).toContainEqual([
      "close_tab",
      { id: exited.id },
    ]);
    expect(invokeMock.mock.calls).toContainEqual([
      "show_tab",
      { id: active.id },
    ]);

    stopListening();
  });

  it("keeps active-tab selection behavior when an active tab exits", async () => {
    const persistence = new InMemoryPersistence();
    await useTabStore.getState().hydrate(persistence);
    const first = await useTabStore
      .getState()
      .newTab("browser", "https://first.example");
    const second = await useTabStore
      .getState()
      .newTab("browser", "https://second.example");

    invokeMock.mockClear();
    const stopListening = listenForTabLifecycleClose("sanctel://tab-exited");
    const handler = handlers.get("sanctel://tab-exited");

    handler?.({ payload: { id: second.id } });
    await flushPromises();

    expect(useTabStore.getState().tabs.map((t) => t.id)).toEqual([first.id]);
    expect(useTabStore.getState().activeTab()?.id).toBe(first.id);
    expect(invokeMock.mock.calls).toContainEqual([
      "show_tab",
      { id: first.id },
    ]);

    invokeMock.mockClear();
    handler?.({ payload: { id: first.id } });
    await flushPromises();

    expect(useTabStore.getState().tabs).toEqual([]);
    expect(useTabStore.getState().activeTab()).toBeUndefined();
    expect(invokeMock.mock.calls).toContainEqual(["hide_all"]);

    stopListening();
  });

  it("stops chat AgentSession capture when a chat tab exits", async () => {
    const persistence = new InMemoryPersistence();
    await useTabStore.getState().hydrate(persistence);
    const chat = await useTabStore.getState().newChatTab("sanctel-main");
    expect(captureStops).toHaveLength(1);

    const stopListening = listenForTabLifecycleClose("sanctel://tab-exited");
    handlers.get("sanctel://tab-exited")?.({ payload: { id: chat.id } });
    await flushPromises();

    expect(captureStops[0]).toHaveBeenCalledTimes(1);
    expect(useTabStore.getState().tabs).toEqual([]);
    expect((await persistence.loadAll()).tabs).toEqual([]);

    stopListening();
  });
});
