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

mod profile_isolation;
mod terminal_runtime;
mod tmux_cli;

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::webview::WebviewBuilder;
use tauri::{Emitter, LogicalPosition, LogicalSize, Manager, Runtime, Webview, WebviewUrl};

use crate::profile_isolation::apply_profile_isolation;
use crate::terminal_runtime::{attach_tab_to_tmux, AttachParams, TerminalRegistry};
use crate::tmux_cli::{allocate_window_name, CommandRunner, TmuxCli, TmuxError};

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
    // Per-tmux-session locks. `create_tab` grabs the inner mutex for the
    // session it's about to touch, then atomically lists existing window
    // names, computes the next `term-N`, and calls `new-window`. Without
    // the lock, two concurrent callers in the same session both read
    // [term-1], both compute term-2, and tmux happily creates two windows
    // with the same name.
    session_locks: SessionLocks,
}

/// Per-tmux-session mutex map. The outer mutex protects the HashMap; the
/// inner `Arc<Mutex<()>>` is held during the list-windows → compute-name →
/// new-window critical section in `create_tab`.
#[derive(Default)]
struct SessionLocks {
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl SessionLocks {
    /// Returns the per-session mutex, creating it on first use. The caller
    /// is expected to drop the returned Arc's MutexGuard before returning
    /// from its critical section; the outer map mutex is held only long
    /// enough to insert/get.
    fn lock_for(&self, session: &str) -> Arc<Mutex<()>> {
        self.locks
            .lock()
            .entry(session.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

/// Result of the one-time `tmux -V` probe. Emitted as a Tauri event and also
/// readable via the `tmux_status` command so React can render synchronously
/// on first paint without waiting for the event.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct TmuxStatus {
    available: bool,
    version: Option<String>,
    error: Option<String>,
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
    /// `worktreeId` keys the tmux session (`sanctel-wt:<id>` per ADR-0012);
    /// `worktreePath` is the cwd passed to `tmux -c` so the shell starts in
    /// the right directory. Both are None for detached terminal tabs.
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
    // per-session mutex when the request asked for "auto" allocation. Doing
    // it before we build the webview means a failure here surfaces to React
    // before any client-visible state is created.
    let asked_for_auto = is_terminal_like
        && matches!(req.window_name.as_deref(), None | Some(AUTO_WINDOW_NAME));
    let allocated_window_name: Option<String> = if asked_for_auto {
        let cwd =
            resolve_worktree_cwd(req.worktree_id.as_deref(), req.worktree_path.as_deref())?;
        let session = tmux_session_name(req.worktree_id.as_deref(), &req.profile_id);
        let locks = &app.state::<AppState>().session_locks;
        let tmux = TmuxCli::default();
        Some(
            allocate_window_under_lock(
                locks,
                &tmux,
                &session,
                &cwd,
                req.initial_command.as_deref(),
            )
            .map_err(|e| e.to_string())?,
        )
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
/// per-session mutex, ensure the session exists, list existing windows,
/// compute the next `term-N`, and call `new-window`. Returns the resolved
/// name. Generic over `CommandRunner` so the concurrency test below can
/// drive it with a scripted-tmux fixture without spawning real processes.
fn allocate_window_under_lock<R: CommandRunner>(
    locks: &SessionLocks,
    tmux: &TmuxCli<R>,
    session: &str,
    cwd: &str,
    initial_command: Option<&str>,
) -> Result<String, TmuxError> {
    let lock = locks.lock_for(session);
    let _guard = lock.lock();
    tmux.ensure_session(session, cwd)?;
    let existing = tmux.list_windows(session)?;
    let name = allocate_window_name(&existing);
    tmux.new_window(session, &name, cwd, initial_command)?;
    Ok(name)
}

#[tauri::command]
fn close_tab(app: tauri::AppHandle, id: String) -> Result<(), String> {
    // Tauri 2 doesn't expose a stable webview.close() at the time of this
    // sanctel — easiest path is to drop the handle and let GC handle it.
    // For now: hide off-screen + remove from our registry. The webview keeps
    // its memory until the window closes; revisit this when Tauri ships a
    // proper destroy API.
    let _ = hide_webview(&app, &id);

    // For terminal/chat tabs, ask tmux to kill the window so the shell dies.
    // tmux automatically destroys the session when its last window closes,
    // so we don't have to track that ourselves.
    let state = app.state::<AppState>();
    let record = state.tabs.lock().get(&id).cloned();
    if let Some(rec) = record {
        if rec.kind == "terminal" || rec.kind == "chat" {
            if let Some(handle) = state.terminals.remove(&id) {
                let tmux = TmuxCli::default();
                let _ = tmux.kill_window(&handle.session, &handle.window_name);
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

/// The tmux session name for a (worktreeId, profileId) pair. Worktree-keyed
/// tabs land on `sanctel-wt:<id>` per ADR-0012; detached tabs share one
/// `sanctel-detached:<profileId>` session. Single source of truth for the
/// naming convention — every command that maps to a tmux session goes
/// through here.
fn tmux_session_name(worktree_id: Option<&str>, profile_id: &str) -> String {
    match worktree_id {
        Some(id) => format!("sanctel-wt:{id}"),
        None => format!("sanctel-detached:{profile_id}"),
    }
}

/// Resolve identity for a terminal/chat tab. Worktree-keyed tabs (ADR-0012)
/// attach to `sanctel-wt:<worktreeId>` with the Worktree's path as cwd;
/// worktree-less tabs attach to `sanctel-detached:<profileId>` and start in
/// `$HOME`. Window name is allocated server-side at create_tab time (under
/// a per-session mutex) and stored on the TabRecord — falling back to
/// "term-1" only when the frontend omitted it (legacy demo path).
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
        session: tmux_session_name(record.worktree_id.as_deref(), &record.profile_id),
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
    let tmux = TmuxCli::default();
    // AttachError::Display emits `worktree-missing: <path>` for the broken-tab
    // case, which the frontend pattern-matches in terminal-runtime.ts. Don't
    // wrap or rephrase — the prefix is the wire contract.
    let handle = attach_tab_to_tmux(&tmux, params, on_output).map_err(|e| e.to_string())?;
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
    handle.with_writer(|w| w.write_all(&bytes).map_err(|e| e.to_string()))
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
    handle.with_master(|m| {
        m.resize(portable_pty::PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())
    })
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
        assert_eq!(p.session, "sanctel-wt:sanctel-main");
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
        assert_eq!(p.session, "sanctel-detached:profile-default");
        assert_eq!(p.worktree_path, std::env::var("HOME").unwrap());
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

    // ─── windowName allocation under the per-session mutex (issue #10) ────

    /// A tmux fixture: maintains a per-session set of window names, services
    /// has-session / new-session / list-windows / new-window. Each operation
    /// holds the inner lock just long enough to mutate state, so without the
    /// per-session mutex inside `allocate_window_under_lock` the
    /// list-windows → new-window window is wide open and concurrent callers
    /// would collide on the same `term-N`.
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
        fn names_for(&self, session: &str) -> Vec<String> {
            self.windows
                .lock()
                .get(session)
                .cloned()
                .unwrap_or_default()
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
                        "has-session" | "new-session" | "list-windows" | "new-window"
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
                    let mut map = self.windows.lock();
                    if let std::collections::hash_map::Entry::Vacant(e) = map.entry(name.clone()) {
                        // Sessions start with one default window "0"; matches
                        // tmux behavior closely enough for the allocator test.
                        e.insert(vec![]);
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

    /// Spawn N parallel `allocate_window_under_lock` calls against the same
    /// session. Without the per-session mutex inside the helper, multiple
    /// callers would race between list-windows and new-window and end up
    /// computing the same `term-N`. With the mutex, the N callers produce N
    /// distinct names `term-1`…`term-N`, with no holes.
    #[test]
    fn allocate_window_under_lock_serializes_concurrent_callers() {
        const N: usize = 12;
        let session = "sanctel-wt:race-test";
        let cwd = "/tmp";

        let locks = Arc::new(SessionLocks::default());
        let runner = TmuxStateRunner::new();

        let handles: Vec<_> = (0..N)
            .map(|_| {
                let runner_clone = runner.clone_shared();
                let locks = Arc::clone(&locks);
                let session = session.to_string();
                let cwd = cwd.to_string();
                std::thread::spawn(move || {
                    let tmux = TmuxCli::new("test-sock", runner_clone);
                    allocate_window_under_lock(&locks, &tmux, &session, &cwd, None)
                })
            })
            .collect();

        let mut got: Vec<String> = handles
            .into_iter()
            .map(|h| h.join().unwrap().expect("allocation must succeed"))
            .collect();
        got.sort();

        let expected: Vec<String> = (1..=N).map(|i| format!("term-{i}")).collect();
        assert_eq!(got, expected, "N concurrent callers must produce N distinct term-N names with no holes");

        // The fixture's stored window list must match too — the mutex makes
        // the new-window call atomic with the listing that drove the name
        // choice.
        let mut tmux_windows = runner.names_for(session);
        tmux_windows.sort();
        assert_eq!(tmux_windows, expected);
    }

    /// Different sessions don't serialize against each other: two callers
    /// against distinct sessions both get `term-1`. Smoke-test that the
    /// per-session keying isn't accidentally globalized.
    #[test]
    fn allocate_window_under_lock_does_not_serialize_distinct_sessions() {
        let locks = SessionLocks::default();
        let runner = TmuxStateRunner::new();
        let tmux_a = TmuxCli::new("t", runner.clone_shared());
        let tmux_b = TmuxCli::new("t", runner.clone_shared());

        let a = allocate_window_under_lock(
            &locks,
            &tmux_a,
            "sanctel-wt:a",
            "/tmp",
            None,
        )
        .unwrap();
        let b = allocate_window_under_lock(
            &locks,
            &tmux_b,
            "sanctel-wt:b",
            "/tmp",
            None,
        )
        .unwrap();
        assert_eq!(a, "term-1");
        assert_eq!(b, "term-1");
    }
}

// ─── tmux probe (Slice 7) ─────────────────────────────────────────────────

/// Run the one-time `tmux -V` probe and seed AppState.tmux_status. Pure
/// over a TmuxCli so unit tests can inject a mock runner.
fn probe_tmux_into<R: crate::tmux_cli::CommandRunner>(
    status: &Mutex<TmuxStatus>,
    tmux: &TmuxCli<R>,
) {
    let resolved = match tmux.version() {
        Ok(v) => TmuxStatus {
            available: true,
            version: Some(v),
            error: None,
        },
        Err(TmuxError::NotFound(msg)) => TmuxStatus {
            available: false,
            version: None,
            error: Some(format!("tmux not installed: {msg}")),
        },
        Err(other) => TmuxStatus {
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
            // One-time tmux startup probe (issue #8 / Slice 7). React listens
            // for `tmux-status` once and gates terminal/chat tab creation
            // behind a setup screen if tmux is unavailable. The status is
            // also exposed via the `tmux_status` command for synchronous
            // first-paint reads.
            let state = app.state::<AppState>();
            probe_tmux_into(&state.tmux_status, &TmuxCli::default());
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
