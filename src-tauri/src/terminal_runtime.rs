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
use serde::Serialize;
use tauri::ipc::Channel;

use crate::tmux_cli::{CommandRunner, TmuxCli, TmuxError};

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TabExitedPayload {
    pub id: String,
}

pub type TabExitedEmitter = Arc<dyn Fn(TabExitedPayload) + Send + Sync + 'static>;

pub fn tab_exited_payload_if_session_missing<R: CommandRunner>(
    tmux: &TmuxCli<R>,
    tab_id: &str,
    session: &str,
) -> Result<Option<TabExitedPayload>, TmuxError> {
    if tmux.has_session(session)? {
        Ok(None)
    } else {
        Ok(Some(TabExitedPayload {
            id: tab_id.to_string(),
        }))
    }
}

// ─── per-tab handle and store ─────────────────────────────────────────────

/// Everything we need to drive one attached terminal tab. Holds a PTY pair
/// attached to `tmux attach-session`. The writer mutex is locked inside
/// `write_bytes`; the master mutex is locked inside `resize`. Locks held
/// only for the duration of the I/O call — no caller-side `MutexGuard` is
/// ever constructed (which would trip Rust 1.95+'s drop-order check E0597
/// against an `Arc<Self>`).
pub struct TerminalHandle {
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// Per-tab session name (`sanctel_wt_<wt>__term-N` per ADR-0012
    /// revised by issue #15). The frontend never sees this; `close_tab`
    /// passes it to `tmux kill-session` to tear down the server-side state.
    pub session: String,
}

impl TerminalHandle {
    /// Write raw bytes to the tmux PTY master.
    pub fn write_bytes(&self, bytes: &[u8]) -> Result<(), String> {
        self.writer
            .lock()
            .write_all(bytes)
            .map_err(|e| e.to_string())
    }

    /// Resize the PTY pane.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        self.master
            .lock()
            .resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())
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
    tab_id: String,
    on_tab_exited: TabExitedEmitter,
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
    //      tmux -L <socket> -f <conf> attach-session -t <session>
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
        tmux.socket(),
        "-f",
        tmux.conf_path(),
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
    spawn_pty_reader(
        reader,
        on_output,
        tab_id,
        params.session.clone(),
        tmux.socket().to_string(),
        tmux.conf_path().to_string(),
        on_tab_exited,
    );

    Ok(TerminalHandle {
        writer: Mutex::new(writer),
        master: Mutex::new(pair.master),
        session: params.session,
    })
}

fn spawn_pty_reader(
    mut reader: Box<dyn Read + Send>,
    on_output: Channel<Vec<u8>>,
    tab_id: String,
    session: String,
    socket: String,
    conf_path: String,
    on_tab_exited: TabExitedEmitter,
) {
    std::thread::spawn(move || {
        let tmux = TmuxCli::new(socket, conf_path, crate::tmux_cli::RealCommandRunner);
        if let Err(e) = drive_pty_reader_until_done(
            &mut reader,
            |chunk| on_output.send(chunk).is_ok(),
            &tmux,
            &tab_id,
            &session,
            &on_tab_exited,
        ) {
            eprintln!("failed to confirm tmux session death for tab {tab_id}: {e}");
        }
    });
}

fn drive_pty_reader_until_done<R, Reader, OnOutput>(
    mut reader: Reader,
    mut on_output: OnOutput,
    tmux: &TmuxCli<R>,
    tab_id: &str,
    session: &str,
    on_tab_exited: &TabExitedEmitter,
) -> Result<(), TmuxError>
where
    R: CommandRunner,
    Reader: Read,
    OnOutput: FnMut(Vec<u8>) -> bool,
{
    let mut buf = [0u8; 8192];
    let reached_eof = loop {
        match reader.read(&mut buf) {
            Ok(0) => break true,
            Ok(n) => {
                if !on_output(buf[..n].to_vec()) {
                    // Channel closed (webview gone). Stop draining.
                    break false;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break false,
        }
    };

    if !reached_eof {
        return Ok(());
    }

    if let Some(payload) = tab_exited_payload_if_session_missing(tmux, tab_id, session)? {
        on_tab_exited(payload);
    }

    Ok(())
}

// ─── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux_cli::{CommandOutput, CommandRunner};
    use std::process::Command;
    use std::sync::{mpsc, Mutex as StdMutex};

    struct HasSessionRunner {
        exists: bool,
    }

    impl CommandRunner for HasSessionRunner {
        fn run(&self, _: &str, _: &[&str]) -> std::io::Result<CommandOutput> {
            Ok(CommandOutput {
                status: if self.exists { 0 } else { 1 },
                stdout: vec![],
                stderr: if self.exists {
                    vec![]
                } else {
                    b"can't find session".to_vec()
                },
            })
        }
    }

    struct RecordingHasSessionRunner {
        exists: bool,
        calls: Arc<StdMutex<Vec<Vec<String>>>>,
    }

    impl CommandRunner for RecordingHasSessionRunner {
        fn run(&self, _: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            self.calls
                .lock()
                .unwrap()
                .push(args.iter().map(|arg| arg.to_string()).collect());
            Ok(CommandOutput {
                status: if self.exists { 0 } else { 1 },
                stdout: vec![],
                stderr: if self.exists {
                    vec![]
                } else {
                    b"can't find session".to_vec()
                },
            })
        }
    }

    fn tmux_available() -> bool {
        Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn assert_single_has_session_call(
        calls: &StdMutex<Vec<Vec<String>>>,
        socket: &str,
        session: &str,
    ) {
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[vec![
                "-L".to_string(),
                socket.to_string(),
                "-f".to_string(),
                "/dev/null".to_string(),
                "has-session".to_string(),
                "-t".to_string(),
                format!("={session}"),
            ]],
        );
    }

    /// AttachError must format its WorktreeMissing variant with the
    /// `worktree-missing:` prefix — the frontend matches on this string to
    /// route the broken-tab UI. Keep the wire shape stable.
    #[test]
    fn attach_error_worktree_missing_serializes_with_prefix() {
        let e = AttachError::WorktreeMissing("/gone".into());
        assert!(e.to_string().starts_with("worktree-missing:"), "got: {e}");
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

    #[test]
    fn pty_eof_missing_tmux_session_produces_tab_exited_payload() {
        let tmux = TmuxCli::new(
            "test",
            crate::tmux_cli::DEFAULT_CONF_PATH,
            HasSessionRunner { exists: false },
        );

        let payload = tab_exited_payload_if_session_missing(&tmux, "tab-1", "session-1").unwrap();

        assert_eq!(
            payload,
            Some(TabExitedPayload {
                id: "tab-1".to_string(),
            }),
        );
    }

    #[test]
    fn pty_eof_existing_tmux_session_does_not_produce_tab_exited_payload() {
        let tmux = TmuxCli::new(
            "test",
            crate::tmux_cli::DEFAULT_CONF_PATH,
            HasSessionRunner { exists: true },
        );

        let payload = tab_exited_payload_if_session_missing(&tmux, "tab-1", "session-1").unwrap();

        assert_eq!(payload, None);
    }

    #[test]
    fn pty_reader_eof_missing_tmux_session_emits_tab_exited() {
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let tmux = TmuxCli::new(
            "test",
            crate::tmux_cli::DEFAULT_CONF_PATH,
            RecordingHasSessionRunner {
                exists: false,
                calls: Arc::clone(&calls),
            },
        );
        let reader = std::io::Cursor::new(b"hello".to_vec());
        let mut output = Vec::new();
        let (tx, rx) = mpsc::channel();
        let on_tab_exited: TabExitedEmitter = Arc::new(move |payload| {
            tx.send(payload).unwrap();
        });

        drive_pty_reader_until_done(
            reader,
            |chunk| {
                output.push(chunk);
                true
            },
            &tmux,
            "tab-1",
            "session-1",
            &on_tab_exited,
        )
        .unwrap();

        assert_eq!(output, vec![b"hello".to_vec()]);
        assert_single_has_session_call(&calls, "test", "session-1");
        assert_eq!(
            rx.try_recv().unwrap(),
            TabExitedPayload {
                id: "tab-1".to_string(),
            },
        );
    }

    #[test]
    fn pty_reader_eof_existing_tmux_session_does_not_emit_tab_exited() {
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let tmux = TmuxCli::new(
            "test",
            crate::tmux_cli::DEFAULT_CONF_PATH,
            RecordingHasSessionRunner {
                exists: true,
                calls: Arc::clone(&calls),
            },
        );
        let reader = std::io::Cursor::new(Vec::<u8>::new());
        let (tx, rx) = mpsc::channel();
        let on_tab_exited: TabExitedEmitter = Arc::new(move |payload| {
            tx.send(payload).unwrap();
        });

        drive_pty_reader_until_done(
            reader,
            |_| true,
            &tmux,
            "tab-1",
            "session-1",
            &on_tab_exited,
        )
        .unwrap();

        assert_single_has_session_call(&calls, "test", "session-1");
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    /// `TerminalHandle::write_bytes` on a tmux-variant handle reaches the
    /// PTY's read side. The handle's write path is the same code
    /// `terminal_write` calls in production; this proves the dispatch +
    /// lock-scope wiring is byte-clean for the tmux backend.
    #[test]
    fn write_bytes_reaches_pty_for_tmux_variant() {
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

        handle.write_bytes(b"hello").expect("write_bytes");

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

        // Per-test socket suffix: cargo runs tests in parallel within one
        // binary, and the sibling integration test
        // (`two_tabs_in_same_worktree_have_independent_output_against_real_tmux`)
        // uses overlapping session names. Distinct sockets give each test
        // its own tmux server so concurrent kill_session calls don't pull
        // the carpet out from under the other test.
        let socket = format!("sanctel-test-{}-idem", std::process::id());
        let tmux = TmuxCli::new(
            socket.clone(),
            crate::tmux_cli::DEFAULT_CONF_PATH,
            crate::tmux_cli::RealCommandRunner,
        );

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

        tmux.ensure_session_window(session, window, &cwd, None)
            .unwrap();
        assert!(tmux.has_session(session).unwrap());
        assert_eq!(
            tmux.list_windows(session).unwrap(),
            vec![window.to_string()],
            "fresh session must contain ONLY the requested window — no phantom shell window",
        );

        // 2. Reattach (second ensure_session_window) is a no-op — no
        //    duplicate window, no extra anything.
        tmux.ensure_session_window(session, window, &cwd, None)
            .unwrap();
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
        tmux.ensure_session_window(session, window, &cwd, None)
            .unwrap();
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

        // Distinct socket suffix so this test runs against its own tmux
        // server in parallel with `idempotent_attach_against_real_tmux`
        // (see that test for the full rationale).
        let socket = format!("sanctel-test-{}-indep", std::process::id());
        let tmux = TmuxCli::new(
            socket.clone(),
            crate::tmux_cli::DEFAULT_CONF_PATH,
            crate::tmux_cli::RealCommandRunner,
        );
        let _ = Command::new("tmux")
            .args(["-L", &socket, "kill-server"])
            .output();

        let cwd = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());

        // Two sibling per-tab sessions. Same Worktree base, distinct
        // windowName suffixes — exactly the post-fix naming convention.
        let session_a = "sanctel_wt_test-wt__term-1";
        let session_b = "sanctel_wt_test-wt__term-2";
        tmux.ensure_session_window(session_a, "term-1", &cwd, None)
            .unwrap();
        tmux.ensure_session_window(session_b, "term-2", &cwd, None)
            .unwrap();

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
        //
        // Target shape note: bare `<session>`, not `=<session>`. tmux 3.3a
        // (bookworm's default) rejects `=name` as a pane-target lookup
        // ("can't find pane: =name") because `=` resolves to a session
        // and send-keys needs a pane. The unique PID-suffixed socket
        // means no ambiguity — bare name resolves to this session's
        // active pane unambiguously.
        let marker_a = "SANCTEL_TAB_A_MARKER";
        let marker_b = "SANCTEL_TAB_B_MARKER";
        let _ = Command::new("tmux")
            .args([
                "-L",
                &socket,
                "send-keys",
                "-t",
                session_a,
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
                session_b,
                &format!("printf '{marker_b}\\n'"),
                "Enter",
            ])
            .output()
            .unwrap();

        let capture = |session: &str| -> String {
            let out = Command::new("tmux")
                .args(["-L", &socket, "capture-pane", "-t", session, "-p"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };

        // Poll-with-timeout rather than a fixed sleep. Shell boot on a
        // cold container can exceed any short fixed sleep; the
        // assertion is structural (which session saw which marker), so
        // it's safe to wait until both markers actually appear. 5s budget.
        let mut cap_a = String::new();
        let mut cap_b = String::new();
        for _ in 0..100 {
            cap_a = capture(session_a);
            cap_b = capture(session_b);
            if cap_a.contains(marker_a) && cap_b.contains(marker_b) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        // The post-fix invariant: each session sees only its own marker.
        // A failing assertion here means the sessions are sharing state —
        // i.e., the bug is back.
        assert!(
            cap_a.contains(marker_a),
            "tab A must see A's marker: {cap_a}"
        );
        assert!(
            !cap_a.contains(marker_b),
            "tab A must NOT see B's marker: {cap_a}"
        );
        assert!(
            cap_b.contains(marker_b),
            "tab B must see B's marker: {cap_b}"
        );
        assert!(
            !cap_b.contains(marker_a),
            "tab B must NOT see A's marker: {cap_b}"
        );

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
