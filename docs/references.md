# Reference projects

Local checkout paths live in `.agents/local-repos.json`; the tracked template
is `.agents/local-repos.example.json`. File pointers below use repo keys from
that map, for example `bushido:src-tauri/src/lib.rs`.

## Index

| Reference | Role |
|---|---|
| **bushido** | Tauri 2 + React Arc-shaped browser; webview-per-tab primary reference |
| **aizen** | Swift + libghostty + tmux; native macOS Arc-shaped workspace |
| **tuicommander** | Tauri 2 + Solid; multi-agent orchestrator with worktrees + mobile PWA |
| **agent-deck** | Go + Bubble Tea + tmux; encyclopedic reference for status detection |
| **claude-squad** | Go + tmux; 7.4k stars, canonical multi-agent TUI |
| **waveterm** | Go + Electron + xterm.js; production-grade terminal with blocks |
| **dmux** | TS + tmux; multi-select launch pattern |
| **amux** | Single-file Python + Tailscale; mobile dashboard reference |
| **zen-browser** | Firefox fork; Arc UX patches at CSS/JS level |
| **min** | Electron browser; clean webview-tab patterns |
| **superset** *(superset.sh)* | Electron + own pty-daemon; the over-architected reference |
| **worktrunk** | Rust CLI; just worktrees done well |
| **acpx** | ACP client; protocol-bridge reference |
| **cmux** | Swift + libghostty; vertical-tab terminal with browser split |
| **beaker** | Electron + p2p (defunct); per-BrowserView pattern |
| **tmux** | tmux source; exact session/window/control-mode behaviour |
| **iterm2** | macOS terminal UX and integration reference |
| **xterm.js** | canonical browser terminal emulator implementation |

## By subsystem

When you hit a specific problem, jump to the listed file rather than the
whole repo.

### Tauri 2 webview-per-tab (the foundation of the current skeleton)

| File | What it teaches |
|---|---|
| `bushido:src-tauri/src/lib.rs` lines 295–580 | the canonical `WebviewBuilder` + `window.add_child` + `with_profile_name` flow |
| `bushido:src-tauri/src/lib.rs` lines 1840–1870 | how to reposition webviews on layout change (the `set_position` / `set_size` pattern this skeleton uses) |
| `bushido:src-tauri/src/lib.rs` lines 580–640 | per-tab WebView2 customization on Windows (COM API dance — only relevant when you need DevTools toggles, downloads, etc.) |
| `tuicommander:src-tauri/src/lib.rs` | a second Tauri 2 reference with agent-detection wired in; cross-check patterns against Bushido |

### Terminal-in-webview (xterm.js + PTY over IPC) — for the terminal runtime

| File | What it teaches |
|---|---|
| `waveterm:frontend/app/view/term/termwrap.ts` | production-grade xterm.js wrapper; the front-end half of "terminal in a webview" |
| `waveterm:cmd/server/` and `waveterm:pkg/` | Go PTY + WebSocket backend; structurally identical to what you'd write in Rust |
| `agent-deck:internal/web/handlers_ws.go` | xterm.js ↔ Go PTY over WebSocket, the simplest end-to-end reference |
| `agent-deck:internal/web/bundle.go` | the esbuild config that bundles `@xterm/xterm` + `@xterm/addon-fit` + `@xterm/addon-webgl` |
| Tauri 2 docs (web): WebviewBuilder, IPC channels | the Rust-specific API surface |
| `portable-pty` crate docs (web) | the Rust PTY library; read its README + examples |

External (not cloned — read on GitHub when needed):

- VS Code terminal: `microsoft/vscode` → `src/vs/workbench/contrib/terminal/browser/terminalInstance.ts` — the definitive xterm.js+PTY integration.

### tmux as the persistence layer

| File | What it teaches |
|---|---|
| `claude-squad:session/tmux/tmux.go` (515 LOC) | **the canonical reference.** Prompt detection, attach/detach, session lifecycle, busy/waiting state — every other tool cites this |
| `agent-deck:internal/tmux/detector.go` | encyclopedic prompt-detection patterns (Claude busy chars, Codex `›`, OpenCode spinners, Gemini, etc.) |
| `agent-deck:internal/tmux/controlpipe.go` | `tmux -CC` control-mode parser; the alternative to running `tmux attach` as a child |
| `aizen:aizen/Features/Terminal/Infrastructure/Tmux/TmuxSessionRuntime.swift` | tmux integration as Swift actor — translates cleanly to Rust |

### Worktree management

| File | What it teaches |
|---|---|
| `tuicommander:src-tauri/src/worktree.rs` | **the directly-portable Rust reference.** Create / list / remove / move — copy-paste-adapt |
| `tuicommander:src-tauri/src/git.rs` | `libgit2` wrappers (already includes `libgit2` as a vendored dep) |
| `agent-deck:internal/git/git.go` | Go reference; identical patterns in a different language |
| `worktrunk:.` (Rust CLI) | study how the CLI itself works; your Rust code can shell out to it for v1 |

### Hook-file status detection

| File | What it teaches |
|---|---|
| `agent-deck:internal/sessionstatus/sessionstatus.go` | hook→status state machine. The decision tree for Claude/Codex/Gemini |
| `agent-deck:internal/session/hook_watcher.go` | fsnotify pattern on `~/.claude/hooks/` |
| `tuicommander:src-tauri/src/output_parser.rs` | output parsing for status detection in Rust (alternative when hooks aren't available) |
| `notify` crate docs (web) | the Rust fsnotify equivalent |

### Mobile companion + Tailscale bridge

| File | What it teaches |
|---|---|
| `amux:amux-server.py` (single 37k-LOC file) | single-binary web server + embedded HTML/JS, auto-TLS via Tailscale. Cleanest reference for "mobile dashboard over Tailscale" |
| `tuicommander:docs/user-guide/remote-access.md` | the UX spec for what a mobile companion looks like |
| `tuicommander:src-tauri/src/relay_client.rs` | HKDF-encrypted relay (if you ever want a cloud-relay alternative to Tailscale) |
| `axum` crate docs (web) | the Rust HTTP framework you'd use |

### Arc-style sidebar / Spaces UX (the visual side)

| File | What it teaches |
|---|---|
| `zen-browser:src/zen/` | Arc-style UX patches: vertical tabs, workspaces, glance, splits. Pure CSS/JS — copy patterns into your React/CSS |
| `zen-browser:src/zen/tabs/zen-tabs/` | the vertical-tabs implementation specifically |
| `zen-browser:prefs/zen/workspaces.yaml` | what configuration surface a workspace exposes |
| `bushido:src/components/Sidebar.tsx` | Tauri+React implementation of similar patterns |
| `min:js/browserUI.js` and `min:js/tabState.js` | Electron browser sidebar + tab-state patterns; less Arc-shaped but clean |

### Multi-agent orchestration (broader patterns)

| File | What it teaches |
|---|---|
| `claude-squad:session/` | overall agent lifecycle; how to spawn, attach, and detect status across N agents |
| `dmux:.` | multi-select launch pattern (one prompt → N agents) and smart-merge after worktree completion |
| `amux:amux-server.py` | inter-agent channels with @mentions, kanban board, self-healing watchdog |
| `agent-deck:internal/` | the conductor / watcher / bridge patterns if you ever want Telegram/Slack integration |

### Browser features in a webview

| File | What it teaches |
|---|---|
| `bushido:src-tauri/src/lib.rs` lines 580–640 | per-tab WebView2 settings (DevTools, status bar, downloads) — Windows-specific COM dance |
| `aizen:aizen/Features/Browser/UI/Components/WebViewWrapper.swift` | per-tab WKWebView settings (DevTools, picture-in-picture, custom UA) — macOS WebKit equivalent |
| Tauri 2 docs: `WebviewWindow::open_devtools()` | the cross-platform DevTools API |
| `min:js/findinpage.js`, `min:js/downloadManager.js` | clean implementations of common browser features |

### Plugin system (TUICommander pattern)

See [docs/design/plugin-system.md → References to study](./design/plugin-system.md#references-to-study)
for specific file pointers inside the `tuicommander` repo. The pattern-recognition
shortcut: when in doubt, do what TUICommander does.

### File editor (CodeMirror 6 + diff)

See [docs/design/file-editor.md → References to study](./design/file-editor.md#references-to-study).
Obsidian (not cloned) is the canonical "CodeMirror in a workspace app"
reference.

### Agent ↔ browser integration (MCP + per-platform WebView APIs)

See [docs/design/agent-browser-control.md → References to study](./design/agent-browser-control.md#references-to-study).
Microsoft Playwright MCP, Browser-Use, Stagehand for tool naming and API
ergonomics.

### State persistence

| Reference | Pattern |
|---|---|
| `agent-deck:internal/statedb/` | `modernc.org/sqlite` (pure-Go SQLite) — clean schema design |
| `tuicommander:src-tauri/src/config.rs` | flat JSON with atomic-write (`temp + rename` — note the symlink caveat) |
| `aizen:aizen/aizen.xcdatamodeld/` | Core Data schema — Mac-only but instructive shape |
| `rusqlite` or `tauri-plugin-sql` (web) | the Rust SQLite options |

Note: Sanctel anchors persistence in the filesystem + tmux server
(see [ADR-0004](./adr/0004-persistence-anchor-pattern.md)); these
references are for the small amount of app-owned state we still need.

## VS Code workbench (external)

Not cloned (it's huge) but the **definitive reference** for "workspace +
sidebar + tabs + terminal + webview" architecture. Read these specific
paths on GitHub when you hit a problem:

- `microsoft/vscode` → `src/vs/workbench/contrib/terminal/browser/terminalInstance.ts` — best xterm.js+PTY code anywhere
- `microsoft/vscode` → `src/vs/workbench/contrib/webview/browser/` — webviews as first-class tabs (same insight Sanctel is using)
- `microsoft/vscode` → `src/vs/workbench/browser/parts/sidebar/` — activity bar + collapsible sidebar pattern
- `microsoft/vscode` → `src/vs/workbench/services/themes/` — per-workspace theming

Sparse-clone just these:

```sh
git clone --depth 1 --filter=blob:none --sparse https://github.com/microsoft/vscode ../vscode
cd ../vscode
git sparse-checkout add src/vs/workbench/contrib/terminal src/vs/workbench/contrib/webview src/vs/workbench/browser/parts/sidebar
```

## Reading priority — if you had 4 hours tonight

```
1. (20 min) bushido:src-tauri/src/lib.rs:295-580
            — the webview-creation flow the current skeleton imitates

2. (30 min) claude-squad:session/tmux/tmux.go
            — prompt detection + tmux integration; the most-cited reference

3. (30 min) waveterm:frontend/app/view/term/termwrap.ts
            — xterm.js production wrapping

4. (30 min) agent-deck:internal/web/handlers_ws.go
            + agent-deck:internal/web/terminal_bridge.go
            — minimum viable "xterm.js ↔ PTY over WebSocket"

5. (30 min) tuicommander:src-tauri/src/worktree.rs
            — the worktree module you'll paste-and-adapt

6. (30 min) agent-deck:internal/sessionstatus/sessionstatus.go
            — hook-file status state machine

7. (30 min) zen-browser:src/zen/tabs/zen-tabs/
            — Arc-style vertical-tab CSS/JS to transplant

8. (20 min) aizen:aizen/Features/Terminal/Infrastructure/Tmux/TmuxSessionRuntime.swift
            — tmux runtime in 250 lines; great translation target for Rust
```

That sequence gets from skeleton-running to "I know how to build every
subsystem."

## What NOT to read

- **Servo / Verso** — too experimental, performance not there yet
- **Chromium / Firefox forks beyond Zen's patches** — 30M+ LOC, overkill
- **Generic Tauri tutorials** — not specific enough; local Bushido /
  TUICommander source is more useful
- **Old Electron browser apps** beyond Min / Beaker — patterns have moved on
