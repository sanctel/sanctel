import { create, type StoreApi, type UseBoundStore } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  Tab,
  Space,
  Profile,
  TabKind,
  ContentRect,
  Worktree,
} from "../types";
import { findWorktree } from "../worktrees";
import { useTmuxStatus } from "./tmuxStatusStore";
import type {
  PersistedProfile,
  PersistedSpace,
  PersistedTab,
  Persistence,
} from "../persistence/persistence";

// Response shape from Rust's `create_tab`. For terminal/chat tabs created
// with `windowName: "auto"`, Rust returns the resolved name here so the
// frontend can persist it; for reattach (explicit name) and non-terminal
// kinds, `windowName` is `null`. See docs/design/terminal-runtime.md
// §"windowName assignment".
interface CreateTabResp {
  windowName: string | null;
}

interface AgentSessionCaptureEvent {
  sessionName: string;
  agent: "claude" | "codex" | "gemini";
  sessionId: string;
  ts: number;
}

const AGENT_SESSION_CAPTURED_EVENT = "agent-session-captured";
const DRAIN_PENDING_AGENT_CAPTURES_COMMAND = "drain_pending_agent_captures";

// Sentinel passed to `create_tab` in place of an explicit windowName. Rust
// interprets it as "allocate the next term-N under the per-session mutex
// and return the resolved name." Pinned as a const so React never types
// the literal twice.
const AUTO_WINDOW_NAME = "auto";

interface TabState {
  profiles: Profile[];
  spaces: Space[];
  tabs: Tab[];
  activeSpaceId: string;

  // selectors
  activeSpace: () => Space | undefined;
  activeProfile: () => Profile | undefined;
  visibleTabs: () => Tab[];
  activeTab: () => Tab | undefined;
  spacesForProfile: (profileId: string) => Space[];

  // mutations
  addProfile: (name: string, color?: string) => Promise<Profile>;
  addSpace: (name: string, profileId?: string, color?: string) => Promise<Space>;
  switchSpace: (id: string) => void;
  newTab: (kind: TabKind, url: string) => Promise<Tab>;
  newTerminalTab: (worktreeId: Worktree["id"] | null) => Promise<Tab>;
  newChatTab: (worktreeId: Worktree["id"]) => Promise<Tab>;
  closeTab: (id: string) => Promise<void>;
  renameTab: (id: string, title: string) => Promise<void>;
  activateTab: (id: string) => Promise<void>;
  setContentRect: (rect: ContentRect) => Promise<void>;
  patchTab: (id: string, patch: Partial<Tab>) => void;

  // persistence
  hydrate: (persistence: Persistence) => Promise<void>;
}

// Default content for new tabs by kind.
const defaultUrl = (kind: TabKind): string => {
  switch (kind) {
    case "browser":  return "https://duckduckgo.com";
    case "terminal": return "local://terminal";
    case "chat":     return "local://chat";
    case "file":     return "local://file";
    case "diff":     return "local://diff";
  }
};

// Bootstrap: one hidden Default profile + one Default space.
// The UI shouldn't surface "profiles" until the user creates a second one —
// 90% of users will have exactly one profile forever.
const DEFAULT_PROFILE: Profile = {
  id: "profile-default",
  name: "Default",
  isDefault: true,
};

const DEFAULT_SPACE: Space = {
  id: "space-default",
  name: "Default",
  color: "#6366f1",
  profileId: DEFAULT_PROFILE.id,
  activeTabId: null,
};

// ─── Persisted ↔ in-memory translation ────────────────────────────────────
//
// Persistence rows are the wire shape (snake_case → camelCase already at
// the SqlPersistence boundary). In-memory `Tab` / `Space` / `Profile` carry
// runtime-only fields (Space.activeTabId, Tab.loading) that don't belong in
// the DB; everything else round-trips.

function profileFromRow(p: PersistedProfile): Profile {
  return {
    id: p.id,
    name: p.name,
    color: p.color ?? undefined,
    isDefault: p.isDefault,
  };
}

function profileToRow(p: Profile): PersistedProfile {
  return {
    id: p.id,
    name: p.name,
    color: p.color ?? null,
    isDefault: p.isDefault,
  };
}

function spaceFromRow(s: PersistedSpace): Space {
  return {
    id: s.id,
    profileId: s.profileId,
    name: s.name,
    color: s.color,
    activeTabId: null,
  };
}

function spaceToRow(s: Space, sortOrder: number): PersistedSpace {
  return {
    id: s.id,
    profileId: s.profileId,
    name: s.name,
    color: s.color,
    sortOrder,
  };
}

function tabFromRow(t: PersistedTab): Tab {
  return {
    id: t.id,
    spaceId: t.spaceId,
    kind: t.kind,
    title: t.title,
    url: t.url ?? defaultUrl(t.kind),
    worktreeId: t.worktreeId ?? undefined,
    windowName: t.windowName ?? undefined,
    initialCommand: t.initialCommand ?? undefined,
    agentSessionId: t.agentSessionId ?? undefined,
    loading: t.kind === "browser",
  };
}

function tabToRow(t: Tab, sortOrder: number): PersistedTab {
  return {
    id: t.id,
    spaceId: t.spaceId,
    kind: t.kind,
    title: t.title,
    sortOrder,
    url: t.url,
    worktreeId: t.worktreeId ?? null,
    windowName: t.windowName ?? null,
    initialCommand: t.initialCommand ?? null,
    agentSessionId: t.agentSessionId ?? null,
  };
}

// ─── Store factory ────────────────────────────────────────────────────────
//
// `createTabStore` returns a fresh store hook. Tests create their own per
// case so state never leaks between them; the production module exports a
// singleton built at the bottom of this file.

export type TabStoreHook = UseBoundStore<StoreApi<TabState>>;

export function createTabStore(): TabStoreHook {
  // Persistence ref held outside the Zustand state so it doesn't trigger
  // re-renders and so mutations can dispatch through it synchronously.
  let persistence: Persistence | null = null;

  return create<TabState>((set, get) => ({
    profiles: [DEFAULT_PROFILE],
    spaces: [DEFAULT_SPACE],
    tabs: [],
    activeSpaceId: DEFAULT_SPACE.id,

    activeSpace: () =>
      get().spaces.find((s) => s.id === get().activeSpaceId),

    activeProfile: () => {
      const sp = get().activeSpace();
      if (!sp) return undefined;
      return get().profiles.find((p) => p.id === sp.profileId);
    },

    visibleTabs: () =>
      get().tabs.filter((t) => t.spaceId === get().activeSpaceId),

    activeTab: () => {
      const sp = get().activeSpace();
      if (!sp?.activeTabId) return undefined;
      return get().tabs.find((t) => t.id === sp.activeTabId);
    },

    spacesForProfile: (profileId) =>
      get().spaces.filter((s) => s.profileId === profileId),

    addProfile: async (name, color) => {
      const p: Profile = {
        id: crypto.randomUUID(),
        name,
        color,
        isDefault: false,
      };
      set((s) => ({ profiles: [...s.profiles, p] }));
      if (persistence) await persistence.saveProfile(profileToRow(p));
      return p;
    },

    addSpace: async (name, profileId, color = "#6366f1") => {
      // Default to the active space's profile, so new spaces inherit identity.
      const activeProfile = get().activeProfile();
      const sp: Space = {
        id: crypto.randomUUID(),
        name,
        color,
        profileId: profileId ?? activeProfile?.id ?? DEFAULT_PROFILE.id,
        activeTabId: null,
      };
      set((s) => ({ spaces: [...s.spaces, sp] }));
      if (persistence) {
        const sortOrder = get().spaces.findIndex((x) => x.id === sp.id);
        await persistence.saveSpace(spaceToRow(sp, sortOrder));
      }
      return sp;
    },

    switchSpace: (id) => {
      set({ activeSpaceId: id });
      // Tell Rust to show the new space's active tab (or hide everything).
      const sp = get().spaces.find((s) => s.id === id);
      if (sp?.activeTabId) {
        invoke("show_tab", { id: sp.activeTabId }).catch(console.error);
      } else {
        invoke("hide_all").catch(console.error);
      }
    },

    newTab: async (kind, url) => {
      const space = get().activeSpace();
      if (!space) throw new Error("no active space");
      const profileId = space.profileId;

      // Issue #8 / Slice 7: gate terminal/chat tab creation on the tmux
      // startup probe. If tmux is missing, the sidebar buttons are already
      // hidden by App.tsx — this is the defensive second check so a stale
      // SQLite restore or a scripted call cannot bypass the setup screen.
      if (kind === "terminal" || kind === "chat") {
        const status = useTmuxStatus.getState().status;
        if (!status.available) {
          throw new Error(
            "tmux-missing: cannot create terminal or chat tab without tmux",
          );
        }
      }

      const id = crypto.randomUUID();
      const tab: Tab = {
        id,
        kind,
        title: kind === "browser" ? "New tab" : kind === "terminal" ? "Terminal" : "Chat",
        url: url || defaultUrl(kind),
        spaceId: space.id,
        loading: kind === "browser",
      };

      // Persist BEFORE invoking create_tab. The tab row is the source of
      // truth across restarts; if create_tab fails (e.g. a transient tmux
      // hiccup), the next launch will replay create_tab and either succeed
      // or surface the same error in the same way.
      if (persistence) {
        const sortOrder = get().tabs.filter(
          (t) => t.spaceId === space.id,
        ).length;
        await persistence.saveTab(tabToRow(tab, sortOrder));
      }

      // Tell Rust to spawn the actual webview, passing profile_id directly —
      // Rust doesn't need to know about Spaces.
      await invoke("create_tab", {
        req: {
          id: tab.id,
          kind: tab.kind,
          url: tab.url,
          profileId,
        },
      });

      set((s) => ({
        tabs: [...s.tabs, tab],
        spaces: s.spaces.map((sp) =>
          sp.id === space.id ? { ...sp, activeTabId: id } : sp
        ),
      }));

      return tab;
    },

    newTerminalTab: async (worktreeId) => {
      const space = get().activeSpace();
      if (!space) throw new Error("no active space");
      const profileId = space.profileId;

      // Issue #8 / Slice 7: belt-and-braces tmux gate, matching `newTab` above.
      // Sidebar already hides terminal buttons when tmux is missing, but a
      // scripted call mustn't be allowed to spawn a doomed PTY either.
      const status = useTmuxStatus.getState().status;
      if (!status.available) {
        throw new Error(
          "tmux-missing: cannot create terminal tab without tmux",
        );
      }

      // For Worktree-keyed tabs we need both worktreeId (session-name
      // prefix) and worktreePath (`-c` cwd). For detached tabs both are
      // null/undefined and Rust falls back to $HOME on
      // `sanctel_detached_<profileId>__<windowName>` per ADR-0012.
      const worktree = worktreeId ? findWorktree(worktreeId) : undefined;
      if (worktreeId && !worktree) {
        throw new Error(`unknown worktreeId: ${worktreeId}`);
      }

      // Issue #10: windowName allocation moved server-side under a per-session
      // mutex. React passes the "auto" sentinel; Rust returns the resolved
      // name in CreateTabResp.windowName, which we then persist.
      const id = crypto.randomUUID();
      const resp = await invoke<CreateTabResp>("create_tab", {
        req: {
          id,
          kind: "terminal",
          url: "local://terminal",
          profileId,
          worktreeId: worktree?.id ?? null,
          worktreePath: worktree?.path ?? null,
          windowName: AUTO_WINDOW_NAME,
        },
      });
      const windowName = resp.windowName;
      if (!windowName) {
        throw new Error(
          "create_tab returned no windowName for an auto-allocated terminal tab",
        );
      }

      const title = worktree ? `${worktree.branch} · ${windowName}` : windowName;
      const tab: Tab = {
        id,
        kind: "terminal",
        title,
        url: "local://terminal",
        spaceId: space.id,
        worktreeId: worktree?.id,
        windowName,
        loading: false,
      };

      // Persist *after* invoke now that the resolved name comes from Rust.
      // If invoke succeeds but persistence fails, we'll have an orphan tmux
      // window — acceptable; the user can `tmux kill-server` or close-and-
      // recreate. The alternative (write a row with null windowName, update
      // after invoke) buys nothing the user actually sees.
      if (persistence) {
        const sortOrder = get().tabs.filter(
          (t) => t.spaceId === space.id,
        ).length;
        await persistence.saveTab(tabToRow(tab, sortOrder));
      }

      set((s) => ({
        tabs: [...s.tabs, tab],
        spaces: s.spaces.map((sp) =>
          sp.id === space.id ? { ...sp, activeTabId: id } : sp,
        ),
      }));

      return tab;
    },

    newChatTab: async (worktreeId) => {
      const space = get().activeSpace();
      if (!space) throw new Error("no active space");
      const profileId = space.profileId;

      // Same tmux gate as newTerminalTab / newTab — the sidebar already hides
      // the buttons when tmux is missing; this is the second line of defence.
      const status = useTmuxStatus.getState().status;
      if (!status.available) {
        throw new Error(
          "tmux-missing: cannot create chat tab without tmux",
        );
      }

      const worktree = findWorktree(worktreeId);
      if (!worktree) throw new Error(`unknown worktreeId: ${worktreeId}`);

      const initialCommand = "claude";

      const id = crypto.randomUUID();
      const resp = await invoke<CreateTabResp>("create_tab", {
        req: {
          id,
          kind: "chat",
          url: "local://chat",
          profileId,
          worktreeId: worktree.id,
          worktreePath: worktree.path,
          windowName: AUTO_WINDOW_NAME,
          initialCommand,
          agentSessionId: null,
        },
      });
      const windowName = resp.windowName;
      if (!windowName) {
        throw new Error(
          "create_tab returned no windowName for an auto-allocated chat tab",
        );
      }

      const title = `${worktree.branch} · chat`;
      const tab: Tab = {
        id,
        kind: "chat",
        title,
        url: "local://chat",
        spaceId: space.id,
        worktreeId: worktree.id,
        windowName,
        initialCommand,
        agentSessionId: undefined,
        loading: false,
      };

      if (persistence) {
        const sortOrder = get().tabs.filter(
          (t) => t.spaceId === space.id,
        ).length;
        await persistence.saveTab(tabToRow(tab, sortOrder));
      }

      set((s) => ({
        tabs: [...s.tabs, tab],
        spaces: s.spaces.map((sp) =>
          sp.id === space.id ? { ...sp, activeTabId: id } : sp,
        ),
      }));

      return tab;
    },

    closeTab: async (id) => {
      await invoke("close_tab", { id }).catch(console.error);
      // Backend cleanup is best-effort: Core still removes the Tab pointer
      // from persistence and memory if Rust cleanup fails.
      if (persistence) await persistence.removeTab(id);
      set((s) => {
        const tabs = s.tabs.filter((t) => t.id !== id);
        const spaces = s.spaces.map((sp) => {
          if (sp.activeTabId !== id) return sp;
          const sibling = tabs.find((t) => t.spaceId === sp.id);
          return { ...sp, activeTabId: sibling?.id ?? null };
        });
        return { tabs, spaces };
      });
      const sp = get().activeSpace();
      if (sp?.activeTabId) await invoke("show_tab", { id: sp.activeTabId });
      else await invoke("hide_all");
    },

    renameTab: async (id, title) => {
      set((s) => ({
        tabs: s.tabs.map((t) => (t.id === id ? { ...t, title } : t)),
      }));
      // Per acceptance criterion #2: rename touches tabs.title only — the
      // tmux window name (and therefore the shell) is unaffected.
      if (persistence) await persistence.renameTab(id, title);
    },

    activateTab: async (id) => {
      const spaceId = get().activeSpaceId;
      set((s) => ({
        spaces: s.spaces.map((sp) =>
          sp.id === spaceId ? { ...sp, activeTabId: id } : sp
        ),
      }));
      await invoke("show_tab", { id }).catch(console.error);
    },

    setContentRect: async (rect) => {
      await invoke("set_content_rect", { rect }).catch(console.error);
    },

    patchTab: (id, patch) =>
      set((s) => ({
        tabs: s.tabs.map((t) => (t.id === id ? { ...t, ...patch } : t)),
      })),

    // ─── hydrate ───────────────────────────────────────────────────────────
    //
    // Two-phase launch (per the issue):
    //   1. Read profiles / spaces / tabs from SQLite → paint the sidebar.
    //   2. Replay `create_tab` per tab row so Rust spawns each webview;
    //      terminal/chat webviews then call `terminal_attach` from mount.
    //
    // If the DB is empty (first launch) we seed it with the default
    // profile + space so subsequent launches always have a row to read.
    hydrate: async (p: Persistence) => {
      persistence = p;
      await p.init();
      const snap = await p.loadAll();

      if (snap.profiles.length === 0) {
        // First launch — persist defaults so a clean re-read returns them.
        await p.saveProfile(profileToRow(DEFAULT_PROFILE));
        await p.saveSpace(spaceToRow(DEFAULT_SPACE, 0));
        listenForAgentSessionCaptures(get, set, () => persistence);
        await drainPendingAgentCaptures(get, set, () => persistence);
        await reapOrphanTmuxSessions([], []);
        return;
      }

      const profiles = snap.profiles.map(profileFromRow);
      const spaces = snap.spaces.map(spaceFromRow);
      const tabs = snap.tabs.map(tabFromRow);

      // Make the first tab in each space its activeTabId (best-effort —
      // the user's last-active selection isn't persisted in v1).
      const spacesWithActive = spaces.map((sp) => {
        const first = tabs.find((t) => t.spaceId === sp.id);
        return { ...sp, activeTabId: first?.id ?? null };
      });

      set({
        profiles,
        spaces: spacesWithActive,
        tabs,
        activeSpaceId: spaces[0]?.id ?? DEFAULT_SPACE.id,
      });

      // Replay create_tab in sort order. We do this AFTER the React state
      // is set so the sidebar paints first; create_tab is fire-and-await
      // per row so failures (e.g. tmux-missing for a terminal tab) surface
      // individually without blocking the rest.
      for (const t of tabs) {
        const space = spacesWithActive.find((sp) => sp.id === t.spaceId);
        if (!space) continue;
        try {
          await invoke("create_tab", { req: buildCreateTabReq(t, space) });
        } catch (e) {
          console.error("hydrate: create_tab failed for", t.id, e);
        }
      }

      // After the create_tab loop, Rust's active_tab is the LAST tab
      // we replayed (each create_tab calls show_webview which updates
      // active_tab). The frontend's activeTabId is the FIRST tab in
      // the active space. Realign by explicitly showing the frontend's
      // chosen active tab so the correct webview is on top from first
      // paint — otherwise the user has to click another tab and back
      // to see the right contents.
      const activeSpace = spacesWithActive.find(
        (sp) => sp.id === (spaces[0]?.id ?? DEFAULT_SPACE.id),
      );
      if (activeSpace?.activeTabId) {
        try {
          await invoke("show_tab", { id: activeSpace.activeTabId });
        } catch (e) {
          console.error("hydrate: show_tab for initial active failed", e);
        }
      }

      listenForAgentSessionCaptures(get, set, () => persistence);
      await drainPendingAgentCaptures(get, set, () => persistence);
      await reapOrphanTmuxSessions(tabs, spacesWithActive);
    },
  }));
}

function buildCreateTabReq(t: Tab, space: Space) {
  const base = {
    id: t.id,
    kind: t.kind,
    url: t.url,
    profileId: space.profileId,
  };
  if (t.kind !== "terminal" && t.kind !== "chat") return base;

  // Terminal / chat: resolve the worktree path from the worktreeId so
  // Rust can `tmux -c <path>` on session creation. The worktree manager
  // is hardcoded in v0.3 (see worktrees.ts); a real lookup will land
  // alongside that subsystem.
  const wt = t.worktreeId ? findWorktree(t.worktreeId) : undefined;
  return {
    ...base,
    worktreeId: t.worktreeId ?? null,
    worktreePath: wt?.path ?? null,
    windowName: t.windowName ?? null,
    initialCommand: t.initialCommand ?? null,
    agentSessionId: t.agentSessionId ?? null,
  };
}

async function drainPendingAgentCaptures(
  get: () => TabState,
  set: StoreApi<TabState>["setState"],
  persistence: () => Persistence | null,
): Promise<void> {
  try {
    const captures = await invoke<AgentSessionCaptureEvent[]>(
      DRAIN_PENDING_AGENT_CAPTURES_COMMAND,
    );
    await applyAgentSessionCaptures(captures, get, set, persistence);
  } catch (e) {
    console.error(`${DRAIN_PENDING_AGENT_CAPTURES_COMMAND} failed`, e);
  }
}

function listenForAgentSessionCaptures(
  get: () => TabState,
  set: StoreApi<TabState>["setState"],
  persistence: () => Persistence | null,
): void {
  listen<AgentSessionCaptureEvent>(AGENT_SESSION_CAPTURED_EVENT, (event) => {
    applyAgentSessionCapture(event.payload, get, set, persistence).catch((e) =>
      console.error(`${AGENT_SESSION_CAPTURED_EVENT} handler failed`, e),
    );
  }).catch((e) =>
    console.error(`listen('${AGENT_SESSION_CAPTURED_EVENT}') failed`, e),
  );
}

async function applyAgentSessionCapture(
  capture: AgentSessionCaptureEvent,
  get: () => TabState,
  set: StoreApi<TabState>["setState"],
  persistence: () => Persistence | null,
): Promise<void> {
  await applyAgentSessionCaptures([capture], get, set, persistence);
}

async function applyAgentSessionCaptures(
  captures: AgentSessionCaptureEvent[],
  get: () => TabState,
  set: StoreApi<TabState>["setState"],
  persistence: () => Persistence | null,
): Promise<void> {
  if (captures.length === 0) return;

  const tabBySessionName = await tabByTmuxSessionName(get().tabs, get().spaces);
  for (const capture of captures) {
    const match = tabBySessionName.get(capture.sessionName);
    if (!match || match.agentSessionId === capture.sessionId) continue;

    await persistence()?.updateTabAgentSession(match.id, capture.sessionId);
    set((s) => ({
      tabs: s.tabs.map((t) =>
        t.id === match.id ? { ...t, agentSessionId: capture.sessionId } : t,
      ),
    }));
  }
}

async function reapOrphanTmuxSessions(
  tabs: Tab[],
  spaces: Space[],
): Promise<void> {
  try {
    const knownSessionNames = await knownTmuxSessionNames(tabs, spaces);
    await invoke("reap_orphan_tmux_sessions", { knownSessionNames });
  } catch (e) {
    console.error("reap_orphan_tmux_sessions failed", e);
  }
}

interface TmuxSessionNameSpec {
  tab: Tab;
  prefix: "sanctel_wt" | "sanctel_detached";
  unsafeId: string;
  windowName: string;
}

async function knownTmuxSessionNames(
  tabs: Tab[],
  spaces: Space[],
): Promise<string[]> {
  return (await tmuxSessionNamePairs(tabs, spaces)).map(
    (pair) => pair.sessionName,
  );
}

async function tmuxSessionNamePairs(
  tabs: Tab[],
  spaces: Space[],
): Promise<{ tab: Tab; sessionName: string }[]> {
  const specs: TmuxSessionNameSpec[] = tabs.flatMap((t) => {
    if (t.kind !== "terminal" && t.kind !== "chat") return [];
    const space = spaces.find((sp) => sp.id === t.spaceId);
    if (!space) return [];
    return [
      {
        tab: t,
        prefix: t.worktreeId ? "sanctel_wt" : "sanctel_detached",
        unsafeId: t.worktreeId ?? space.profileId,
        windowName: t.windowName ?? "term-1",
      },
    ];
  });

  if (specs.length === 0) return [];

  const safeIds = await invoke<string[]>("tmux_safe_many", {
    inputs: specs.map((s) => s.unsafeId),
  });
  if (safeIds.length !== specs.length) {
    throw new Error(
      `tmux_safe_many returned ${safeIds.length} ids for ${specs.length} inputs`,
    );
  }

  return specs.map((spec, i) => ({
    tab: spec.tab,
    sessionName: `${spec.prefix}_${safeIds[i]}__${spec.windowName}`,
  }));
}

async function tabByTmuxSessionName(
  tabs: Tab[],
  spaces: Space[],
): Promise<Map<string, Tab>> {
  return new Map(
    (await tmuxSessionNamePairs(tabs, spaces)).map((pair) => [
      pair.sessionName,
      pair.tab,
    ]),
  );
}

// Production singleton. App.tsx calls `hydrate(new SqlPersistence())` on
// mount; until then the store holds the default profile/space and no tabs.
export const useTabStore = createTabStore();
