// ───────────────────────────────────────────────────────────────────────────
// terminal_runtime — Tauri command surface and PTY lifecycle for terminal /
// chat tabs.
//
// The three commands (terminal_attach, terminal_write, terminal_resize) all
// derive identity (worktreeId, windowName, initialCommand) from the calling
// webview's label by looking up the TabRecord stored at create_tab time. The
// frontend never passes its own tabId — this is enforced by the IPC shape.
//
// Slice 2 ships a HARDCODED worktree path ($HOME) and windowName ("term-1");
// per-Worktree allocation and SQLite-backed identity come in later slices.
//
// See docs/design/terminal-runtime.md, especially §"Idempotent attach
// algorithm" and §"IPC contract".
// ───────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;

use parking_lot::Mutex;
use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use tauri::ipc::Channel;

use crate::tmux_cli::{TmuxCli, TmuxError};

// ─── per-tab handle and store ─────────────────────────────────────────────

/// Everything we need to drive one attached terminal tab.
pub struct TerminalHandle {
    /// The PTY master end — writer side for keystrokes / resizes.
    pub writer: Mutex<Box<dyn Write + Send>>,
    /// Master so we can call `resize()` on it.
    pub master: Mutex<Box<dyn MasterPty + Send>>,
    /// Server-held identity used by terminal_attach lookups.
    pub session: String,
    pub window_name: String,
}

/// In-memory map from webview label → handle. Lives inside Tauri's managed
/// state so commands can grab it from `AppHandle`.
#[derive(Default)]
pub struct TerminalRegistry {
    handles: Mutex<HashMap<String, Arc<TerminalHandle>>>,
}

impl TerminalRegistry {
    pub fn insert(&self, label: String, handle: Arc<TerminalHandle>) {
        self.handles.lock().insert(label, handle);
    }
    pub fn get(&self, label: &str) -> Option<Arc<TerminalHandle>> {
        self.handles.lock().get(label).cloned()
    }
    pub fn remove(&self, label: &str) -> Option<Arc<TerminalHandle>> {
        self.handles.lock().remove(label)
    }
}

// ─── idempotent attach algorithm ──────────────────────────────────────────

/// Per-tab inputs to `attach_tab_to_tmux`. Slice 2 fills these in from a
/// hardcoded source ($HOME, "term-1") inside `terminal_attach`; later slices
/// pull them from the TabRecord stored at create_tab time.
#[derive(Clone)]
pub struct AttachParams {
    pub session: String,
    pub window_name: String,
    pub worktree_path: String,
    pub initial_command: Option<String>,
    pub cols: u16,
    pub rows: u16,
}

/// Ensure (session, window) exist in tmux, then spawn a portable-pty client
/// that runs `tmux attach-session -t <session> \; select-window -t :<window>`.
/// Reads from the PTY in a background thread and pushes raw bytes into
/// `on_output`. Returns the handle holding the master/writer so subsequent
/// terminal_write / terminal_resize / close_tab calls can drive it.
pub fn attach_tab_to_tmux(
    tmux: &TmuxCli,
    params: AttachParams,
    on_output: Channel<Vec<u8>>,
) -> Result<TerminalHandle, TmuxError> {
    // 1. Ensure the tmux session exists (with race retry).
    tmux.ensure_session(&params.session, &params.worktree_path)?;

    // 2. Ensure the named window exists. initial_command only fires here, for
    //    fresh windows — reattach paths never re-run the shell command.
    tmux.ensure_window(
        &params.session,
        &params.window_name,
        &params.worktree_path,
        params.initial_command.as_deref(),
    )?;

    // 3. Spawn the portable-pty client. The PTY runs:
    //      tmux -L <socket> -f /dev/null attach-session -t <session> \
    //                                  \; select-window -t :<window>
    //    portable-pty's `;` argument is the same `;` tmux uses to chain.
    let pty_system = NativePtySystem::default();
    let pair = pty_system
        .openpty(PtySize {
            rows: params.rows.max(1),
            cols: params.cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| TmuxError::Command {
            command: "openpty".into(),
            stderr: e.to_string(),
        })?;

    let mut cmd = CommandBuilder::new("tmux");
    cmd.args([
        "-L",
        crate::tmux_cli::DEFAULT_SOCKET,
        "-f",
        "/dev/null",
        "attach-session",
        "-t",
        &format!("={}", params.session),
        ";",
        "select-window",
        "-t",
        &format!(":{}", params.window_name),
    ]);
    cmd.cwd(&params.worktree_path);

    let _child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| TmuxError::Command {
            command: "spawn tmux attach".into(),
            stderr: e.to_string(),
        })?;

    // The slave side is owned by the child once spawned; drop our handle so
    // EOF on the master propagates correctly when the child exits.
    drop(pair.slave);

    let writer = pair.master.take_writer().map_err(|e| TmuxError::Command {
        command: "take_writer".into(),
        stderr: e.to_string(),
    })?;
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| TmuxError::Command {
            command: "try_clone_reader".into(),
            stderr: e.to_string(),
        })?;

    // Forward bytes from the PTY to the channel. Raw bytes only — no UTF-8
    // transcoding on the data path (design invariant).
    spawn_pty_reader(reader, on_output);

    Ok(TerminalHandle {
        writer: Mutex::new(writer),
        master: Mutex::new(pair.master),
        session: params.session,
        window_name: params.window_name,
    })
}

fn spawn_pty_reader(
    mut reader: Box<dyn Read + Send>,
    on_output: Channel<Vec<u8>>,
) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF — PTY closed (e.g., tmux detached or died).
                Ok(n) => {
                    if on_output.send(buf[..n].to_vec()).is_err() {
                        // Channel closed (webview gone). Stop draining.
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
}

// ─── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn tmux_available() -> bool {
        Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Real-tmux integration test for the idempotent attach algorithm.
    ///
    /// Skips if tmux isn't installed (sandcastle CI doesn't ship it). Runs on
    /// a temp socket so it never touches the user's tmux or sanctel's prod
    /// socket.
    #[test]
    fn idempotent_attach_against_real_tmux() {
        if !tmux_available() {
            eprintln!("skipping: tmux not installed");
            return;
        }

        let socket = format!("sanctel-test-{}", std::process::id());
        let tmux = TmuxCli::new(socket.clone(), crate::tmux_cli::RealCommandRunner);

        // Belt-and-braces cleanup if a prior run leaked.
        let _ = Command::new("tmux")
            .args(["-L", &socket, "kill-server"])
            .output();

        // 1. Fresh create.
        let session = "sanctel-wt:test-wt";
        let window = "term-1";
        let cwd = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());

        tmux.ensure_session(session, &cwd).unwrap();
        tmux.ensure_window(session, window, &cwd, None).unwrap();
        assert!(tmux.has_session(session).unwrap());
        let windows = tmux.list_windows(session).unwrap();
        assert!(windows.contains(&window.to_string()));

        // 2. Reattach (second ensure_*) is a no-op — no duplicate window.
        tmux.ensure_session(session, &cwd).unwrap();
        tmux.ensure_window(session, window, &cwd, None).unwrap();
        let windows = tmux.list_windows(session).unwrap();
        let term1_count = windows.iter().filter(|w| *w == window).count();
        assert_eq!(term1_count, 1, "reattach must not duplicate window");

        // 3. Externally kill the window — next ensure recreates it.
        tmux.kill_window(session, window).unwrap();
        // After last window dies, tmux destroys the session. ensure_session
        // recreates both.
        tmux.ensure_session(session, &cwd).unwrap();
        tmux.ensure_window(session, window, &cwd, None).unwrap();
        let windows = tmux.list_windows(session).unwrap();
        assert!(windows.contains(&window.to_string()));

        // Cleanup.
        let _ = Command::new("tmux")
            .args(["-L", &socket, "kill-server"])
            .output();
    }
}
