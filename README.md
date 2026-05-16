# Sanctel

Arc-shaped workspace app where every tab is a Tauri webview — browser,
terminal, chat, file, or diff — backed by tmux for persistence and
positioned over an empty React content area by Rust. Built for
agent-assisted workflows.

```
┌──────────────────────────────────────────────────────────┐
│  React shell (sidebar + chrome)                          │
│  ┌────────────┐  ┌──────────────────────────────────┐   │
│  │  Sidebar   │  │  ContentArea (just an empty div) │   │
│  │  - profiles│  │                                  │   │
│  │  - spaces  │  │  ◄── Tauri webviews are          │   │
│  │  - tabs    │  │      positioned ABSOLUTELY        │   │
│  └────────────┘  │      over this div by Rust.      │   │
│                  └──────────────────────────────────┘   │
└──────────────────────────────────────────────────────────┘
```

## Documentation

- **[CONTEXT-MAP.md](./CONTEXT-MAP.md)** — domain entry point; map of
  contexts and how they relate
- **[src/core/CONTEXT.md](./src/core/CONTEXT.md)** — Core glossary
  (Profile / Space / Tab / Worktree)
- **[docs/adr/](./docs/adr/)** — architectural decision records
- **[docs/design/](./docs/design/)** — full specs for planned subsystems
  (plugin system, file editor, agent ↔ browser control)
- **[docs/references.md](./docs/references.md)** — reference projects
  studied, by subsystem
- **[CLAUDE.md](./CLAUDE.md)** — agent / contributor working principles
- **[CONTRIBUTING.md](./CONTRIBUTING.md)** — setup, prerequisites,
  reading order

## Quick start

```sh
npm install
npm run tauri dev
```

Requires Node 22+, Rust toolchain, and tmux. See
[CONTRIBUTING.md](./CONTRIBUTING.md) for full prerequisites.

## Status

Working skeleton: webview-per-tab with Profile-isolated cookies and
position-as-visibility. Specialized contexts (terminal runtime, agent
runtime, file editor, plugin system, mobile bridge, agent-browser control)
are designed but not yet implemented — see
[CONTEXT-MAP.md](./CONTEXT-MAP.md) for the roadmap.

## License

Apache-2.0 — see [LICENSE](./LICENSE).
