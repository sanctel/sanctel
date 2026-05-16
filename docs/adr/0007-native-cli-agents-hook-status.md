# 0007 — Run agents as native CLIs in tmux; detect status via hook files

**Status:** Accepted

**Decision:** Agents (Claude Code, Codex, Gemini, Aider, …) run as their
**native CLIs inside a tmux pane**. xterm.js renders their TUI as-is. Status
(`idle | working | waiting | error | rate-limited`) is detected primarily
via **hook files** (`~/.claude/hooks/` and equivalents) watched by the
`notify` crate; pane scraping is a fallback for agents without hook files.

## Considered options

- **ACP-mode subprocess driven by our UI (superset.sh's path)** — hides the
  native TUI, requires building chat panels per agent, and breaks every
  time an agent ships TUI improvements.
- **Chat-panel-only (Zed's path)** — wrong paradigm; the user wants the
  full agent TUI experience.
- **Pane scraping only (agent-deck / claude-squad)** — works but fragile;
  every agent has different spinner glyphs and prompt patterns.

## Consequences

- Agent UIs always look exactly like they do in a normal terminal — we
  don't reimplement them.
- Per-agent status detection is small and additive: a new agent ships with
  hook support (clean) or a regex pattern set (messy but fine).
- AgentSession transcripts are written by the agents themselves
  (`~/.claude/projects/<encoded-cwd>/<id>.jsonl`) — portable across
  orchestrators (Claude Squad, agent-deck, Sanctel all read the same files).
- The "AgentSession is keyed by cwd, not by Tab" invariant follows
  naturally; see
  [src/core/CONTEXT.md](../../src/core/CONTEXT.md#language).
