use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Runtime};

use crate::hook_handler;

const DEBOUNCE_WINDOW: Duration = Duration::from_millis(100);
const AGENT_SESSION_CAPTURED_EVENT: &str = "agent-session-captured";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionCapture {
    pub session_name: String,
    pub agent: String,
    pub session_id: String,
    pub ts: u64,
}

#[derive(Deserialize)]
struct AgentSessionSidecar {
    agent: String,
    session_id: String,
    ts: u64,
}

#[derive(Default)]
struct AgentSessionWatcherState {
    captures: Mutex<HashMap<String, AgentSessionCapture>>,
}

pub struct AgentSessionWatcher {
    _watcher: RecommendedWatcher,
    _state: Arc<AgentSessionWatcherState>,
    _thread: thread::JoinHandle<()>,
}

pub fn drain_pending_agent_captures() -> Result<Vec<AgentSessionCapture>, String> {
    read_agent_session_captures(&hook_handler::default_hooks_dir()?)
}

pub fn read_agent_session_captures(hooks_dir: &Path) -> Result<Vec<AgentSessionCapture>, String> {
    let mut captures = read_agent_session_capture_map(hooks_dir)?
        .into_values()
        .collect::<Vec<_>>();
    captures.sort_by(|a, b| a.session_name.cmp(&b.session_name));
    Ok(captures)
}

pub fn start<R: Runtime>(app: AppHandle<R>) -> Result<AgentSessionWatcher, String> {
    let hooks_dir = hook_handler::default_hooks_dir()?;
    std::fs::create_dir_all(&hooks_dir)
        .map_err(|e| format!("create hooks dir for watcher failed: {e}"))?;

    let state = Arc::new(AgentSessionWatcherState::default());
    if let Ok(captures) = read_agent_session_capture_map(&hooks_dir) {
        *state.captures.lock() = captures;
    }

    let (tx, rx) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(
        move |result| {
            let _ = tx.send(result);
        },
        Config::default(),
    )
    .map_err(|e| format!("create hooks watcher failed: {e}"))?;
    watcher
        .watch(&hooks_dir, RecursiveMode::NonRecursive)
        .map_err(|e| format!("watch hooks dir failed: {e}"))?;

    let thread_state = Arc::clone(&state);
    let thread = thread::spawn(move || {
        watch_loop(app, hooks_dir, thread_state, rx);
    });

    Ok(AgentSessionWatcher {
        _watcher: watcher,
        _state: state,
        _thread: thread,
    })
}

fn watch_loop<R: Runtime>(
    app: AppHandle<R>,
    hooks_dir: PathBuf,
    state: Arc<AgentSessionWatcherState>,
    rx: mpsc::Receiver<notify::Result<Event>>,
) {
    while let Ok(first) = rx.recv() {
        let mut paths = HashSet::new();
        let mut full_rescan = false;
        collect_watch_result(first, &mut paths, &mut full_rescan);

        while let Ok(next) = rx.recv_timeout(DEBOUNCE_WINDOW) {
            collect_watch_result(next, &mut paths, &mut full_rescan);
        }

        if full_rescan {
            match read_agent_session_capture_map(&hooks_dir) {
                Ok(captures) => {
                    let values = captures.values().cloned().collect::<Vec<_>>();
                    *state.captures.lock() = captures;
                    emit_captures(&app, values);
                }
                Err(e) => eprintln!("agent session capture full rescan failed: {e}"),
            }
            continue;
        }

        for path in paths {
            match read_agent_session_capture_file(&path) {
                Ok(capture) => {
                    state
                        .captures
                        .lock()
                        .insert(capture.session_name.clone(), capture.clone());
                    let _ = app.emit(AGENT_SESSION_CAPTURED_EVENT, capture);
                }
                Err(e) => eprintln!("agent session capture read failed: {e}"),
            }
        }
    }
}

fn collect_watch_result(
    result: notify::Result<Event>,
    paths: &mut HashSet<PathBuf>,
    full_rescan: &mut bool,
) {
    let Ok(event) = result else {
        *full_rescan = true;
        return;
    };

    if matches!(event.kind, EventKind::Other) {
        *full_rescan = true;
        return;
    }

    if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
        return;
    }

    for path in event.paths {
        if is_sidecar_path(&path) {
            paths.insert(path);
        }
    }
}

fn emit_captures<R: Runtime>(app: &AppHandle<R>, captures: Vec<AgentSessionCapture>) {
    for capture in captures {
        let _ = app.emit(AGENT_SESSION_CAPTURED_EVENT, capture);
    }
}

fn read_agent_session_capture_map(
    hooks_dir: &Path,
) -> Result<HashMap<String, AgentSessionCapture>, String> {
    if !hooks_dir.exists() {
        return Ok(HashMap::new());
    }

    let entries =
        std::fs::read_dir(hooks_dir).map_err(|e| format!("read hooks dir failed: {e}"))?;
    let mut captures = HashMap::new();
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if !is_sidecar_path(&path) {
            continue;
        }
        match read_agent_session_capture_file(&path) {
            Ok(capture) => {
                captures.insert(capture.session_name.clone(), capture);
            }
            Err(e) => eprintln!("skip malformed agent session sidecar: {e}"),
        }
    }
    Ok(captures)
}

fn read_agent_session_capture_file(path: &Path) -> Result<AgentSessionCapture, String> {
    let session_name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| format!("sidecar path has no session name: {}", path.display()))?
        .to_string();
    let body = std::fs::read_to_string(path)
        .map_err(|e| format!("read sidecar {} failed: {e}", path.display()))?;
    let sidecar: AgentSessionSidecar = serde_json::from_str(&body)
        .map_err(|e| format!("parse sidecar {} failed: {e}", path.display()))?;
    if sidecar.agent.is_empty() || sidecar.session_id.is_empty() {
        return Err(format!(
            "sidecar {} is missing agent or session_id",
            path.display()
        ));
    }

    Ok(AgentSessionCapture {
        session_name,
        agent: sidecar.agent,
        session_id: sidecar.session_id,
        ts: sidecar.ts,
    })
}

fn is_sidecar_path(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_hooks_dir(test_name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("sanctel-watcher-{test_name}-{nonce}"))
    }

    #[test]
    fn drain_reads_every_sidecar_idempotently() {
        let hooks_dir = temp_hooks_dir("drain");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(
            hooks_dir.join("sanctel_wt_sanctel-main__term-2.json"),
            r#"{"agent":"claude","session_id":"claude-session-1","ts":1779311720}"#,
        )
        .unwrap();
        fs::write(
            hooks_dir.join("sanctel_wt_sanctel-main__term-1.json"),
            r#"{"agent":"codex","session_id":"codex-session-1","ts":1779311721}"#,
        )
        .unwrap();
        fs::write(
            hooks_dir.join("sanctel_wt_sanctel-main__term-1.json.tmp"),
            r#"{"agent":"gemini","session_id":"ignored","ts":1779311722}"#,
        )
        .unwrap();

        let first = read_agent_session_captures(&hooks_dir).unwrap();
        let second = read_agent_session_captures(&hooks_dir).unwrap();

        assert_eq!(first, second);
        assert_eq!(
            first,
            vec![
                AgentSessionCapture {
                    session_name: "sanctel_wt_sanctel-main__term-1".to_string(),
                    agent: "codex".to_string(),
                    session_id: "codex-session-1".to_string(),
                    ts: 1_779_311_721,
                },
                AgentSessionCapture {
                    session_name: "sanctel_wt_sanctel-main__term-2".to_string(),
                    agent: "claude".to_string(),
                    session_id: "claude-session-1".to_string(),
                    ts: 1_779_311_720,
                },
            ]
        );

        let _ = fs::remove_dir_all(hooks_dir);
    }
}
