// ───────────────────────────────────────────────────────────────────────────
// Sanctel backend — Tauri 2 webview-per-tab pattern, adapted from Bushido.
//
// The whole architecture in one paragraph:
//   - React is just chrome (sidebar, top bar). It has no terminal/browser
//     rendering of its own.
//   - Each tab is a Tauri webview (`window.add_child(builder, pos, size)`).
//   - The active webview is positioned to overlay the React "content area".
//   - Inactive webviews are moved far off-screen (-9999, -9999) — they keep
//     running but aren't visible. Switching tabs is instant.
//   - Identity isolation (Arc's "Profile" concept, ADR-0003): each webview
//     is configured via `profile_isolation::apply_profile_isolation`, which
//     picks the right per-platform Tauri 2.11 API — `data_directory` on
//     Windows/Linux, `data_store_identifier` on macOS WKWebView. All tabs
//     across all Spaces that share a `profile_id` share cookies; different
//     `profile_id`s are fully isolated.
// ───────────────────────────────────────────────────────────────────────────

mod backend;
mod profile_isolation;
mod terminal_runtime;
mod tmux_cli;
mod zellij_auth;
mod zellij_cli;
mod zellij_daemon;
mod zellij_ws;

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::webview::WebviewBuilder;
use tauri::{Emitter, LogicalPosition, LogicalSize, Manager, Runtime, Webview, WebviewUrl};

use crate::backend::Backend;
use crate::profile_isolation::apply_profile_isolation;
use crate::terminal_runtime::{
    attach_tab_to_tmux, attach_tab_to_zellij, AttachParams, TerminalRegistry,
};
use crate::tmux_cli::{allocate_window_name, tmux_safe, CommandRunner, TmuxCli, TmuxError};
use crate::zellij_cli::{ZellijCli, ZellijError};
use crate::zellij_daemon::{RealAuthenticator, RealLauncher, ZellijDaemon};

// ─── shared state ─────────────────────────────────────────────────────────

#[derive(Default)]
struct AppState {
    // tab_id → record (profile_id retained for diagnostics / future ops;
    // terminal/chat tabs also carry the server-held identity used by the
    // terminal_attach lookup path).
    tabs: Mutex<HashMap<String, TabRecord>>,
    // The React shell's content rect (x, y, w, h) inside the window.
    // Updated whenever React resizes its content area.
    content_rect: Mutex<Rect>,
    // ID of the tab currently visible (positioned over the content area).
    active_tab: Mutex<Option<String>>,
    // Per-tab terminal handles (PTY + tmux session/window names). Populated
    // by terminal_attach, consumed by terminal_write / terminal_resize /
    // close_tab.
    terminals: TerminalRegistry,
    // Result of the one-time `tmux -V` startup probe. Populated by `run()`
    // before any frontend invokes; read by the `tmux_status` command so
    // React can gate terminal/chat tab creation behind a setup screen.
    tmux_status: Mutex<TmuxStatus>,
    // Per-Worktree-prefix locks. `create_tab` grabs the inner mutex for
    // the (worktreeId | detachedProfileId) base it's about to allocate
    // against, then atomically scans existing sessions for that base,
    // computes the next `term-N`, and calls `new-session`. Without the
    // lock, two concurrent callers in the same Worktree both see no
    // existing sessions and both pick `term-1`, racing on `new-session`
    // for the same name. Per-tab session model (issue #15) means each
    // tab is its own session — the lock now serializes the allocator
    // across the *group* of sessions sharing a Worktree base.
    allocation_locks: AllocationLocks,
    // Supervised `zellij web` daemon for the spike backend (issue #16 /
    // issue #17). Populated only when `SANCTEL_BACKEND=zellij` is set and
    // the zellij version probe succeeded; `None` otherwise. Drop on this
    // field is what tears the daemon down at sanctel shutdown — the
    // acceptance criterion that the spawned `zellij web` process must be
    // terminated on clean exit rides on this Drop firing.
    zellij_daemon: Mutex<Option<ZellijDaemon>>,
}

/// Per-Worktree-base mutex map. The outer mutex protects the HashMap; the
/// inner `Arc<Mutex<()>>` is held during the list-sessions → compute-name →
/// new-session critical section in `create_tab`. Keyed by the Worktree (or
/// detached-profile) base prefix that groups all of a tab-set's sessions.
#[derive(Default)]
struct AllocationLocks {
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl AllocationLocks {
    /// Returns the per-base mutex, creating it on first use. The caller is
    /// expected to drop the returned Arc's MutexGuard before returning from
    /// its critical section; the outer map mutex is held only long enough
    /// to insert/get.
    fn lock_for(&self, base: &str) -> Arc<Mutex<()>> {
        self.locks
            .lock()
            .entry(base.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

/// Result of the one-time backend startup probe. Emitted as a Tauri event
/// and also readable via the `tmux_status` command so React can render
/// synchronously on first paint without waiting for the event.
///
/// The struct name is historical (tmux was the only backend when the
/// field was added). `backend` identifies which backend was probed so
/// the frontend setup screen can render the appropriate copy and install
/// instructions — a zellij failure must not render "needs tmux".
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TmuxStatus {
    backend: String,
    available: bool,
    version: Option<String>,
    error: Option<String>,
}

impl Default for TmuxStatus {
    fn default() -> Self {
        Self {
            backend: Backend::Tmux.name().to_string(),
            available: false,
            version: None,
            error: None,
        }
    }
}

#[derive(Clone)]
struct TabRecord {
    profile_id: String,
    kind: String,
    // Server-held terminal identity. Populated at create_tab time for
    // terminal/chat kinds (Slice 4 — worktree-aware). `worktree_id` keys
    // the tmux session per ADR-0012; `worktree_path` is the cwd passed
    // to `-c` so the shell starts in the right directory.
    worktree_id: Option<String>,
    worktree_path: Option<String>,
    window_name: Option<String>,
    initial_command: Option<String>,
    /// Forward-compat slot for chat tabs (Slice 6): the AgentSession id used
    /// by `claude --resume`. Stored on the record but not yet consumed.
    #[allow(dead_code)]
    agent_session_id: Option<String>,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

const OFFSCREEN: f64 = -9999.0;

// ─── helpers ──────────────────────────────────────────────────────────────

/// Move a webview to overlay the current content rect (visible).
fn show_webview(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    let state = app.state::<AppState>();
    let rect = *state.content_rect.lock();

    let webview = app
        .get_webview(id)
        .ok_or_else(|| format!("webview not found: {id}"))?;

    webview
        .set_position(LogicalPosition::new(rect.x, rect.y))
        .map_err(|e| e.to_string())?;
    webview
        .set_size(LogicalSize::new(rect.w.max(1.0), rect.h.max(1.0)))
        .map_err(|e| e.to_string())?;

    *state.active_tab.lock() = Some(id.to_string());
    Ok(())
}

/// Move a webview off-screen (hidden but still running).
fn hide_webview(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    let webview = app
        .get_webview(id)
        .ok_or_else(|| format!("webview not found: {id}"))?;
    webview
        .set_position(LogicalPosition::new(OFFSCREEN, OFFSCREEN))
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ─── Tauri commands invoked from React ────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateTabReq {
    id: String,
    kind: String,   // "browser" | "terminal" | "chat"
    url: String,    // external URL for "browser"; "local://terminal" etc. for the others
    /// Drives Profile cookie / localStorage isolation per ADR-0003. Threaded
    /// through `profile_isolation::apply_profile_isolation`, which picks the
    /// right Tauri 2.11 API per platform (data_directory on Windows/Linux,
    /// data_store_identifier on macOS WKWebView). In Arc terms: this is the
    /// Profile, NOT the Space. The frontend computes it via `space.profileId`
    /// and sends it directly.
    profile_id: String,
    /// Terminal/chat tabs carry server-held identity from create_tab time.
    /// `worktreeId` is the Worktree-prefix component of the tmux session
    /// name (`sanctel_wt_<id>__<windowName>` per ADR-0012 revised by
    /// issue #15); `worktreePath` is the cwd passed to `tmux -c` so the
    /// shell starts in the right directory. Both are None for detached
    /// terminal tabs.
    worktree_id: Option<String>,
    worktree_path: Option<String>,
    window_name: Option<String>,
    initial_command: Option<String>,
    /// Forward-compat slot for chat tabs (Slice 6). Stored on the TabRecord
    /// but not consumed in this slice — Slice 6's chat-tab flow turns it
    /// into `initial_command = "claude --resume <agentSessionId>"`.
    agent_session_id: Option<String>,
}

/// Response from `create_tab`. For terminal/chat tabs created with the
/// `windowName: "auto"` sentinel (or no windowName at all), Rust allocates
/// the next `term-N` under the per-session mutex and returns it here so the
/// frontend can persist the resolved name. For the reattach path (explicit
/// windowName) and for non-terminal kinds, this is `None`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateTabResp {
    window_name: Option<String>,
}

/// `"auto"` is the explicit sentinel from React asking Rust to allocate the
/// next `term-N`. We also treat `None` the same way for terminal/chat tabs,
/// so omitting the field is equivalent.
const AUTO_WINDOW_NAME: &str = "auto";

#[tauri::command]
fn create_tab(app: tauri::AppHandle, req: CreateTabReq) -> Result<CreateTabResp, String> {
    let is_terminal_like = matches!(req.kind.as_str(), "terminal" | "chat");

    // Belt-and-braces gate: React already hides the new-terminal/new-chat
    // buttons behind `tmux-missing`. This second check makes sure a stale
    // SQLite restore or a scripted invoke can't slip past and spawn a
    // doomed PTY.
    if is_terminal_like {
        let status = app.state::<AppState>().tmux_status.lock().clone();
        if !status.available {
            return Err("tmux-missing".to_string());
        }
    }

    // For terminal/chat tabs, resolve `windowName` server-side under a
    // per-Worktree mutex when the request asked for "auto" allocation. Doing
    // it before we build the webview means a failure here surfaces to React
    // before any client-visible state is created.
    let asked_for_auto = is_terminal_like
        && matches!(req.window_name.as_deref(), None | Some(AUTO_WINDOW_NAME));
    let allocated_window_name: Option<String> = if asked_for_auto {
        let cwd =
            resolve_worktree_cwd(req.worktree_id.as_deref(), req.worktree_path.as_deref())?;
        let base = tmux_session_base(req.worktree_id.as_deref(), &req.profile_id);
        let locks = &app.state::<AppState>().allocation_locks;
        let allocated = match Backend::from_env() {
            Backend::Tmux => {
                let tmux = TmuxCli::default();
                allocate_session_for_tab(
                    locks,
                    &tmux,
                    &base,
                    &cwd,
                    req.initial_command.as_deref(),
                )
                .map_err(|e| e.to_string())?
            }
            Backend::Zellij => {
                let zellij = ZellijCli::default();
                let window_name =
                    allocate_session_for_zellij_tab(locks, &zellij, &base, &cwd)
                        .map_err(|e| e.to_string())?;
                // Zellij has no CLI flag to start a session with a running
                // command, so the chat tab's initial_command rides over a
                // transient WebSocket (see `write_initial_command`).
                if let Some(cmd) = req.initial_command.as_deref() {
                    let state = app.state::<AppState>();
                    let (port, session_token) = {
                        let daemon = state.zellij_daemon.lock();
                        let d = daemon.as_ref().ok_or_else(|| {
                            "zellij daemon not running — startup probe failed".to_string()
                        })?;
                        (d.port(), d.session_token())
                    };
                    let session = format!("{base}__{window_name}");
                    zellij_ws::write_initial_command(&session, port, &session_token, cmd)
                        .map_err(|e| e.to_string())?;
                }
                window_name
            }
        };
        Some(allocated)
    } else {
        None
    };

    // Effective name stored on the TabRecord: the freshly allocated one
    // when asked, otherwise whatever the frontend supplied (explicit name
    // on the reattach path, or `None` for non-terminal kinds).
    let effective_window_name = allocated_window_name.clone().or(req.window_name.clone());

    let window = app
        .get_window("main")
        .ok_or_else(|| "main window not found".to_string())?;

    // Map the tab kind+url to an actual WebviewUrl.
    //
    // - browser:  external URL → real navigation.
    // - terminal: served from our frontend bundle at /terminal.html — that page
    //             hosts xterm.js and connects to the backend over a Tauri
    //             command channel.
    // - chat:     same idea, /chat.html (a React chat UI bundled with the app).
    let webview_url = match req.kind.as_str() {
        "browser" => {
            let parsed = req.url.parse().map_err(|e: url::ParseError| e.to_string())?;
            WebviewUrl::External(parsed)
        }
        "terminal" => WebviewUrl::App("terminal.html".into()),
        "chat" => WebviewUrl::App("chat.html".into()),
        other => return Err(format!("unknown tab kind: {other}")),
    };

    // Build the webview. Profile isolation lives in `apply_profile_isolation`
    // — the single place that branches per-platform on the right Tauri 2.11
    // API (data_directory on Windows/Linux, data_store_identifier on macOS
    // WKWebView). ALL tabs across ALL Spaces that share a profile_id share
    // cookies/localStorage; different profile_ids are fully isolated. This
    // is the Arc model encoded in ADR-0003: Profile is the cookie boundary,
    // Space is purely organizational.
    let builder = apply_profile_isolation(
        WebviewBuilder::new(&req.id, webview_url),
        &req.profile_id,
        &app,
    )?
    .auto_resize();

    // Initial position: off-screen, then we'll show_webview to bring it on.
    let rect = *app.state::<AppState>().content_rect.lock();
    let _webview = window
        .add_child(
            builder,
            LogicalPosition::new(rect.x, rect.y),
            LogicalSize::new(rect.w.max(1.0), rect.h.max(1.0)),
        )
        .map_err(|e| e.to_string())?;

    // Record it. The TabRecord carries the *resolved* window name so the
    // attach-path lookup (resolve_attach_params) sees the same value the
    // frontend persists.
    app.state::<AppState>().tabs.lock().insert(
        req.id.clone(),
        TabRecord {
            profile_id: req.profile_id,
            kind: req.kind,
            worktree_id: req.worktree_id,
            worktree_path: req.worktree_path,
            window_name: effective_window_name,
            initial_command: req.initial_command,
            agent_session_id: req.agent_session_id,
        },
    );

    // Hide currently-active tab (if any), then make this new tab visible.
    let prev = app.state::<AppState>().active_tab.lock().clone();
    if let Some(prev_id) = prev {
        let _ = hide_webview(&app, &prev_id);
    }
    show_webview(&app, &req.id)?;

    Ok(CreateTabResp {
        window_name: allocated_window_name,
    })
}

/// Worktree-path resolution shared by `create_tab` (for the auto-allocation
/// path) and `resolve_attach_params` (for the attach path). Single source of
/// truth: detached → $HOME; worktree-keyed → the request's path.
fn resolve_worktree_cwd(
    worktree_id: Option<&str>,
    worktree_path: Option<&str>,
) -> Result<String, String> {
    match (worktree_id, worktree_path) {
        (Some(_), Some(path)) => Ok(path.to_string()),
        (None, _) => std::env::var("HOME").map_err(|_| "HOME not set".to_string()),
        (Some(_), None) => Err(
            "worktreeId set without worktreePath — create_tab must carry both".to_string(),
        ),
    }
}

/// The atomic critical section behind `windowName: "auto"`: under the
/// per-Worktree-base mutex, scan existing sessions whose name starts with
/// `<base>__`, extract the suffix as the candidate window-name set, compute
/// the next `term-N`, and let `ensure_session_window` create the session
/// `<base>__<term-N>` with that window as its first and only child.
/// Returns the resolved window name (the session-name suffix). Generic
/// over `CommandRunner` so the concurrency test below can drive it with a
/// scripted-tmux fixture without spawning real processes.
///
/// Per-tab session model (ADR-0012 revised by issue #15): each terminal /
/// chat tab is its own tmux session. The allocator picks a fresh
/// `term-N` by scanning the sibling sessions sharing a Worktree base, not
/// by scanning windows inside one session.
fn allocate_session_for_tab<R: CommandRunner>(
    locks: &AllocationLocks,
    tmux: &TmuxCli<R>,
    base: &str,
    cwd: &str,
    initial_command: Option<&str>,
) -> Result<String, TmuxError> {
    let lock = locks.lock_for(base);
    let _guard = lock.lock();
    let prefix = format!("{base}__");
    let existing_suffixes: Vec<String> = tmux
        .list_sessions()?
        .into_iter()
        .filter_map(|s| s.strip_prefix(&prefix).map(str::to_string))
        .collect();
    let window_name = allocate_window_name(&existing_suffixes);
    let session = format!("{base}__{window_name}");
    tmux.ensure_session_window(&session, &window_name, cwd, initial_command)?;
    Ok(window_name)
}

/// Zellij analog of [`allocate_session_for_tab`]. Under the same per-base
/// mutex, scans existing zellij sessions matching `<base>__`, picks the
/// next `term-N`, and creates the session via
/// `zellij attach --create-background` (empty — zellij has no CLI flag
/// to splice a shell-command argument). The caller is responsible for
/// writing the chat-tab's `initial_command` into the new pane via
/// [`zellij_ws::write_initial_command`] — kept separate so this function
/// stays a pure CLI shell, unit-testable with the mock `CommandRunner`
/// without faking a WebSocket server.
///
/// Race protection mirrors the tmux allocator: the per-base mutex spans
/// list → pick → create, so two concurrent callers can never compute the
/// same `term-N` against the same Worktree. The WebSocket write done by
/// the caller post-allocator is per-session (uniquely named at this point)
/// and doesn't need additional serialisation.
fn allocate_session_for_zellij_tab<R: CommandRunner>(
    locks: &AllocationLocks,
    zellij: &ZellijCli<R>,
    base: &str,
    cwd: &str,
) -> Result<String, ZellijError> {
    let lock = locks.lock_for(base);
    let _guard = lock.lock();
    let prefix = format!("{base}__");
    let existing_suffixes: Vec<String> = zellij
        .list_sessions()?
        .into_iter()
        .filter_map(|s| s.strip_prefix(&prefix).map(str::to_string))
        .collect();
    let window_name = allocate_window_name(&existing_suffixes);
    let session = format!("{base}__{window_name}");
    zellij.new_session(&session, cwd, None)?;
    Ok(window_name)
}

#[tauri::command]
fn close_tab(app: tauri::AppHandle, id: String) -> Result<(), String> {
    // Tauri 2 doesn't expose a stable webview.close() at the time of this
    // sanctel — easiest path is to drop the handle and let GC handle it.
    // For now: hide off-screen + remove from our registry. The webview keeps
    // its memory until the window closes; revisit this when Tauri ships a
    // proper destroy API.
    let _ = hide_webview(&app, &id);

    // For terminal/chat tabs, kill the per-tab session on whichever backend
    // owns it so the shell dies. Each tab owns its own session
    // (`sanctel_wt_<wt>__term-N`) per ADR-0012 revised by issue #15, so a
    // single `kill_session` is the complete cleanup.
    let state = app.state::<AppState>();
    let record = state.tabs.lock().get(&id).cloned();
    if let Some(rec) = record {
        if rec.kind == "terminal" || rec.kind == "chat" {
            if let Some(handle) = state.terminals.remove(&id) {
                match Backend::from_env() {
                    Backend::Tmux => {
                        let _ = TmuxCli::default().kill_session(&handle.session);
                    }
                    Backend::Zellij => {
                        let _ = ZellijCli::default().kill_session(&handle.session);
                    }
                }
            }
        }
    }

    state.tabs.lock().remove(&id);
    let mut active = state.active_tab.lock();
    if active.as_deref() == Some(&id) {
        *active = None;
    }
    Ok(())
}

#[tauri::command]
fn show_tab(app: tauri::AppHandle, id: String) -> Result<(), String> {
    // Hide previous active tab first.
    let prev = app.state::<AppState>().active_tab.lock().clone();
    if let Some(prev_id) = prev {
        if prev_id != id {
            let _ = hide_webview(&app, &prev_id);
        }
    }
    show_webview(&app, &id)
}

#[tauri::command]
fn hide_all(app: tauri::AppHandle) -> Result<(), String> {
    let ids: Vec<String> = app.state::<AppState>().tabs.lock().keys().cloned().collect();
    for id in ids {
        let _ = hide_webview(&app, &id);
    }
    *app.state::<AppState>().active_tab.lock() = None;
    Ok(())
}

#[tauri::command]
fn set_content_rect(app: tauri::AppHandle, rect: Rect) -> Result<(), String> {
    *app.state::<AppState>().content_rect.lock() = rect;

    // If a tab is visible, reposition it to match the new content rect.
    let active = app.state::<AppState>().active_tab.lock().clone();
    if let Some(id) = active {
        let _ = show_webview(&app, &id);
    }
    Ok(())
}

// ─── terminal commands ────────────────────────────────────────────────────
//
// All three commands derive identity (worktree path, window name) from the
// calling webview's label by looking up the TabRecord stored at create_tab
// time. The frontend never passes its own tabId — enforced by the IPC shape.

/// The Worktree-base prefix that groups all sessions belonging to the same
/// Worktree (or detached-profile fallback). The full per-tab session name
/// is `<base>__<windowName>`; the allocator scans existing sessions by this
/// prefix to find the next `term-N`.
///
/// `_` (not `:`) is the separator because tmux parses `:` and `.` as the
/// session/window/pane delimiters in target specs — `tmux list-windows -t
/// sanctel-wt:<id>` reads as "session=sanctel-wt, window=<id>" and fails
/// to address the session sanctel created. Both `<id>` components are
/// passed through `tmux_safe` so a branch name with `/`, `.`, or whitespace
/// (e.g., a Worktree built from `feature/foo`) cannot reintroduce the bug.
fn tmux_session_base(worktree_id: Option<&str>, profile_id: &str) -> String {
    match worktree_id {
        Some(id) => format!("sanctel_wt_{}", tmux_safe(id)),
        None => format!("sanctel_detached_{}", tmux_safe(profile_id)),
    }
}

/// The full tmux session name for one terminal/chat tab. Worktree-keyed
/// tabs land on `sanctel_wt_<worktreeId>__<windowName>`; detached tabs on
/// `sanctel_detached_<profileId>__<windowName>`. One tab → one tmux session
/// (per ADR-0012 revised by issue #15). The Worktree prefix preserves
/// "all tabs for one Worktree grouped together" in `tmux ls` for power
/// users while making sure two clients never attach to the same session
/// (the bug class issue #15 closes).
///
/// `__` is the suffix separator: every sanctel-built id flows through
/// `tmux_safe`, which only ever produces single-`_` runs, so `__`
/// unambiguously marks where the base ends and the window name begins.
fn tmux_session_name(
    worktree_id: Option<&str>,
    profile_id: &str,
    window_name: &str,
) -> String {
    format!(
        "{}__{}",
        tmux_session_base(worktree_id, profile_id),
        window_name
    )
}

/// Resolve identity for a terminal/chat tab. Worktree-keyed tabs (ADR-0012)
/// attach to `sanctel_wt_<worktreeId>__<windowName>` with the Worktree's
/// path as cwd; worktree-less tabs attach to
/// `sanctel_detached_<profileId>__<windowName>` and start in `$HOME`.
/// Per-tab session model (issue #15): the window name is the session-name
/// suffix, allocated server-side at create_tab time under the per-Worktree
/// mutex and stored on the TabRecord. Falls back to `term-1` only when the
/// frontend omitted it (legacy demo path).
fn resolve_attach_params(
    record: &TabRecord,
    cols: u16,
    rows: u16,
) -> Result<AttachParams, String> {
    let window_name = record
        .window_name
        .clone()
        .unwrap_or_else(|| "term-1".to_string());

    let worktree_path = resolve_worktree_cwd(
        record.worktree_id.as_deref(),
        record.worktree_path.as_deref(),
    )?;

    Ok(AttachParams {
        session: tmux_session_name(
            record.worktree_id.as_deref(),
            &record.profile_id,
            &window_name,
        ),
        window_name,
        worktree_path,
        initial_command: record.initial_command.clone(),
        cols,
        rows,
    })
}

#[tauri::command]
fn terminal_attach<R: Runtime>(
    webview: Webview<R>,
    cols: u16,
    rows: u16,
    on_output: Channel<Vec<u8>>,
) -> Result<(), String> {
    let app = webview.app_handle();
    let label = webview.label().to_string();
    let record = app
        .state::<AppState>()
        .tabs
        .lock()
        .get(&label)
        .cloned()
        .ok_or_else(|| format!("no TabRecord for webview '{label}'"))?;

    if record.kind != "terminal" && record.kind != "chat" {
        return Err(format!(
            "terminal_attach called on non-terminal tab '{label}' (kind={})",
            record.kind
        ));
    }

    let params = resolve_attach_params(&record, cols, rows)?;
    // AttachError::Display emits `worktree-missing: <path>` for the broken-tab
    // case, which the frontend pattern-matches in terminal-runtime.ts. Don't
    // wrap or rephrase — the prefix is the wire contract.
    let handle = match Backend::from_env() {
        Backend::Tmux => attach_tab_to_tmux(&TmuxCli::default(), params, on_output)
            .map_err(|e| e.to_string())?,
        Backend::Zellij => {
            let state = app.state::<AppState>();
            let (port, session_token) = {
                let daemon = state.zellij_daemon.lock();
                let d = daemon.as_ref().ok_or_else(|| {
                    "zellij daemon not running — startup probe failed".to_string()
                })?;
                (d.port(), d.session_token())
            };
            attach_tab_to_zellij(
                &ZellijCli::default(),
                port,
                &session_token,
                params,
                on_output,
            )
            .map_err(|e| e.to_string())?
        }
    };
    app.state::<AppState>().terminals.insert(label, Arc::new(handle));
    Ok(())
}

#[tauri::command]
fn tmux_status(app: tauri::AppHandle) -> TmuxStatus {
    app.state::<AppState>().tmux_status.lock().clone()
}

#[tauri::command]
fn terminal_write<R: Runtime>(webview: Webview<R>, bytes: Vec<u8>) -> Result<(), String> {
    let app = webview.app_handle();
    let handle = app
        .state::<AppState>()
        .terminals
        .get(webview.label())
        .ok_or_else(|| "terminal not attached".to_string())?;
    handle.write_bytes(&bytes)
}

#[tauri::command]
fn terminal_resize<R: Runtime>(
    webview: Webview<R>,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let app = webview.app_handle();
    let handle = app
        .state::<AppState>()
        .terminals
        .get(webview.label())
        .ok_or_else(|| "terminal not attached".to_string())?;
    handle.resize(cols, rows)
}


// ─── backend probe (Slice 7 + spike slice 1) ──────────────────────────────

/// Run the one-time `tmux -V` probe and seed AppState.tmux_status. Pure
/// over a TmuxCli so unit tests can inject a mock runner.
fn probe_tmux_into<R: crate::tmux_cli::CommandRunner>(
    status: &Mutex<TmuxStatus>,
    tmux: &TmuxCli<R>,
) {
    let backend = Backend::Tmux.name().to_string();
    let resolved = match tmux.version() {
        Ok(v) => TmuxStatus {
            backend,
            available: true,
            version: Some(v),
            error: None,
        },
        Err(TmuxError::NotFound(msg)) => TmuxStatus {
            backend,
            available: false,
            version: None,
            error: Some(format!("tmux not installed: {msg}")),
        },
        Err(other) => TmuxStatus {
            backend,
            available: false,
            version: None,
            error: Some(other.to_string()),
        },
    };
    *status.lock() = resolved;
}

/// `zellij --version` probe, mirror of `probe_tmux_into`. Reuses the
/// `tmux_status` AppState field by design: the field is structurally a
/// "current backend ready / not ready" signal, and the spike's "no
/// frontend changes" constraint forbids renaming it or adding a parallel
/// command surface. The version string is whatever zellij reports (e.g.,
/// `zellij 0.42.0`) so the frontend setup screen surfaces something
/// recognisable rather than a confusing "tmux 0.42.0".
fn probe_zellij_into<R: crate::tmux_cli::CommandRunner>(
    status: &Mutex<TmuxStatus>,
    zellij: &ZellijCli<R>,
) {
    let backend = Backend::Zellij.name().to_string();
    let resolved = match zellij.version() {
        Ok(v) => TmuxStatus {
            backend,
            available: true,
            version: Some(v),
            error: None,
        },
        Err(ZellijError::NotFound(msg)) => TmuxStatus {
            backend,
            available: false,
            version: None,
            error: Some(format!("zellij not installed: {msg}")),
        },
        Err(other) => TmuxStatus {
            backend,
            available: false,
            version: None,
            error: Some(other.to_string()),
        },
    };
    *status.lock() = resolved;
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        // Issue #6 / Slice 5: SQLite persistence lives entirely in the
        // frontend (sql.js + this fs plugin reading/writing the .db file).
        // Rust never imports a SQLite library or reads the .db directly —
        // every per-Tab fact arrives via `create_tab` arguments.
        .plugin(tauri_plugin_fs::init())
        .manage(AppState::default())
        .setup(|app| {
            // One-time backend startup probe. The current backend is
            // resolved from `SANCTEL_BACKEND` exactly once here — switching
            // mid-process is not a goal. The `tmux_status` field is the
            // structural "current backend ready" signal that the frontend
            // setup-screen gates on (Slice 7 wired this for tmux; the spike
            // reuses it so no frontend changes are needed). When
            // `SANCTEL_BACKEND=zellij`, we additionally spawn the
            // `zellij web` daemon supervisor and stash it on AppState so
            // its Drop tears the child down at sanctel shutdown.
            let state = app.state::<AppState>();
            match Backend::from_env() {
                Backend::Tmux => {
                    probe_tmux_into(&state.tmux_status, &TmuxCli::default());
                }
                Backend::Zellij => {
                    // Must run BEFORE the version probe and daemon launch so
                    // every child zellij subprocess inherits the short socket
                    // path. See `zellij_cli::set_zellij_socket_dir` for the
                    // macOS sun_path arithmetic.
                    crate::zellij_cli::set_zellij_socket_dir(
                        std::path::Path::new("/tmp/sanctel-zellij"),
                    );
                    probe_zellij_into(&state.tmux_status, &ZellijCli::default());
                    if state.tmux_status.lock().available {
                        match ZellijDaemon::start(RealLauncher, RealAuthenticator::default()) {
                            Ok(daemon) => {
                                *state.zellij_daemon.lock() = Some(daemon);
                            }
                            Err(e) => {
                                // Surface the spawn (or auth) failure through
                                // the same available/error channel the
                                // version probe uses; the frontend setup
                                // screen renders identically. Routing auth
                                // failures here is what keeps the failure
                                // mode named (diagnostic detail in the
                                // setup screen) rather than an opaque
                                // HTTP 401 per terminal tab.
                                *state.tmux_status.lock() = TmuxStatus {
                                    backend: Backend::Zellij.name().to_string(),
                                    available: false,
                                    version: None,
                                    error: Some(format!(
                                        "zellij daemon failed to start: {e}"
                                    )),
                                };
                            }
                        }
                    }
                }
            }
            let snapshot = state.tmux_status.lock().clone();
            let _ = app.emit("tmux-status", snapshot);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            create_tab,
            close_tab,
            show_tab,
            hide_all,
            set_content_rect,
            terminal_attach,
            terminal_write,
            terminal_resize,
            tmux_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
// ─── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux_cli::CommandOutput;

    fn record(
        worktree_id: Option<&str>,
        worktree_path: Option<&str>,
        window_name: Option<&str>,
    ) -> TabRecord {
        TabRecord {
            profile_id: "profile-default".into(),
            kind: "terminal".into(),
            worktree_id: worktree_id.map(str::to_string),
            worktree_path: worktree_path.map(str::to_string),
            window_name: window_name.map(str::to_string),
            initial_command: None,
            agent_session_id: None,
        }
    }

    #[test]
    fn worktree_keyed_record_yields_sanctel_wt_session_and_cwd() {
        let rec = record(Some("sanctel-main"), Some("/home/me/code/sanctel"), Some("term-2"));
        let p = resolve_attach_params(&rec, 80, 24).unwrap();
        // Per-tab session model (issue #15): the session name carries the
        // window-name suffix. One tab → one session → one window.
        assert_eq!(p.session, "sanctel_wt_sanctel-main__term-2");
        assert_eq!(p.worktree_path, "/home/me/code/sanctel");
        assert_eq!(p.window_name, "term-2");
    }

    #[test]
    fn detached_record_yields_detached_session_and_home_cwd() {
        let rec = record(None, None, Some("term-1"));
        // $HOME is reliably set in dev/CI environments; if not, the test
        // documents the contract by erroring out — `resolve_attach_params`
        // would surface the same error to the user.
        let p = resolve_attach_params(&rec, 80, 24).unwrap();
        assert_eq!(p.session, "sanctel_detached_profile-default__term-1");
        assert_eq!(p.worktree_path, std::env::var("HOME").unwrap());
    }

    /// Two tabs in the same Worktree but with different windowNames must
    /// land on **distinct** session names. This is the structural reason
    /// the issue-#15 bug class (shared `curw` between two clients on one
    /// session) becomes impossible — the two tabs are never two clients
    /// on one session in the first place. The Worktree prefix is still
    /// shared so `tmux ls` groups them visually.
    #[test]
    fn two_tabs_in_same_worktree_get_distinct_sessions() {
        let a = tmux_session_name(Some("sanctel-main"), "profile-default", "term-1");
        let b = tmux_session_name(Some("sanctel-main"), "profile-default", "term-2");
        assert_ne!(a, b);
        // Both share the Worktree base so a `tmux ls` filter on
        // `sanctel_wt_sanctel-main__` lists exactly this tab-set.
        let base = tmux_session_base(Some("sanctel-main"), "profile-default");
        assert!(a.starts_with(&format!("{base}__")));
        assert!(b.starts_with(&format!("{base}__")));
    }

    /// A worktreeId containing `:` or `.` (e.g., a Worktree built from a
    /// branch like `feature/foo` or `release.2025-05`) must NOT produce a
    /// session name with those characters — tmux would parse them as
    /// target separators and the session would be unreachable. This is the
    /// regression test for issue #13, carried through the per-tab session
    /// rename in issue #15.
    #[test]
    fn session_name_sanitizes_unsafe_characters_in_worktree_id() {
        let session =
            tmux_session_name(Some("feature/foo:bar.baz"), "profile-default", "term-1");
        assert!(!session.contains(':'), "session must not contain ':'");
        assert!(!session.contains('.'), "session must not contain '.'");
        assert!(!session.contains('/'), "session must not contain '/'");
        assert_eq!(session, "sanctel_wt_feature_foo_bar_baz__term-1");
    }

    /// Same regression test for the detached fallback: a profileId with
    /// `:` or `.` cannot produce an unreachable session name.
    #[test]
    fn session_name_sanitizes_unsafe_characters_in_profile_id() {
        let session = tmux_session_name(None, "work:profile.1", "term-1");
        assert!(!session.contains(':'));
        assert!(!session.contains('.'));
        assert_eq!(session, "sanctel_detached_work_profile_1__term-1");
    }

    #[test]
    fn worktree_id_without_path_is_an_error() {
        let rec = record(Some("sanctel-main"), None, Some("term-1"));
        match resolve_attach_params(&rec, 80, 24) {
            Err(msg) => assert!(msg.contains("worktreeId"), "got: {msg}"),
            Ok(_) => panic!("expected an error when worktreeId is set without worktreePath"),
        }
    }

    struct FailingRunner;
    impl CommandRunner for FailingRunner {
        fn run(&self, _: &str, _: &[&str]) -> std::io::Result<CommandOutput> {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "tmux: command not found",
            ))
        }
    }

    struct OkRunner;
    impl CommandRunner for OkRunner {
        fn run(&self, _: &str, _: &[&str]) -> std::io::Result<CommandOutput> {
            Ok(CommandOutput {
                status: 0,
                stdout: b"tmux 3.4\n".to_vec(),
                stderr: vec![],
            })
        }
    }

    /// Probe with a runner that fails to spawn tmux must surface
    /// `available: false` + an error. This is what triggers the React
    /// setup screen on a machine without tmux installed.
    #[test]
    fn probe_marks_unavailable_when_tmux_missing() {
        let status = Mutex::new(TmuxStatus::default());
        let tmux = TmuxCli::new("test", FailingRunner);
        probe_tmux_into(&status, &tmux);
        let result = status.lock().clone();
        assert!(!result.available);
        assert!(result.error.is_some());
    }

    /// Probe with a runner that prints `tmux 3.4` reports available + version.
    #[test]
    fn probe_marks_available_when_tmux_present() {
        let status = Mutex::new(TmuxStatus::default());
        let tmux = TmuxCli::new("test", OkRunner);
        probe_tmux_into(&status, &tmux);
        let result = status.lock().clone();
        assert!(result.available);
        assert_eq!(result.version.as_deref(), Some("tmux 3.4"));
        assert!(result.error.is_none());
    }

    /// The tmux probe must name itself on the `backend` field so the
    /// frontend setup screen renders tmux-flavoured copy and install
    /// instructions even when the probe fails. Pinned on both branches —
    /// success AND failure — because the setup screen only shows up on the
    /// failure branch and that's where mis-labelling would mislead a user.
    #[test]
    fn tmux_probe_names_backend_in_status() {
        let status = Mutex::new(TmuxStatus::default());
        probe_tmux_into(&status, &TmuxCli::new("test", OkRunner));
        assert_eq!(status.lock().backend, "tmux");

        let status = Mutex::new(TmuxStatus::default());
        probe_tmux_into(&status, &TmuxCli::new("test", FailingRunner));
        assert_eq!(status.lock().backend, "tmux");
    }

    // ─── zellij probe (spike slice 1 / issue #17) ────────────────────────

    struct ZellijOkRunner;
    impl CommandRunner for ZellijOkRunner {
        fn run(&self, _: &str, _: &[&str]) -> std::io::Result<CommandOutput> {
            Ok(CommandOutput {
                status: 0,
                stdout: b"zellij 0.42.0\n".to_vec(),
                stderr: vec![],
            })
        }
    }

    /// When zellij is on PATH, the probe writes `available: true` and the
    /// version string into the same `tmux_status` field (reused by design
    /// — the spike forbids frontend changes). The version string carries
    /// the actual backend name so the setup screen can still tell users
    /// which backend it's reporting about.
    #[test]
    fn zellij_probe_marks_available_with_version() {
        let status = Mutex::new(TmuxStatus::default());
        let zellij = ZellijCli::new(ZellijOkRunner);
        probe_zellij_into(&status, &zellij);
        let result = status.lock().clone();
        assert!(result.available);
        assert_eq!(result.version.as_deref(), Some("zellij 0.42.0"));
        assert!(result.error.is_none());
    }

    /// Missing zellij surfaces as `available: false` + an error containing
    /// `zellij not installed` so the setup screen can describe the actual
    /// problem rather than the existing tmux-shaped message.
    #[test]
    fn zellij_probe_marks_unavailable_when_missing() {
        let status = Mutex::new(TmuxStatus::default());
        let zellij = ZellijCli::new(FailingRunner);
        probe_zellij_into(&status, &zellij);
        let result = status.lock().clone();
        assert!(!result.available);
        let err = result.error.expect("error must be populated");
        assert!(
            err.contains("zellij not installed"),
            "error should name zellij, got: {err}"
        );
    }

    /// The zellij probe writes `backend: "zellij"` so the frontend setup-
    /// screen renders zellij-flavoured copy and install instructions when
    /// the spike backend is the one that failed. Pinned on both branches
    /// so the failure path (where the setup screen is actually rendered)
    /// is covered.
    #[test]
    fn zellij_probe_names_backend_in_status() {
        let status = Mutex::new(TmuxStatus::default());
        probe_zellij_into(&status, &ZellijCli::new(ZellijOkRunner));
        assert_eq!(status.lock().backend, "zellij");

        let status = Mutex::new(TmuxStatus::default());
        probe_zellij_into(&status, &ZellijCli::new(FailingRunner));
        assert_eq!(status.lock().backend, "zellij");
    }

    /// The default value of the field — what `TmuxStatus::default()` yields
    /// before any probe has run — must be `"tmux"`. This matches the
    /// frontend's defensive fallback (a missing or malformed value renders
    /// the existing tmux copy) so the two sides agree on which backend is
    /// implied by a bare status.
    #[test]
    fn default_status_names_tmux_backend() {
        assert_eq!(TmuxStatus::default().backend, "tmux");
    }

    /// The dispatcher acceptance criterion: `Backend::from_env_value(None)`
    /// drives the tmux probe and only the tmux probe — wiring this end to
    /// end here so a future contributor can't accidentally invert the
    /// branches in `run()`'s setup hook.
    #[test]
    fn dispatcher_tmux_branch_runs_tmux_probe() {
        let status = Mutex::new(TmuxStatus::default());
        match Backend::from_env_value(None) {
            Backend::Tmux => {
                probe_tmux_into(&status, &TmuxCli::new("test", OkRunner));
            }
            Backend::Zellij => panic!("default branch must select Tmux"),
        }
        assert_eq!(
            status.lock().version.as_deref(),
            Some("tmux 3.4"),
            "tmux branch must produce a tmux-shaped version string",
        );
    }

    /// `SANCTEL_BACKEND=zellij` drives the zellij probe (and, in `run()`,
    /// the daemon supervisor). The probe-only half is unit-testable here;
    /// the supervisor half is exercised by zellij_daemon::tests.
    #[test]
    fn dispatcher_zellij_branch_runs_zellij_probe() {
        let status = Mutex::new(TmuxStatus::default());
        match Backend::from_env_value(Some("zellij")) {
            Backend::Zellij => {
                probe_zellij_into(&status, &ZellijCli::new(ZellijOkRunner));
            }
            Backend::Tmux => panic!("'zellij' env value must select Zellij"),
        }
        assert_eq!(
            status.lock().version.as_deref(),
            Some("zellij 0.42.0"),
            "zellij branch must produce a zellij-shaped version string",
        );
    }

    // ─── windowName allocation under the per-Worktree mutex (issue #10/#15) ───

    /// A tmux fixture: maintains a per-session set of window names, services
    /// has-session / new-session / list-sessions / list-windows / new-window.
    /// Per-tab session model (issue #15) means the allocator drives
    /// `list-sessions` (then filters by prefix), so this fixture grew a
    /// `list-sessions` handler. Each operation holds the inner lock just
    /// long enough to mutate state — without the per-base mutex inside
    /// `allocate_session_for_tab`, concurrent callers would race between
    /// list-sessions and new-session and collide on the same `term-N`.
    struct TmuxStateRunner {
        windows: Arc<Mutex<HashMap<String, Vec<String>>>>,
    }

    impl TmuxStateRunner {
        fn new() -> Self {
            TmuxStateRunner {
                windows: Arc::new(Mutex::new(HashMap::new())),
            }
        }
        fn clone_shared(&self) -> Self {
            TmuxStateRunner {
                windows: Arc::clone(&self.windows),
            }
        }
        fn session_names(&self) -> Vec<String> {
            self.windows.lock().keys().cloned().collect()
        }
    }

    fn arg_after<'a>(args: &'a [&'a str], flag: &str) -> Option<&'a str> {
        args.iter().position(|a| *a == flag).and_then(|i| args.get(i + 1)).copied()
    }

    impl CommandRunner for TmuxStateRunner {
        fn run(&self, _: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            let sub = args
                .iter()
                .find(|a| {
                    matches!(
                        **a,
                        "has-session"
                            | "new-session"
                            | "list-sessions"
                            | "list-windows"
                            | "new-window"
                    )
                })
                .copied()
                .unwrap_or("");

            match sub {
                "has-session" => {
                    let target = arg_after(args, "-t")
                        .map(|s| s.trim_start_matches('=').to_string())
                        .unwrap_or_default();
                    let exists = self.windows.lock().contains_key(&target);
                    Ok(CommandOutput {
                        status: if exists { 0 } else { 1 },
                        stdout: vec![],
                        stderr: if exists {
                            vec![]
                        } else {
                            b"can't find session".to_vec()
                        },
                    })
                }
                "new-session" => {
                    let name = arg_after(args, "-s").unwrap_or_default().to_string();
                    // The new primitive always carries `-n <window_name>`,
                    // so the simulated session is created with exactly that
                    // window — no phantom, no separate `new-window` step.
                    let window_name = arg_after(args, "-n").unwrap_or_default().to_string();
                    let mut map = self.windows.lock();
                    if let std::collections::hash_map::Entry::Vacant(e) = map.entry(name.clone()) {
                        let initial = if window_name.is_empty() {
                            vec![]
                        } else {
                            vec![window_name]
                        };
                        e.insert(initial);
                        Ok(CommandOutput {
                            status: 0,
                            stdout: vec![],
                            stderr: vec![],
                        })
                    } else {
                        Ok(CommandOutput {
                            status: 1,
                            stdout: vec![],
                            stderr: format!("duplicate session: {name}").into_bytes(),
                        })
                    }
                }
                "list-sessions" => {
                    let mut names: Vec<String> =
                        self.windows.lock().keys().cloned().collect();
                    names.sort();
                    Ok(CommandOutput {
                        status: 0,
                        stdout: format!("{}\n", names.join("\n")).into_bytes(),
                        stderr: vec![],
                    })
                }
                "list-windows" => {
                    let target = arg_after(args, "-t")
                        .map(|s| s.trim_start_matches('=').to_string())
                        .unwrap_or_default();
                    let body = self
                        .windows
                        .lock()
                        .get(&target)
                        .cloned()
                        .unwrap_or_default()
                        .join("\n");
                    Ok(CommandOutput {
                        status: 0,
                        stdout: format!("{body}\n").into_bytes(),
                        stderr: vec![],
                    })
                }
                "new-window" => {
                    let target = arg_after(args, "-t")
                        .map(|s| s.trim_start_matches('=').to_string())
                        .unwrap_or_default();
                    let name = arg_after(args, "-n").unwrap_or_default().to_string();
                    let mut map = self.windows.lock();
                    let windows = map.entry(target).or_default();
                    windows.push(name);
                    Ok(CommandOutput {
                        status: 0,
                        stdout: vec![],
                        stderr: vec![],
                    })
                }
                _ => Ok(CommandOutput {
                    status: 0,
                    stdout: vec![],
                    stderr: vec![],
                }),
            }
        }
    }

    /// Spawn N parallel `allocate_session_for_tab` calls against the same
    /// Worktree base. Without the per-base mutex inside the helper,
    /// multiple callers would race between list-sessions and new-session
    /// and end up computing the same `term-N` (then either collide on the
    /// same session name or split into wrong N-counts). With the mutex,
    /// the N callers produce N distinct names `term-1`…`term-N` AND N
    /// distinct session names `<base>__term-1`…`<base>__term-N`, with
    /// no holes. The distinct-session property is the load-bearing
    /// invariant from issue #15.
    #[test]
    fn allocate_session_for_tab_serializes_concurrent_callers() {
        const N: usize = 12;
        let base = "sanctel_wt_race-test";
        let cwd = "/tmp";

        let locks = Arc::new(AllocationLocks::default());
        let runner = TmuxStateRunner::new();

        let handles: Vec<_> = (0..N)
            .map(|_| {
                let runner_clone = runner.clone_shared();
                let locks = Arc::clone(&locks);
                let base = base.to_string();
                let cwd = cwd.to_string();
                std::thread::spawn(move || {
                    let tmux = TmuxCli::new("test-sock", runner_clone);
                    allocate_session_for_tab(&locks, &tmux, &base, &cwd, None)
                })
            })
            .collect();

        let sort_by_term_index = |v: &mut Vec<String>| {
            v.sort_by_key(|s| {
                s.rsplit("term-")
                    .next()
                    .and_then(|n| n.parse::<usize>().ok())
                    .expect("name ends in term-N")
            });
        };

        let mut got: Vec<String> = handles
            .into_iter()
            .map(|h| h.join().unwrap().expect("allocation must succeed"))
            .collect();
        sort_by_term_index(&mut got);

        let expected_names: Vec<String> = (1..=N).map(|i| format!("term-{i}")).collect();
        assert_eq!(
            got, expected_names,
            "N concurrent callers must produce N distinct term-N names with no holes"
        );

        // The fixture must have N distinct sessions, one per tab — this is
        // the load-bearing invariant from issue #15. A pre-fix build would
        // have created one shared session with N windows; the new model
        // creates N sessions with one window each.
        let mut sessions = runner.session_names();
        sort_by_term_index(&mut sessions);
        let expected_sessions: Vec<String> = (1..=N)
            .map(|i| format!("{base}__term-{i}"))
            .collect();
        assert_eq!(sessions, expected_sessions);
    }

    /// Different Worktree bases don't serialize against each other: two
    /// callers against distinct bases both get `term-1` for their first
    /// tab. Smoke-test that the per-base keying isn't accidentally
    /// globalized.
    #[test]
    fn allocate_session_for_tab_does_not_serialize_distinct_worktrees() {
        let locks = AllocationLocks::default();
        let runner = TmuxStateRunner::new();
        let tmux_a = TmuxCli::new("t", runner.clone_shared());
        let tmux_b = TmuxCli::new("t", runner.clone_shared());

        let a =
            allocate_session_for_tab(&locks, &tmux_a, "sanctel_wt_a", "/tmp", None).unwrap();
        let b =
            allocate_session_for_tab(&locks, &tmux_b, "sanctel_wt_b", "/tmp", None).unwrap();
        assert_eq!(a, "term-1");
        assert_eq!(b, "term-1");
    }

    /// Allocator scan correctness: when sibling sessions with the same
    /// base prefix already exist, the next allocation continues the
    /// monotonic `term-N` counter — picking max + 1, not lowest free.
    /// The scan must IGNORE sessions on other bases (a different
    /// Worktree) and sessions whose suffix doesn't match the `term-N`
    /// pattern.
    #[test]
    fn allocate_session_for_tab_scans_existing_sibling_sessions() {
        let locks = AllocationLocks::default();
        let runner = TmuxStateRunner::new();
        // Seed: two prior tabs on this Worktree + one on a different
        // Worktree. The unrelated session must not perturb the counter.
        {
            let mut map = runner.windows.lock();
            map.insert("sanctel_wt_target__term-1".into(), vec!["term-1".into()]);
            map.insert("sanctel_wt_target__term-3".into(), vec!["term-3".into()]);
            map.insert("sanctel_wt_other__term-1".into(), vec!["term-1".into()]);
        }

        let tmux = TmuxCli::new("t", runner.clone_shared());
        let next =
            allocate_session_for_tab(&locks, &tmux, "sanctel_wt_target", "/tmp", None)
                .unwrap();
        // Max term-N on the target base is 3 → next is term-4. The
        // unrelated `sanctel_wt_other__term-1` must NOT push us to 5.
        assert_eq!(next, "term-4");

        // The new session is `sanctel_wt_target__term-4` — visible in the
        // fixture, isolated from the other Worktree's sessions.
        let sessions = runner.session_names();
        assert!(sessions.contains(&"sanctel_wt_target__term-4".to_string()));
        assert!(sessions.contains(&"sanctel_wt_other__term-1".to_string()));
    }

    // ─── zellij allocator ────────────────────────────────────────────────

    /// A zellij-shaped runner that maintains a session set in memory.
    /// `list-sessions` returns the sorted set; `attach --create-background
    /// <name>` adds (collision = "session already exists"); `kill-session
    /// <name>` removes. Mirrors `TmuxStateRunner`'s shape so the zellij
    /// allocator tests can reuse the same concurrency-stress pattern.
    struct ZellijStateRunner {
        sessions: Arc<Mutex<std::collections::HashSet<String>>>,
    }

    impl ZellijStateRunner {
        fn new() -> Self {
            ZellijStateRunner {
                sessions: Arc::new(Mutex::new(std::collections::HashSet::new())),
            }
        }
        fn clone_shared(&self) -> Self {
            ZellijStateRunner {
                sessions: Arc::clone(&self.sessions),
            }
        }
        fn session_names(&self) -> Vec<String> {
            self.sessions.lock().iter().cloned().collect()
        }
        fn seed(&self, names: &[&str]) {
            let mut s = self.sessions.lock();
            for n in names {
                s.insert((*n).to_string());
            }
        }
    }

    impl CommandRunner for ZellijStateRunner {
        fn run(&self, _: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            let sub = args
                .iter()
                .find(|a| matches!(**a, "list-sessions" | "kill-session"))
                .copied()
                .unwrap_or("");
            match sub {
                "list-sessions" => {
                    let mut names: Vec<String> =
                        self.sessions.lock().iter().cloned().collect();
                    names.sort();
                    if names.is_empty() {
                        return Ok(CommandOutput {
                            status: 1,
                            stdout: vec![],
                            stderr: b"No active zellij sessions found.\n".to_vec(),
                        });
                    }
                    Ok(CommandOutput {
                        status: 0,
                        stdout: format!("{}\n", names.join("\n")).into_bytes(),
                        stderr: vec![],
                    })
                }
                "kill-session" => {
                    let pos = args.iter().position(|a| *a == "kill-session").unwrap();
                    let name = args.get(pos + 1).copied().unwrap_or("");
                    self.sessions.lock().remove(name);
                    Ok(CommandOutput {
                        status: 0,
                        stdout: vec![],
                        stderr: vec![],
                    })
                }
                _ => Ok(CommandOutput {
                    status: 0,
                    stdout: vec![],
                    stderr: vec![],
                }),
            }
        }

        fn run_in_dir(
            &self,
            program: &str,
            args: &[&str],
            _cwd: &str,
        ) -> std::io::Result<CommandOutput> {
            // `new_session` invokes `zellij attach --create-background <name>`.
            if args.contains(&"--create-background") {
                let name = args.last().copied().unwrap_or("");
                let mut sessions = self.sessions.lock();
                if sessions.contains(name) {
                    return Ok(CommandOutput {
                        status: 1,
                        stdout: vec![],
                        stderr: format!("session already exists: {name}").into_bytes(),
                    });
                }
                sessions.insert(name.to_string());
                return Ok(CommandOutput {
                    status: 0,
                    stdout: vec![],
                    stderr: vec![],
                });
            }
            self.run(program, args)
        }
    }

    /// N concurrent callers against the same Worktree base each land on
    /// their OWN session, not on a shared one. Without the per-base mutex
    /// (which the allocator inherits from the tmux version via
    /// `AllocationLocks`), the cold-start path would see `list-sessions`
    /// return empty for all callers and they would all pick `term-1`,
    /// colliding on `new_session`. Per-tab session isolation is the
    /// load-bearing invariant captured in ADR-0012.
    #[test]
    fn allocate_session_for_zellij_tab_serializes_concurrent_callers() {
        const N: usize = 12;
        let base = "sanctel_wt_race-test";
        let cwd = "/tmp";

        let locks = Arc::new(AllocationLocks::default());
        let runner = ZellijStateRunner::new();

        let handles: Vec<_> = (0..N)
            .map(|_| {
                let runner_clone = runner.clone_shared();
                let locks = Arc::clone(&locks);
                let base = base.to_string();
                let cwd = cwd.to_string();
                std::thread::spawn(move || {
                    let zellij = ZellijCli::new(runner_clone);
                    allocate_session_for_zellij_tab(&locks, &zellij, &base, &cwd)
                })
            })
            .collect();

        let sort_by_term_index = |v: &mut Vec<String>| {
            v.sort_by_key(|s| {
                s.rsplit("term-")
                    .next()
                    .and_then(|n| n.parse::<usize>().ok())
                    .expect("name ends in term-N")
            });
        };

        let mut got: Vec<String> = handles
            .into_iter()
            .map(|h| h.join().unwrap().expect("allocation must succeed"))
            .collect();
        sort_by_term_index(&mut got);

        let expected_names: Vec<String> = (1..=N).map(|i| format!("term-{i}")).collect();
        assert_eq!(
            got, expected_names,
            "N concurrent callers must produce N distinct term-N names with no holes"
        );

        let mut sessions = runner.session_names();
        sort_by_term_index(&mut sessions);
        let expected_sessions: Vec<String> = (1..=N)
            .map(|i| format!("{base}__term-{i}"))
            .collect();
        assert_eq!(sessions, expected_sessions);
    }

    /// On a fresh zellij server, the allocator's `list_sessions()` call
    /// hits the cold-start path (zellij exits non-zero with "No active
    /// zellij sessions found."). `zellij_cli::list_sessions` translates
    /// that to `Ok(Vec::new())`; the allocator must then pick `term-1`.
    /// This is the very first chat-tab on a freshly started sanctel —
    /// regression here breaks every "new chat" on a new install.
    #[test]
    fn allocate_session_for_zellij_tab_picks_term_1_on_cold_start() {
        let locks = AllocationLocks::default();
        let runner = ZellijStateRunner::new();
        let zellij = ZellijCli::new(runner.clone_shared());
        let next =
            allocate_session_for_zellij_tab(&locks, &zellij, "sanctel_wt_a", "/tmp")
                .unwrap();
        assert_eq!(next, "term-1");
        assert!(runner
            .session_names()
            .iter()
            .any(|s| s == "sanctel_wt_a__term-1"));
    }

    /// Allocator scan correctness on the zellij path: the next allocation
    /// continues the monotonic `term-N` counter against the target base,
    /// ignoring siblings on other Worktrees and any non-`term-N`-suffix
    /// sessions left over from prior sanctel versions / external creates.
    #[test]
    fn allocate_session_for_zellij_tab_scans_existing_zellij_sessions() {
        let locks = AllocationLocks::default();
        let runner = ZellijStateRunner::new();
        runner.seed(&[
            "sanctel_wt_target__term-1",
            "sanctel_wt_target__term-3",
            "sanctel_wt_other__term-1",
            // An external `zellij`-created session that doesn't match our
            // naming pattern — must not perturb the counter.
            "my-debug-session",
        ]);

        let zellij = ZellijCli::new(runner.clone_shared());
        let next =
            allocate_session_for_zellij_tab(&locks, &zellij, "sanctel_wt_target", "/tmp")
                .unwrap();
        // Max term-N on target is 3 → next is term-4.
        assert_eq!(next, "term-4");
        let sessions = runner.session_names();
        assert!(sessions.contains(&"sanctel_wt_target__term-4".to_string()));
        // Sibling Worktree's session is untouched.
        assert!(sessions.contains(&"sanctel_wt_other__term-1".to_string()));
    }

    /// Different Worktree bases don't serialize against each other on
    /// the zellij path either: two callers against distinct bases both
    /// get `term-1`. The per-base keying mirrors the tmux allocator's
    /// behavior so two Worktrees can have parallel chat-tab spawns.
    #[test]
    fn allocate_session_for_zellij_tab_does_not_serialize_distinct_worktrees() {
        let locks = AllocationLocks::default();
        let runner = ZellijStateRunner::new();
        let zellij_a = ZellijCli::new(runner.clone_shared());
        let zellij_b = ZellijCli::new(runner.clone_shared());
        let a =
            allocate_session_for_zellij_tab(&locks, &zellij_a, "sanctel_wt_a", "/tmp")
                .unwrap();
        let b =
            allocate_session_for_zellij_tab(&locks, &zellij_b, "sanctel_wt_b", "/tmp")
                .unwrap();
        assert_eq!(a, "term-1");
        assert_eq!(b, "term-1");
    }

    /// The allocator never splices `initial_command` into zellij's argv —
    /// `attach --create-background` has no CLI flag for it, and the
    /// chat-tab command instead reaches the pane via the WS write that
    /// `create_tab` does separately. `zellij_cli::new_session` already
    /// pins the no-splice contract at its layer; this test pins it at the
    /// allocator layer so a future contributor can't sneak it in here
    /// either.
    #[test]
    fn allocate_session_for_zellij_tab_does_not_pass_initial_command_to_new_session() {
        // Recording runner: pushes argv into an external Arc<Mutex<Vec>>
        // so the assertions can read it back without touching ZellijCli's
        // private `runner` field.
        struct RecordingRunner {
            seen: Arc<Mutex<Vec<Vec<String>>>>,
        }
        impl CommandRunner for RecordingRunner {
            fn run(&self, _: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
                self.seen
                    .lock()
                    .push(args.iter().map(|s| s.to_string()).collect());
                if args.contains(&"list-sessions") {
                    return Ok(CommandOutput {
                        status: 1,
                        stdout: vec![],
                        stderr: b"No active zellij sessions found.\n".to_vec(),
                    });
                }
                Ok(CommandOutput {
                    status: 0,
                    stdout: vec![],
                    stderr: vec![],
                })
            }
            fn run_in_dir(
                &self,
                _: &str,
                args: &[&str],
                _: &str,
            ) -> std::io::Result<CommandOutput> {
                self.seen
                    .lock()
                    .push(args.iter().map(|s| s.to_string()).collect());
                Ok(CommandOutput {
                    status: 0,
                    stdout: vec![],
                    stderr: vec![],
                })
            }
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let zellij = ZellijCli::new(RecordingRunner {
            seen: Arc::clone(&seen),
        });
        let locks = AllocationLocks::default();
        allocate_session_for_zellij_tab(&locks, &zellij, "sanctel_wt_x", "/tmp").unwrap();
        let calls = seen.lock().clone();
        // No call should carry a `claude` token (or any token resembling
        // a chat command) — the allocator's signature doesn't accept
        // initial_command, but a future contributor adding it must not
        // silently splice without redesign.
        for argv in &calls {
            assert!(
                !argv.iter().any(|a| a.contains("claude")),
                "argv must not splice initial_command: {argv:?}"
            );
        }
        // The `attach --create-background` argv must be exactly three
        // positional tokens — same shape `zellij_cli::tests` pins.
        let attach_call = calls
            .iter()
            .find(|a| a.iter().any(|t| t == "--create-background"))
            .expect("allocator must invoke attach --create-background");
        assert_eq!(
            attach_call,
            &vec![
                "attach".to_string(),
                "--create-background".to_string(),
                "sanctel_wt_x__term-1".to_string(),
            ],
        );
    }
}
