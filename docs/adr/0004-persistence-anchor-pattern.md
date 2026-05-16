# 0004 — Persistence Anchor pattern: filesystem and tmux are durable, app state is recomputable

**Status:** Accepted

**Decision:** Sanctel's durable state lives in the filesystem (worktrees,
agent transcripts, profile data dirs) and in the tmux server. In-memory app
state — Tabs, Spaces, active selections — is **recomputable** on launch by
replaying its references to those durable entities. Sanctel saves almost
nothing of its own during normal operation.

## Why this matters

```
Ephemeral (recreated on launch)   Durable (outlives app)
─────────────────────────────     ─────────────────────────────────
Tab                               Profile data dir (cookies, etc.)
Space.activeTabId                 Worktree directory (real git wt)
Space (visual state)              AgentSession transcript
                                     (~/.claude/projects/<encoded>/…)
                                  TmuxSession (tmux server outlives app)
```

## Considered options

- **App owns durable state** (superset.sh's path) — requires its own
  daemon, fd-handoff, crash recovery, schema migration. Months of
  infrastructure with no user-facing payoff.
- **No persistence** (TUICommander's default) — every restart loses
  context. Unacceptable for an agent orchestrator.

## Consequences

- App restart restores Tabs by replaying their references: load Profiles,
  load Spaces, load Tabs, recreate each Tauri webview pointing at its URL
  with its Profile's `profile_name`, re-attach terminal tabs to their
  TmuxSession.
- Code that tries to make Tab the source of truth for something durable
  is wrong. Review rejects this.
- The pattern extends to file editor (files on disk are durable; CodeMirror
  state is ephemeral; unsaved buffers in a per-tab recovery file). See
  [docs/design/file-editor.md](../design/file-editor.md).
