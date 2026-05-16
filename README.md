# Sanctel — Tauri 2 + React + mixed-tab-types

A minimal, working starting point for "Arc-shaped workspace where tabs can be
browsers, terminals, or chats." Adapted from Bushido's webview-per-tab pattern
and TUICommander/Aizen's tmux-backed terminal model.

## Architecture in one paragraph

React is the chrome (sidebar, top bar). Each tab is a Tauri webview created
via `window.add_child(builder, position, size)`. The active webview is
positioned to overlay the React content area; inactive webviews are moved
off-screen at `(-9999, -9999)`. Per-workspace cookie isolation is achieved
via `WebviewBuilder::with_profile_name(workspace_id)`.

```
┌──────────────────────────────────────────────────────────┐
│  React shell                                             │
│  ┌────────────┐  ┌──────────────────────────────────┐   │
│  │  Sidebar   │  │  ContentArea (just an empty div) │   │
│  │  - tabs    │  │                                  │   │
│  │  - spaces  │  │  ◄── Tauri webviews are          │   │
│  │            │  │      positioned ABSOLUTELY        │   │
│  │            │  │      over this div by Rust.      │   │
│  └────────────┘  └──────────────────────────────────┘   │
└──────────────────────────────────────────────────────────┘
```

## What's here

```
package.json            React 19 + Zustand + Tauri 2 client
src/types.ts            Tab / Workspace / CreateTabRequest / Rect
src/store/tabStore.ts   Zustand state; invokes Rust to spawn/show/hide tabs
src/components/
  Sidebar.tsx           workspace pills + new-tab buttons + tab list
  ContentArea.tsx       measures its rect, reports to Rust on resize
src/App.tsx, main.tsx, styles/app.css

src-tauri/
  Cargo.toml            tauri 2, parking_lot, serde
  tauri.conf.json       window config; no security CSP yet
  src/main.rs           entry
  src/lib.rs            create_tab / close_tab / show_tab / hide_all /
                        set_content_rect — the whole webview-mgmt layer

public/
  terminal.html         placeholder served at tauri://localhost/terminal.html
  chat.html             placeholder served at tauri://localhost/chat.html
```

Total: ~600 LOC across Rust, TS, CSS.

## To run

```
npm install
npm run tauri dev
```

Required: Node 20+, Rust toolchain, `cargo install create-tauri-app` not needed
since this sanctel already has the structure.

## How to extend, in order

### 1. Real terminal tabs (1–2 days)

The `public/terminal.html` page is a placeholder. To make it real:

1. Add `@xterm/xterm` + `@xterm/addon-webgl` + `@xterm/addon-fit` to package.json.
2. Make `terminal.html` import a TS module that opens xterm.js into a div.
3. In `src-tauri/src/lib.rs`, add a tmux-backed PTY runtime:
   - Use `portable-pty` to spawn `tmux new-session -d -s <id> ...` if a session
     for this tab doesn't exist, then `tmux attach-session -t <id>` to the
     PTY this webview controls.
   - Expose `terminal_create(session_id)`, `terminal_write(session_id, bytes)`,
     and event `terminal_output(session_id, bytes)`.
4. Wire xterm.js → Rust: `terminal.onData((d) => invoke("terminal_write", ...))`,
   and `listen("terminal_output", ...)` to push bytes into xterm.

Aizen has the reference implementation of this in
`aizen/Features/Terminal/Infrastructure/Tmux/TmuxSessionRuntime.swift` — same
pattern in Swift. The fact that you can run tmux as a child process and
attach to it cleanly is the whole persistence trick.

### 2. Worktree integration (a few hours)

Add a `worktree` module in Rust that shells out to `worktrunk` (or to `git
worktree add` directly). Expose `worktree_create(repo, branch)` and
`worktree_remove(path)`. When you spawn a terminal tab tied to a worktree,
pass `cwd=<worktree-path>` to the PTY runtime.

worktrunk: https://github.com/max-sixty/worktrunk

### 3. Per-workspace data isolation (already wired)

`WebviewBuilder::with_profile_name(workspace_id)` is set in
`create_tab`. Bushido proved this isolates cookies/localStorage. Verify
on your platform: log into GitHub in workspace A, switch to workspace B,
visit github.com — you should be logged out.

### 4. Status detection (a day)

Use `notify` crate to fsnotify-watch `~/.claude/projects/` and `~/.claude/hooks/`.
Emit `tab_status_change` events to the frontend; the Sidebar shows a
status dot per tab. Pattern from agent-deck's `internal/sessionstatus/`.

### 5. Mobile via Tailscale (a few hours)

Add an `axum` HTTP server in Rust that exposes the same state as the UI.
Serve a mobile-friendly HTML page from `/mobile`. Bind to `0.0.0.0`.
Users install Tailscale → connect from phone to the Mac's tailnet IP.

### 6. Chat tabs (a day, if you want them)

`public/chat.html` becomes a real React app (bundle separately or share the
main bundle). It listens for hook-file events from the same daemon as
status detection. Tab kind = "chat", url = "local://chat?session=<id>",
the page reads `?session=` and subscribes to that session's events.

## Open questions you'll hit

| Question | Where to look |
|---|---|
| How do I destroy a webview properly? | Tauri 2 webview destroy API is still evolving. Sanctel currently hides off-screen and forgets. Revisit when Tauri exposes it. |
| Can I show DevTools per webview? | Yes — Tauri 2's `webview.open_devtools()`. Wire to Cmd+Opt+I. |
| How do I sync state if app crashes? | Add a SQLite DB in `~/.sanctel/state.db`. Save tabs/workspaces on every mutation. Restore on launch. Bushido uses Zustand + manual persistence; agent-deck uses `modernc.org/sqlite` (Go) — same shape. |
| Right-click context menu? | Tauri 2's `Menu` API + `WebviewWindow::show_menu`. |
| Drag-to-reorder tabs? | Pure React — Zustand mutation. No Tauri call needed. |
| URL bar / address bar for browser tabs? | Add a Toolbar component above ContentArea. On URL change, call `invoke("navigate_tab", {id, url})` which calls `webview.eval("window.location = ...")` or destroys + recreates the tab. |
| Per-tab DevTools — Inspect Element | See Bushido's `src-tauri/src/lib.rs:618` for the WebView2 `SetAreDevToolsEnabled` pattern, or `webview.open_devtools()` on macOS. |

## References by subsystem

All cloned locally at sibling paths (`../<repo>/`). When you hit a specific
problem, jump to the listed file rather than the whole repo.

### Tauri 2 webview-per-tab (the foundation of this sanctel)

| File | What it teaches |
|---|---|
| `../bushido/src-tauri/src/lib.rs` lines 295–580 | the canonical `WebviewBuilder` + `window.add_child` + `with_profile_name` flow |
| `../bushido/src-tauri/src/lib.rs` lines 1840–1870 | how to reposition webviews on layout change (the `set_position` / `set_size` pattern this sanctel uses) |
| `../bushido/src-tauri/src/lib.rs` lines 580–640 | per-tab WebView2 customization on Windows (COM API dance — only relevant when you need DevTools toggles, downloads, etc.) |
| `../tuicommander/src-tauri/src/lib.rs` | a second Tauri 2 reference with agent-detection wired in; cross-check patterns against Bushido |

### Terminal-in-webview (xterm.js + PTY over IPC) — for step 1

| File | What it teaches |
|---|---|
| `../waveterm/frontend/app/view/term/termwrap.ts` | production-grade xterm.js wrapper; the front-end half of "terminal in a webview" |
| `../waveterm/cmd/server/` and `../waveterm/pkg/` | Go PTY + WebSocket backend; structurally identical to what you'd write in Rust |
| `../agent-deck/internal/web/handlers_ws.go` | xterm.js ↔ Go PTY over WebSocket, the simplest end-to-end reference |
| `../agent-deck/internal/web/bundle.go` | the esbuild config that bundles `@xterm/xterm` + `@xterm/addon-fit` + `@xterm/addon-webgl` |
| Tauri 2 docs (web): WebviewBuilder, IPC channels | the Rust-specific API surface |
| `portable-pty` crate docs (web) | the Rust PTY library; read its README + examples |

External (not cloned — read on GitHub when needed):
- VS Code terminal: `microsoft/vscode` → `src/vs/workbench/contrib/terminal/browser/terminalInstance.ts` — the definitive xterm.js+PTY integration. Worth reading even though VS Code is huge.

### tmux as the persistence layer

| File | What it teaches |
|---|---|
| `../claude-squad/session/tmux/tmux.go` (515 LOC) | **the canonical reference.** Prompt detection, attach/detach, session lifecycle, busy/waiting state — every other tool cites this |
| `../agent-deck/internal/tmux/detector.go` | encyclopedic prompt-detection patterns (Claude busy chars, Codex `›`, OpenCode spinners, Gemini, etc.) |
| `../agent-deck/internal/tmux/controlpipe.go` | `tmux -CC` control-mode parser; the alternative to running `tmux attach` as a child |
| `../aizen/aizen/Features/Terminal/Infrastructure/Tmux/TmuxSessionRuntime.swift` | tmux integration as Swift actor — translates cleanly to Rust |

### Worktree management

| File | What it teaches |
|---|---|
| `../tuicommander/src-tauri/src/worktree.rs` | **the directly-portable Rust reference.** Create / list / remove / move — copy-paste-adapt |
| `../tuicommander/src-tauri/src/git.rs` | `libgit2` wrappers (already includes `libgit2` as a vendored dep) |
| `../agent-deck/internal/git/git.go` | Go reference; identical patterns in a different language |
| `../worktrunk/` (Rust CLI) | study how the CLI itself works; your Rust code can shell out to it for v1 |

### Hook-file status detection (step 4)

| File | What it teaches |
|---|---|
| `../agent-deck/internal/sessionstatus/sessionstatus.go` | hook→status state machine. The decision tree for Claude/Codex/Gemini |
| `../agent-deck/internal/session/hook_watcher.go` | fsnotify pattern on `~/.claude/hooks/` |
| `../tuicommander/src-tauri/src/output_parser.rs` | output parsing for status detection in Rust (alternative when hooks aren't available) |
| `notify` crate docs (web) | the Rust fsnotify equivalent |

### Mobile companion + Tailscale bridge (step 5)

| File | What it teaches |
|---|---|
| `../amux/amux-server.py` (single 37k-LOC file) | single-binary web server + embedded HTML/JS, auto-TLS via Tailscale. Cleanest reference for "mobile dashboard over Tailscale" |
| `../tuicommander/docs/user-guide/remote-access.md` | the UX spec for what a mobile companion looks like |
| `../tuicommander/src-tauri/src/relay_client.rs` | HKDF-encrypted relay (if you ever want a cloud-relay alternative to Tailscale) |
| `axum` crate docs (web) | the Rust HTTP framework you'd use |

### Arc-style sidebar / Spaces UX (the visual side)

| File | What it teaches |
|---|---|
| `../zen-browser/src/zen/` | Arc-style UX patches: vertical tabs, workspaces, glance, splits. Pure CSS/JS — copy patterns into your React/CSS |
| `../zen-browser/src/zen/tabs/zen-tabs/` | the vertical-tabs implementation specifically |
| `../zen-browser/prefs/zen/workspaces.yaml` | what configuration surface a workspace exposes |
| `../bushido/src/components/Sidebar.tsx` | Tauri+React implementation of similar patterns |
| `../min/js/browserUI.js` and `../min/js/tabState.js` | Electron browser sidebar + tab-state patterns; less Arc-shaped but clean |

### Multi-agent orchestration (broader patterns)

| File | What it teaches |
|---|---|
| `../claude-squad/session/` | overall agent lifecycle; how to spawn, attach, and detect status across N agents |
| `../dmux/` | multi-select launch pattern (one prompt → N agents) and smart-merge after worktree completion |
| `../amux/amux-server.py` | inter-agent channels with @mentions, kanban board, self-healing watchdog |
| `../agent-deck/internal/` | the conductor / watcher / bridge patterns if you ever want Telegram/Slack integration |

### Browser features in a webview

| File | What it teaches |
|---|---|
| `../bushido/src-tauri/src/lib.rs` lines 580–640 | per-tab WebView2 settings (DevTools, status bar, downloads) — Windows-specific COM dance |
| `../aizen/aizen/Features/Browser/UI/Components/WebViewWrapper.swift` | per-tab WKWebView settings (DevTools, picture-in-picture, custom UA) — macOS WebKit equivalent |
| Tauri 2 docs: `WebviewWindow::open_devtools()` | the cross-platform DevTools API |
| `../min/js/findinpage.js`, `downloadManager.js` | clean implementations of common browser features |

### State persistence

| Reference | Pattern |
|---|---|
| `../agent-deck/internal/statedb/` | `modernc.org/sqlite` (pure-Go SQLite) — clean schema design |
| `../tuicommander/src-tauri/src/config.rs` | flat JSON with atomic-write (`temp + rename` — note the symlink caveat) |
| `../aizen/aizen/aizen.xcdatamodeld/` | Core Data schema — Mac-only but instructive shape |
| `rusqlite` or `tauri-plugin-sql` (web) | the Rust SQLite options |

### Cross-cutting: the VS Code workbench (external)

Not cloned (it's huge) but the **definitive reference** for "workspace + sidebar + tabs + terminal + webview" architecture. Read these specific paths on GitHub when you hit a problem:

- `microsoft/vscode` → `src/vs/workbench/contrib/terminal/browser/terminalInstance.ts` — best xterm.js+PTY code anywhere
- `microsoft/vscode` → `src/vs/workbench/contrib/webview/browser/` — webviews as first-class tabs (same insight you're using)
- `microsoft/vscode` → `src/vs/workbench/browser/parts/sidebar/` — activity bar + collapsible sidebar pattern
- `microsoft/vscode` → `src/vs/workbench/services/themes/` — per-workspace theming

If you want to sparse-clone just these:
```sh
git clone --depth 1 --filter=blob:none --sparse https://github.com/microsoft/vscode ../vscode
cd ../vscode
git sparse-checkout add src/vs/workbench/contrib/terminal src/vs/workbench/contrib/webview src/vs/workbench/browser/parts/sidebar
```

## Reading priority — if you had 4 hours tonight

```
1. (20 min) ../bushido/src-tauri/src/lib.rs:295-580
            — the webview-creation flow this sanctel imitates

2. (30 min) ../claude-squad/session/tmux/tmux.go
            — prompt detection + tmux integration; the most-cited reference

3. (30 min) ../waveterm/frontend/app/view/term/termwrap.ts
            — xterm.js production wrapping

4. (30 min) ../agent-deck/internal/web/handlers_ws.go
            + ../agent-deck/internal/web/terminal_bridge.go
            — minimum viable "xterm.js ↔ PTY over WebSocket"

5. (30 min) ../tuicommander/src-tauri/src/worktree.rs
            — the worktree module you'll paste-and-adapt

6. (30 min) ../agent-deck/internal/sessionstatus/sessionstatus.go
            — hook-file status state machine

7. (30 min) ../zen-browser/src/zen/tabs/zen-tabs/
            — Arc-style vertical-tab CSS/JS to transplant

8. (20 min) ../aizen/aizen/Features/Terminal/Infrastructure/Tmux/TmuxSessionRuntime.swift
            — tmux runtime in 250 lines; great translation target for Rust
```

That sequence gets you from sanctel-running to "I know how to build every subsystem."

## What I'd NOT recommend reading

- **Servo / Verso** — too experimental, performance not there yet
- **Chromium / Firefox forks beyond Zen's patches** — 30M+ LOC, overkill
- **Generic Tauri tutorials** — not specific enough; the local Bushido/TUICommander source is more useful
- **Old Electron browser apps** beyond Min/Beaker — patterns have moved on

## License

This sanctel is unlicensed example code — paste it into your own repo
under whatever license you choose. None of the referenced projects' actual
code is included here; only architectural patterns, which are not
copyrightable.
