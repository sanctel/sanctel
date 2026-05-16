# 0002 — Terminal architecture: tmux as PTY owner, xterm.js as renderer

**Status:** Accepted

**Decision:** Terminals are rendered by **xterm.js** (with the WebGL addon)
inside the webview. The PTY is owned by a long-running **tmux** server;
Rust spawns `tmux attach-session` via **portable-pty** to host the client
subprocess. Persistence across app restarts is a byproduct: the tmux server
outlives the app.

## Considered options

- **Own pty-daemon (superset.sh's path)** — ~30k LOC and an fd-handoff
  protocol just to get persistence. tmux already does this.
- **portable-pty without tmux** — simple but loses persistence; restarting
  the app kills every shell.
- **libghostty (Aizen / cmux)** — native VT + Metal renderer, smaller
  per-pane footprint, but macOS-only as of writing and pulls us off the
  cross-platform thesis.
- **alacritty_terminal + canvas** — viable but requires writing more
  rendering glue than xterm.js already provides.

## Consequences

- Persistence is free; we don't write a daemon. A crashed app restarts and
  re-attaches to tmux.
- Terminal performance is bounded by xterm.js's WebGL renderer (acceptable
  for typical workloads; not Ghostty-class for extreme throughput).
- Users must have tmux installed; documented in
  [CONTRIBUTING.md](../../CONTRIBUTING.md).
- tmux semantics (windows, panes, control mode) become part of our
  vocabulary; see Core's [Flagged ambiguities](../../src/core/CONTEXT.md#flagged-ambiguities)
  for the "Window" disambiguation.
