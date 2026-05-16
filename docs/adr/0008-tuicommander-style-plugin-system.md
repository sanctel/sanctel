# 0008 — TUICommander-style plugin system: in-webview JS + Rust capability gates

**Status:** Accepted (architecture); **Implementation:** deferred to v0.5

**Decision:** Plugins run as ES modules **in the webview** (full DOM, fast
hot-reload). Privileged operations (filesystem, network, exec, PTY,
worktree, agent spawn, tab control) go through **Rust functions** that
check the plugin's **manifest-declared capabilities** at the boundary. The
manifest on disk is the single source of truth; the frontend cannot lie.

## Considered options

- **VS Code separate-process** — too heavy for our scale; the IPC tax
  isn't justified until we have an untrusted marketplace.
- **Pure WASM (Zed)** — narrow API ceiling; Zed has had an open RFC for
  custom UI panels for 18+ months and the ecosystem can't grow past
  languages and themes.
- **In-process without capability gates (Obsidian / Hyper / Sublime)** —
  works but every plugin can do anything; we want a clearer trust
  boundary for v1.
- **Config-only (tmux)** — too limited for tab kinds, agent integrations,
  output watchers.

## Consequences

- Tier-3 and Tier-4 operations each get a Rust enforcement module
  (`plugin_fs.rs`, `plugin_pty.rs`, `plugin_http.rs`, `plugin_exec.rs`,
  `plugin_worktree.rs`, `plugin_agent.rs`). JS cannot bypass them.
- Plugins ship as filesystem directories in v0.5; a community registry
  follows in v0.6.
- This decision composes with
  [ADR-0010](./0010-architecture-b-browser-control.md): agent-browser
  control is delivered as a plugin, with `tab:read | tab:control | tab:create`
  capabilities gated the same way.
- Full design (capability tiers, manifest format, PluginHost API, lifecycle,
  distribution) in
  [docs/design/plugin-system.md](../design/plugin-system.md).
