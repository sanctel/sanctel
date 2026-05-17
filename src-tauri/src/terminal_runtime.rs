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
use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use tauri::ipc::Channel;

use crate::tmux_cli::{TmuxCli, TmuxError};

// ─── attach errors ────────────────────────────────────────────────────────

/// Errors surfaced from `attach_tab_to_tmux`. The string form is what the
/// frontend pattern-matches on for the broken-tab UI (see
/// docs/design/terminal-runtime.md §"Broken-tab UX").
#[derive(Debug)]
pub enum AttachError {
    /// The worktree path stored on the TabRecord does not exist on disk.
    /// Frontend renders the inline "worktree-missing" panel.
    WorktreeMissing(String),
    /// Anything else — tmux invocation failed, spawn failed, etc.
    Other(String),
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The exact "worktree-missing:" prefix is the wire contract the
            // frontend matches on. Keep it stable.
            AttachError::WorktreeMissing(path) => write!(f, "worktree-missing: {path}"),
            AttachError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl From<TmuxError> for AttachError {
    fn from(e: TmuxError) -> Self {
        AttachError::Other(e.to_string())
    }
}

// ─── per-tab handle and store ─────────────────────────────────────────────

/// Everything we need to drive one attached terminal tab.
///
/// The `writer` and `master` mutexes are private on purpose. Callers reach
/// them through [`TerminalHandle::with_writer`] and
/// [`TerminalHandle::with_master`], which hold the lock for the closure's
/// lifetime and release it before the function returns. See those methods
/// for why the closure pattern is mandatory.
pub struct TerminalHandle {
    /// The PTY master end — writer side for keystrokes / resizes.
    writer: Mutex<Box<dyn Write + Send>>,
    /// Master so we can call `resize()` on it.
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// Server-held identity used by terminal_attach lookups.
    pub session: String,
    pub window_name: String,
}

impl TerminalHandle {
    /// Run `f` with the PTY writer locked. Lock is released the moment
    /// the closure returns.
    ///
    /// Why a closure-scoped accessor rather than exposing the raw
    /// `Mutex<Box<dyn Write + Send>>`: the obvious caller-side pattern
    ///
    /// ```ignore
    /// let h = registry.get(label).unwrap();           // Arc<TerminalHandle>
    /// h.writer.lock().write_all(&bytes)?;             // temporary MutexGuard
    /// ```
    ///
    /// creates an unnamed `MutexGuard` that borrows from `h.writer`, which
    /// borrows from the local `h`. In Rust 1.95+, locals drop in reverse
    /// declaration order — `h` drops first, then the guard's `Drop` runs
    /// and reaches into an already-dropped `Mutex`. The compiler rejects
    /// this with E0597. Holding the lock inside this method means no
    /// caller can construct a guard that outlives its `Arc`.
    pub fn with_writer<R>(&self, f: impl FnOnce(&mut dyn Write) -> R) -> R {
        f(&mut **self.writer.lock())
    }

    /// Run `f` with the PTY master locked. Same drop-order reasoning as
    /// [`TerminalHandle::with_writer`].
    pub fn with_master<R>(&self, f: impl FnOnce(&dyn MasterPty) -> R) -> R {
        f(&**self.master.lock())
    }
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

/// Worktree-existence preflight, extracted so it's unit-testable without
/// constructing a Tauri Channel. Called first by `attach_tab_to_tmux` so
/// the broken-tab UI path runs even when tmux is unreachable.
pub fn check_worktree_exists(worktree_path: &str) -> Result<(), AttachError> {
    if Path::new(worktree_path).is_dir() {
        Ok(())
    } else {
        Err(AttachError::WorktreeMissing(worktree_path.to_string()))
    }
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
) -> Result<TerminalHandle, AttachError> {
    // 0. The Worktree directory may have been deleted between sanctel
    //    sessions. We surface this as a structured error so the frontend can
    //    render the broken-tab UI (recreate / remove) instead of letting tmux
    //    fail downstream with an opaque "-c <cwd> not found".
    check_worktree_exists(&params.worktree_path)?;

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
        .map_err(|e| AttachError::Other(format!("openpty: {e}")))?;

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
        .map_err(|e| AttachError::Other(format!("spawn tmux attach: {e}")))?;

    // The slave side is owned by the child once spawned; drop our handle so
    // EOF on the master propagates correctly when the child exits.
    drop(pair.slave);

    let writer = pair
        .master
        .take_writer()
        .map_err(|e| AttachError::Other(format!("take_writer: {e}")))?;
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| AttachError::Other(format!("try_clone_reader: {e}")))?;

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

    /// AttachError must format its WorktreeMissing variant with the
    /// `worktree-missing:` prefix — the frontend matches on this string to
    /// route the broken-tab UI. Keep the wire shape stable.
    #[test]
    fn attach_error_worktree_missing_serializes_with_prefix() {
        let e = AttachError::WorktreeMissing("/gone".into());
        assert!(
            e.to_string().starts_with("worktree-missing:"),
            "got: {e}"
        );
    }

    /// The worktree preflight (used by attach_tab_to_tmux's first step) must
    /// return WorktreeMissing for a path that doesn't exist on disk, so the
    /// broken-tab UI fires even when tmux is unreachable.
    #[test]
    fn check_worktree_exists_flags_missing_path() {
        match check_worktree_exists("/this/path/should/not/exist/sanctel-test") {
            Err(AttachError::WorktreeMissing(p)) => {
                assert!(p.contains("sanctel-test"), "path: {p}")
            }
            other => panic!("expected WorktreeMissing, got: {other:?}"),
        }
    }

    /// And succeeds for any path that does exist; HOME is always present.
    #[test]
    fn check_worktree_exists_accepts_extant_path() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        check_worktree_exists(&home).expect("HOME exists");
    }

    /// Build a `TerminalHandle` backed by a real (but otherwise unused) PTY.
    /// We don't spawn a child, so the slave side stays open and the master
    /// can be locked / written to / resized in isolation.
    fn handle_with_real_pty() -> TerminalHandle {
        let pair = NativePtySystem::default()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let writer = pair.master.take_writer().expect("take_writer");
        TerminalHandle {
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            session: "test-session".into(),
            window_name: "term-1".into(),
        }
    }

    /// `with_writer` must hold the writer mutex for the duration of the
    /// closure (so concurrent callers serialize on PTY writes) and release
    /// it the instant the closure returns.
    #[test]
    fn with_writer_holds_lock_for_closure_only() {
        let handle = handle_with_real_pty();

        let entered = handle.with_writer(|_w| {
            assert!(
                handle.writer.try_lock().is_none(),
                "writer lock must be held inside the closure"
            );
            "ok"
        });
        assert_eq!(entered, "ok", "closure return value must propagate");

        assert!(
            handle.writer.try_lock().is_some(),
            "writer lock must be released after the closure returns"
        );
    }

    /// Same guarantee for the master side: lock held during the closure,
    /// released afterwards.
    #[test]
    fn with_master_holds_lock_for_closure_only() {
        let handle = handle_with_real_pty();

        handle.with_master(|_m| {
            assert!(
                handle.master.try_lock().is_none(),
                "master lock must be held inside the closure"
            );
        });

        assert!(
            handle.master.try_lock().is_some(),
            "master lock must be released after the closure returns"
        );
    }

    /// Smoke test that the closure receives a usable writer — bytes
    /// written inside the closure reach the PTY's read side.
    #[test]
    fn with_writer_actually_writes_to_pty() {
        let pair = NativePtySystem::default()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut reader = pair.master.try_clone_reader().expect("clone_reader");
        let writer = pair.master.take_writer().expect("take_writer");
        let handle = TerminalHandle {
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            session: "test-session".into(),
            window_name: "term-1".into(),
        };

        handle
            .with_writer(|w| w.write_all(b"hello"))
            .expect("write_all");

        let mut buf = [0u8; 5];
        reader.read_exact(&mut buf).expect("read_exact");
        assert_eq!(&buf, b"hello");
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
