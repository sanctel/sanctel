# Spike — laptop-restart restore feasibility

> **Status:** complete. Findings inform ADR-0013 and the v0.3 work on
> tab+agent state surviving laptop restart.

## Question

Make Sanctel tabs survive a laptop restart, including the chat-tab case
where the user could have launched the agent CLI any way (sanctel's
"new chat" button, typing `claude`/`codex`/`gemini` in a normal terminal
tab, alias, absolute path, subshell, `--resume`d).

Restated as a research question: at the moment Sanctel needs to save its
state, how does it know which agent session each pane is running?

## Constraints discovered

- Filesystem write attribution by pid is **not available on macOS**
  without privileged tracing (dtrace requires sudo, Endpoint Security
  requires Apple entitlements). So passive FSEvents on agent transcript
  dirs cannot deterministically map a new file back to the pid that
  wrote it.
- PATH wrappers are bypassed by aliases (`alias claude=/abs/path`),
  absolute-path invocations, and any pre-existing shadowing in the
  user's rc files. Confirmed in spike: an aliased `claude` does not
  hit a `$PATH/claude` shim.
- The three CLIs differ in what they expose:
  - Claude writes `~/.claude/sessions/<pid>.json` with
    `{pid, sessionId, cwd, startedAt, ...}` — passively discoverable.
  - Codex writes `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` with
    `session_meta` (id + cwd) on line 1. No per-pid file. No
    `--session-id` flag for fresh launches.
  - Gemini writes `~/.gemini/tmp/<project>/chats/session-*.jsonl`
    with `{sessionId, projectHash}` on line 1. No per-pid file.
    Has `--session-id <uuid>` for fresh launches.

Any uniform solution has to bridge the codex/gemini gap.

## Mechanism that works: per-CLI hook into Sanctel-tagged env

All three CLIs support a hooks system whose config lives next to their
own settings:

- Claude: `~/.claude/settings.json` → `hooks.{SessionStart,
  UserPromptSubmit, Stop, PermissionRequest, PostToolUse, SessionEnd, …}`
- Codex: `~/.codex/hooks.json` → same shape, events `SessionStart`,
  `UserPromptSubmit`, `Stop`
- Gemini: `~/.gemini/settings.json` → `hooks.{SessionStart, BeforeAgent,
  AfterAgent, AfterTool, SessionEnd}`

Each event's value is an array of hook entries. Multiple integrations
coexist by appending. The user's machine already runs production hooks
from superset (claude + codex) and TUICommander (gemini), proving the
multi-tenant array pattern in production.

The hook command runs in the CLI's process environment. Hook payload
arrives on stdin as JSON, including `session_id` (verified in
agent-deck's production hook handler at
`agent-deck/cmd/agent-deck/hook_handler.go:30-42`).

The mechanism:

```
At sanctel install time:
  Append a `sanctel hook-handler` entry to each CLI's hook config,
  alongside existing entries. Idempotent. Reversible.

At sanctel pane creation:
  `tmux new-session -e SANCTEL_TAB_ID=<uuid>` so the shell — and any
  CLI it spawns — inherits the tab id.

At any agent launch in the pane (sanctel-spawned OR user-typed):
  CLI loads its settings file regardless of invocation path
    (alias, absolute, subshell — all read settings).
  CLI fires SessionStart hook.
  `sanctel hook-handler` runs in the CLI's process tree → inherits
    SANCTEL_TAB_ID env.
  Reads payload from stdin → gets session_id.
  Writes ~/.sanctel/hooks/<SANCTEL_TAB_ID>.json with
    {agent, session_id, cwd, ts}.

In sanctel:
  fsnotify watcher on ~/.sanctel/hooks/ updates
  Tab.agentSessionId in SQLite live.

At resurrect-save:
  Sanctel post-processes the snapshot, injecting
  `<cli> --resume <session_id>` for each pane that has a binding.

At restore:
  Resurrect's restore.sh replays the snapshot; each pane re-launches
  the right CLI against the right session.
```

## Why this works across the constraints

| Bypass attempt | Effect |
|---|---|
| Alias `claude=/abs/path` | not bypassed; CLI still loads its settings file |
| Absolute path `/opt/homebrew/bin/claude` | not bypassed; same reason |
| Subshell / Makefile invocation | not bypassed; env inherits, CLI loads settings |
| `--resume <id>` / `--continue` | not bypassed; hook still fires with the resumed id |
| Two agents same cwd in two panes | not bypassed; each pane has a distinct `SANCTEL_TAB_ID`; each hook writes a distinct sidecar |
| `env -u SANCTEL_TAB_ID claude` | deliberately bypassed; documented |
| Run claude in a non-sanctel tmux pane | bypassed (no env from sanctel); out of scope |

## Verifications run

| # | Question | Result |
|---|---|---|
| A1 | Claude's `--resume <id>` makes `sessions/<pid>.json` carry the resumed id | ✓ verified (pid 13173 was `--resume fc2b2c51…`, file's `sessionId` field matches) |
| A2 | Claude `--print` writes the session file (not relevant for restore — non-interactive) | ✓ verified, file is cleaned on exit |
| A3 | Plain `claude` writes the per-pid file | ✓ verified (30+ live claudes on host, all with files) |
| A4 | `claude --bare` writes the session file | unverified (requires `ANTHROPIC_API_KEY` env that we don't have; documented gap) |
| B1 | tmux-continuum behaviour on a sanctel-shaped host | refuses to restore because `another_tmux_server_running_on_startup` short-circuits when other servers exist. The user has 50+ tmux servers, including their own `default`. Drop continuum entirely. |
| B2 | tmux-resurrect's save.sh / restore.sh callable synchronously from sanctel | ✓ via `tmux run-shell`. Total launch overhead with 3 sessions: ~900ms. Empty-snapshot restore: 150ms. Save: ~280ms. |
| C1 | Codex has a `--session-id` flag for fresh launches | ✗ does not (resume by id only). |
| C2 | Gemini has `--session-id` | ✓ |
| C3 | Codex/gemini have a per-pid attribution file analogous to claude's | ✗ neither does. |
| C4 | All three CLIs have a hook config schema | ✓ verified — homogeneous `hooks.<Event>[]` array. |
| C5 | Hook payload includes `session_id` | ✓ verified by inspection of agent-deck and superset production hook handlers that depend on it. |
| E1 | `tmux new-session -e VAR=val` propagates to spawned shell | ✓ |
| E2 | env survives subshell (`bash -c …`) | ✓ |
| E3 | env survives alias indirection | ✓ |

## Reference scan summary

| Project | Binding mechanism | All 3 CLIs? | User-typed handled? | Multi-agent-per-cwd? | Notes |
|---|---|---|---|---|---|
| claude-squad | none — persists layout + program name only; no session id | yes (no tracking) | n/a | n/a | `session/storage.go` InstanceData fields confirm |
| tuicommander | `--session-id` on harness-spawn + filesystem scan with `claimed_ids` | claude / gemini / codex / goose | best-effort | best-effort | `src-tauri/src/agent_session.rs` doc-comment describes exactly the heuristic we evaluated |
| amux | `~/.claude/sessions/<pid>.json` passive read + child-walk fallback | claude only | yes (deterministic) | yes (via per-pid file) | `amux-server.py:904-943` — independent validation of the claude-only approach |
| superset | own pty-daemon + settings.json hooks (already installed on user's host) | claude + codex (confirmed) | yes | yes | over-engineered for our needs but pattern is sound |
| **agent-deck** | **hooks-in-settings + `AGENTDECK_INSTANCE_ID` env** | **yes** | **yes (deterministic)** | **yes (deterministic)** | `cmd/agent-deck/hook_handler.go`, `internal/session/hook_watcher.go`, `internal/sessionstatus/sessionstatus.go` |
| paseo | `@anthropic-ai/claude-agent-sdk` + `codex app-server` protocol mode | yes (SDK) | n/a — no terminal | n/a — owns session_id | wrong architecture for sanctel ("native TUI per tab" is core); flagged for a future strategic ADR |

Notable patterns worth importing besides the headline mechanism:

- **amux's child-walk fallback** (`_find_claude_pid`): if `pgrep -P
  shell -x claude` finds nothing, walk all children and check whose
  pid has a `sessions/<pid>.json` file. Catches wrapped/renamed
  invocations (`npm exec claude`, `bun --bun claude`, etc.). ~10 LOC
  of free robustness.
- **paseo's OSC 633 shell integration**
  (`packages/server/src/terminal/shell-integration/zsh/paseo-integration.zsh`):
  VS Code's terminal shell-integration protocol. Emits invisible
  escape sequences at `precmd` / `preexec` time, giving free
  per-pane command boundaries (`A` = prompt-start, `B/C` =
  command-start, `D;<exit>` = command-end). Not needed for Slice 3
  but a clean optional secondary signal if we ever want it for
  activity-detection or multi-agent-per-cwd edge cases.

## Slice impact summary

```
Slice 1  Bundle tmux-resurrect (NOT continuum); -f <bundled-conf>;
         scrollback-capture on; sanctel-driven save timer in Rust;
         anchor → restore.sh → kill-anchor on launch         ~2-3 days

Slice 2  SQLite ↔ tmux reconciliation (orphans + missing)      ~½ day

Slice 3  Hooks-based agent-session discovery:
         3a — `sanctel hook-handler` subcommand
         3b — `sanctel install-hooks` / `uninstall-hooks` that merge
              entries into ~/.claude/settings.json,
              ~/.codex/hooks.json, ~/.gemini/settings.json
         3c — tmux session created with `-e SANCTEL_TAB_ID=<uuid>`
         3d — fsnotify watcher on ~/.sanctel/hooks/ →
              Tab.agentSessionId in SQLite
         3e — resurrect-snapshot post-processor that injects
              `<cli> --resume <id>` based on Tab.agentSessionId    ~2-3 days

Slice 4  ADR-0013 + design-doc updates                         ~½ day
```

## Deferred / follow-up

- **`--bare` behaviour for claude:** verify in a follow-up that
  `claude --bare` writes the session file (it almost certainly does;
  `--bare`'s description lists what it skips, and session persistence
  is not in that list). File an issue if confirmed otherwise.
- **paseo SDK-mode strategic question:** separate ADR.
  Do we ever want to offer an SDK-mode chat tab? Defer.
- **OSC 633 shell integration:** optional, opt-in, v0.4+.
- **Upstream feature request to codex:** `--session-id` flag for
  fresh sessions. Would let us drop the hook dependency for codex
  if it ever lands. Low priority because hooks already solve the
  problem.
