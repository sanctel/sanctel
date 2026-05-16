# Contributing to Sanctel

## What is Sanctel?

A workspace app where every tab is a Tauri webview. The tab's "kind"
(browser / terminal / chat / file / diff) is just which URL the webview
loads. Tabs are grouped into Arc-style **Spaces**; Spaces belong to a
**Profile** (the cookie isolation boundary). Terminal tabs are xterm.js
pages backed by tmux; agents (Claude / Codex / …) run as native CLIs
inside terminal tabs.

For the full domain model, start with **[CONTEXT-MAP.md](./CONTEXT-MAP.md)**
then **[src/core/CONTEXT.md](./src/core/CONTEXT.md)**.

## Reading order for a new contributor

1. **[CONTEXT-MAP.md](./CONTEXT-MAP.md)** — what contexts exist, how they
   relate.
2. **[src/core/CONTEXT.md](./src/core/CONTEXT.md)** — Core glossary
   (Profile / Space / Tab / Worktree).
3. **[CLAUDE.md](./CLAUDE.md)** — working principles + the vocabulary
   review enforces.
4. **[docs/adr/](./docs/adr/)** in numerical order — why we made every
   architectural decision.
5. **[docs/design/](./docs/design/)** — full specs for planned subsystems
   (plugin system, file editor, agent ↔ browser).

## Prerequisites

- macOS, Windows, or Linux
- Node.js ≥ 22 and npm
- Rust toolchain (`rustup`, stable channel) with `clippy` and `rustfmt`
- **tmux** (used as the PTY owner; see
  [ADR-0002](./docs/adr/0002-terminal-architecture.md))
- For Linux only: WebKitGTK 4.1 dev headers (`libwebkit2gtk-4.1-dev`)

## Running the app

```sh
cd sanctel
npm install
npm run tauri dev
```

The first `cargo build` takes a few minutes; subsequent runs are fast.

## Project layout

```
sanctel/
├── CONTEXT-MAP.md         ← domain entry point
├── CLAUDE.md              ← agent / contributor guide
├── CONTRIBUTING.md        ← this file
├── README.md              ← brief project overview
├── docs/
│   ├── adr/               ← architectural decision records
│   ├── design/            ← planned-subsystem specs
│   └── references.md      ← projects studied
├── src/                   ← frontend (TypeScript + React)
│   ├── core/              ← Core context (Profile, Space, Tab, Worktree)
│   │   ├── CONTEXT.md
│   │   ├── types.ts
│   │   ├── components/
│   │   └── store/
│   ├── App.tsx            ← composition root
│   ├── main.tsx
│   └── styles/
├── src-tauri/             ← backend (Rust + Tauri)
│   └── src/lib.rs         ← commands for tab lifecycle, webview positioning
├── public/                ← bundled HTML pages loaded by webviews
├── package.json
├── tsconfig.json
├── vite.config.ts
└── .sandcastle/           ← Sandcastle harness (implement / review / merge)
```

When specialized contexts gain code, they will appear as siblings of
`src/core/` (e.g., `src/plugin/`, `src/files/`) and as new directories
under `src-tauri/src/`. Each one carries its own `CONTEXT.md` per the
[CLAUDE.md](./CLAUDE.md) discipline.

## How to extend, in order

The current skeleton ships **browser tabs only** (one webview per tab,
Profile-isolated). The order in which to bring up planned subsystems is
optimized for shortest path to a useful app:

1. **Terminal tabs** (v0.3) — xterm.js + tmux. The biggest payoff.
2. **Worktree management** (v0.3) — shell out to worktrunk; create / list /
   remove worktrees from the sidebar.
3. **Chat tabs** (v0.3) — agent runtime; hook-file status detection (see
   [ADR-0007](./docs/adr/0007-native-cli-agents-hook-status.md)).
4. **File and diff tabs** (v0.4) — CodeMirror 6 +
   `@codemirror/merge`. Spec in
   [docs/design/file-editor.md](./docs/design/file-editor.md).
5. **Plugin system** (v0.5) — TUICommander-style. Spec in
   [docs/design/plugin-system.md](./docs/design/plugin-system.md).
6. **Mobile bridge** (v0.5) — axum + Tailscale tunnel; PWA.
7. **Agent ↔ browser control** (v0.6–v0.8) — MCP server +
   per-platform WebView APIs. Spec in
   [docs/design/agent-browser-control.md](./docs/design/agent-browser-control.md).

## Coding standards

Reviewed automatically by the Sandcastle harness against
**[.sandcastle/CODING_STANDARDS.md](./.sandcastle/CODING_STANDARDS.md)**.
That document is the source of truth for:

- Frontend conventions (Zustand stores, named exports, strict TypeScript,
  no `any` without comment, Tauri IPC contract)
- Backend conventions (clippy clean, no unwrap in non-test code, struct
  arguments to Tauri commands, per-platform module layout)
- Architecture invariants (the ones also captured as ADRs)

## Submitting changes

- Branch from `main`. PRs go to `main`.
- Every behavior change ships with at least one test that fails without
  the change.
- New decisions get an ADR. New contexts get a `CONTEXT.md`. New planned
  subsystems get a design doc. See
  [CLAUDE.md → "When you add something new"](./CLAUDE.md#when-you-add-something-new).
- Commit messages: imperative mood ("add tab close shortcut"), focus on
  the *why* in the body when non-obvious.
