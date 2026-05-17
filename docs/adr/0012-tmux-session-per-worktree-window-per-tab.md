# 0012 — tmux session per Worktree, window per Tab

**Status:** Accepted

**Decision:** Sanctel maps its persistence model onto tmux's native
hierarchy as **one tmux session per Worktree, one tmux window per
terminal Tab**. Sessions are named `sanctel_wt_<worktreeId>` and live on
a dedicated tmux server (`tmux -L sanctel -f /dev/null`). Window names
(`term-1`, `term-2`, …) are allocated monotonically per Worktree and
stored as the Tab's only durable backend handle beyond `worktreeId`.

Worktree-less terminal tabs attach to a fallback session named
`sanctel_detached_<profileId>`.

The separator is `_`, not `:`, because tmux interprets `:` and `.` as
session/window/pane delimiters in target specs (`tmux list-windows -t
foo:bar` parses as `session=foo, window=bar`). Sanctel-built names
contain only characters in `[A-Za-z0-9_-]`; any `worktreeId` or
`profileId` is passed through `tmux_safe` before concatenation, which
collapses any other character to `_`. (Issue #13 documents the original
`sanctel-wt:<id>` format and the silent breakage it caused.)

## Why this matters

[ADR-0002](./0002-terminal-architecture.md) picked tmux as the
persistence anchor and [ADR-0004](./0004-persistence-anchor-pattern.md)
made "filesystem and tmux are durable; app state is recomputable" an
invariant. Neither said *how* to name tmux sessions, leaving the open
question of whether session identity is keyed by Tab, by Worktree, by
an opaque id, or by something else. That ambiguity is load-bearing —
the choice determines the reconnect flow, cleanup semantics, and
whether Tab id has to become durable.

## Considered options

- **Tab-keyed sessions** (`sanctel:<tabId>`) — forces Tab id to become
  durable so the session can be re-found on launch. Contradicts the
  Persistence Anchor: Tab is supposed to be ephemeral, recomputable
  from durable references.
- **Worktree-keyed sessions, shared shell across Tabs** — one session
  per Worktree, multiple Tabs share one tmux window (extra clients on
  the same window). Mirrored scrollback: a test running in Tab 1 is
  visible in Tab 2 in the same Worktree. Doesn't match VS Code / iTerm
  user expectations.
- **Opaque-id sessions with Worktree as FK metadata** (superset.sh's
  pattern) — durable opaque session id per Tab, Worktree stored as a
  separate reference. Works but requires a per-Tab durable identifier
  in SQLite, plus a "session id no longer exists" recovery flow. More
  state than the chosen option, no compensating benefit.
- **One tmux session per worktree-branch, one shell only**
  (claude-squad's pattern) — too coarse. Sanctel's pitch is multiple
  tabs per project context (terminal + chat + build watcher), which
  this can't express.

## Consequences

- **Tab id stays fully ephemeral.** A terminal Tab's only durable
  backend fields are `worktreeId` and `windowName`. Both are
  references, not state.
- **Many Tabs per Worktree get independent shells**, matching VS Code /
  iTerm. Future "mirrored view" feature is opt-in (extra client on the
  same window), not the default.
- **Cleanup is automatic.** `close_tab` for `kind=terminal` runs
  `tmux kill-window`; tmux destroys the session when its last window
  dies. No bookkeeping in Rust. **Caveat (issue #14):** this only
  holds if sanctel never lets a phantom window sneak into the session.
  A bare `tmux new-session -d -s <s> -c <cwd>` auto-creates an initial
  window named after the user's shell (`zsh-`, `bash-`, …), which is
  not one sanctel will ever kill. Sanctel therefore creates sessions
  exclusively through `TmuxCli::ensure_session_window`, which folds
  the session-creation and first-window-creation into a single
  `tmux new-session -d -s <s> -n <window_name> -c <cwd>` call. The
  session is born with exactly one window — the one sanctel asked
  for — and dies the moment that window dies.
- **Reconnect is idempotent.** `ensure_session_window` is the single
  primitive: it creates the session+window when missing and is a
  pure no-op when both exist. Same code path for first creation and
  reattach-after-restart.
- **No daemon to write.** Superset.sh's pty-daemon (~30k LOC + fd
  handoff) is avoided. Sanctel uses tmux as tmux was designed.
- **`-L sanctel -f /dev/null`** isolates sanctel from the user's
  existing tmux server and ignores their `~/.tmux.conf`. The user's
  `tmux ls` does not show sanctel sessions; their `tmux kill-server`
  does not affect sanctel.
- **windowName allocation** uses a per-Worktree monotonic counter
  derived from existing `tmux list-windows` output at creation time;
  the value is persisted on the Tab record and never changes.
- **Tab title and tmux window name are separate fields.** Title is
  user-editable UI; windowName is an immutable internal handle. Rename
  affects only title; the tmux side is untouched.
- **Chat tabs use the same mechanism** plus an `initialCommand` (e.g.,
  `claude --resume <agentSessionId>`) consumed only when the window is
  freshly created. Existing windows on reattach are never re-`--resume`d.
- **glossary updated:** `src/core/CONTEXT.md` no longer says
  "TmuxSession is named by Tab or Worktree." The TmuxSession entry now
  names the session by Worktree and references the window-per-Tab
  structure.

See [docs/design/terminal-runtime.md](../design/terminal-runtime.md)
for the full design — IPC contract, reconnect algorithm, two-layer
durability story, and implementation order.
