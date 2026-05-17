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
    /// Per-tab tmux session name (`sanctel_wt_<wt>__term-N` per ADR-0012
    /// revised by issue #15). `close_tab` passes it to
    /// `TmuxCli::kill_session` to tear down the tab's tmux server-side
    /// state in one shot. The window name is encoded in the session name
    /// suffix so it is not stored separately.
    pub session: String,
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
/// that runs `tmux attach-session -t <session>`. Per-tab session model
/// (ADR-0012 revised by issue #15): each tab owns its own tmux session
/// (`sanctel_wt_<wt>__term-N`) containing exactly one window, so there is
/// never anything to `select-window` to and the attach is byte-isolated
/// from other tabs in the same Worktree by construction.
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

    // 1. Ensure the (session, window) pair exists in tmux. Atomic in one
    //    primitive so a missing session is created with the desired window
    //    as its FIRST and ONLY window (`new-session -n …`), preventing the
    //    phantom-`zsh-` orphan that previously kept sessions alive after
    //    sanctel killed its term-N. See ADR-0012 / issue #14. The
    //    initial_command only fires when the window is genuinely new —
    //    reattach paths never re-run the shell command.
    tmux.ensure_session_window(
        &params.session,
        &params.window_name,
        &params.worktree_path,
        params.initial_command.as_deref(),
    )?;

    // 2. Spawn the portable-pty client. The PTY runs:
    //      tmux -L <socket> -f /dev/null attach-session -t <session>
    //    No `select-window` clause: the session has exactly one window
    //    (created with `new-session -n`) and tmux's session-scoped `curw`
    //    pointer is therefore unique to this tab — two tabs in the same
    //    Worktree don't share their active-window pointer, which is the
    //    bug class issue #15 closes.
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
        };

        handle
            .with_writer(|w| w.write_all(b"hello"))
            .expect("write_all");

        let mut buf = [0u8; 5];
        reader.read_exact(&mut buf).expect("read_exact");
        assert_eq!(&buf, b"hello");
    }

    /// Real-tmux integration test for the per-tab session model (issue #15).
    ///
    /// Skips if tmux isn't installed (sandcastle CI doesn't ship it). Runs on
    /// a temp socket so it never touches the user's tmux or sanctel's prod
    /// socket.
    ///
    /// Asserts the *exact* window list (length plus contents), not just
    /// "contains term-1". This catches the phantom-window regression from
    /// issue #14 — a bare `new-session` leaves a `zsh-` window in the
    /// session, which would extend the list and fail this assertion.
    ///
    /// The two-tabs-share-Worktree regression guard for issue #15 lives in
    /// `two_tabs_in_same_worktree_have_independent_output_against_real_tmux`
    /// below — kept separate so each test exercises one invariant.
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

        // 1. Fresh create: per-tab session contains EXACTLY the requested
        //    window. The session name itself encodes the windowName per
        //    issue #15's new naming.
        let session = "sanctel_wt_test-wt__term-1";
        let window = "term-1";
        let cwd = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());

        tmux.ensure_session_window(session, window, &cwd, None).unwrap();
        assert!(tmux.has_session(session).unwrap());
        assert_eq!(
            tmux.list_windows(session).unwrap(),
            vec![window.to_string()],
            "fresh session must contain ONLY the requested window — no phantom shell window",
        );

        // 2. Reattach (second ensure_session_window) is a no-op — no
        //    duplicate window, no extra anything.
        tmux.ensure_session_window(session, window, &cwd, None).unwrap();
        assert_eq!(
            tmux.list_windows(session).unwrap(),
            vec![window.to_string()],
            "reattach must not duplicate or add windows",
        );

        // 3. `kill_session` is the one-shot cleanup `close_tab` uses for
        //    terminal/chat tabs in the per-tab session model. Must be
        //    idempotent on a missing session so reattach paths never
        //    error out from a redundant call.
        tmux.kill_session(session).unwrap();
        assert!(
            !tmux.has_session(session).unwrap(),
            "session must be gone after kill_session",
        );
        tmux.kill_session(session).unwrap();

        // 4. Re-creating after kill puts us back to exactly one window
        //    (no resurrected stale state).
        tmux.ensure_session_window(session, window, &cwd, None).unwrap();
        assert_eq!(
            tmux.list_windows(session).unwrap(),
            vec![window.to_string()],
        );

        // Cleanup.
        let _ = Command::new("tmux")
            .args(["-L", &socket, "kill-server"])
            .output();
    }

    /// Regression guard for the bug class issue #15 closes: two terminal
    /// tabs in the same Worktree must NOT share xterm output. The cause
    /// was structural in tmux — `struct session` carries the `curw`
    /// (current window) pointer, so two clients attached to one session
    /// always render the same window. The fix is one tmux session per
    /// tab, named with the Worktree as prefix.
    ///
    /// This test exercises the fix end-to-end against a real tmux server:
    /// two sessions for the same Worktree base, a distinct marker sent
    /// to each, then `capture-pane` asserts each session captured ONLY
    /// its own marker. A pre-fix build (one shared session) would see
    /// both markers in both captures.
    ///
    /// Skips if tmux isn't installed.
    #[test]
    fn two_tabs_in_same_worktree_have_independent_output_against_real_tmux() {
        if !tmux_available() {
            eprintln!("skipping: tmux not installed");
            return;
        }

        let socket = format!("sanctel-test-{}", std::process::id());
        let tmux = TmuxCli::new(socket.clone(), crate::tmux_cli::RealCommandRunner);
        let _ = Command::new("tmux")
            .args(["-L", &socket, "kill-server"])
            .output();

        let cwd = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());

        // Two sibling per-tab sessions. Same Worktree base, distinct
        // windowName suffixes — exactly the post-fix naming convention.
        let session_a = "sanctel_wt_test-wt__term-1";
        let session_b = "sanctel_wt_test-wt__term-2";
        tmux.ensure_session_window(session_a, "term-1", &cwd, None).unwrap();
        tmux.ensure_session_window(session_b, "term-2", &cwd, None).unwrap();

        // Both sessions exist and contain exactly one window each — the
        // load-bearing structural invariant from the fix.
        assert!(tmux.has_session(session_a).unwrap());
        assert!(tmux.has_session(session_b).unwrap());
        assert_eq!(tmux.list_windows(session_a).unwrap().len(), 1);
        assert_eq!(tmux.list_windows(session_b).unwrap().len(), 1);

        // Send a distinct marker into each session. `send-keys` writes
        // into the session's current window — and because each session
        // has its OWN `curw` (the bug fix), writing to A lands only in
        // A's pane. Pre-fix, A and B would have been one shared session
        // with one `curw`, so both writes would target the same pane.
        let marker_a = "SANCTEL_TAB_A_MARKER";
        let marker_b = "SANCTEL_TAB_B_MARKER";
        let _ = Command::new("tmux")
            .args([
                "-L",
                &socket,
                "send-keys",
                "-t",
                &format!("={session_a}"),
                &format!("printf '{marker_a}\\n'"),
                "Enter",
            ])
            .output()
            .unwrap();
        let _ = Command::new("tmux")
            .args([
                "-L",
                &socket,
                "send-keys",
                "-t",
                &format!("={session_b}"),
                &format!("printf '{marker_b}\\n'"),
                "Enter",
            ])
            .output()
            .unwrap();

        // Give the shells a beat to render. Short sleep is fine — the
        // assertion is structural (which session saw which marker), not
        // timing-sensitive.
        std::thread::sleep(std::time::Duration::from_millis(300));

        let capture = |session: &str| -> String {
            let out = Command::new("tmux")
                .args([
                    "-L",
                    &socket,
                    "capture-pane",
                    "-t",
                    &format!("={session}"),
                    "-p",
                ])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };

        let cap_a = capture(session_a);
        let cap_b = capture(session_b);

        // The post-fix invariant: each session sees only its own marker.
        // A failing assertion here means the sessions are sharing state —
        // i.e., the bug is back.
        assert!(cap_a.contains(marker_a), "tab A must see A's marker: {cap_a}");
        assert!(!cap_a.contains(marker_b), "tab A must NOT see B's marker: {cap_a}");
        assert!(cap_b.contains(marker_b), "tab B must see B's marker: {cap_b}");
        assert!(!cap_b.contains(marker_a), "tab B must NOT see A's marker: {cap_b}");

        // close_tab path (one `kill_session` per tab). Each kill must
        // affect only its own tab; the sibling stays up.
        tmux.kill_session(session_a).unwrap();
        assert!(!tmux.has_session(session_a).unwrap());
        assert!(
            tmux.has_session(session_b).unwrap(),
            "killing tab A must not affect tab B (sibling session)",
        );
        tmux.kill_session(session_b).unwrap();
        assert!(!tmux.has_session(session_b).unwrap());

        // Cleanup.
        let _ = Command::new("tmux")
            .args(["-L", &socket, "kill-server"])
            .output();
    }
}
