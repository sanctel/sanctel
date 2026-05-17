# 0012 — one tmux session per Tab, Worktree as name prefix

**Status:** Accepted (revised by issue #15)

**Decision:** Sanctel maps its persistence model onto tmux's native
hierarchy as **one tmux session per terminal Tab**, named with the
Worktree as a prefix so power users still see Worktree grouping in
`tmux ls`. Sessions are named
`sanctel_wt_<worktreeId>__<windowName>` and live on a dedicated tmux
server (`tmux -L sanctel -f /dev/null`). Window names (`term-1`,
`term-2`, …) are allocated monotonically per Worktree and stored as the
Tab's only durable backend handle beyond `worktreeId`. They appear in
the **session-name suffix**, not as in-session tmux windows — each
session contains exactly one window with that same name.

Worktree-less terminal tabs attach to per-tab sessions named
`sanctel_detached_<profileId>__<windowName>`.

The session-suffix separator is `__` (double underscore). Every
sanctel-built id flows through `tmux_safe`, which collapses non-safe
characters to a single `_`, so `__` unambiguously marks where the
Worktree base ends and the windowName begins.

The base-segment separator within the Worktree-prefix is `_`, not `:`,
because tmux interprets `:` and `.` as session/window/pane delimiters
in target specs (`tmux list-windows -t foo:bar` parses as
`session=foo, window=bar`). Sanctel-built names contain only characters
in `[A-Za-z0-9_-]`; any `worktreeId` or `profileId` is passed through
`tmux_safe` before concatenation. (Issue #13 documents the original
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

- **Tab-keyed sessions with opaque ids** (`sanctel:<tabId>`) — forces
  Tab id to become durable so the session can be re-found on launch.
  Contradicts the Persistence Anchor: Tab is supposed to be ephemeral,
  recomputable from durable references.
- **Session per Worktree, multiple tmux windows per Worktree** (the
  previous form of this ADR, pre-issue-#15) — one session per Worktree
  with a window per Tab. Reads as the natural mapping but **breaks
  multi-tab independence**: `struct session` in tmux carries the
  `curw` (current window) pointer, so two PTY clients attached to one
  session always render the same window. Type `echo A` in tab A and
  it also appears in tab B. The bug is structural in tmux, not
  configurable. Issue #15 documents the diagnosis (tmux source pointers
  in `tmux.h` / `cmd-select-window.c`) and the rejection.
- **Grouped sessions** (`new-session -t <base> -s <member>`) — a
  way-station between the per-Worktree-shared and per-Tab approaches
  that gives each client its own `curw` while keeping the Worktree as
  a structural tmux entity. Rejected: ~150 LOC of group-member
  plumbing, and the unique benefits (shared session-level options,
  per-Worktree event subscription) aren't on the v0.3–v0.6 roadmap.
- **Opaque-id sessions with Worktree as FK metadata** (superset.sh's
  pattern) — durable opaque session id per Tab, Worktree stored as a
  separate reference. Works but requires a per-Tab durable identifier
  in SQLite, plus a "session id no longer exists" recovery flow. More
  state than the chosen option, no compensating benefit.
- **One tmux session per worktree-branch, one shell only**
  (claude-squad's pattern) — too coarse. Sanctel's pitch is multiple
  tabs per project context (terminal + chat + build watcher), which
  this can't express.
- **Control mode (`tmux -CC`)** — would give us structured event
  output for window/pane changes and is iTerm2's choice for the same
  problem space. Deferred to a separate ~1,500–2,000 LOC undertaking
  if/when a feature requiring per-tab event subscription (activity
  indicator, bell, shell-exit notification) materializes.

## Consequences

- **Tab id stays fully ephemeral.** A terminal Tab's only durable
  backend fields are `worktreeId` and `windowName`. Both are
  references, not state. The session name is recomputed at attach time
  from `tmux_session_name(worktreeId, profileId, windowName)`.
- **Many Tabs per Worktree get fully independent shells.** Two tabs in
  the same Worktree write to and read from two *different* tmux
  sessions; tmux's session-scoped `curw` pointer no longer crosses
  tabs. The bug class issue #15 closes is **structurally impossible**,
  not "guarded against."
- **Cleanup is one shot.** `close_tab` for `kind=terminal | chat` runs
  `tmux kill-session -t <session>`. The single window dies with the
  session; no two-level `kill-window` + base-survival dance. The
  `kill_session` helper is idempotent on missing sessions so retry /
  race scenarios are safe.
- **`new-session -n <windowName>`** is still load-bearing (carried
  from issue #14): without `-n`, tmux auto-creates a phantom shell
  window whose name pins the session's lifecycle, so the session
  outlives sanctel's `term-N`. The single primitive
  `ensure_session_window` guarantees every session is born with
  exactly its intended window — same code path used by `create_tab`
  (auto-allocate path) and `terminal_attach` (reattach path).
- **Reconnect is idempotent.** `ensure_session_window` is a no-op when
  the session already exists with the right window. Same code path
  for first creation and reattach-after-restart.
- **No daemon to write.** Superset.sh's pty-daemon (~30k LOC + fd
  handoff) is avoided. Sanctel uses tmux as tmux was designed.
- **`-L sanctel -f /dev/null`** isolates sanctel from the user's
  existing tmux server and ignores their `~/.tmux.conf`. The user's
  `tmux ls` does not show sanctel sessions; their `tmux kill-server`
  does not affect sanctel.
- **windowName allocation** is unchanged in semantics: a per-Worktree
  monotonic counter (`term-N` where N = max + 1). The implementation
  detail that changed is *what gets scanned*. Pre-issue-#15: the
  windows inside one shared session. Post-issue-#15: the sibling
  sessions sharing a Worktree-base prefix, filtered with
  `tmux list-sessions -F '#{session_name}'`. The per-Worktree mutex
  serializes the scan + allocate critical section.
- **Tab title and tmux window name are separate fields.** Title is
  user-editable UI; windowName is an immutable internal handle. Rename
  affects only title; the tmux side is untouched.
- **Power users still get Worktree grouping in `tmux ls`.** Sessions
  share a prefix (`sanctel_wt_<worktreeId>__`), so
  `tmux -L sanctel ls | grep sanctel_wt_<id>__` lists all tabs in
  that Worktree. `tmux -L sanctel attach -t sanctel_wt_<wt>__term-1`
  attaches directly to one specific tab.
- **No `select-window` in the attach command.** Each session has
  exactly one window, so the PTY runs only
  `tmux attach-session -t =<session>` — no `; select-window …` clause
  that could move the current-window pointer.
- **Chat tabs use the same mechanism** plus an `initialCommand` (e.g.,
  `claude --resume <agentSessionId>`) consumed only when the session
  is freshly created. Existing sessions on reattach never re-run the
  command.
- **glossary updated:** `src/core/CONTEXT.md` says "one TmuxSession
  per Tab, Worktree as name prefix" (post-issue-#15). The previous
  wording — "Many tabs on the same Worktree share one TmuxSession via
  separate windows" — is the rejected approach above.

## Migration / one-time cleanup

The pre-issue-#15 session naming (`sanctel_wt_<wt>` with multiple
windows) is unreachable by the post-fix attach paths. Dev machines
that ran a pre-fix build should run
`tmux -L sanctel kill-server` once after pulling. There is no
auto-migration: pre-fix sessions can't be byte-stream-isolated by any
amount of post-hoc renaming.

See [docs/design/terminal-runtime.md](../design/terminal-runtime.md)
for the full design — IPC contract, reconnect algorithm, two-layer
durability story, and implementation order.
