# CONTEXT.md

Domain context for this product. Read top-to-bottom on first encounter; jump
back to the glossary later.

> **Audience**: future you, future AI agents. Skim time: ~10 min.

---

## 1. The product, in one paragraph

A workspace app where every tab is a **Tauri webview**, and the tab's "kind"
(browser / terminal / chat / file) is just which URL the webview loads.
Tabs are grouped into Arc-style **Spaces** (one per project or branch).
Spaces belong to a **Profile** — the cookie/storage isolation boundary
(via Tauri's `WebviewBuilder::with_profile_name`). Many Spaces can share
one Profile (e.g., all work-account spaces share the Work profile's GitHub
login). Terminal tabs are xterm.js pages that connect to a **tmux**-backed
PTY runtime in the Rust backend; tmux outliving the app gives us
**persistence** for free. Agents (Claude / Codex / etc.) run as native CLIs
inside terminal tabs — their TUIs render directly, and status comes from
**hook files** (`~/.claude/hooks/`) watched via `notify`. A small `axum`
HTTP server exposes a mobile-friendly UI reachable over **Tailscale**.

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

---

## 2. Architecture decisions made

These have been weighed against alternatives during exploration; revisit
only with explicit reason.

| Decision | Choice | Rejected alternatives |
|---|---|---|
| UI shell | Tauri 2 + React + Zustand | Electron (heavy), native AppKit (mac-only), TUI (no mobile) |
| Terminal renderer | xterm.js + WebGL addon | libghostty (native, mac-only), alacritty_terminal+canvas (more code) |
| PTY ownership | tmux as long-running server, Rust spawns `tmux attach-session` | own pty-daemon (superset.sh's path; ~30k LOC overhead), direct portable-pty without tmux (no persistence) |
| Persistence | tmux server outlives the app | own daemon with fd-handoff (overkill), no persistence (TUICommander default — unacceptable) |
| Identity isolation | `WebviewBuilder::with_profile_name(profile_id)` — **Profile is the cookie boundary, not Space** (Arc model) | per-Space isolation (Bushido's model — forces re-login when reorganizing), shared cookie store (Aizen's current state — wrong), per-window apps (clunky) |
| Agent execution | native CLI in a tmux pane, rendered by xterm.js | ACP-mode subprocess driven by our UI (superset.sh's path — hides the native TUI), chat-panel-only (Zed's path — wrong paradigm) |
| Status detection | hook files via fsnotify (primary), pane scraping (fallback) | ACP events only (incompatible with native TUI), pane scraping only (fragile per-agent) |
| Worktree management | shell out to `worktrunk` initially; consider absorbing logic later | `git worktree` raw shell (works but verbose), libgit2 directly (more code) |
| Mobile bridge | `axum` HTTP server + Tailscale tunnel | cloud relay (TUICommander pattern; extra service), Apple push (vendor-locked) |
| State persistence | SQLite via `tauri-plugin-sql` or `rusqlite` | flat JSON (atomic-write symlink trap), Core Data (mac-only) |
| Plugin system *(planned, §6)* | TUICommander-style: ES modules in webview + Rust capability gates, manifest-declared permissions | VS Code-style separate process (heavy; defer to v2), pure WASM (Zed; narrow ecosystem), no plugins (Bushido; ceiling too low) |
| File editing *(planned, §7)* | Levels 1+2+5 (view + light edit + diff) via CodeMirror 6 in new TabKinds (`file`, `diff`); external IDE for real editing | Monaco (too heavy for our scope), full level-3 IDE (months of work, not our differentiator), no editing (forces external IDE every time — UX friction) |
| Agent ↔ browser control *(planned, §8)* | Architecture B: agent drives the user's tabs in-place via MCP server → per-platform `evaluateJavaScript` / `ExecuteScriptAsync` / `webkit_web_view_run_javascript`. Plugin-delivered with manifest-declared `tab:*` capabilities. | Playwright (can't drive in-place WKWebView/WebView2; would bundle 300MB of separate browsers — Architecture A), Computer Use vision loop (slow + token-heavy; defer as fallback), don't support browser-agent control (loses a major workflow class) |
| Browser extensions *(decision, §8)* | **No Chrome extension runtime.** WebView-only browser tabs. Extension-equivalent features as plugins (adblock, vault/autofill, userscripts, reader-mode, vim-nav). "Open in real browser" escape hatch. | Full Chromium via CEF (~150MB bundle; weeks of integration; loses Tauri thesis), Electron with extensions API (rewrite off Tauri; 100-150MB; different product identity), fork Chromium (team-scale engineering), no extension equivalents at all (lose adblock + autofill use cases that matter) |

---

## 3. Glossary — domain terms

### Product entities

The model has two parallel hierarchies — in-app (cookies + organization) and
filesystem (git + transcripts) — bridged by Tab.

```
In-app hierarchy                       Filesystem entities (orthogonal)
─────────────────                      ─────────────────────────────────
Profile  (identity, cookies)           Project  (a git repo on disk)
   └── Space  (color, tab list)              └── Worktree  (one per branch)
         └── Tab  ──────────────────────────────────┘
              │                        AgentSession  (Claude/Codex transcript,
              │   bridges via                        keyed by cwd path)
              │     spaceId, worktreeId?,
              │     sessionId?         TmuxSession  (PTY persistence handle)
              ▼
              kind: browser | terminal | chat
```

Profile and Space form the cookie+organization tree (left side). Project,
Worktree, AgentSession, and TmuxSession are filesystem entities that exist
regardless of which Profile is active (right side). A Tab references both
sides via foreign keys.

| Term | Definition | Type in code |
|---|---|---|
| **Profile** | The cookie/storage isolation boundary. Maps 1:1 to Tauri's `WebviewBuilder::with_profile_name`. User has 1 (typical) or 2-3 ("Work", "Personal"). | `Profile` |
| **Space** *(Arc's term, was "Workspace")* | Organizational grouping (color, tab list). Belongs to exactly one Profile. Switching Spaces may implicitly switch Profiles. | `Space` |
| **Tab** | Atomic unit shown in the sidebar. Each tab is a Tauri webview. Has a `kind`. Bridges in-app and filesystem worlds: references a `spaceId` (mandatory), `worktreeId` (optional), `sessionId` (optional). Inherits identity from its Space's Profile. | `Tab` |
| **TabKind** | `"browser" \| "terminal" \| "chat"` (and eventually `"file"`, `"diff"`). Determines what URL the webview loads. | `TabKind` |
| **Project** | A git repo on disk. **Filesystem entity** — exists regardless of which Profile is active. One Project can be touched by tabs in many Spaces, across multiple Profiles. | `Project` |
| **Worktree** | A git working directory (real `git worktree`). Tied to a branch. **Filesystem entity, orthogonal to Profile/Space.** A Tab optionally attaches via `worktreeId`. Terminal tabs typically attach; browser tabs don't. Many tabs can share one worktree; one Space can contain tabs across many worktrees. | `Worktree` |
| **AgentSession** | A Claude/Codex conversation thread. **Keyed by cwd path** (Claude stores transcripts at `~/.claude/projects/<encoded-cwd>/<id>.jsonl`). So an AgentSession is implicitly scoped to a Worktree, not to a Tab. A Tab is a *viewer* of the AgentSession that exists in its cwd. | `AgentSession` |
| **TmuxSession** | A server-side tmux session (the persistence handle). Named by tab or worktree. Outlives the app. | `TmuxSession` |
| **Pane** | A split within a tab. Inside a single tab. (Distinct from "tab".) | `Pane` |
| **Window** | Top-level OS window of your app. Usually one per app instance. | `tauri::Window` |

### Worktree invariants (orthogonal to in-app hierarchy)

- A Worktree is a **filesystem entity** — a real `git worktree` on disk. It
  exists regardless of which Profile/Space is active.
- A Worktree belongs to exactly one Project (its parent repo).
- A Tab optionally attaches to a Worktree via `worktreeId`:
  - **Terminal tabs**: usually attach. The worktree.path becomes the cwd.
  - **Browser tabs**: usually null. The web doesn't care about cwd.
  - **Chat tabs**: optional. Attach when the chat is about a specific task.
- **Many tabs can share one Worktree**. They get separate tmux sessions
  (independent shells) but share the cwd and the Claude transcript history.
- **One Space can contain tabs in many Worktrees** (project-Space workflow).
- **One Worktree can be referenced from tabs in many Spaces** (e.g., "Tasks"
  Space and "Watch" Space both have a tab on fix-auth).
- **No profile↔worktree relationship.** You can `cd ~/code/personal/...`
  from a terminal tab in any Profile. Filesystem doesn't see cookies.
- **AgentSession is keyed by cwd.** Two tabs in the same Worktree see the
  same `claude --resume` history. The transcript path is
  `~/.claude/projects/<encoded-cwd>/<id>.jsonl` — encoded from cwd, not
  from any tab/Space/Profile identifier.

### The Persistence Anchor pattern

Tabs are ephemeral. Durable entities are on disk:

```
Ephemeral (recreated on launch)        Durable (outlives app)
─────────────────────────────          ─────────────────────────────────
Tab                                    Profile data dir
Space.activeTabId                         (cookies, localStorage)
Space (purely visual state)            Worktree directory (real git wt)
                                       AgentSession transcript
                                          (~/.claude/projects/<encoded>/...)
                                       TmuxSession (tmux server outlives app)
```

App restart restores Tabs by replaying their references:

1. Load Profiles (from app data dir).
2. Load Spaces (with their profileId).
3. Load Tabs (with spaceId, optional worktreeId, kind, url).
4. For each Tab: recreate the Tauri webview pointing at its URL with its
   Profile's `profile_name`.
5. For terminal tabs: reconnect to the tmux session named after the
   Worktree (or recreate if missing).
6. For chat tabs: load the chat page; it auto-discovers the AgentSession
   transcript by encoded cwd.

The point: the app stores almost no state. The filesystem and tmux server
hold the durable state; tabs are thin pointers.

### Profile invariants (Arc-aligned)

- A Profile owns the cookie jar. All Spaces under one Profile **share logins**.
- A Space belongs to **exactly one** Profile (no cross-profile Spaces).
- Switching Spaces *may* switch Profiles (when the destination Space is on a different Profile).
- Profile switching is rare; Space switching is frequent.
- Default behavior: one hidden "Default" profile is auto-created. UI surfaces the profile concept only when the user creates a second one (Arc-style hide-when-trivial).

### What's per-profile vs per-space vs global

| Concern | Scope | Why |
|---|---|---|
| Cookies / localStorage / IndexedDB | per-profile | identity (Tauri does this) |
| Browser history | per-profile | privacy + relevance |
| Bookmarks | per-profile | identity-scoped |
| Saved passwords | per-profile | identity |
| Autofill / form data | per-profile | identity |
| Search engine default | per-profile (optional) | flexibility |
| Color theme | per-space | visual differentiator |
| Tab list / pinned tabs | per-space | organizational |
| Adblock rules | global | hygiene |
| Theme (light/dark) | global | app-wide |
| Worktree storage location | global | filesystem concern |
| Agent CLI configs (`~/.claude/`) | global | filesystem concern, not browser |
| SSH keys / `gh auth` / `git config` | global | filesystem, outside Tauri |

### Tab types — the unification

| Kind | URL it loads | Backend it talks to | Status |
|---|---|---|---|
| `browser` | `https://...` (external) | the web; cookies isolated per profile | implemented |
| `terminal` | `tauri://localhost/terminal.html` | tmux runtime via tRPC/IPC | placeholder; v0.3 |
| `chat` | `tauri://localhost/chat.html` | agent runtime (hook files / ACP) | placeholder; v0.3 |
| `file` | `tauri://localhost/file.html?path=...&worktree=...` | `file_read`/`file_write`/`file_watch`; CodeMirror | planned §7; v0.4 |
| `diff` | `tauri://localhost/diff.html?worktree=...&base=...` | `git_diff`; CodeMirror merge | planned §7; v0.4 |
| *(plugin-registered)* | `plugin://<plugin-id>/<entry>.html?...` | plugin's own commands | planned §6; v0.5+ |

**Core insight**: in Tauri, the only meaningful difference between tab kinds
is which URL the webview loads. New kinds = new bundled pages + new
Rust commands. The Tab type itself doesn't change.

### Agent vocabulary

| Term | Definition |
|---|---|
| **Agent** | A CLI tool that talks to an LLM (Claude Code, Codex, Gemini, Aider, OpenCode, Amp, Cursor Agent, Copilot, Pi, Droid, Qwen, Goose, Crush) |
| **Status** | The current state of an agent. Canonical values: `idle`, `working`, `waiting`, `error`, `rate-limited`. |
| **Hook file** | A JSONL event Claude/Codex/Gemini write to `~/.claude/hooks/` (or similar) when state changes. Watched via fsnotify. |
| **Transcript** | The full conversation, written by Claude itself to `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`. **Portable across orchestrators.** |
| **Permission request** | Agent UI pause asking the user to authorize an action. Detected via hook file (clean) or pattern match (messy). |
| **Prompt detection** | Pattern-matching ANSI-stripped tmux pane output to infer agent status. Pre-hook-file alternative. |
| **Spinner characters** | Specific Unicode points (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`, `✳✽✶✢`) Claude uses to indicate "working." Reference: claude-squad's `session/tmux/tmux.go`. |
| **Conductor** *(agent-deck)* | An agent whose job is to watch other agents. Optionally bridged to Telegram/Slack. |
| **Watcher** *(agent-deck)* | Event listener (webhook/ntfy/Slack) that wakes a conductor. |

### Architectural roles (the functional decomposition)

Every terminal app answers five questions. Lens for comparing references.

| Role | What it does | Owned by, in this product |
|---|---|---|
| **PTY owner** | `forkpty`s the shell, holds the master fd | tmux server |
| **Multiplexer** | Maps many shells → one UI; tabs/panes | Tauri layer + Zustand sidebar |
| **VT engine** | Parses CSI/OSC → cell grid | xterm.js |
| **Renderer** | Draws cells to pixels | xterm.js + WebGL addon |
| **Persistence** | Outlives the UI process | tmux server (the trick) |

### Infrastructure

| Term | Definition |
|---|---|
| **PTY** | Pseudo-terminal pair (master/slave fds). |
| **tmux** | Terminal multiplexer (server + client). The de-facto persistence layer in this space. |
| **`tmux -CC` (control mode)** | Text protocol where tmux emits `%output` / `%window-add` / `%session-changed`. iTerm2 uses this to embed tmux. |
| **WebView** | Platform-native browser engine. WKWebView (macOS), WebView2/Chromium (Windows), WebKitGTK (Linux). |
| **xterm.js** | TypeScript VT100/xterm emulator. Renders to canvas. The standard frontend terminal library. |
| **libghostty** | Ghostty's terminal engine as a C library (`GhosttyKit.xcframework` on macOS). VT + Metal rendering. Used by Aizen and cmux. |
| **portable-pty** | Rust crate for cross-platform PTY allocation. |
| **node-pty** | Node.js PTY library (VS Code, Hyper, superset.sh). |
| **creack/pty** | Go PTY library (Wave Terminal, agent-deck). |

### Protocols

| Protocol | Definition | Used by |
|---|---|---|
| **ACP** (Agent Client Protocol) | JSON-RPC for editor-to-agent communication | Zed, Claude Code `--acp`, acpx, superset.sh |
| **MCP** (Model Context Protocol) | Standard for agents to consume external context/tools | Anthropic, Cursor, Zed, TUICommander |
| **tmux control mode** | Text protocol over a tmux client's stdin/stdout | iTerm2, agent-deck's controlpipe.go |
| **Tauri IPC** | JSON message passing webview ↔ Rust | every `invoke()` / `emit()` in this codebase |

### UX patterns

| Term | Definition |
|---|---|
| **Sidebar / Vertical tabs** | Arc-style left column listing tabs + workspaces. |
| **Activity bar** | VS Code's leftmost icon strip. |
| **Spaces** *(Arc)* | Color-themed workspace separation; own tab list and cookies. |
| **Glance** *(Arc / Zen)* | Peek at a link/tab in an overlay without committing. |
| **Pinned tab** | Anchored, persists across sessions, not closeable normally. |
| **Preview tab** *(VS Code)* | Non-pinned tab replaced in-place when you click another file. |
| **Command palette** | `Cmd+K` overlay for fuzzy actions/files/URLs. |
| **Status dot** | Colored indicator next to a tab showing agent status. |

### Tauri-specific (this stack)

| Term | Definition |
|---|---|
| **WebviewBuilder** | `WebviewBuilder::new(label, WebviewUrl)`; chained methods configure init scripts, profile, UA. |
| **WebviewUrl::External** | Webview loads a remote URL (browser tab). |
| **WebviewUrl::App** | Webview loads a path from bundled frontend (terminal/chat tabs). |
| **`with_profile_name(id)`** | Per-webview data store isolation — cookies/localStorage scoped to workspace. |
| **`window.add_child(builder, pos, size)`** | The Tauri 2 primitive that creates a tab. Returns a Webview handle. |
| **LogicalPosition / LogicalSize** | DPI-aware coordinates (vs PhysicalPosition). |
| **Initialization script** | JS injected into a webview before page load. |

### Persistence patterns (lessons from references)

| Pattern | Definition |
|---|---|
| **Atomic write** | `write(temp) + rename(temp, target)`. Standard for safe config saves. |
| **Symlink-breaking atomic write** | When target is a symlink, `rename` replaces it with a regular file. (We hit this with TUICommander.) |
| **fd-handoff** *(superset.sh)* | Pass PTY master fds through `stdio` to a successor process via IPC, so sessions survive a daemon-binary swap. |
| **Manifest adoption** *(superset.sh)* | A `manifest.json` records the daemon's PID; on restart, the existing daemon is adopted via `kill(pid, 0)`. |
| **Crash circuit breaker** *(superset.sh)* | Auto-respawn up to N crashes per window, then refuse until user intervention. |
| **Hook fast path** *(agent-deck)* | Prefer hook-file status over pane-scraping when fresh. |

### Mobile / remote

| Term | Definition |
|---|---|
| **Tailscale** | WireGuard-based mesh VPN. Stable `100.x.x.x` tailnet IP + MagicDNS name per device. |
| **MagicDNS** | Tailscale's hostname resolution — devices reachable at `<machine>.tail<id>.ts.net`. |
| **`tailscale serve`** | Tailscale feature that terminates TLS with Let's Encrypt cert tied to your MagicDNS name. |
| **PWA** (Progressive Web App) | Installable mobile web app with offline + push. |
| **LAN sync** *(Bushido)* | mDNS-discovered, encrypted device-to-device sync over local network. |
| **E2E relay** *(TUICommander)* | Encrypted cloud relay alternative when neither LAN nor Tailscale is available. |

### Worktree layouts (storage strategies)

| Strategy | Path pattern | Pros |
|---|---|---|
| **Sibling** | `<parent>/<repo>__wt/<branch>` | Easy to find; `git worktree list` from main repo sees them |
| **AppDir** | `~/Library/Application Support/<app>/worktrees/<repo>/<branch>` | One central place; doesn't pollute |
| **InsideRepo** | `<repo>/.worktrees/<branch>` | Self-contained per repo |
| **ClaudeCodeDefault** | `<repo>/.claude/worktrees/<branch>` | Matches Claude's own convention |

### Status states (canonical taxonomy)

```
idle            agent alive, no recent activity, ready for input
working         actively producing output (streaming, spinner)
waiting         paused for user input (permission, confirm)
error           exited non-zero, crashed, fatal error
rate-limited    paused due to provider rate limit
suspended       (optional) intentionally backgrounded
done            (optional) task completed cleanly
```

Stick to the first five for the data model.

---

## 4. Ambiguity hotspots — be careful

Four terms regularly cause confusion across references. Use the
disambiguated names in code.

### "Profile" vs "Space" vs "Workspace"

This is the most important distinction, and easy to get wrong.

| Concept | Definition | Maps to |
|---|---|---|
| **Profile** | identity (cookies) | `Profile` in our code, `with_profile_name` in Tauri |
| **Space** | organizational (color, tab list) | `Space` in our code (was "Workspace"; renamed for Arc alignment) |
| **Workspace** | the entire app's UI; the whole product | informal English only — DO NOT use as a type name |

Why "Workspace" is informal-only: Arc, VS Code, superset.sh, and our earlier
drafts all used "Workspace" to mean wildly different things (a Space, a
folder, a DB row, a tab). To avoid this, we adopt Arc's terms: **Profile**
(identity) and **Space** (organization).

**In code**: `Profile`, `Space`, `Tab`. Never `Workspace`.

Anti-pattern to avoid: making `Profile` a tag/metadata on tabs. Profile
must be a strict isolation boundary — cookies cannot leak. Cookies belong
to the Profile, NOT to anything else.

### "Session"

| Context | Means |
|---|---|
| tmux | a named container holding windows + panes; outlives clients |
| Claude / Codex / Gemini | a conversation thread; has a UUID; transcript on disk |
| acpx | ACP JSON-RPC handle to an agent subprocess |
| user-facing in this app | a tab in the sidebar (which may map to all three above) |

**In code**: distinguish `TmuxSession`, `AgentSession`, `AcpxSession`, `Tab`.

### "Window"

| Context | Means |
|---|---|
| tmux | a tab in tmux's UI (containing panes) |
| OS / Tauri | a top-level OS window |
| browser | a browser-engine tab |

**In code**: `Window` = OS window. tmux "windows" are mapped to our `Tab`.

---

## 5. Reference projects — one-liner each

All cloned at sibling paths (`../<repo>/`).

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

Detailed file-by-file pointers in `README.md` → "References by subsystem".

---

## 6. Plugin architecture (planned)

> **Status**: planned, not yet implemented. Phased rollout starting at v0.3.

### The chosen archetype

A **hybrid in-webview JS + Rust capability gates** model, modeled directly on
TUICommander's plugin system (which has the most thoughtful design among the
references we surveyed).

```
webview (in-process JS)              Rust backend (capability gates)
─────────────────────────            ───────────────────────────────
plugin/main.js                       
   │                                 
   │ host.registerOutputWatcher(...) ◄── handled in JS, no Rust call
   │ host.addItem(...)
   │
   │ host.fs.read("...")           ───►  plugin_fs.rs:
   │                                     manifest declares "fs:read"?
   │                                     path inside sandbox?
   │
   │ host.http.fetch("https://...")  ───►  plugin_http.rs:
   │                                       matches manifest.allowedUrls?
   │
   │ host.exec.run(["rtk",...])     ───►  plugin_exec.rs:
   │                                       binary in manifest.binaries?
   │
   │ host.pty.read(sessionId)       ───►  plugin_pty.rs: "pty:read"?
```

**Why this hybrid**: same-process JS gives Phase-1 productivity (hot reload,
trivial debugging, full API access for UI extension). Manifest-declared
capabilities checked in Rust make the dangerous operations (FS, network,
exec, PTY) safe — the manifest on disk is the source of truth, and the
frontend cannot lie about it.

The four archetypes we rejected and why:

- **VS Code separate-process**: too heavy for our scale; defer the IPC cost until we have a public marketplace with untrusted plugins.
- **Pure WASM (Zed)**: narrow API ceiling — Zed has had an open RFC for custom UI panels for 18+ months and the ecosystem can't grow past languages/themes.
- **In-process without capability gates (Obsidian/Hyper/Sublime)**: works but every plugin can do anything; we want a clearer trust boundary for v1.
- **Config-only (tmux)**: too limited for tab kinds / agent integrations / output watchers.

### Phased rollout

```
v0.1  no plugin system — themes / agents / keybindings loaded from JSON config files in ~/.config/<app>/
v0.3  Phase 1 plugins: in-webview JS + Rust capability gates (this spec)
v0.6  community registry — "Browse plugins" UI, signed updates from a curated index
v∞    Phase 2 WASM — optional sandboxed runtime for untrusted plugins;
      first-party / trusted plugins remain JS
```

Don't build Phase 1 until v0.3 — the core (terminal tabs, worktrees, mobile
bridge) must ship first. Plugins extend a working app; they don't substitute
for one.

### Capability tiers

```
Tier 0  always-on             logging
Tier 1  always-on             commands (Cmd+K palette)
                              sidebar widgets
                              tab decorations (status dots, badges)
                              status bar segments
                              output watchers (regex on PTY output)
                              event subscribers (tab/agent/worktree events)
                              theme contributions
                              read-only state queries (tabs, spaces, profiles…)
                              notify (toast)
Tier 2  always-on             register new TabKind  (e.g., "kanban", "music")
                              per-plugin sandboxed KV storage
Tier 3  manifest-declared     fs:read / fs:list / fs:watch
                              pty:read
Tier 4  manifest-declared     net:http  (URL allowlist via manifest.allowedUrls)
                              exec:cli  (binary allowlist via manifest.binaries)
                              worktree:create / worktree:remove
                              spawn:agent  (declared agent types)
```

Tier 3 and Tier 4 each correspond to a Rust file (`plugin_fs.rs`,
`plugin_pty.rs`, `plugin_http.rs`, `plugin_exec.rs`, `plugin_worktree.rs`,
`plugin_agent.rs`) that enforces the manifest's declared scope. JS cannot
bypass these — Rust holds the gate.

### Extension surface — what plugins actually do

The four "very high value" extension points (what plugins should be best at):

1. **Register new TabKinds** — beyond browser/terminal/chat. A "kanban" plugin
   adds `kind: "kanban"` with a bundled HTML page; the rest of the app
   treats it like any other tab.
2. **Agent integrations** — new agent CLIs, ACP adapters, slash commands,
   per-agent status patterns.
3. **Output watchers** — regex against PTY output → custom actions (the
   pattern that powers Claude Squad's status detection, TUICommander's
   activity center).
4. **Worktree hooks** — pre/post-create, pre/post-finish handlers (env
   file copying, dependency installation, branch labeling).

Other capabilities (commands, decorations, themes, notifications, settings
panels) are tablestakes and should ship in Tier 1.

### Manifest format

```jsonc
// ~/.config/<your-app>/plugins/<plugin-id>/manifest.json
{
  "id": "@you/cool-plugin",        // must match the directory name
  "name": "Cool Plugin",
  "version": "1.0.0",
  "minAppVersion": "0.3.0",
  "main": "main.js",                // ES module entry point
  "description": "Adds a kanban tab and watches for TODO comments.",
  "author": "you",

  // Tier 3/4 capabilities — must be declared explicitly
  "capabilities": ["fs:read", "pty:read", "net:http"],
  "allowedUrls": ["https://api.linear.app/*"],   // required if net:http
  "binaries":   ["rtk", "mdkb"],                  // required if exec:cli
  "agentTypes": ["claude", "codex"],              // scope plugin to certain agents

  "contributes": {
    "tabKinds":   [{ "id": "kanban", "label": "Kanban",
                     "entry": "kanban.html", "icon": "..." }],
    "commands":   [{ "id": "kanban.new", "title": "Kanban: New board" }],
    "themes":     [{ "id": "cyberpunk", "path": "themes/cyberpunk.json" }]
  }
}
```

All manifest fields use **camelCase** (matches Rust serde defaults).

### Plugin interface

```typescript
// What every plugin's main.js exports
export default {
  id: "plugin-id",
  onload(host: PluginHost): void { /* register your contributions */ },
  onunload(): void { /* optional cleanup; auto-disposers handle the rest */ },
};
```

### PluginHost API (the surface plugins can use)

```typescript
interface PluginHost {
  // Tier 0
  log(level: "debug"|"info"|"warn"|"error", msg: string, data?: unknown): void;

  // Tier 1 — UI extension
  registerCommand(cmd: Command): Disposable;
  registerSidebarWidget(widget: SidebarWidget): Disposable;
  registerTabDecoration(decorator: TabDecorator): Disposable;
  registerStatusBarItem(item: StatusItem): Disposable;
  registerOutputWatcher(w: { pattern: RegExp; onMatch(m, ctx): void }): Disposable;
  registerThemeContribution(theme: Theme): Disposable;
  notify(toast: { title: string; level: "info"|"warn"|"error" }): void;

  // Tier 1 — read-only state queries
  state: {
    activeProfile(): Profile;
    activeSpace(): Space;
    tabs(filter?: TabFilter): Tab[];
    worktrees(filter?: WorktreeFilter): Worktree[];
    agentSessions(): AgentSession[];
  };

  // Tier 1 — events (cleanup auto-handled by the registry on unload)
  on(event:
      | "tab:created"  | "tab:closed"  | "tab:focused"
      | "agent:status-changed" | "agent:permission-request"
      | "worktree:created"     | "worktree:finished"
      | "profile:switched"     | "space:switched",
    handler: (payload) => void
  ): Disposable;

  // Tier 1 — actions
  tabs: {
    create(req: CreateTabRequest): Promise<Tab>;
    close(id: string): Promise<void>;
    focus(id: string): Promise<void>;
  };
  spaces: { switch(id): Promise<void>; create(...): Promise<Space>; };

  // Tier 2 — per-plugin sandboxed KV storage
  storage: {
    get(key: string): Promise<unknown>;
    set(key: string, value: unknown): Promise<void>;
    delete(key: string): Promise<void>;
  };

  // Tier 3/4 — privileged; only present if declared in manifest.capabilities
  fs?:       { read; list; watch; };
  pty?:      { read(sessionId): Promise<string>; };
  http?:     { fetch(url, init): Promise<Response>; };
  exec?:     { run(binary, args): Promise<{stdout, stderr, code}>; };
  worktree?: { create(...); remove(...); };
  agent?:    { spawn(type, cwd, prompt): Promise<AgentSession>; };
}
```

### Lifecycle + crash safety (TUICommander's pattern)

```
1. Discovery     Rust scans ~/.config/<app>/plugins/<id>/manifest.json
2. Validation    manifest schema + minAppVersion + capability declarations
3. Import        await import("plugin://<id>/main.js") via custom URI scheme
4. Module check  default export has id, onload, onunload
5. Register      pluginRegistry.register(plugin) → plugin.onload(host)
6. Active        receives events, output, structured events
7. Hot reload    file watcher → unregister + re-import
8. Unload        plugin.onunload() → auto-dispose all registrations
```

Every boundary is wrapped in try/catch. A broken plugin logs to its own
ring buffer, gets a red error badge in Settings → Plugins, and is skipped.
The app continues. **Plugin failures must never crash the app.**

### Distribution

Phase 1: **filesystem-only**. Users drop directories into
`~/.config/<app>/plugins/<id>/`. Power-user shape; intentional friction
keeps the early ecosystem trusted.

Phase 2 (~v0.6): a **community registry**. Pattern options:
- a GitHub repo with `plugins.json` index (Zed/TPM style — simplest)
- a hosted `plugins.<app>.com` site with search + reviews (Obsidian/VS Code style — bigger lift)

Settings → Plugins → "Browse" reads the registry and installs to the same
filesystem path. Auto-update via signed manifests.

### What plugins do NOT get

These are deliberate non-features in Phase 1:

- **No direct DOM access** to the main app's webview. Plugins can render
  inside their own sidebar widgets / tab kinds / settings panels — but
  can't reach into the main React tree.
- **No access to other plugins' storage.** Each plugin has its own KV
  store keyed by plugin id.
- **No raw access to other plugins' webviews.** Inter-plugin
  communication must go through registered events.
- **No `eval`-style escape hatches** like "registerRustFunction" — every
  privileged op must be a typed Rust command behind a capability.

### References to study (specific files in cloned repos)

The plugin system is essentially a port of TUICommander's, with tweaks for
our domain model:

| File | What you'll learn |
|---|---|
| `../tuicommander/docs/plugins.md` | the user-facing authoring guide — read first, it's the spec |
| `../tuicommander/src-tauri/src/plugins.rs` | discovery, validation, `plugin://` URI protocol, hot reload, capability check |
| `../tuicommander/src-tauri/src/plugin_fs.rs` | sandboxed filesystem capability |
| `../tuicommander/src-tauri/src/plugin_pty.rs` | scoped PTY read |
| `../tuicommander/src-tauri/src/plugin_http.rs` | URL-allowlisted HTTP w/ SSRF protection |
| `../tuicommander/src-tauri/src/plugin_exec.rs` | binary-allowlisted CLI exec |
| `../tuicommander/examples/plugins/hello-world/` | the simplest possible plugin (Tier 1 only) |
| `../tuicommander/examples/plugins/claude-status/` | agent-scoped plugin pattern |
| `../tuicommander/src/stores/keybindings.ts:140` | how dynamic actions appear in Keyboard Shortcuts UI |
| `../waveterm/schema/widgets.json` | alternative declarative model — widgets as JSON, no JS at all |

The pattern-recognition shortcut: when in doubt, do what TUICommander
does. Their plugin system is the closest existing thing to what we want.

## 7. File editor capabilities (planned)

> **Status**: planned, not yet implemented. Two new TabKinds + a few Rust
> commands; ships in v0.4 alongside the plugin system.

### Scope decision: levels 1+2+5, not 3-4

"File editor" bundles five distinct products. The scope must be clear:

```
                Read-only    Light edit    Code intel    Full IDE
                ─────────    ──────────    ──────────    ────────
1. File viewer       ✓
2. Quick editor      ✓           ✓
3. Code editor       ✓           ✓           ✓
4. IDE               ✓           ✓           ✓             ✓
5. Diff viewer       ✓                                       
```

**We ship levels 1, 2, and 5. We delegate 3-4 to the user's external IDE.**

Why: every Arc-shaped agent orchestrator (Aizen, TUICommander, superset.sh,
Wave) follows this pattern. Real code editing is months of LSP/treesitter/
debugger work that doesn't differentiate an agent orchestrator from Cursor.
Users have strong editor preferences; the "Open in IDE" pill is the
canonical solution.

### The two new TabKinds

```ts
type TabKind = "browser" | "terminal" | "chat" | "file" | "diff";
```

**`file` tab** — view + light edit of a single file:
```
url:  tauri://localhost/file.html?path=<abs-path>&worktree=<id>
worktreeId: optional (gives file git context)
```

**`diff` tab** — side-by-side diff for a worktree's branch vs base:
```
url:  tauri://localhost/diff.html?worktree=<id>&base=main
worktreeId: required (diff is always worktree-anchored)
```

One tab per file (Arc model), not one editor area hosting many tabs (VS
Code model). Matches the existing one-webview-per-tab pattern. Lots-of-tabs
problem is mitigated by Space grouping.

### Library: CodeMirror 6, not Monaco

| | CodeMirror 6 | Monaco |
|---|---|---|
| Core size | ~50KB | ~10MB |
| Language packs | lazy, 10-30KB each | bundled always |
| Diff support | `@codemirror/merge` | built-in |
| Mobile-friendly | yes | no |
| Used by | Obsidian, Sourcegraph, Jupyter | VS Code, GitHub web, Wave Terminal |

For "view + light edit + diff," CodeMirror wins on every axis. Monaco
would only be right if we were building level 3-4 (LSP + completion +
go-to-def). We aren't.

Obsidian uses CodeMirror; Obsidian is the closest mental-model match
to our product. Adopt their choice.

### The agent ↔ editor ↔ file triangle

Most editors solve "user edits file." Our editor solves
"user **and** agents both edit the same file."

```
                     Agent (in tmux pane)
                       │
              writes   │   reads
                       ▼
                     File ◄──── reads ──── Editor (in webview tab)
                       ▲                     │
                       └──── writes ─────────┘
```

Requirements unique to this triangle:

1. **File watcher non-negotiable**. Agent writes → editor refreshes.
   Use `notify` crate; emit `file:changed`; reload editor buffer.
2. **Optimistic concurrency on save**. Editor stores mtime at read.
   On save: re-stat; if mtime moved, prompt user (overwrite? merge?
   reload?). `file_write(path, content, expected_mtime)` enforces this.
3. **Audit trail of file edits**. Each write logs source: `user`,
   `agent:<type>:<session>`, `external`. Powers future "who wrote this line?"
4. **Cross-boundary undo** *(advanced, defer)*. Editor's Cmd+Z can undo
   recent agent edits if a change journal is kept.
5. **Diff-before-write for agents** *(advanced, defer)*. Cursor-style
   "review each agent edit before commit." Big feature; out of scope v1.

For v1, ship items 1 and 2. That's 80% of the safety with 20% of the work.

### Rust commands

```rust
// src-tauri/src/files.rs (new module)
#[tauri::command] fn file_read(path: String) -> Result<FileContents, String>;
#[tauri::command] fn file_write(
    path: String,
    content: String,
    expected_mtime: i64,            // ← optimistic concurrency
) -> Result<i64, String>;            // returns new mtime
#[tauri::command] fn file_watch(path: String) -> Result<(), String>;
                                     // emits "file:changed" events

#[tauri::command] fn git_diff(
    worktree: String,
    base: String,
) -> Result<DiffResult, String>;     // git diff <base>...HEAD
```

`FileContents` = `{ content, mtime, encoding }`. `DiffResult` =
`{ files: [{path, hunks: [...]}] }` — a structured diff the diff page can
render.

### Bundled pages

```
public/
  file.html       CodeMirror 6 editor, reads ?path&worktree
  diff.html       CodeMirror merge view, reads ?worktree&base
```

Each page mounts CodeMirror, subscribes to backend events for live
updates, calls the appropriate Tauri commands.

### File-tree sidebar widget (v1.1)

For browsing files in the active worktree:

```
Sidebar:
  [profile pills]
  [space pills]
  + Browser  + Terminal  + Chat  + File
  
  ▾ tour (main)
     ▸ src/
     ▸ public/
       package.json
  
  ─── tabs ───
  …
```

Click file → open file tab (or focus existing).

Architecturally a sidebar widget. Ships as core but built as if it were a
plugin — so it can become a plugin example later. The first canonical
"plugin pattern" for the registry.

### Unsaved buffer recovery

Files on disk are durable; editor buffers are not. If the app crashes with
unsaved edits, we recover from a per-tab journal:

```
~/.<app>/recovery/<tab-id>.json
   { path, content, baseMtime, lastEditedAt }
```

Auto-saved every 2s while a buffer is dirty. Cleared on save or discard.
On launch: each file tab checks for a matching recovery file; if found
and newer than the file's mtime, offer "restore unsaved changes."

### Plugin extension points (Phase 1+)

Once the plugin system ships (§6), file editing exposes:

```typescript
host.registerFileKind({
  id: "csv",
  extensions: [".csv", ".tsv"],
  entry: "csv-viewer.html",
  icon: "...",
});

host.registerEditorCommand({
  id: "format",
  title: "Format Document",
  shortcut: "Cmd+Shift+F",
  run: ({ path, content, save }) => { /* format + save */ },
});

host.on("file:changed", ({ path, source }) => {
  // source: "user" | "external" | "agent:claude:<session>"
});
```

Enables plugins to add:
- Image / video viewers (`.png`, `.mp4` → custom view)
- Notebook viewer (`.ipynb`)
- Spreadsheet viewer (`.csv`)
- Format-on-save (prettier, black, gofmt)
- Vim / emacs modes (`@replit/codemirror-vim`)
- Per-language LSP bridges *(eventually)*

### What's deliberately NOT in v1

Each of these is a real feature we can defer:

| Feature | Why defer |
|---|---|
| **LSP / completion / go-to-def** | Months of work. External IDE handles. |
| **Multi-cursor advanced editing** | Review use case doesn't need it. |
| **Vim/emacs modes** | Ship as plugins. |
| **Find across files** | External IDE. |
| **Refactoring (rename, extract)** | LSP-dependent. |
| **Inline AI edit-suggest (Cursor)** | Agents are external (tmux). Don't compete. |
| **Notebook editing** | Plugin territory. |
| **Image / video preview** | Plugin territory. |
| **Project-wide formatter** | Plugin or external command. |
| **Settings.json-style schema validation** | Plugin territory. |

Discipline: if it requires LSP, defer. If it requires real code
intelligence, defer. If it's view + light edit + diff, ship.

### How this maps to the Persistence Anchor

Same pattern as terminal tabs:

```
Ephemeral (recreated on launch)        Durable
─────────────────────────────          ─────────────────────────────
Tab (kind: file | diff)                File on disk
Editor's CodeMirror state              Git history (for diffs)
Open file path                         Unsaved buffer recovery
                                          (~/.<app>/recovery/<tab-id>.json)
```

App restart: file/diff tabs replay their `url` (encodes path + worktree),
editor reopens, reads file fresh. Unsaved edits restore from recovery.

**Files are the durable layer. Editors are pure views.** Closing a file
tab and reopening it should be lossless.

### Implementation order (when you tackle v0.4)

```
1. (½ day) New TabKind "file"; types.ts + sanctel tabStore
2. (1 day) Rust: file_read / file_write / file_watch with optimistic concurrency
3. (1 day) public/file.html with CodeMirror 6 + basic syntax highlight
4. (½ day) Sidebar: + File button + native file picker dialog
5. (½ day) Dirty indicator, save UI, file:changed event handling
6. (1 day) Rust: git_diff using libgit2 or shelling to git
7. (1 day) public/diff.html with @codemirror/merge view
8. (1 day) Sidebar: + Diff button (creates diff tab for active worktree)
9. (1 day) Unsaved buffer recovery (auto-save + restore on launch)
10. (½ day) File-tree sidebar widget (basic; v1.1 polish)
```

Total: ~8 days of focused work for v1.0 file editing. Defer item 10 to
v1.1 if you want to ship faster.

### References to study

| File | What you'll learn |
|---|---|
| `../waveterm/frontend/app/view/preview/` | embedding Monaco in a Tauri-like webview (worth studying even though we use CodeMirror — same plumbing pattern) |
| `../waveterm/frontend/app/monaco/` | their Monaco wrapper components |
| `../aizen/aizen/Features/Files/` | native Swift file browser + editor; different architecture but same UX problem |
| `../tuicommander/src-tauri/src/plugin_fs.rs` | sandboxed filesystem patterns for the file_read/write/watch commands |
| `../min/js/findinpage.js` | classic find-in-page pattern (for CodeMirror in-file search) |
| Obsidian source (not cloned; on GitHub) | the canonical "CodeMirror in a workspace app" reference — Obsidian *is* what we're building, minus agents |
| CodeMirror 6 docs (codemirror.net/docs/) | the API |
| `@codemirror/merge` package | diff view |

## 8. Agent ↔ browser integration (planned)

> **Status**: planned, not yet implemented. Ships in v0.6–v0.8, after the
> plugin system (§6). Architecture is settled; implementation deferred.

### The decision: Architecture B (agent drives the user's tabs in-place)

Four candidate architectures, with the chosen one in bold:

```
A.  Agent has its own headless browser    (Playwright + bundled Chromium)
B.  Agent drives the USER's tabs ★ chosen (in-place WebView APIs)
C.  Hybrid — shadow tabs for agent work   (extra complexity, defer)
D.  Computer Use vision loop              (slow, expensive; v2 fallback)
```

Why B and not A: the obvious answer ("just use Playwright") doesn't work
for our stack. Playwright drives Chromium / Firefox / its-own-WebKit-build
in separate processes, not the WKWebView / WebView2 / WebKitGTK webviews
inside a Tauri app. Going with Playwright means **shipping a second
browser** alongside the user's — 300MB bundle, separate session, no Profile
sharing, user can't watch live.

Architecture B gives us Profile-aware automation for free (the webview
already has the right cookies), real-time observability (user watches the
cursor, navigations, scrolling), and a small bundle.

### The protocol: MCP

We don't invent a protocol. The agent ↔ browser bridge speaks MCP (Model
Context Protocol). Already supported by Claude Desktop, Cursor, Cline, Zed,
and most modern agent CLIs.

```
Agent (Claude / Cursor / etc.)
   │
   │  tools/list, tools/call   (MCP over stdio or socket)
   ▼
Rust MCP server in src-tauri/src/mcp/browser.rs
   │
   │  Tauri commands
   ▼
Per-platform WebView APIs
   ├─ macOS:    WKWebView.evaluateJavaScript(...)
   ├─ Windows:  WebView2.ExecuteScriptAsync(...)
   └─ Linux:    webkit_web_view_run_javascript_async(...)
   │
   ▼
The user's tab (same webview the user sees)
```

### Tool inventory (the MCP surface)

Eleven tools, grouped by capability tier:

```
Tier tab:read           tabs.list                  → [{ id, url, title, spaceId, profileId }]
                        tabs.read(id)              → { url, title, visible_text, html }
                        tabs.screenshot(id)        → PNG bytes

Tier tab:control        tabs.navigate(id, url)
                        tabs.eval(id, code)        → return value
                        tabs.click(id, selector)
                        tabs.type(id, selector, text)
                        tabs.scroll(id, ...)
                        tabs.wait_for(id, condition)
                        tabs.focus(id)

Tier tab:create         tabs.open(profileId?, url) → id
                        tabs.close(id)
```

Each MCP tool is a thin Rust function that dispatches to the right platform
WebView API. ~500-800 LOC total for the bridge.

### Per-platform glue (Rust modules)

```
src-tauri/src/browser_control/
   ├── mod.rs                  // dispatch + shared types
   ├── browser_control_mac.rs  // WKWebView via objc/cocoa
   ├── browser_control_win.rs  // WebView2 via webview2-com
   └── browser_control_linux.rs // WebKitGTK via gtk-rs
```

The mod.rs exposes a unified API; platform files implement it. Same pattern
Tauri itself uses internally.

### Capability tiers (manifest-declared)

A plugin or built-in caller must declare the tiers it uses in
`manifest.json` (matches §6's capability system):

| Capability | What's possible | Risk |
|---|---|---|
| `tab:read` | observe URL/title/text/screenshot | low — read-only |
| `tab:control` | navigate, click, type, eval JS | high — can do anything on logged-in sites |
| `tab:create` | spawn or close tabs | medium — can flood the user |

Plus optional `allowedDomains: ["github.com/*", ...]` in the manifest to
restrict which URLs a plugin can drive — a Tier-3-style allowlist mirroring
`net:http`'s `allowedUrls`.

### Profile-inheritance invariant

```
Tab in Profile "Work"
   → agent navigates the tab to github.com
   → agent sees the Work GitHub login (Work profile's cookies)
   → NOT the Personal profile's login
```

This is **automatic**: the agent drives the existing webview, which already
has the correct `with_profile_name` from §3's Profile invariants. The agent
cannot cross profiles by accident.

A hypothetical `tab:cross-profile` capability would let plugins move tabs
between Profiles. Treat this as a red flag — almost never needed; explicit
opt-in only.

### Trust + visibility UX (mandatory in v1)

Without these, agent-controlled browser tabs feel like spyware. Required
from v0.7:

1. **Visual indicator on agent-driven tabs** — small "agent: claude" badge in
   the tab title; dimmed background tint; optional live cursor overlay.
2. **One-click pause** — user freezes agent control; subsequent agent calls
   return an "agent paused" error.
3. **Action log per tab** — every agent action (navigate, click, eval) is
   recorded with timestamp. Viewable in a tab side panel.
4. **Auto-pause on user input** — if user clicks/types in a tab the agent
   is driving, the agent yields. Resume requires user action.
5. **Approval gates for destructive actions** — patterns like clicking
   "Delete", "Confirm", "Send", or URLs matching destructive heuristics
   require user approval. Configurable per plugin via manifest.

### Implementation order (v0.6 → v0.8)

```
v0.6  (3 days)   MCP server sanctel + read-only tools
                 tabs.list, tabs.read, tabs.screenshot
                 — useful immediately for "agent looks at what I'm
                 viewing" workflows; minimal trust risk

v0.7  (~1 week)  Write tools per platform
                 tabs.navigate, tabs.eval, tabs.click, tabs.type,
                 tabs.scroll, tabs.wait_for, tabs.focus
                 — each platform's evaluateJavaScript / 
                 ExecuteScriptAsync / run_javascript_async wired

v0.8  (~1 week)  Trust + visibility UX
                 indicator, pause button, action log,
                 user-input auto-pause, destructive-action gates

v1.0+ (later)    tabs.open / tabs.close for new-tab control;
                 cross-tab orchestration; opt-in headless tabs
```

### Architecture as a plugin, not core

Per §6, agent-browser integration is delivered as a **plugin**, not core.
The sanctel ships the underlying browser-control Tauri commands and the
capability gates; an `browser-agent-bridge` plugin spawns an MCP server and
exposes the tools to MCP-aware clients.

This means:
- Plugins compete (Cursor-flavored, Claude-Desktop-flavored, custom)
- You don't carry the agent-integration maintenance in core
- Users explicitly opt in by installing a bridge plugin
- New agent UIs ship by writing new bridge plugins, not by patching core

### What's NOT in v1

| Feature | Defer to | Why |
|---|---|---|
| Anthropic Computer Use (vision loop) | v2+ | slow + token-heavy; rarely needed if DOM access works |
| Cross-browser fingerprint spoofing | never | not our problem |
| Recording macros / replay | v2+ | nice but big work |
| Headless browser pool | only on demand | bundle cost; most workflows don't need it |
| Multi-tab parallel orchestration | v2+ | sequence first, parallel later |
| AI-decided element targeting (no selectors) | v2+ | Stagehand pattern; needs more reasoning |
| Form filling from credential vault | v2+ | sensitive; needs Profile-aware vault first |
| OAuth / SSO flows mediated by agent | never | leave to user; security minefield |

### How this composes with worktrees

Browser tabs in Spaces tied to a Profile don't have `worktreeId` (browser
tabs are URL-shaped, not filesystem-shaped). But agent workflows often span
both:

```
Space "Maze: fix-auth" (Profile "Work")
   ├── Terminal tab    cwd=worktree/fix-auth, running claude
   ├── Browser tab     linear.app/issue/123  (Work cookies)
   └── Browser tab     github.com/maze/pr/456  (Work cookies)

A "browser-agent-bridge" plugin could expose to claude (running in the
terminal tab) MCP tools to read/navigate the two browser tabs. Claude can
say "check the linear issue, then read the PR comments, then update the
PR title to match the issue title" — all driving the user's actual tabs,
respecting Profile cookies.
```

This is the **killer workflow** the architecture unlocks. The agent has
shared state with the human: same logins, same view, same context.

### What we reuse vs build

```
Reuse (don't build):
   • MCP protocol                  (already a standard)
   • WKWebView.evaluateJavaScript  (in macOS)
   • WebView2.ExecuteScriptAsync   (in Windows Edge runtime)
   • webkit_web_view_run_javascript (in WebKitGTK)
   • TUICommander's MCP server bootstrap pattern (mcp_http/, desktop-mcp/)
   • Microsoft Playwright MCP — for tool-name conventions (apache-2.0)
   • Stagehand's act/extract/observe naming (apache-2.0)

Build (must write):
   • Per-platform browser_control_*.rs glue        (~300-500 LOC each)
   • Rust MCP server registering browser tools     (~300 LOC)
   • Trust UX (indicator, pause, log, gates)       (~500 LOC frontend)
   • Bridge plugin (the one users install)         (~200 LOC; ships as example)

Don't reuse:
   • Playwright (drives separate browsers, not in-place webviews)
   • Browser-Use, Stagehand (Python or Playwright-bound)
   • Browserbase (cloud-only)
```

### References to study

| File / project | What you'll learn |
|---|---|
| `../tuicommander/src-tauri/src/mcp_http/` | how Tauri exposes MCP tools |
| `../tuicommander/packages/desktop-mcp/` | the MCP-server-as-sidecar pattern |
| Microsoft Playwright MCP ([github.com/microsoft/playwright-mcp](https://github.com/microsoft/playwright-mcp)) | tool naming conventions; argument shapes |
| Browser-Use ([github.com/browser-use/browser-use](https://github.com/browser-use/browser-use)) | high-level agent API design (what Claude "thinks in") |
| Stagehand ([github.com/browserbase/stagehand](https://github.com/browserbase/stagehand)) | act/extract/observe API ergonomics |
| Anthropic Computer Use docs | the vision-loop fallback pattern |
| VS Code's integrated browser ([github.com/microsoft/vscode](https://github.com/microsoft/vscode), `src/vs/workbench/contrib/webview/`) | how an open-source editor lets agents inspect webview content |
| Cursor (closed) | observe the UX: badges, pause, action history — adopt the patterns |
| Dia by Browser Company (closed) | observe the agent-driven browser-tab UX |

The pattern-recognition shortcut: TUICommander's MCP plumbing is the
closest local reference for "Tauri app exposes MCP tools that drive
internal state." Their browser-tab story is empty, but the MCP scaffolding
is reusable.

### Extension support stance (decision; permanent unless overruled)

> **Decision: no Chrome extension runtime in v1.** Browser tabs use the OS
> WebView (WKWebView / WebView2 / WebKitGTK); none of these run Chrome
> extensions. Implementing a Chrome extension runtime outside Chromium is
> a multi-person-year project.

**What that means concretely:**
- **No uBlock / Grammarly / 1Password browser extension / etc.** inside our browser tabs.
- The 50% of high-value extension behavior we *do* care about is delivered as **plugins** (per §6), implemented via JS injection à la Bushido's pattern:
  - `adblock` plugin — EasyList rules + injected content script
  - `vault` plugin — system keychain + injected autofill (Bushido's pattern)
  - `userscripts` plugin — Tampermonkey-equivalent
  - `reader-mode` plugin — Readability.js injection
  - `vim-nav` plugin — Vimium-style keyboard nav
- **"Open in real browser" escape hatch.** Right-click on any tab → "Open URL in default browser" → spawns Chrome/Arc/Safari/etc. Users who must have Grammarly or a vendor-specific extension use their real browser for those tasks.

**Rejected alternatives (all carry real costs):**

| Path | Why we passed |
|---|---|
| **CEF (Chromium Embedded Framework)** | +150-200 MB bundle; weeks of Tauri↔CEF integration; ongoing Chromium-version-bump work; loses Tauri's "small native app" thesis |
| **Move to Electron** | Rewrite off Tauri; 100-150 MB bundle; slower startup; ~3-10x RAM; abandons stack discipline |
| **Fork Chromium** | Team-scale engineering (Arc/Brave/Vivaldi tier); not solo-doable; you become a browser company |
| **Implement extension runtime ourselves** | Many person-years; nobody outside Google has done it; MV3 service workers are essentially impossible to host outside Chrome |
| **No extension-equivalent features at all** | Lose adblock / autofill / Vimium — features users genuinely benefit from in a workspace app |

**Future escape hatch:** if user data later shows extension support is critical, ship a `chromium-tabs` plugin that bundles CEF as an optional component, registers a `chromium-browser` TabKind, and uses CEF webviews instead of OS WebViews. Power users opt-in with the bundle cost; default users keep the small binary. **This is a v3+ concern; do not pre-build.**

**Rationale in one line:** agent orchestration is the differentiator, not browser power-user features. We pay the small loss of "no Chrome extensions" to keep the 30 MB binary, the cross-platform parity, and the elegant Tauri integration.

## 9. Working principles (carried over from the project root)

From the parent repo's `CLAUDE.md`:

- **Think before coding** — surface tradeoffs; don't pick silently.
- **Simplicity first** — minimum code that solves the problem; no speculative
  features or abstractions.
- **Surgical changes** — touch only what you must; match existing style.
- **Goal-driven execution** — define success criteria; loop until verified.
- **Bias toward concrete + visual** — code sketches, diagrams, tables.
- **Co-locate what changes together.** Domain names over implementation names.
- **Test observable behavior**, not implementation details.

---

## 10. Where to start, on a new session

1. Read this file top-to-bottom.
2. Read `README.md` → "Reading priority" section (the 4-hour curriculum).
3. Run the sanctel: `cd sanctel && npm install && npm run tauri dev`.
4. Pick one extension from `README.md` → "How to extend, in order".
5. Build it; ship; iterate.
