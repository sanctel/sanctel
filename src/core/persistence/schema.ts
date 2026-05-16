// SQLite schema for the sanctel state store. One file, no migrations —
// Slice 5 only ships v1.
//
// Every nullable column is a sticky-note pointer to state owned outside
// sanctel (worktree dir, tmux window, agent transcript). See ADR-0004.

export const SCHEMA_V1 = `
CREATE TABLE IF NOT EXISTS profiles (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  color       TEXT,
  is_default  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS spaces (
  id          TEXT PRIMARY KEY,
  profile_id  TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
  name        TEXT NOT NULL,
  color       TEXT NOT NULL,
  sort_order  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS tabs (
  id                TEXT PRIMARY KEY,
  space_id          TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
  kind              TEXT NOT NULL,
  title             TEXT NOT NULL,
  sort_order        INTEGER NOT NULL,
  url               TEXT,
  worktree_id       TEXT,
  window_name       TEXT,
  initial_command   TEXT,
  agent_session_id  TEXT
);

CREATE INDEX IF NOT EXISTS idx_spaces_profile_id ON spaces(profile_id);
CREATE INDEX IF NOT EXISTS idx_tabs_space_id     ON tabs(space_id);
`;
