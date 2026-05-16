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

use parking_lot::Mutex;
use serde::Deserialize;
use std::collections::HashMap;
use tauri::{LogicalPosition, LogicalSize, Manager, WebviewUrl};
use tauri::webview::WebviewBuilder;

// ─── shared state ─────────────────────────────────────────────────────────

#[derive(Default)]
struct AppState {
    // tab_id → record (profile_id retained for diagnostics / future ops)
    tabs: Mutex<HashMap<String, TabRecord>>,
    // The React shell's content rect (x, y, w, h) inside the window.
    // Updated whenever React resizes its content area.
    content_rect: Mutex<Rect>,
    // ID of the tab currently visible (positioned over the content area).
    active_tab: Mutex<Option<String>>,
}

#[derive(Clone)]
struct TabRecord {
    profile_id: String,
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
    app.state::<AppState>().tabs.lock().remove(&id);
    let mut active = app.state::<AppState>().active_tab.lock();
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
