// Status of the one-time backend startup probe Rust runs at app launch.
// Mirrors src-tauri/src/lib.rs `TmuxStatus`. React consumes this to gate
// terminal/chat tab creation behind a setup screen when the active
// backend isn't usable.
//
// `backend` names which one was probed ("tmux" or "zellij") so the setup
// screen can render backend-appropriate copy and install instructions
// (issue #27). The field name is historical (the struct was added when
// only tmux existed); both tmux probe and zellij probe write to it now.
//
// Lifecycle:
//   - On mount, App reads the current status synchronously via the
//     `tmux_status` command (so first paint is correct).
//   - The same App also subscribes to the `tmux-status` event, which fires
//     exactly once during Rust setup(). Late subscriptions land via the
//     command read; live updates land via the event.

import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type BackendName = "tmux" | "zellij";

export interface TmuxStatus {
  backend: BackendName;
  available: boolean;
  version: string | null;
  error: string | null;
}

interface TmuxStatusState {
  status: TmuxStatus;
  loaded: boolean;
  hydrate: () => Promise<void>;
}

const INITIAL: TmuxStatus = {
  backend: "tmux",
  available: false,
  version: null,
  error: null,
};

export const useTmuxStatus = create<TmuxStatusState>((set, get) => ({
  status: INITIAL,
  loaded: false,
  hydrate: async () => {
    if (get().loaded) return;
    // Read the synchronous snapshot so the very first paint already knows.
    try {
      const snapshot = await invoke<TmuxStatus>("tmux_status");
      set({ status: normalize(snapshot), loaded: true });
    } catch (e) {
      // If even the command call fails, treat as unavailable so the UI
      // doesn't pretend the backend works. The backend is unknown at this
      // point — fall back to "tmux" so the setup screen renders the
      // existing copy rather than blank.
      set({
        status: {
          backend: "tmux",
          available: false,
          version: null,
          error: `tmux_status command failed: ${String(e)}`,
        },
        loaded: true,
      });
    }
    // Subscribe for future emissions. Rust emits once during setup(); this
    // mostly catches the case where React mounts before setup() finishes.
    listen<TmuxStatus>("tmux-status", (event) => {
      set({ status: normalize(event.payload), loaded: true });
    }).catch((e) => console.error("listen('tmux-status') failed", e));
  },
}));

function normalize(s: TmuxStatus | undefined | null): TmuxStatus {
  if (!s) return INITIAL;
  return {
    // Defensive fallback per issue #27 acceptance: an unrecognised or
    // missing backend value renders the existing tmux copy rather than
    // a blank setup screen.
    backend: s.backend === "zellij" ? "zellij" : "tmux",
    available: !!s.available,
    version: s.version ?? null,
    error: s.error ?? null,
  };
}
