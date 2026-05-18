# Terminal runtime (design)

> **Status**: planned, not yet implemented. Ships in v0.3. When the code
> lands in `src-tauri/src/terminal/` and `src/terminal/`, this document
> moves to `src-tauri/src/terminal/DESIGN.md` and a glossary is extracted
> to `src/terminal/CONTEXT.md`.
>
> Anchor decisions:
> - [ADR-0002](../adr/0002-terminal-architecture.md) — tmux + xterm.js + portable-pty.
> - [ADR-0004](../adr/0004-persistence-anchor-pattern.md) — durable state lives outside sanctel.
> - [ADR-0006](../adr/0006-tabkind-unification.md) — every TabKind is a webview loading a URL.

## Scope

The Terminal runtime is what gives sanctel its `terminal` and `chat` TabKinds:
a Tauri webview running xterm.js, attached to a long-lived tmux window
hosted by the user's tmux server. The same machinery underlies chat tabs —
those are terminals whose first command is `claude` / `codex` / `gemini`
(possibly with `--resume`).

The runtime is responsible for: (1) creating tmux sessions/windows on
demand, (2) wiring a webview's xterm.js to a PTY whose other end attaches
to the tmux window, (3) cleaning up windows when tabs close, and (4)
reattaching on launch from durable Tab records.

## Isolation from the user's tmux

Every sanctel tmux invocation uses a **dedicated socket** and **no user
config**:

```
tmux -L sanctel -f /dev/null <command...>
```

- `-L sanctel` puts sanctel's tmux server on its own socket
  (`/tmp/tmux-<uid>/sanctel`). Completely isolated from the user's
  default tmux server.
- `-f /dev/null` ignores the user's `~/.tmux.conf`. Sanctel manages its
  own terminal environment; the user's customizations apply to their
  own tmux, not ours.
- A user can attach to a sanctel session from their own shell with
  `tmux -L sanctel attach -t sanctel_wt_<wt>__term-1` — supported,
  undocumented power-user affordance. `tmux -L sanctel ls | grep
  sanctel_wt_<wt>__` lists every tab in that Worktree.
- `tmux kill-server` in the user's terminal only kills their server;
  sanctel's is untouched. Symmetric.

For brevity, the rest of this doc abbreviates `tmux -L sanctel -f /dev/null`
as just `tmux`.

If we later ship sanctel-flavored tmux defaults (`history-limit`,
`mouse on`, etc.), they go in `app-bundle/sanctel.tmux.conf` and replace
`-f /dev/null` with `-f <bundled-conf-path>`.

## Distribution: bundled vs system tmux

**v0.3:** tmux is a documented prerequisite. The `tmux -V` startup probe
shows a setup screen ("Install tmux to use sanctel") if missing.

**v0.4+ (planned):** bundle a tmux binary in the app (Tauri
`bundle.resources`). Sanctel invokes the bundled binary by absolute
path, never the user's PATH. Eliminates the prerequisite entirely.

Why phase it: bundling is a build-pipeline problem (cross-compile or
vendor binaries for macOS x86_64/arm64 + Linux x86_64/arm64) orthogonal
to the architecture. Solving both at once doubles the v0.3 risk. The
`-L sanctel` isolation works identically with bundled or system tmux —
adopt it on day one regardless.

Windows-native is deferred. tmux doesn't run on Windows; v0.5+ may add a
ConPTY-based backend behind the same Rust interface.

## tmux session naming

**One tmux session per terminal Tab, named with the Worktree as a
prefix.** Per-tab sessions are the structural fix for the bug class
issue #15 closes (two clients on one session share `curw`).

```
tmux session "sanctel_wt_<worktreeId>__term-1"          ← Tab A (one window: term-1)
tmux session "sanctel_wt_<worktreeId>__term-2"          ← Tab B (one window: term-2)
tmux session "sanctel_wt_<worktreeId>__term-3"          ← Tab C (one window: term-3)
```

`tmux -L sanctel ls | grep sanctel_wt_<worktreeId>__` lists every tab
in that Worktree — Worktree grouping is preserved at the name level
even though no tmux entity groups them structurally.

Worktree-less terminal tabs (a plain shell in `$HOME`) attach to
per-tab sessions named `sanctel_detached_<profileId>__<windowName>`.

The suffix separator is `__` (double underscore). Every sanctel-built
id flows through `tmux_safe`, which only ever produces single-`_`
runs, so `__` unambiguously marks where the Worktree base ends and the
windowName begins.

The base-segment separator is `_` (not `:`) because tmux parses `:`
and `.` in target specs as session/window/pane delimiters — a session
named `sanctel-wt:<id>` is unreachable for `list-sessions`,
`kill-session`, or `attach-session` lookups. Sanctel-built names are
kept in `[A-Za-z0-9_-]`, and any `worktreeId` / `profileId` is passed
through `tmux_safe` (replaces unsafe characters with `_`) before
concatenation.

### Why

- **Each tab is its own session, so two clients never attach to the
  same session.** The `curw` pointer in tmux's `struct session` cannot
  be shared between tabs because they have no session in common. The
  bug class issue #15 closes is structurally impossible.
- **`worktreeId` is durable by definition** (a Worktree is a
  filesystem entity; ADR-0005). Using it as the session-name prefix
  means the session name is reproducible on every launch with no
  per-tab UUIDs.
- **Tab id stays fully ephemeral**, in line with ADR-0004. The Tab
  record needs only a small immutable `windowName` string — which is
  also the session-name suffix.
- **Multiple terminal tabs per worktree get independent shells**,
  matching VS Code / iTerm mental model.
- **Cleanup is one shot**: closing a tab runs
  `tmux kill-session -t <session>`. The session's one window dies with
  it. No bookkeeping in Rust, no two-level kill dance.

### `windowName` assignment

Stable, monotonic per Worktree:

```
windowName = "term-N"   where N = 1 + max(existing term-N in sibling sessions)
```

The `windowName` is the **session-name suffix** in the per-tab model;
it remains the in-session window name as well so `tmux list-windows`
of a tab's session returns exactly `["term-N"]`.

**Allocation is server-side and atomic.** React passes
`window_name: "auto"` (or omits the field) on `create_tab` for
terminal/chat Tabs. Inside `create_tab`, Rust takes a per-Worktree-base
mutex, lists existing sessions via
`tmux list-sessions -F '#{session_name}'`, filters by the
`sanctel_wt_<wt>__` prefix, extracts each suffix as a `term-N`
candidate, runs the monotonic allocator, calls
`tmux new-session -d -s <base>__<term-N> -n <term-N>`, releases the
mutex, and returns the resolved name in `CreateTabResp.window_name`.
React reads the resolved name back and persists it in SQLite. The
window name is **immutable for the Tab's lifetime** — the Tab's
display title is a separate field (see Two-layer durability below).

The mutex matters: without it two concurrent callers in the same
Worktree both see "no existing sessions" and both compute `term-1`,
racing on `new-session` for the same session name. The per-Worktree
mutex is the smallest critical section that closes that window.

### Comparison to references

- **claude-squad** uses one tmux session per Instance (one shell per
  worktree-branch). We extend this to N windows per session.
- **superset.sh** owns a custom pty-daemon and stores opaque session ids
  in SQLite with a Worktree FK; this pays a daemon-LOC tax we explicitly
  rejected in ADR-0002.
- **TUICommander** uses portable-pty without tmux; PTYs die on app
  restart. Acceptable for them because their persistence model lives in
  agent resume state, not the PTY. Sanctel wants more.

## IPC contract

Three commands and one streaming channel between the terminal webview
(loaded from `public/terminal.html`) and Rust.

Per-tab parameters (`worktreeId`, `windowName`, `initialCommand`) are
passed by React to Rust **once** in `create_tab` and stored in
`TabRecord`. `terminal_attach` then needs only runtime args.

**The `create_tab` extension lands in Step 1 (Slice 2).** The IPC shape
below is the final shape; later slices only change the *values* React
passes (Slice 3 wires real `worktreeId`s; Slice 5 wires
`initialCommand` for chat). Pinning the shape up front prevents
Slice 2's hardcoded-worktree path from becoming a different IPC
contract than the one the rest of the ladder relies on:

```rust
// Extended create_tab (existing command — adds optional terminal fields)
#[tauri::command]
fn create_tab(app, req: CreateTabReq) -> Result<CreateTabResp, String>;
//   CreateTabReq:  { id, kind, url, profile_id,
//                    worktree_id?, window_name?, initial_command?,
//                    agent_session_id? }
//   CreateTabResp: { window_name? }   ← Rust returns the resolved
//                                       window name when the request
//                                       passed window_name: "auto".

// window_name sentinel: pass "auto" (or omit) to delegate allocation to
// Rust. Rust holds a per-session mutex, runs the allocator under the
// lock, and returns the resolved name in CreateTabResp.window_name —
// see "`windowName` assignment" above.

// First mount or post-restart reattach. Idempotent: creates session/window
// if missing, attaches if present. Same call path either way.
#[tauri::command]
fn terminal_attach(
    webview: tauri::Webview,                   // label IS tabId; Rust looks up
                                               //   worktreeId/windowName/initialCommand
                                               //   from TabRecord
    cols: u16,
    rows: u16,
    on_output: tauri::ipc::Channel<Vec<u8>>,   // Rust → frontend byte stream
) -> Result<(), String>;

#[tauri::command]
fn terminal_write(webview: tauri::Webview, bytes: Vec<u8>) -> Result<(), String>;

#[tauri::command]
fn terminal_resize(webview: tauri::Webview, cols: u16, rows: u16) -> Result<(), String>;
```

Lifecycle close is handled by extending the existing `close_tab` in
`src-tauri/src/lib.rs` to call `tmux kill-session -t <session>` for
`kind=terminal | chat` tabs. Each tab owns its own session per
issue #15, so a single `kill-session` removes the tab's shell and the
session in one shot — no two-level `kill-window` + base-survival
coordination. No separate `terminal_close` command — one close path,
one source of truth.

### Key properties

- **`tabId` is derived from the calling `Webview`'s label.** The frontend
  never passes its own id. A terminal webview can only act on itself; the
  invariant is enforced by the IPC shape, not by checks.
- **Bytes, not strings.** PTY output is not always valid UTF-8 (control
  codes, partial multi-byte chunks, binary pastes). Transcoding corrupts.
  Adopt superset's `no-encoding-hops` invariant: bytes in, bytes out, no
  intermediate string form on the data path.
- **`Channel<Vec<u8>>` for output, not events.** Events broadcast to all
  webviews; channels are a private pipe to one. Avoids cross-webview
  fan-out, gives backpressure semantics from the runtime.
- **`terminal_attach` is the single mount entry point** for both fresh
  tabs and reattach-on-launch. The Rust side runs the single
  `ensure_session_window` primitive against the tab's *own* per-tab
  session — `tmux has-session` then `new-session -d -s
  <base>__<window> -n <window> -c <cwd>` (when missing) — and either
  spawns or reattaches a `portable-pty` client running
  `tmux attach-session -t =<session>`. No `select-window` clause: the
  session has exactly one window by construction (issue #15), so the
  active-window pointer is uniquely the tab's. The `-n` on
  `new-session` matters: without it tmux auto-creates a phantom shell
  window that keeps the session alive after sanctel's `term-N` dies
  (issue #14).

## Two-layer durability

Sanctel's persistence story splits cleanly along **what survives sanctel
quit** vs **what survives laptop reboot**. Both layers cooperate; neither
duplicates the other.

| What | Where it lives | Survives sanctel quit | Survives laptop reboot |
|---|---|---|---|
| Running processes (e.g., `npm run dev`) | tmux | ✅ | ❌ |
| Agent conversation history | `~/.claude/projects/<encoded-cwd>/<id>.jsonl` | ✅ | ✅ |
| Tab metadata (title, kind, worktreeId, windowName, agentSessionId) | SQLite (v0.3 state persistence) | ✅ | ✅ |
| Profile cookies / localStorage | Tauri profile data directory | ✅ | ✅ |

### Consequences for terminal vs chat tabs

- **Terminal tab**: ephemeral commands (`npm run dev`, an interactive
  shell session) survive sanctel quit because tmux holds them. On laptop
  reboot, tmux is gone and the tab reopens as an empty shell in the same
  Worktree — same cwd, fresh process. This is acceptable; it matches the
  user expectation that compute state dies on power-off.
- **Chat tab**: a TabKind whose Tab record carries `worktreeId` and later
  carries `agentSessionId` once Sanctel sees that tab's own transcript.
  New chat tabs start plain `claude`; they do not infer a resume target
  from the newest transcript in the Worktree because that file can belong
  to another Claude process.

### What the SQLite Tab record holds for each kind

```
common              : { id, spaceId, kind, title }
terminal additions  : { worktreeId | null, windowName }
chat additions      : { worktreeId, windowName, agentSessionId }
browser additions   : { url }
file/diff additions : { worktreeId, path }
```

Every value above is a **sticky-note pointer** to durable state owned
elsewhere — never a copy of it. This is the Persistence Anchor pattern
applied at the field level: ADR-0004 forbids storing the *data*, not
storing the *address*.

## Reconnect-on-launch flow

**Webview-driven, not Rust-driven.** Rust never reads SQLite, never
enumerates tabs, never pre-spawns PTYs. Each terminal/chat webview calls
`terminal_attach` when its page is ready; Rust serves the call. One
idempotent code path handles both "first creation" and "reattach after
restart."

### Launch sequence

```
1. Rust startup probe: `tmux -V`. If absent, emit `tmux-missing` event
   and short-circuit the rest of the startup. React renders a setup
   screen, no tab attaches are attempted.

2. React reads SQLite (Profiles, Spaces, Tabs). Paints the sidebar
   immediately — user sees tab titles before any PTY is wired.

3. React invokes `create_tab` per saved Tab record (existing flow in
   src-tauri/src/lib.rs:101). Each call constructs a Tauri webview
   pointing at terminal.html / chat.html / a browser URL.

4. Each terminal/chat webview boots independently and, when xterm.js is
   mounted, calls `terminal_attach(worktreeId, cols, rows, initialCommand,
   onOutput)`.

5. Rust runs the idempotent attach algorithm (see below). On success,
   bytes flow through the channel into xterm.js. On failure, the call
   returns an error and the webview renders an inline broken-tab UI.
```

Inactive tabs follow the same path in parallel; their webviews are
created off-screen but their attach happens normally. The active tab may
optionally be created first as a render-order optimization (not
architectural).

### Idempotent attach algorithm

```
fn attach_tab_to_tmux(webview, worktreeId, windowName, initialCommand) -> Result:
  worktreePath = resolve(worktreeId)
  if not exists(worktreePath):
    return Err("worktree-missing")                            # case D

  base    = "sanctel_wt_" + worktreeId    # or sanctel_detached_<profileId>
  session = base + "__" + windowName      # one tmux session per Tab (issue #15)

  # ensure_session_window encapsulates the single primitive
  # (race-safe: retry once on "session exists" from concurrent caller).
  # initialCommand only fires on the new-session branch — reattach is a
  # pure no-op.
  if not tmux has-session -t =<session>:
    tmux new-session -d -s <session> -n <windowName> -c <worktreePath> [initialCommand]
    # -n is critical: without it tmux creates a phantom shell window
    # that outlives sanctel's term-N and leaks the session forever
    # (issue #14). The session contains exactly its one term-N window.

  # spawn pty client; wire to channel
  pty = portable_pty::spawn(["tmux", "attach-session", "-t", "=" + session])
  # No `select-window` clause: the session has exactly one window
  # (issue #15), so its `curw` pointer cannot be moved by attaching to
  # it. Two tabs in the same Worktree are two clients on TWO sessions
  # — they cannot share an active-window pointer.
  spawn_thread { for chunk in pty.read(): channel.send(chunk) }
  store(webview.label → {pty, session})
  return Ok
```

`initialCommand` is `Some("claude")` for new chat tabs that do not yet
have a known AgentSession, `Some("claude --resume <agentSessionId>")`
only when the Tab record already carries a verified resume target, and
`None` for plain terminal tabs. It is wired through `tmux new-window` so
it only fires when the window is genuinely new — reattaching to an
existing window never re-runs the command.

### Race handling

Two flavors:

- **Same tab, multiple attaches** (e.g., a webview reload re-running
  `terminal_attach`): both calls target the same session name. The
  loser's `new-session` errors with "duplicate session"; the wrapper
  re-checks `has-session` (now succeeds) and proceeds. Two lines of
  defensive code in `ensure_session_window`; no global lock.
- **Allocator race** (multiple new tabs in the same Worktree at the
  same time): each call computes its own `term-N` under the
  per-Worktree-base mutex in `allocate_session_for_tab`, so the
  scan + new-session step is atomic per Worktree. Without the mutex,
  two callers both see "no existing sessions" and both pick
  `term-1`; with it, they see `[]` then `[term-1]` in sequence and
  pick `term-1` and `term-2`.

### Broken-tab UX (worktree-missing case)

`terminal_attach` returns `Err("worktree-missing")`. The webview's
frontend handles this by rendering an inline panel:

```
  ⚠ Worktree no longer exists at <path>.
  [ Recreate from main ]   [ Remove this tab ]
```

The sidebar entry remains; only the content area shows the error. Other
tabs are unaffected. No auto-cleanup — the user might have moved the
worktree and want to re-link.

### What we explicitly do not do

- **No eager PTY pre-spawn at app start.** Webviews trigger their own
  attach. Pre-spawning would buffer output for consumers that don't yet
  exist; tmux already buffers scrollback for free.
- **No state machine enumerating "the 5 cases."** The `has || create`
  pattern collapses fresh-launch, sanctel-quit, laptop-reboot, and
  manually-killed-window into one code path.
- **No auto-`--resume` when an agent died inside an existing window.**
  If the tmux window is alive but the `claude` process exited, the user
  sees a shell prompt and can manually resume. Sanctel never reruns the
  initial command for a pre-existing window.
- **No SQLite reads from Rust.** Every per-tab fact Rust needs
  (worktreeId, windowName, initialCommand) arrives via the
  `terminal_attach` arguments. SQLite is React's concern.

## Frontend module shape

**One shared TypeScript module, two thin HTML entries.** The xterm.js +
IPC logic lives once in `src/terminal/terminal-runtime.ts`. The
`terminal.html` page mounts it bare; the `chat.html` page mounts it with
a small header above. Neither page pulls in React — these are
terminal-canvas pages, not application UIs, and React's boot cost is
not justified for them.

### Files

```
src/terminal/
  terminal-runtime.ts     # xterm setup, addons, Channel wiring,
                            terminal_attach/write/resize calls
  link-handler.ts         # URL click → invoke("create_tab", {kind:"browser"})
  clipboard.ts            # Tauri clipboard-plugin glue for copy/paste

terminal.html             # Vite entry: vanilla; full-bleed xterm container
chat.html                 # Vite entry: vanilla; <header> + xterm container
```

`vite.config.ts` is updated to declare `terminal.html` and `chat.html`
as multi-page entries (`rollupOptions.input`), so they get bundled with
the shared module and TS imports work.

### xterm.js addon set for v0.3

| Addon | Why now |
|---|---|
| `@xterm/addon-fit` | Required — pixel→cols/rows math for the container. |
| `@xterm/addon-webgl` | GPU renderer; without it, large output is sluggish. |
| `@xterm/addon-web-links` | URL detection + click handling. |
| `@xterm/addon-unicode11` | Correct character widths (emoji, CJK). |

Deferred: search, image (sixel/iterm2), ligatures, serialize. Add when
there's a user-visible reason.

### What `terminal-runtime.ts` does, in order

```
1. Create the xterm.js Terminal instance with sane defaults
   (theme, font, cursor blink, scrollback = 10_000).
2. Load addons: fit, webgl, web-links (with linkHandler injected),
   unicode11. activate unicode11.
3. Mount the Terminal into a container <div>.
4. Set up a ResizeObserver on the container:
     onResize → fit.fit() → invoke("terminal_resize", {cols, rows}).
5. Create a Channel<Uint8Array>; set onmessage = bytes => term.write(bytes).
6. Call invoke("terminal_attach", {cols, rows, onOutput: channel}).
   Rust looks up worktreeId/windowName/initialCommand by webview label
   from the TabRecord stored at create_tab time.
7. Wire term.onData(s => invoke("terminal_write", {bytes: encode(s)})).
8. Wire clipboard handlers via the clipboard module.
9. On terminal_attach error, dispatch a custom "tab-broken" DOM event
   that the page's vanilla code renders into an inline broken-tab UI.
```

### Identity is server-held

The webview does **not** carry its identity through query strings or URL
fragments. Per-tab parameters (worktreeId, windowName, initialCommand,
agentSessionId) are passed by React to Rust **once**, at `create_tab`
time, and stored in the in-memory `TabRecord`. The webview later calls
`terminal_attach` with only `(cols, rows, onOutput)`; Rust looks up the
rest by webview label.

This refines the Q2 contract: `terminal_attach` no longer takes
`worktree_id`/`window_name`/`initial_command` arguments. `create_tab`
takes them instead.

### `chat.html` header

Plain HTML, no framework. Three elements: tab title, agent-type label
(e.g., "claude — sonnet-4.6"), stop button. The stop button sends
`Ctrl+C` via `terminal_write`. v0.3 ships nothing beyond this header.
Model picker, conversation list, transcript export are post-v0.3.

### Link handling

`addon-web-links` detects URLs. The handler is injected as a callback,
keeping `terminal-runtime.ts` decoupled from the rest of the app:

```ts
new WebLinksAddon((event, url) => linkHandler(url));
// terminal.html / chat.html wires:
//   linkHandler = url => invoke("create_tab", {kind:"browser", url, ...})
```

A click opens a new browser tab in the same Space — identical code path
to clicking "+ new tab" in the sidebar.

### Clipboard

xterm.js handles selection; the system clipboard is bridged via
`@tauri-apps/plugin-clipboard-manager`. Copy on selection (or
Cmd/Ctrl+C) writes to the clipboard; Cmd/Ctrl+V reads and dispatches to
`terminal_write`. Wired in the `clipboard.ts` module.

### Why no React in these pages

- ~50-100 KB and 50-200 ms boot cost per tab mount. Multiplied across N
  open terminals on startup, this is meaningful.
- These pages have no routing, no component tree, no shared state with
  the React shell. They're single-purpose canvases.
- The chat header is three DOM elements; vanilla suffices.

If a future requirement genuinely needs framework-level state in the
chat page (side panels, multi-pane), migrate then — the cost is small
because `terminal-runtime.ts` is framework-agnostic.

## Implementation order

Six steps, ~4–6 days. Each step ends with a runnable demo. Step 1 burns
down both architectural bets (xterm/IPC latency + tmux re-attach)
simultaneously; everything after is incremental.

### Step 0 — Setup (~½ day)

- npm deps: `@xterm/xterm`, `@xterm/addon-{fit,webgl,web-links,unicode11}`,
  `@tauri-apps/plugin-clipboard-manager`.
- Rust deps: `portable-pty`, `tokio`.
- `vite.config.ts` multi-page entries for `terminal.html` and `chat.html`.
- Skeleton `src/terminal/terminal-runtime.ts`; rewrite placeholder HTML
  entries.

**Demo:** clicking "new terminal tab" opens a black box with a cursor.

### Step 1 — One terminal, hardcoded worktree, tmux from day one (~1 day) ⚠ keystone

- Rust commands: `terminal_attach`, `terminal_write`, `terminal_resize`.
- **`create_tab`'s final IPC shape lands here.** `CreateTabReq` is
  extended now with the optional per-kind fields (`worktreeId`,
  `windowName`, `initialCommand`, `agentSessionId`) and `CreateTabResp`
  carries back the resolved `windowName`. Slice 2 passes constants
  (`worktreeId: "default"`, `windowName: "term-1"`, the rest `None`);
  later slices change only the *values*, never the shape.
- Idempotent attach algorithm against tmux (not raw portable-pty).
- Hardcoded worktree path (e.g., `$HOME`); no SQLite, no Worktree object.
- `terminal-runtime.ts` fully wired: xterm + fit + webgl + Channel.

**Demo:** open terminal, run `npm run dev`, quit sanctel, reopen, click
"new terminal tab" — scrollback and process still there.

**Validates:** ADR-0002's two bets (xterm/IPC latency and tmux re-attach)
in one pass. If either fails, pivot before building more on top.

### Step 2 — Polish: resize, paste, link clicks (~½ day)

- ResizeObserver wired to xterm fit + `terminal_resize`.
- Tauri clipboard plugin for copy/paste.
- web-links addon → `invoke("create_tab", {kind:"browser", url, …})`.

**Demo:** resize window, terminal reflows. Cmd-V pastes. Clicking a URL
opens a browser tab.

### Step 3 — Worktree-aware (~½ day)

- React holds a hardcoded list of Worktrees with real paths.
- React passes a real `worktreeId` (the IPC shape was already extended
  in Step 1; this step changes only the *value*).
- React passes `windowName: "auto"`; Rust allocates `term-N` under a
  per-session mutex and returns the resolved name in `CreateTabResp`.
  React persists the resolved name in SQLite.
- tmux session name = `sanctel_wt_<worktreeId>__<windowName>`,
  `-c <worktree.path>` (one session per Tab, Worktree as prefix —
  see issue #15).

**Demo:** two terminals in same worktree = two sibling sessions
sharing a `sanctel_wt_<worktreeId>__` prefix, each with its own
window. Different worktree = different prefix. `tmux ls` from outside
confirms.

### Step 4 — Persistence (1–2 days)

- SQLite schema: profiles, spaces, tabs (with kind, title, worktreeId,
  windowName, agentSessionId).
- React loads on launch and replays `create_tab` per row.
- React writes on every mutation.

**Demo:** create 3 terminal tabs, rename one, quit, reopen — all three
return with titles intact and shells reattached. Laptop reboot — tabs
return; shells are fresh (expected) but tab metadata survives.

### Step 5 — Chat tabs (~½–1 day)

- `initialCommand` plumbed through `create_tab` → `TabRecord` →
  `terminal_attach` (the algorithm already supports this).
- `chat.html` gets its small header (title, agent label, stop button).
- React computes `initialCommand = "claude"` for a new chat tab and
  stores `agentSessionId = null` at create time. It then polls
  `~/.claude/projects/<encoded-cwd>/` for a post-create jsonl and updates
  the Tab record to `claude --resume <agentSessionId>` after capture.

**Demo:** chat with `claude` in a worktree, quit sanctel, reopen —
conversation continues because the tmux session survived. `tmux
kill-server`, reopen — Sanctel resumes with `claude --resume` once the
Tab carries a verified `agentSessionId`.

### Step 6 — Edge cases & hardening (~½ day)

- `tmux -V` startup probe + setup screen when missing.
- Broken-tab UI for the worktree-missing case.
- Race-retry on `new-session` conflict.
- Stress-test parallel attach with many tabs.

**Demo:** delete a worktree directory between launches — that one tab
shows a clear error; the rest are unaffected.

### What changed from the original prompt's 4-step ladder

| Original | Revised | Why |
|---|---|---|
| Step 1: raw portable-pty | Step 1: portable-pty hosting `tmux attach-session` | Burns down both architectural bets day one; we ship tmux anyway, so validating raw PTY validates the wrong layer. |
| Step 3: "tmux integration" | Absorbed into Step 1 | Same reason. |
| Step 4 cleanup ambiguity | Step 6, decision already made (kill-window) | Q1 settled the kill-vs-detach question. |
| No persistence step | Step 4 explicit | Reconnect-on-launch is untestable without it. |
| No chat-tab step | Step 5 explicit | Same module + a `--resume` initial command; cheap once Step 4 lands. |

## Out of scope (for v0.3)

- **Tmux control-mode** (`tmux -CC`) — would give us structured event
  output for window/pane changes, but is macOS-iTerm-history-flavored and
  not needed until we add multi-pane-per-window UI.
- **Scrollback search / link parsing / image rendering** — xterm.js
  addons exist for all of these; deferred until terminal MVP works.
- **Mirrored-view tabs** (two tabs as two clients on one window) —
  designed-for but not built in v0.3.
- **Windows / WSL** — ConPTY support deferred. macOS + Linux only for v0.3.

## Zellij protocol reference (issue #29 audit)

This section captures the empirically-verified protocol sanctel's
zellij backend must speak with `zellij web`. Established via the
issue #29 audit after a five-bug chain (#23 auth, #25 socket-dir, #26
invalid CLI flag, #28 missing web_client_id, and the closed-channel
symptom) revealed that source-reading-only briefs had been missing
load-bearing protocol details. **This is empirical ground truth.**
When zellij's version changes and behaviors shift, this section must
be re-verified end-to-end via websocat (or equivalent), not just
re-read from source.

### Verified against zellij 0.44.3

The complete handshake sanctel performs for one terminal tab:

```
1. Subprocess: `zellij web --create-token`
   → stdout: "Created token successfully\n\ntoken_<N>: <UUID>\n"
   → parse out (token_name, auth_token_uuid).

2. HTTP: POST http://127.0.0.1:<port>/command/login
   body: {"auth_token": "<UUID>", "remember_me": false}
   headers: Content-Type: application/json
   → response: Set-Cookie: session_token=<UUID>; HttpOnly; SameSite=Strict; Path=/
   → parse out the session_token cookie value.

3. HTTP: POST http://127.0.0.1:<port>/session
   body: {}
   headers: Content-Type: application/json, Cookie: session_token=<UUID>
   → response: {"web_client_id": "<UUID>", "is_read_only": false}
   → parse out web_client_id.

4. WebSocket: open ws://127.0.0.1:<port>/ws/control
   headers: Cookie: session_token=<UUID>
   (no web_client_id query param on this endpoint — the id rides on
    every message body)

5. WebSocket SEND on /ws/control (TEXT frame, JSON):
   {"web_client_id": "<UUID>",
    "payload": {"type": "TerminalResize", "rows": 24, "cols": 80}}

6. WebSocket SEND on /ws/control (TEXT frame, JSON):
   {"web_client_id": "<UUID>",
    "payload": {"type": "TerminalMetrics",
                "cell_pixel_width": 7,
                "cell_pixel_height": 14,
                "text_area_pixel_width": 560,
                "text_area_pixel_height": 336}}

7. WebSocket: open ws://127.0.0.1:<port>/ws/terminal/<session>?web_client_id=<UUID>
   headers: Cookie: session_token=<UUID>
   (web_client_id is REQUIRED as a query parameter on this endpoint)

8. Server pushes PTY output on /ws/terminal as TEXT frames.
   Sanctel forwards the bytes to the webview's Channel<Vec<u8>>.

9. Client sends keystrokes on /ws/terminal as Binary frames
   (Text frames also accepted server-side; binary preferred so
    non-UTF-8 paste content doesn't require transcoding).

10. Server may at any time send inbound control messages on /ws/control:
    - {"type": "QueryTerminalSize"} → client responds by re-sending
      messages 5 and 6 with the latest size.
    - {"type": "SetConfig", ...} → informational (sanctel ignores; xterm
      is styled by the webview).
    - {"type": "Log", "lines": [...]} → informational.
    - {"type": "LogError", "lines": [...]} → informational.
    - {"type": "SwitchedSession", "new_session_name": "..."} → server
      switched the client to a different session; sanctel currently
      ignores (future work).
    - Any unknown type → silently ignored for forward-compat.

11. On tab close: cleanly close both WebSockets, then subprocess
    `zellij web --revoke-token <token_name>` to clean up.
```

### Load-bearing details that earlier briefs missed

| Detail | What earlier briefs assumed | Empirical truth |
|---|---|---|
| Output frame type | binary | **text** |
| Control message shape | `{type, ...fields}` flat | **`{web_client_id, payload: {type, ...fields}}` two-level envelope** |
| Type tag casing | snake_case | **PascalCase** (matches zellij's Rust variant names) |
| Connection order | open both in parallel | **control first + send envelopes, then terminal** (otherwise the listener's `send_control(SwitchedSession)` races the control_tx registration and silently fails) |
| Initial size message | not sent (waited for resize event) | **must be sent on connect** — server uses it to register control_tx in connection_table |
| QueryTerminalSize | could be ignored | **must respond** with the latest TerminalResize+TerminalMetrics pair |

### Frame-type details

- **Outbound (client → server) on /ws/terminal**: sanctel uses
  `Message::Binary`. Zellij's `ws_handler_terminal` accepts both Binary
  and Text and routes both through `parse_stdin`. Binary is preferred
  because xterm.js can emit non-UTF-8 byte sequences (binary pastes)
  that would fail Text frame's UTF-8 validation.

- **Outbound (client → server) on /ws/control**: TEXT frames carrying
  serialized `ControlEnvelope` JSON. Cannot be binary — zellij's control
  handler expects JSON.

- **Inbound (server → client) on /ws/terminal**: zellij sends TEXT
  frames containing terminal escape sequences. Sanctel's
  `decode_binary_frame` accepts BOTH `Message::Binary` and
  `Message::Text` and extracts the bytes either way — defensive
  against a future zellij version that switches frame types.

- **Inbound (server → client) on /ws/control**: TEXT frames carrying
  `WebServerToWebClientControlMessage` JSON. Sanctel parses via
  `ServerControlMessage` with `#[serde(other)]` fallback for
  forward-compatibility.

### Where this lives in the codebase

- `src-tauri/src/zellij_auth.rs` — steps 1, 2, 3, 11.
- `src-tauri/src/zellij_ws.rs::mount` — steps 4–10. The function's
  internal order matches the canonical sequence above.
- `src-tauri/src/zellij_daemon.rs` — supervises `zellij web` itself.

### Maintenance contract

If you touch zellij integration:

1. Run `cargo test --lib zellij_ws` and `cargo test --lib zellij_auth`.
   These pin the wire shapes via unit tests.
2. Run a real end-to-end test: `npm run tauri dev` with
   `SANCTEL_BACKEND=zellij`, create a terminal tab, type, see output.
3. If zellij's version has been bumped, re-walk this protocol
   against the new version before claiming the integration works.
   The wire shape can change between versions; the unit tests pin our
   side but cannot detect server-side protocol drift.
