# 0012 — One tmux session per Tab, named with Worktree as prefix

**Status:** Accepted (revised — see Revision history)

**Decision:** Sanctel creates **one tmux session per terminal/chat Tab**,
named with the Worktree as a prefix so power users still see Worktree
grouping in `tmux ls`. The session contains exactly one window — the
tab's shell. All sessions live on a dedicated tmux server
(`tmux -L sanctel -f /dev/null`).

Naming convention:

- Worktree-keyed Tab: `sanctel_wt_<worktreeId>__<windowName>`
- Worktree-less Tab: `sanctel_detached_<profileId>__<windowName>`

`<windowName>` is the per-Worktree monotonic identifier (`term-1`,
`term-2`, …) allocated server-side under a per-Worktree mutex (issue
#10). It's the Tab's durable backend handle — same role as before; only
its placement in the name changes from "window within a session" to
"suffix on the session name."

The separator is `_`, not `:`, because tmux interprets `:` and `.` as
session/window/pane delimiters in target specs. Sanctel-built names
contain only characters in `[A-Za-z0-9_-]`; any `worktreeId` or
`profileId` is passed through `tmux_safe` before concatenation (issue
#13).

## Why the revision

The earlier version of this ADR ("one tmux session per Worktree, one
window per Tab") didn't account for a load-bearing property of tmux's
session/client model:

> **The "current window" is a property of the session struct, not the
> client struct.** All clients attached to a session render whatever
> `session->curw` points at. Two clients on one session = two views of
> the same active window, and either client's `select-window` mutates
> the pointer for both.

(Verified in `tmux/tmux.h`: `struct session` has a `curw` field;
`struct client` does not. `tmux/session.c::session_set_current` mutates
`s->curw`. `tmux/cmd-select-window.c` is the user-facing entry point.)

The original architecture had each Tab open its own client attached to
the shared Worktree session, then issue `select-window` to its own
window. Two tabs in the same Worktree both attached; both ran
`select-window`; the second won the race; both ended up rendering the
second tab's window. The user's `ls` in one tab appeared identically in
the other (issue #15).

The bug is structural — not a configuration to flip, not a race to
serialize, not a tmux quirk. It's how tmux defines its abstractions.
Any architecture where multiple sanctel viewports attach as separate
clients to one session will exhibit it.

## Considered options

- **Worktree-keyed sessions with windows per Tab** (the original
  decision in this ADR). Suffers the active-window-sharing bug above.
  Rejected.
- **Tab-keyed sessions** (`sanctel:<tabId>`) — earlier alternative.
  Forces Tab id to become durable; contradicts the Persistence Anchor.
  Rejected.
- **Worktree-keyed sessions, shared shell across Tabs** — one session,
  one window, multiple clients all rendering the same shell. Doesn't
  match VS Code / iTerm user expectations of "two tabs, two shells."
  Rejected.
- **Opaque-id sessions with Worktree as FK metadata** (superset.sh's
  pattern) — requires a per-Tab durable identifier in SQLite plus a
  "session id no longer exists" recovery flow. More state than the
  chosen option, no compensating benefit. Rejected.
- **One session per worktree-branch, one shell only** (claude-squad's
  pattern) — too coarse. Sanctel's pitch is multiple tabs per project
  context (terminal + chat + build watcher), which this can't express.
  Rejected.
- **Grouped sessions** (`tmux new-session -t <base>` — one base session
  per Worktree plus one group-member session per Tab, members share the
  window list but have independent `curw`). Technically delivers the
  per-tab independent view, and is what tmux's own `session_group_*`
  primitives exist for. Two reasons rejected for sanctel:
    1. **Way-station problem.** Groups deliver the per-tab `curw` but
       not the structured event subscription that more advanced features
       would want. Sanctel's natural endpoint beyond plain attach is
       control mode (`tmux -CC`), which subsumes grouped sessions'
       capabilities. Picking groups means paying their cost without
       reaching that endpoint.
    2. **No peer precedent.** No production agent-orchestrator-shaped
       project uses groups this way (claude-squad and agent-deck use
       one-session-per-thing; iTerm2 uses control mode). Using groups
       programmatically as the attach primitive is technically valid but
       off-label.
- **tmux control mode (`-CC`)** — long-term architectural endpoint when
  external-tmux-state sync or advanced tmux-protocol features (pause /
  continue / per-window sizing within one session) become real
  requirements. Not justified by anything on the v0.x roadmap; deferred.
  See "Migration path to control mode" below.

## Consequences

- **The active-window-sharing bug is structurally impossible.** Each
  Tab is its own session with one window; multiple clients per session
  never occur in the production path. Even if a power user manually
  attaches to a sanctel session from their own shell, there's only one
  window for them to see.
- **The model is a clean 1:1:1 mapping:** one Tab = one tmux session =
  one window = one shell. The user's mental model and the
  implementation align without translation.
- **Tab id stays fully ephemeral** (preserves the Persistence Anchor
  invariant from ADR-0004). The Tab's only durable backend fields are
  `worktreeId` (for Worktree-keyed tabs) and `windowName` (the
  session-name suffix). Both are references, not state.
- **Cleanup is one operation:** `close_tab` for `kind=terminal | chat`
  calls `tmux kill-session -t <session>`. The window and session die
  together. The "tmux destroys the session when its last window dies"
  guarantee from issue #14 still holds — it's just that "last window"
  is "the only window" by construction.
- **No phantom-window risk** (issue #14): `tmux new-session -d -s <s>
  -c <cwd> -n <windowName> [shell-cmd]` creates the session with the
  windowed-as-asked window as its initial child. No bare `new-session`
  call ever runs in production code.
- **Reconnect is idempotent.** `ensure_session_window` (the primitive
  from issue #14) takes the session name and window name; when both
  exist it's a no-op. The same code path covers fresh-create,
  sanctel-quit reattach, and laptop-reboot recreate.
- **No `select-window` in the attach command.** The session has one
  window; it's already the active one. Removing the clause closes the
  category of bugs that arose from session-scoped `curw` mutation.
- **`tmux ls` from outside shows N sessions per Worktree**, all sharing
  the `sanctel_wt_<id>__` prefix (or `sanctel_detached_<profileId>__`
  for worktree-less ones). The Worktree grouping is preserved by
  naming convention. Power users can attach to any individual session
  with `tmux -L sanctel attach -t sanctel_wt_main__term-1`.
- **No daemon to write.** Sanctel still uses tmux as tmux was designed;
  the ~30k-LOC fd-handoff path from superset.sh remains avoided.
- **`-L sanctel -f /dev/null` isolation is unchanged.** The user's
  `tmux ls` doesn't show sanctel; their `tmux kill-server` doesn't
  affect sanctel; sanctel ignores their `~/.tmux.conf`.
- **windowName allocation** is unchanged from issue #10: per-Worktree
  monotonic counter (`term-1`, `term-2`, …), allocated server-side
  under a per-Worktree mutex inside `create_tab`. Now the suffix
  identifies the session rather than a window-within-a-session; the
  uniqueness invariant and the mutex are unchanged.
- **Tab title and tmux session name are separate.** Title is
  user-editable UI; the session name is an immutable internal handle.
  Rename a tab and the tmux side is untouched (only the UI label
  changes).
- **Chat tabs use the same mechanism** plus an `initialCommand` (e.g.,
  `claude --resume <agentSessionId>`) consumed only when the session
  is freshly created. Existing sessions on reattach are never
  re-`--resume`d.

## Per-tab event subscription is delivered by this architecture

This ADR's choice does NOT close off per-tab event-subscription features
(activity indicators, bell, shell-exit detection, agent-turn detection,
prompt detection). Those features are byte-stream-shaped, and the
per-tab PTY attach already delivers byte-stream-per-tab as a structural
property:

- The Rust PTY-read thread produces every byte the shell outputs. We
  can intercept on the way to the Channel and emit per-tab events
  (`lastActivityMs`, BEL detected, OSC 133 prompt markers parsed,
  pattern-matched agent prompts).
- Activity-while-tab-is-hidden works because Tauri's
  `Channel<Vec<u8>>` runs in Rust regardless of webview visibility.
  The byte stream flows whether the user is looking or not.
- Shell exit / agent died is signaled by EOF on the PTY (the
  `tmux attach-session` client receives EOF when the session dies).
  Rust's read thread returns 0 bytes and emits `tab-exited`.

These features add ~10–50 LOC each on top of the C model. They do not
require a migration to control mode.

## Migration path to control mode (when and only when)

Control mode (`tmux -CC`) becomes the right migration target only for
the narrow set of features that are protocol-shaped, not byte-stream-
shaped:

1. **External tmux state sync** — push notifications when the user
   creates/renames/destroys windows or sessions from outside sanctel.
   `%window-add`, `%session-renamed`, etc.
2. **Tmux pause/continue protocol** — buffer-backpressure primitives
   added in tmux 3.4. Relevant only if sanctel becomes a slow consumer
   that needs the protocol's explicit pause mechanism.
3. **Multi-window-in-one-session UI rendering** — only applicable if
   sanctel ever moves away from one-session-per-Tab. Not planned.

None of these are on sanctel's v0.3 – v0.6 roadmap. The migration is
deferred until at least one of those use cases has a real PRD.

## Revision history

- **2026-05-18:** Revised from "one tmux session per Worktree, one
  window per Tab" to the current per-tab-session model. Triggered by
  issue #15 (two tabs in same Worktree share xterm output because tmux
  `curw` is session-scoped). The original model is documented in
  "Considered options" above as the rejected baseline.
- **2026-05-17:** Issue #14 added the `ensure_session_window` atomic
  primitive (sessions are created with their initial window in one
  `new-session -n <name>` call, avoiding phantom `zsh-` windows).
- **2026-05-17:** Issue #13 changed the session-name separator from
  `:` to `_` after discovering tmux's target-syntax interpretation of
  the original format.
- **2026-05-17:** Issue #10 moved windowName allocation server-side
  under a per-Worktree mutex.
- **Original:** ADR-0012 accepted with the Worktree-keyed-session +
  window-per-Tab model.

## Migration / one-time cleanup

The pre-issue-#15 session naming (`sanctel_wt_<wt>` with multiple
windows) is unreachable by the post-fix attach paths. Dev machines
that ran a pre-fix build should run
`tmux -L sanctel kill-server` once after pulling. There is no
auto-migration: pre-fix sessions can't be byte-stream-isolated by any
amount of post-hoc renaming.

See [docs/design/terminal-runtime.md](../design/terminal-runtime.md)
for the full implementation design — IPC contract, reconnect algorithm,
two-layer durability story, and implementation order. (The design doc
update lands alongside the implementation per issue #15's acceptance
criteria; the ADR is updated ahead of implementation so the
architectural decision is recorded as soon as it's made.)
