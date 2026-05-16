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
//   - Identity isolation (Arc's "Profile" concept): each webview is created
//     with `with_profile_name(profile_id)`, giving it a cookie /
//     localStorage / IndexedDB store keyed to that profile. All tabs across
//     all Spaces that share a profile_id share the same cookies. The
//     frontend computes profile_id via `space.profileId` and sends it.
// ───────────────────────────────────────────────────────────────────────────

mod terminal_runtime;
mod tmux_cli;

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::Deserialize;
use tauri::ipc::Channel;
use tauri::webview::WebviewBuilder;
use tauri::{LogicalPosition, LogicalSize, Manager, Runtime, Webview, WebviewUrl};

use crate::terminal_runtime::{attach_tab_to_tmux, AttachParams, TerminalRegistry};
use crate::tmux_cli::TmuxCli;

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
}

#[derive(Clone)]
struct TabRecord {
    profile_id: String,
    kind: String,
    // Server-held terminal identity. Slice 2 always populates these from
    // hardcoded defaults at create_tab time for terminal/chat kinds. Later
    // slices (worktree-aware, persistence, chat) will flow real values in.
    worktree_id: Option<String>,
    window_name: Option<String>,
    initial_command: Option<String>,
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
    /// Maps directly to Tauri's `WebviewBuilder::with_profile_name`.
    /// All tabs sharing this `profile_id` share cookies/localStorage.
    /// In Arc terms: this is the Profile, NOT the Space. The frontend
    /// computes it via `space.profileId` and sends it directly.
    profile_id: String,
    /// Terminal/chat tabs carry server-held identity from create_tab time.
    /// Slice 2 lets the frontend leave these unset and falls back to
    /// hardcoded defaults; later slices flow real worktree-aware values.
    worktree_id: Option<String>,
    window_name: Option<String>,
    initial_command: Option<String>,
}

#[tauri::command]
fn create_tab(app: tauri::AppHandle, req: CreateTabReq) -> Result<(), String> {
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

    // Build the webview. `with_profile_name` gives this webview a data store
    // scoped to the given profile_id. ALL tabs across ALL spaces that share a
    // profile_id share cookies/localStorage. This is the Arc model: Profile is
    // the cookie boundary; Space is purely organizational.
    //
    // Platform notes:
    //   - Windows (WebView2): profile_name cleanly isolates user data.
    //   - macOS (WKWebView): mapped to a separate WKWebsiteDataStore per name.
    //   - Linux (WebKitGTK): isolated per WebContext keyed by name.
    let builder = WebviewBuilder::new(&req.id, webview_url)
        .with_profile_name(&req.profile_id)
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

    // Record it.
    app.state::<AppState>().tabs.lock().insert(
        req.id.clone(),
        TabRecord {
            profile_id: req.profile_id,
            kind: req.kind,
            worktree_id: req.worktree_id,
            window_name: req.window_name,
            initial_command: req.initial_command,
        },
    );

    // Hide currently-active tab (if any), then make this new tab visible.
    let prev = app.state::<AppState>().active_tab.lock().clone();
    if let Some(prev_id) = prev {
        let _ = hide_webview(&app, &prev_id);
    }
    show_webview(&app, &req.id)?;

    Ok(())
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

/// Resolve identity for a terminal/chat tab. Slice 2 falls back to hardcoded
/// defaults ($HOME + "term-1") when create_tab didn't carry per-Worktree
/// values, so the demo works without a Worktree UI. Later slices remove the
/// fallback once create_tab always carries real values.
fn resolve_attach_params(
    record: &TabRecord,
    cols: u16,
    rows: u16,
) -> Result<AttachParams, String> {
    let window_name = record
        .window_name
        .clone()
        .unwrap_or_else(|| "term-1".to_string());
    let worktree_path = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;

    let session = match &record.worktree_id {
        Some(id) => format!("sanctel-wt:{id}"),
        None => format!("sanctel-detached:{}", record.profile_id),
    };

    Ok(AttachParams {
        session,
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
    let handle = attach_tab_to_tmux(&tmux, params, on_output).map_err(|e| e.to_string())?;
    app.state::<AppState>().terminals.insert(label, Arc::new(handle));
    Ok(())
}

#[tauri::command]
fn terminal_write<R: Runtime>(webview: Webview<R>, bytes: Vec<u8>) -> Result<(), String> {
    let app = webview.app_handle();
    let handle = app
        .state::<AppState>()
        .terminals
        .get(webview.label())
        .ok_or_else(|| "terminal not attached".to_string())?;
    handle.writer.lock().write_all(&bytes).map_err(|e| e.to_string())
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
    handle
        .master
        .lock()
        .resize(portable_pty::PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())
}

// ─── entry ────────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            create_tab,
            close_tab,
            show_tab,
            hide_all,
            set_content_rect,
            terminal_attach,
            terminal_write,
            terminal_resize,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
