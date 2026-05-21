# 0013 — Agent-session discovery via CLI hooks, not creation-time intent

**Status:** Accepted

**Decision:** Sanctel discovers which agent CLI session is running in
each pane by **installing a hook into the user's per-CLI settings
files** (`~/.claude/settings.json`, `~/.codex/hooks.json`,
`~/.gemini/settings.json`) and **tagging each pane with a
`SANCTEL_TAB_ID` env var** at tmux session creation. The hook,
invoked by the CLI itself on every session lifecycle event, reads
`SANCTEL_TAB_ID` from its inherited environment and writes a sidecar
`~/.sanctel/hooks/<tab-id>.json` containing the agent's session id.
Sanctel's fsnotify watcher reads the sidecar and updates
`Tab.agentSessionId` in SQLite.

Agent identity is therefore **observed**, not **declared at tab
creation**. A tab's `kind: "chat"` becomes a UI affordance (icon,
default initial command) rather than a precondition for session
tracking. A user running `claude` (or `codex`, or `gemini`) directly
in any sanctel pane — by alias, absolute path, subshell, `--resume`,
`--continue`, or the "new chat" button — produces the same binding.

## Why this matters

The prior model assumed agent identity was set at tab creation: a
"chat tab" was a terminal tab whose initial command was the agent CLI.
This left a structural gap: a user running the same CLI in a "normal"
terminal tab got no tracking and no restore-on-reboot guarantees.

For sanctel, where multiple agents per worktree is the dominant
workflow (verified empirically: 30+ live claudes on the author's host,
including 7 in `/Users/almeynman/code/sanctel`), creation-time intent
is the wrong identity model. Observation, not declaration.

The two structural facts that make hook-based discovery work:

> 1. **Hook configuration lives in the CLI's settings file, not in
>    `$PATH`.** Aliases, absolute-path invocations, subshells, and
>    Makefile rules all load the same settings file. The hook fires
>    regardless of how the binary was launched.
>
> 2. **`tmux new-session -e VAR=val` propagates the variable to the
>    pane's shell and through to any subprocess.** Sanctel can mark
>    a pane "this is sanctel pane <uuid>" durably via env, without
>    modifying anything the user could bypass with an alias.

(Verified in the spike at `docs/design/spikes/restore-feasibility.md`.)

## Considered options

- **`--session-id <uuid>` injection at sanctel-spawn time.** Works
  for claude and gemini; codex doesn't expose the flag for fresh
  sessions. Only covers sanctel-spawned chat tabs — a user typing
  `claude` in a normal terminal tab gets no binding. Asymmetric and
  fragile. Rejected.

- **PATH-prepend wrapper script.** Sanctel installs
  `claude` / `codex` / `gemini` shims in `$SANCTEL_BIN`, prepended to
  the pane's PATH. Bypassed by aliases (the user already has these),
  absolute paths, and any subshell that doesn't inherit PATH. Was the
  obvious-looking option; the user correctly flagged the fragility.
  Rejected.

- **Passive observation via filesystem (no intervention).** For
  claude, read `~/.claude/sessions/<pid>.json` after walking each
  pane's process tree — works deterministically (claude embeds its
  own pid). For codex/gemini, watch their transcript dirs and
  correlate new files to pids by cwd + start-time. macOS does not
  provide pid-of-writer attribution for filesystem events without
  privileged tracing (dtrace, ESF entitlements), so multi-agent-of-
  same-kind-in-same-cwd cases collapse to best-effort. Insufficient
  precision for sanctel's stated requirement; the multi-codex-per-
  worktree case is exactly the scenario where ambiguity would bite.
  Rejected as primary; kept as the fallback for `--bare` and other
  edge cases where the hook doesn't fire (see Consequences).

- **`HOME` or `<CLI>_CONFIG_DIR` override per pane.** Would give
  structural per-pane isolation but nukes the user's existing CLI
  auth, history, settings, and resume affordances. Rejected as
  hostile to the user's environment.

- **dyld interposition / DTrace tracing.** macOS-native pid-of-
  writer attribution. Privileged or magical. Cross-platform fragile.
  Rejected.

- **SDK / protocol mode (paseo's pattern).** Run claude via the
  Anthropic Agent SDK and codex via `codex app-server`, owning
  session_id by construction. Requires abandoning native TUIs —
  no `/skills`, no claude's `/resume` picker, no `--continue` UX.
  Architecturally inconsistent with sanctel's "native TUI per tab"
  premise. Rejected for sanctel; flagged for a separate strategic
  ADR if the tradeoff is ever revisited.

- **agent-deck's hooks-in-settings + `SANCTEL_TAB_ID` env** (chosen).
  Uniform across the three CLIs. Deterministic for every invocation
  path except deliberate `env -u` strips. Mutates the user's CLI
  settings files (the cost), but multi-tenant: existing hook
  entries are preserved (verified on the author's host, which
  already runs superset hooks on claude/codex and TUICommander
  hooks on gemini).

## Consequences

- **Tab `kind: "chat"` becomes a UI affordance.** It controls icon
  + default initial command + first-paint behaviour, but `kind` is
  no longer load-bearing for restore. `Tab.agentSessionId` is
  populated by observation, not by tab kind.

- **Sanctel mutates the user's `~/.claude/settings.json` /
  `~/.codex/hooks.json` / `~/.gemini/settings.json`.** Mutation is
  additive: sanctel appends a tagged entry to each event's hook
  array. Removal (during uninstall or `sanctel uninstall-hooks`)
  removes only tagged entries. Pre-existing entries from other
  tools (superset, TUICommander, the user's own configs) are
  preserved.

- **Install is explicit and reversible.** First-launch flow asks
  for consent: "Sanctel needs to install hooks in your
  `~/.claude/settings.json` to track agent sessions. The hooks are
  no-ops outside sanctel panes. Install? [Y/n]". Decline falls back
  to the passive-observation model (claude-only, sanctel-spawn-
  only for gemini/codex, lossy for user-typed in non-sanctel-aware
  panes) and surfaces that limitation in the UI.

- **`SANCTEL_TAB_ID` is set at session-create time**, not via
  `set-environment` after the fact. `tmux new-session -e
  SANCTEL_TAB_ID=<uuid> -s <name> …`. Verified in spike that
  post-creation `set-environment` does not affect the pane's
  existing shell.

- **The hook handler is a `sanctel hook-handler` subcommand on the
  sanctel binary**, not a separate script. Settings entry is
  `{"type":"command","command":"<sanctel-binary-path> hook-handler"}`.
  Single binary; no path-finding ambiguity.

- **Per-CLI event mapping is contained.** The hook handler
  normalises across claude's `SessionStart / UserPromptSubmit /
  Stop / PermissionRequest / Notification / SessionEnd`, codex's
  `SessionStart / UserPromptSubmit / Stop`, and gemini's
  `SessionStart / BeforeAgent / AfterAgent / AfterTool /
  SessionEnd`. Mapping table modelled on agent-deck's
  `cmd/agent-deck/hook_handler.go:53-79`.

- **Multi-agent-per-cwd is structurally precise.** Each pane has
  a distinct `SANCTEL_TAB_ID`. Each hook write keys on that.
  N concurrent agents in the same cwd in N panes produce N
  distinct sidecars. No race window.

- **`Tab.agentSessionId` retains its existing role** as the
  durable handle for `--resume`. The change is in how it gets
  populated (observation, not creation-time intent).

- **The resurrect snapshot is post-processed by sanctel**, not by
  a separate shell hook in the resurrect plugin. Sanctel calls
  `tmux run-shell <resurrect>/save.sh` to write the baseline
  snapshot, then rewrites pane lines to inject
  `<cli> --resume <session-id>` based on the `Tab.agentSessionId`
  lookup. ~50 LOC of Rust, well-contained.

- **Honest residual asymmetries.** Documented:
  - `claude --bare` skips hooks. Falls back to the passive
    `~/.claude/sessions/<pid>.json` read (amux's mechanism).
  - `env -u SANCTEL_TAB_ID claude` deliberately strips. No
    binding. User chose this.
  - Running an agent in a non-sanctel-aware tmux pane that
    happened to share the sanctel socket: no env, no binding.
    Out of scope.

- **Persistence Anchor (ADR-0004) is preserved.** Sanctel still
  doesn't own durable agent state. `~/.sanctel/hooks/` is an
  observation cache, not a source of truth; the durable state
  lives in the agent's own transcripts (`~/.claude/projects/`,
  `~/.codex/sessions/`, `~/.gemini/tmp/<project>/chats/`). The
  cache is rebuilt from hook events on every launch.

- **Per-tab event subscription** (the byte-stream-shaped properties
  described in ADR-0012's "Per-tab event subscription is delivered
  by this architecture" section) is unchanged — that's PTY-level
  observation, orthogonal to agent-identity discovery here.

## Implementation notes

- Hook entry shape (claude example, all three are isomorphic):
  ```
  {
    "hooks": [
      {
        "type": "command",
        "command": "/Applications/Sanctel.app/.../sanctel hook-handler"
      }
    ]
  }
  ```
  Sanctel tags its entries with a stable marker (e.g.
  `"_sanctel": "v1"`) on the inner object so uninstall is a clean
  filter. agent-deck does this pattern.

- The sidecar JSON shape:
  ```
  {
    "agent": "claude" | "codex" | "gemini",
    "session_id": "<uuid>",
    "cwd": "...",
    "event": "SessionStart" | …,
    "ts": 1779311720
  }
  ```

- The fsnotify watcher debounces (100ms, as in agent-deck) to
  coalesce rapid events.

- Inode-overflow recovery: full re-scan of `~/.sanctel/hooks/`,
  not retry. agent-deck pattern at
  `internal/session/hook_watcher.go:151-166`.

## Migration

- No existing data to migrate. `Tab.agentSessionId` is already a
  field on the Tab record (see `src/core/types.ts:78-98`); the
  change is in how it's populated.
- First-launch consent prompt for hook installation. Decline path
  routes to the documented best-effort fallback.

## Revision history

- **2026-05-21:** Accepted. Triggered by the v0.3 work to make
  tabs survive a laptop restart; the chat-tab restore requirement
  surfaced the creation-time-intent gap and led to the observation
  model. Spike memo: `docs/design/spikes/restore-feasibility.md`.

## See also

- ADR-0004 — Persistence Anchor pattern; this ADR is consistent
  with it (the sidecar is a cache, not state).
- ADR-0006 — TabKind unification; demoting `kind: "chat"` to a
  UI affordance completes that direction.
- ADR-0012 — One tmux session per Tab; agent-identity observation
  is orthogonal to session/window structure.
- `docs/design/spikes/restore-feasibility.md` — the spike evidence
  backing this ADR.
- `docs/design/terminal-runtime.md` — to be updated alongside
  Slice 3 implementation with the hook installation flow.
