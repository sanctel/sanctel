// Persistence layer for sanctel state — Slice 5 / issue #6.
//
// Persistence-anchor pattern (ADR-0004): durable state lives in the
// filesystem and in tmux. The SQLite store here holds *pointers* —
// `tabs.window_name`, `tabs.worktree_id`, `tabs.agent_session_id` — to
// the durable entities. On launch, React reads these rows, paints the
// sidebar, then replays `create_tab` per row so each Tab's webview
// reattaches to (or recreates) its server-held identity.
//
// Rust never reads SQLite directly. The frontend owns the wire and the
// schema; Rust only ever sees `create_tab` arguments. See the matching
// acceptance criterion in issue #6.

import type { TabKind } from "../types";

export interface PersistedProfile {
  id: string;
  name: string;
  color: string | null;
  isDefault: boolean;
}

export interface PersistedSpace {
  id: string;
  profileId: string;
  name: string;
  color: string;
  sortOrder: number;
}

export interface PersistedTab {
  id: string;
  spaceId: string;
  kind: TabKind;
  title: string;
  sortOrder: number;
  url: string | null;
  worktreeId: string | null;
  windowName: string | null;
  initialCommand: string | null;
  agentSessionId: string | null;
}

export interface Snapshot {
  profiles: PersistedProfile[];
  spaces: PersistedSpace[];
  tabs: PersistedTab[];
}

// The contract. Implementations:
//   - InMemoryPersistence  (tests; the test double of record)
//   - SqlPersistence       (production; sql.js + Tauri fs plugin)
//
// Methods that mutate must be persisted before the IPC call that creates
// or kills the matching webview/tmux window — that ordering is what makes
// the persistence durable across crashes.
export interface Persistence {
  // Apply schema if missing; load the DB into memory if it exists.
  init(): Promise<void>;

  loadAll(): Promise<Snapshot>;

  saveProfile(profile: PersistedProfile): Promise<void>;
  saveSpace(space: PersistedSpace): Promise<void>;
  saveTab(tab: PersistedTab): Promise<void>;

  renameTab(id: string, title: string): Promise<void>;
  removeTab(id: string): Promise<void>;

  // Persist a new ordering for the tabs in `spaceId`. Tabs not in
  // `orderedIds` are left alone (e.g. tabs in other spaces).
  reorderTabs(spaceId: string, orderedIds: string[]): Promise<void>;
}
