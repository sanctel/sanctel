# Sanctel Context Map

Sanctel is a workspace app where every tab is a Tauri webview, and the tab's
"kind" (browser / terminal / chat / file / diff) is just which URL the
webview loads. The domain decomposes into one **Core** context (shared
kernel, referenced by every other context) and several **specialized**
contexts that own their own vocabulary on top of Core's entities.

```
┌──────────────────────────────────────────────────────────┐
│  React shell (sidebar + chrome)                          │
│  ┌────────────┐  ┌──────────────────────────────────┐   │
│  │  Sidebar   │  │  ContentArea (just an empty div) │   │
│  │  - tabs    │  │                                  │   │
│  │  - spaces  │  │  ◄── Tauri webviews are          │   │
│  └────────────┘  │      positioned ABSOLUTELY        │   │
│                  │      over this div by Rust.      │   │
│                  └──────────────────────────────────┘   │
└──────────────────────────────────────────────────────────┘
```

## Contexts

### Core / shared kernel

**[Core domain](./src/core/CONTEXT.md)** — Profile, Space, Tab, TabKind,
Worktree, Project, AgentSession, TmuxSession. The entities every specialized
context references.

- Frontend code: [`src/core/`](./src/core/)
- Rust code: [`src-tauri/src/lib.rs`](./src-tauri/src/lib.rs) (will migrate
  to `src-tauri/src/core/` when sibling Rust modules emerge)

### Specialized

Each specialized context creates its own `CONTEXT.md` lazily — only when the
code for it gets a directory. Until then, the design lives in
`docs/design/<sub>.md`.

| Context | Status | Glossary | Design |
|---|---|---|---|
| Plugin runtime | planned v0.5 | _(lazy — `src/plugin/CONTEXT.md` and `src-tauri/src/plugin/CONTEXT.md`)_ | [docs/design/plugin-system.md](./docs/design/plugin-system.md) |
| Terminal runtime | planned v0.3 | _(lazy)_ | [docs/design/terminal-runtime.md](./docs/design/terminal-runtime.md) |
| File editor | planned v0.4 | _(lazy)_ | [docs/design/file-editor.md](./docs/design/file-editor.md) |
| Agent runtime | planned v0.3 | _(lazy)_ | — |
| Agent ↔ browser control | planned v0.6 | _(lazy)_ | [docs/design/agent-browser-control.md](./docs/design/agent-browser-control.md) |
| Mobile bridge | planned v0.5 | _(lazy)_ | — |
| State persistence | planned v0.3 | _(lazy)_ | — |
| Worktree management | planned v0.3 | _(lazy)_ | — |

When a specialized context grows code in `src/<sub>/` or
`src-tauri/src/<sub>/`, its `CONTEXT.md` is extracted there, and the design
doc (if any) migrates from `docs/design/` to alongside the code. This map
is updated to point at the new location.

## Relationships

- **Core ↔ every specialized context.** Every context references Core
  entities (Tab, Profile, Space, Worktree). Core itself never references
  specialized terms.
- **Worktree ↔ Terminal runtime.** Terminal spawns a Pty whose cwd is a
  Worktree.path; TmuxSession persists across app restarts via the tmux
  server.
- **Worktree ↔ Agent runtime.** AgentSession transcripts are keyed by
  Worktree.path (e.g., `~/.claude/projects/<encoded-cwd>/<id>.jsonl`).
- **Worktree ↔ File editor.** Files belong to Worktrees; diff tabs are
  computed at the Worktree level (`base..HEAD`).
- **Plugin ↔ every other context.** Plugins declare Capabilities that
  authorize access to Terminal, File, Agent, and Agent-browser operations.
  Manifest-declared capabilities are enforced in Rust at the boundary
  between Plugin and the called context.
- **Agent-browser ↔ Core.** Browser tabs (a Core TabKind) are driven by
  Agent-browser's MCP tools. Profile inheritance is automatic: the agent
  drives the tab's existing webview, which already has the right cookies.
- **Mobile bridge → Core (read-only).** The PWA exposes Core state
  (Tabs, Profiles, Spaces) read-only over an HTTP server.
- **State persistence ↔ everything that's durable.** Profiles, Spaces, Tabs,
  Worktree references — all persisted through one schema. In-memory state
  is recomputable from filesystem + this schema (Persistence Anchor).

## Cross-cutting invariants

These are not contexts but invariants every context honors. Recorded as
ADRs so they have a permanent home:

- **Persistence Anchor** —
  [ADR-0004](./docs/adr/0004-persistence-anchor-pattern.md). Durable state
  lives in the filesystem (worktrees, transcripts, profile data dirs, tmux
  server). In-memory state is recomputable on launch.
- **Profile-as-identity-boundary** —
  [ADR-0003](./docs/adr/0003-profile-as-identity-boundary.md). Cookies isolate
  on Profile, never on Space.
- **Worktree orthogonality** —
  [ADR-0005](./docs/adr/0005-worktree-orthogonality.md). A Worktree belongs
  to a Project, not a Profile or Space.
- **TabKind unification** —
  [ADR-0006](./docs/adr/0006-tabkind-unification.md). Every TabKind is a
  webview loading a different URL; no per-kind tab data structures.

For all architectural decisions, see [docs/adr/](./docs/adr/).
For project conventions, see [CLAUDE.md](./CLAUDE.md).
For onboarding, see [CONTRIBUTING.md](./CONTRIBUTING.md).
