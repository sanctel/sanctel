// Status of the one-time `tmux -V` probe Rust runs at app startup.
// Mirrors src-tauri/src/lib.rs `TmuxStatus`. React consumes this to gate
// terminal/chat tab creation behind a setup screen when tmux is missing.
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

export interface TmuxStatus {
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
      // doesn't pretend tmux works.
      set({
        status: {
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
    available: !!s.available,
    version: s.version ?? null,
    error: s.error ?? null,
  };
}
