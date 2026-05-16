import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import type { Tab, Space, Profile, TabKind, ContentRect, Worktree } from "../types";
import { allocateWindowName } from "../../terminal/window-name-allocator";
import { findWorktree } from "../worktrees";
import { useTmuxStatus } from "./tmuxStatusStore";

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
  closeTab: (id: string) => Promise<void>;
  activateTab: (id: string) => Promise<void>;
  setContentRect: (rect: ContentRect) => Promise<void>;
  patchTab: (id: string, patch: Partial<Tab>) => void;
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

export const useTabStore = create<TabState>((set, get) => ({
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

    // For Worktree-keyed tabs we need both worktreeId (session key) and
    // worktreePath (`-c` cwd). For detached tabs both are null/undefined and
    // Rust falls back to $HOME on `sanctel-detached:<profileId>`.
    const worktree = worktreeId ? findWorktree(worktreeId) : undefined;
    if (worktreeId && !worktree) {
      throw new Error(`unknown worktreeId: ${worktreeId}`);
    }

    // Ask Rust for the existing window names in this Worktree's session, then
    // allocate the next term-N locally. Empty list when the session doesn't
    // exist yet — first tab into a Worktree.
    const existing = await invoke<string[]>("terminal_list_window_names", {
      req: { worktreeId: worktreeId ?? null, profileId },
    });
    const windowName = allocateWindowName(existing);

    const id = crypto.randomUUID();
    const title = worktree ? `${worktree.branch} · ${windowName}` : windowName;
    const tab: Tab = {
      id,
      kind: "terminal",
      title,
      url: "local://terminal",
      spaceId: space.id,
      worktreeId: worktree?.id,
      sessionId: windowName,
      loading: false,
    };

    await invoke("create_tab", {
      req: {
        id: tab.id,
        kind: tab.kind,
        url: tab.url,
        profileId,
        worktreeId: worktree?.id ?? null,
        worktreePath: worktree?.path ?? null,
        windowName,
      },
    });

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
}));
