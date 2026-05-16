// ───────────────────────────────────────────────────────────────────────────
// tmux_cli — small typed wrapper over `tmux -L sanctel -f /dev/null`.
//
// Every tmux interaction in sanctel goes through this module. It is the
// only place tmux CLI quirks are allowed (parsing list-windows output,
// the race retry on new-session, the -L/-f prefix).
//
// Architecture:
//   - `CommandRunner` is a trait so unit tests can inject a mock without
//     spawning real processes. Production uses `RealCommandRunner` which
//     shells out via std::process::Command.
//   - `TmuxCli::new(socket)` builds a wrapper bound to a specific socket
//     name (default: "sanctel"). Tests use a temp socket to stay isolated.
//   - All public methods return Result<T, TmuxError>; race conditions on
//     new-session are handled internally with a single retry.
// ───────────────────────────────────────────────────────────────────────────

use std::fmt;
use std::process::Output;

/// The default socket name used by the production sanctel app.
pub const DEFAULT_SOCKET: &str = "sanctel";

/// Next `term-N` for the given existing window names. Gaps are tolerated
/// (max + 1 rather than the lowest free integer) so that closing window N
/// never re-uses N for a future tab in the same session — keeps `windowName`
/// stable as a tmux handle across the session's lifetime.
///
/// Non-numeric names (`bash`, `build-watcher`, malformed `term-`, `term-abc`)
/// are ignored. The `term-` prefix is the only one this allocator owns —
/// users renaming their own tmux windows to anything else don't perturb the
/// counter.
pub fn allocate_window_name(existing: &[String]) -> String {
    let mut max: u64 = 0;
    for name in existing {
        let Some(rest) = name.strip_prefix("term-") else {
            continue;
        };
        if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Ok(n) = rest.parse::<u64>() {
            if n > max {
                max = n;
            }
        }
    }
    format!("term-{}", max + 1)
}

/// Errors surfaced from the tmux wrapper. Stringly-typed kinds are fine —
/// callers either retry (Race) or surface to the user (everything else).
#[derive(Debug, Clone)]
pub enum TmuxError {
    /// `tmux -V` failed to spawn (tmux likely not installed).
    NotFound(String),
    /// A tmux invocation exited non-zero with a parseable stderr.
    Command { command: String, stderr: String },
    /// `new-session` lost a race with a concurrent caller — caller should retry.
    SessionAlreadyExists(String),
}

impl fmt::Display for TmuxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TmuxError::NotFound(msg) => write!(f, "tmux not found: {msg}"),
            TmuxError::Command { command, stderr } => {
                write!(f, "tmux {command} failed: {stderr}")
            }
            TmuxError::SessionAlreadyExists(name) => {
                write!(f, "tmux session already exists: {name}")
            }
        }
    }
}

impl std::error::Error for TmuxError {}

/// What a Command runner returns. Mirrors the bits of `std::process::Output`
/// we actually inspect.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl From<Output> for CommandOutput {
    fn from(o: Output) -> Self {
        CommandOutput {
            status: o.status.code().unwrap_or(-1),
            stdout: o.stdout,
            stderr: o.stderr,
        }
    }
}

/// Abstraction over `std::process::Command::output()`. Production uses
/// `RealCommandRunner`; unit tests inject a scripted mock.
pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput>;
}

/// Production runner: shells out via std::process::Command.
pub struct RealCommandRunner;

impl CommandRunner for RealCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
        std::process::Command::new(program)
            .args(args)
            .output()
            .map(Into::into)
    }
}

/// tmux wrapper bound to a single socket name. Construct one and call
/// methods on it.
pub struct TmuxCli<R: CommandRunner = RealCommandRunner> {
    runner: R,
    socket: String,
}

impl Default for TmuxCli<RealCommandRunner> {
    fn default() -> Self {
        TmuxCli::new(DEFAULT_SOCKET, RealCommandRunner)
    }
}

impl<R: CommandRunner> TmuxCli<R> {
    pub fn new(socket: impl Into<String>, runner: R) -> Self {
        TmuxCli {
            runner,
            socket: socket.into(),
        }
    }

    /// The argv prefix every tmux call uses. `-L <socket>` puts sanctel on a
    /// dedicated socket; `-f /dev/null` ignores the user's `~/.tmux.conf`.
    fn base_args<'a>(&'a self, rest: &'a [&'a str]) -> Vec<&'a str> {
        let mut v = Vec::with_capacity(rest.len() + 4);
        v.push("-L");
        v.push(self.socket.as_str());
        v.push("-f");
        v.push("/dev/null");
        v.extend_from_slice(rest);
        v
    }

    fn run(&self, args: &[&str]) -> Result<CommandOutput, TmuxError> {
        let full = self.base_args(args);
        let out = self
            .runner
            .run("tmux", &full)
            .map_err(|e| TmuxError::NotFound(e.to_string()))?;
        Ok(out)
    }

    /// `tmux -V` → "tmux 3.4" (or similar). Used by the startup probe.
    pub fn version(&self) -> Result<String, TmuxError> {
        // No -L/-f prefix needed for -V, but using it is harmless and keeps
        // the invocation shape uniform.
        let out = self.run(&["-V"])?;
        if out.status != 0 {
            return Err(TmuxError::Command {
                command: "-V".into(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Returns true if `tmux has-session -t <session>` exits 0.
    pub fn has_session(&self, session: &str) -> Result<bool, TmuxError> {
        let target = format!("={session}");
        let out = self.run(&["has-session", "-t", &target])?;
        Ok(out.status == 0)
    }

    /// `tmux new-session -d -s <session> -c <cwd>`.
    ///
    /// Returns `Err(SessionAlreadyExists)` if tmux reports a name collision
    /// (concurrent caller won the race). Caller should re-check
    /// `has_session` and proceed.
    pub fn new_session(&self, session: &str, cwd: &str) -> Result<(), TmuxError> {
        let out = self.run(&["new-session", "-d", "-s", session, "-c", cwd])?;
        if out.status == 0 {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("duplicate session") || stderr.contains("already exists") {
            return Err(TmuxError::SessionAlreadyExists(session.into()));
        }
        Err(TmuxError::Command {
            command: format!("new-session -s {session}"),
            stderr: stderr.into_owned(),
        })
    }

    /// Idempotent: creates the session if missing, retries once on the
    /// race-with-concurrent-caller case. This is the only method callers
    /// should normally use to ensure a session exists.
    pub fn ensure_session(&self, session: &str, cwd: &str) -> Result<(), TmuxError> {
        if self.has_session(session)? {
            return Ok(());
        }
        match self.new_session(session, cwd) {
            Ok(()) => Ok(()),
            Err(TmuxError::SessionAlreadyExists(_)) => {
                // Another caller created it between has-session and new-session.
                // One re-check is enough; if it still says missing, something else
                // is wrong (e.g., the session died) — surface that as an error.
                if self.has_session(session)? {
                    Ok(())
                } else {
                    Err(TmuxError::Command {
                        command: format!("new-session -s {session}"),
                        stderr: "session reported as duplicate but does not exist".into(),
                    })
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Returns the window names in `session`, parsed from
    /// `tmux list-windows -t <session> -F '#{window_name}'`.
    pub fn list_windows(&self, session: &str) -> Result<Vec<String>, TmuxError> {
        let target = format!("={session}");
        let out = self.run(&[
            "list-windows",
            "-t",
            &target,
            "-F",
            "#{window_name}",
        ])?;
        if out.status != 0 {
            return Err(TmuxError::Command {
                command: format!("list-windows -t {session}"),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        let body = String::from_utf8_lossy(&out.stdout);
        Ok(body
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// `tmux new-window -t <session> -n <name> -c <cwd> [shell-cmd]`.
    pub fn new_window(
        &self,
        session: &str,
        name: &str,
        cwd: &str,
        initial_command: Option<&str>,
    ) -> Result<(), TmuxError> {
        let target = format!("={session}");
        let mut args: Vec<&str> = vec!["new-window", "-t", &target, "-n", name, "-c", cwd];
        if let Some(cmd) = initial_command {
            args.push(cmd);
        }
        let out = self.run(&args)?;
        if out.status == 0 {
            return Ok(());
        }
        Err(TmuxError::Command {
            command: format!("new-window -t {session} -n {name}"),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    /// Idempotent: lists existing windows, creates only if absent. The
    /// `initial_command` only runs when the window is genuinely new — by
    /// design, reattach paths never re-run the shell command.
    pub fn ensure_window(
        &self,
        session: &str,
        name: &str,
        cwd: &str,
        initial_command: Option<&str>,
    ) -> Result<(), TmuxError> {
        let existing = self.list_windows(session)?;
        if existing.iter().any(|w| w == name) {
            return Ok(());
        }
        self.new_window(session, name, cwd, initial_command)
    }

    /// `tmux kill-window -t <session>:<name>`. Used by close_tab for
    /// terminal/chat tabs.
    pub fn kill_window(&self, session: &str, name: &str) -> Result<(), TmuxError> {
        let target = format!("={session}:{name}");
        let out = self.run(&["kill-window", "-t", &target])?;
        if out.status == 0 {
            return Ok(());
        }
        Err(TmuxError::Command {
            command: format!("kill-window -t {session}:{name}"),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

// ─── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Scripted runner: each `run` call shifts the front of an expectation
    /// queue and returns its canned output. Records the args it saw.
    struct MockRunner {
        scripted: Mutex<Vec<MockCall>>,
        seen: Mutex<Vec<Vec<String>>>,
    }

    struct MockCall {
        // Substrings the args must contain (in order). None means any args.
        expect_args_contain: Option<Vec<&'static str>>,
        result: std::io::Result<CommandOutput>,
    }

    impl MockRunner {
        fn new(calls: Vec<MockCall>) -> Self {
            MockRunner {
                scripted: Mutex::new(calls),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn seen_args(&self) -> Vec<Vec<String>> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl CommandRunner for MockRunner {
        fn run(&self, _program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            self.seen
                .lock()
                .unwrap()
                .push(args.iter().map(|s| s.to_string()).collect());
            let next = self.scripted.lock().unwrap().remove(0);
            if let Some(expected) = &next.expect_args_contain {
                for sub in expected {
                    assert!(
                        args.iter().any(|a| a.contains(sub)),
                        "expected args to contain '{sub}', got: {args:?}"
                    );
                }
            }
            next.result
        }
    }

    fn ok(stdout: &str) -> std::io::Result<CommandOutput> {
        Ok(CommandOutput {
            status: 0,
            stdout: stdout.as_bytes().to_vec(),
            stderr: vec![],
        })
    }

    fn err(stderr: &str) -> std::io::Result<CommandOutput> {
        Ok(CommandOutput {
            status: 1,
            stdout: vec![],
            stderr: stderr.as_bytes().to_vec(),
        })
    }

    #[test]
    fn allocate_window_name_empty_yields_term_1() {
        assert_eq!(allocate_window_name(&[]), "term-1");
    }

    #[test]
    fn allocate_window_name_advances_past_sequential_list() {
        let existing = vec!["term-1".to_string(), "term-2".to_string()];
        assert_eq!(allocate_window_name(&existing), "term-3");
    }

    #[test]
    fn allocate_window_name_tolerates_gaps_by_picking_max_plus_one() {
        let existing = vec!["term-2".to_string(), "term-5".to_string()];
        assert_eq!(allocate_window_name(&existing), "term-6");
    }

    #[test]
    fn allocate_window_name_ignores_non_numeric_names() {
        let existing = vec!["bash".to_string(), "build-watcher".to_string()];
        assert_eq!(allocate_window_name(&existing), "term-1");
    }

    #[test]
    fn allocate_window_name_ignores_mixed_non_numeric_and_term_n_names() {
        let existing = vec![
            "term-3".to_string(),
            "bash".to_string(),
            "term-1".to_string(),
            "deploy".to_string(),
        ];
        assert_eq!(allocate_window_name(&existing), "term-4");
    }

    #[test]
    fn allocate_window_name_ignores_malformed_term_entries() {
        let existing = vec![
            "term-".to_string(),
            "term-abc".to_string(),
            "term-2".to_string(),
        ];
        assert_eq!(allocate_window_name(&existing), "term-3");
    }

    #[test]
    fn base_args_prefixes_socket_and_no_config() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: None,
            result: ok("tmux 3.4\n"),
        }]);
        let cli = TmuxCli::new("test-sock", mock);
        cli.version().unwrap();
        let seen = cli.runner.seen_args();
        // Every call must start with `-L test-sock -f /dev/null …`.
        assert_eq!(seen.len(), 1);
        let args = &seen[0];
        assert_eq!(args[0], "-L");
        assert_eq!(args[1], "test-sock");
        assert_eq!(args[2], "-f");
        assert_eq!(args[3], "/dev/null");
    }

    #[test]
    fn version_parses_tmux_v_output() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["-V"]),
            result: ok("tmux 3.4\n"),
        }]);
        let cli = TmuxCli::new("s", mock);
        assert_eq!(cli.version().unwrap(), "tmux 3.4");
    }

    #[test]
    fn version_propagates_spawn_error_as_not_found() {
        struct FailingRunner;
        impl CommandRunner for FailingRunner {
            fn run(&self, _: &str, _: &[&str]) -> std::io::Result<CommandOutput> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no such file",
                ))
            }
        }
        let cli = TmuxCli::new("s", FailingRunner);
        match cli.version() {
            Err(TmuxError::NotFound(_)) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn has_session_is_true_on_exit_zero() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["has-session", "=foo"]),
            result: ok(""),
        }]);
        let cli = TmuxCli::new("s", mock);
        assert!(cli.has_session("foo").unwrap());
    }

    #[test]
    fn has_session_is_false_on_nonzero_exit() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["has-session"]),
            result: err("can't find session"),
        }]);
        let cli = TmuxCli::new("s", mock);
        assert!(!cli.has_session("foo").unwrap());
    }

    #[test]
    fn list_windows_parses_one_name_per_line() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["list-windows", "#{window_name}"]),
            result: ok("term-1\nterm-2\nterm-3\n"),
        }]);
        let cli = TmuxCli::new("s", mock);
        let names = cli.list_windows("sess").unwrap();
        assert_eq!(names, vec!["term-1", "term-2", "term-3"]);
    }

    #[test]
    fn list_windows_ignores_blank_lines() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["list-windows"]),
            result: ok("term-1\n\nterm-2\n\n"),
        }]);
        let cli = TmuxCli::new("s", mock);
        assert_eq!(cli.list_windows("sess").unwrap(), vec!["term-1", "term-2"]);
    }

    #[test]
    fn new_session_recognizes_duplicate_session_stderr() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["new-session"]),
            result: err("duplicate session: foo"),
        }]);
        let cli = TmuxCli::new("s", mock);
        match cli.new_session("foo", "/tmp") {
            Err(TmuxError::SessionAlreadyExists(name)) => assert_eq!(name, "foo"),
            other => panic!("expected SessionAlreadyExists, got {other:?}"),
        }
    }

    #[test]
    fn ensure_session_returns_ok_when_already_present() {
        let mock = MockRunner::new(vec![MockCall {
            // has-session → exit 0
            expect_args_contain: Some(vec!["has-session"]),
            result: ok(""),
        }]);
        let cli = TmuxCli::new("s", mock);
        cli.ensure_session("foo", "/tmp").unwrap();
        // No new-session call should have fired.
        assert_eq!(cli.runner.seen_args().len(), 1);
    }

    #[test]
    fn ensure_session_creates_when_missing() {
        let mock = MockRunner::new(vec![
            MockCall {
                // has-session → nonzero (missing)
                expect_args_contain: Some(vec!["has-session"]),
                result: err("no session"),
            },
            MockCall {
                // new-session → success
                expect_args_contain: Some(vec!["new-session", "foo", "/tmp"]),
                result: ok(""),
            },
        ]);
        let cli = TmuxCli::new("s", mock);
        cli.ensure_session("foo", "/tmp").unwrap();
        assert_eq!(cli.runner.seen_args().len(), 2);
    }

    #[test]
    fn ensure_session_retries_on_race() {
        // Sequence simulated: has-session miss → new-session loses race →
        // has-session hit → Ok.
        let mock = MockRunner::new(vec![
            MockCall {
                expect_args_contain: Some(vec!["has-session"]),
                result: err("no session"),
            },
            MockCall {
                expect_args_contain: Some(vec!["new-session"]),
                result: err("duplicate session: foo"),
            },
            MockCall {
                expect_args_contain: Some(vec!["has-session"]),
                result: ok(""),
            },
        ]);
        let cli = TmuxCli::new("s", mock);
        cli.ensure_session("foo", "/tmp").unwrap();
        assert_eq!(cli.runner.seen_args().len(), 3);
    }

    #[test]
    fn ensure_window_skips_create_when_present() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["list-windows"]),
            result: ok("term-1\nterm-2\n"),
        }]);
        let cli = TmuxCli::new("s", mock);
        cli.ensure_window("sess", "term-1", "/tmp", None).unwrap();
        assert_eq!(cli.runner.seen_args().len(), 1);
    }

    #[test]
    fn ensure_window_creates_when_absent_with_initial_command() {
        let mock = MockRunner::new(vec![
            MockCall {
                expect_args_contain: Some(vec!["list-windows"]),
                result: ok("term-1\n"),
            },
            MockCall {
                expect_args_contain: Some(vec![
                    "new-window",
                    "term-2",
                    "/tmp",
                    "claude --resume",
                ]),
                result: ok(""),
            },
        ]);
        let cli = TmuxCli::new("s", mock);
        cli.ensure_window("sess", "term-2", "/tmp", Some("claude --resume abc"))
            .unwrap();
        assert_eq!(cli.runner.seen_args().len(), 2);
    }

    #[test]
    fn kill_window_targets_session_colon_name() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["kill-window", "=sess:term-1"]),
            result: ok(""),
        }]);
        let cli = TmuxCli::new("s", mock);
        cli.kill_window("sess", "term-1").unwrap();
    }

    /// A runner that models a tiny piece of "real tmux": a shared set of
    /// existing session names. has-session reads it; new-session writes it
    /// atomically (compare-and-set) and returns the duplicate-session
    /// stderr if the name was already there. Lets us write thread-based
    /// concurrency tests without spawning real tmux.
    struct StateRunner {
        sessions: Arc<Mutex<std::collections::HashSet<String>>>,
        new_session_wins: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl StateRunner {
        fn new() -> Self {
            StateRunner {
                sessions: Arc::new(Mutex::new(std::collections::HashSet::new())),
                new_session_wins: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }
        fn clone_shared(&self) -> Self {
            StateRunner {
                sessions: Arc::clone(&self.sessions),
                new_session_wins: Arc::clone(&self.new_session_wins),
            }
        }
    }

    impl CommandRunner for StateRunner {
        fn run(&self, _: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            let sub = args
                .iter()
                .find(|a| {
                    matches!(
                        **a,
                        "has-session" | "new-session" | "list-windows" | "kill-window"
                    )
                })
                .copied()
                .unwrap_or("");

            match sub {
                "has-session" => {
                    // Find the target after -t, strip leading "=".
                    let target: String = args
                        .iter()
                        .position(|a| *a == "-t")
                        .and_then(|i| args.get(i + 1))
                        .map(|s| s.trim_start_matches('=').to_string())
                        .unwrap_or_default();
                    let exists = self.sessions.lock().unwrap().contains(&target);
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
                    let name: String = args
                        .iter()
                        .position(|a| *a == "-s")
                        .and_then(|i| args.get(i + 1))
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    // Compare-and-set: only the first inserter wins.
                    let inserted = self.sessions.lock().unwrap().insert(name.clone());
                    if inserted {
                        self.new_session_wins
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
                _ => Ok(CommandOutput {
                    status: 0,
                    stdout: vec![],
                    stderr: vec![],
                }),
            }
        }
    }

    /// Multiple threads calling ensure_session for the same session must all
    /// succeed, and exactly one `new-session` call must "win" (compare-and-set
    /// at the simulated tmux layer). Models the real race the issue calls out:
    /// "multiple tabs in the same Worktree call terminal_attach simultaneously".
    #[test]
    fn ensure_session_is_concurrent_safe_under_real_race() {
        let shared = StateRunner::new();
        let session = "sanctel-wt:race-test";
        let cwd = "/tmp";
        const THREADS: usize = 16;

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let runner = shared.clone_shared();
                let session = session.to_string();
                let cwd = cwd.to_string();
                std::thread::spawn(move || {
                    let cli = TmuxCli::new("s", runner);
                    cli.ensure_session(&session, &cwd)
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap().expect("ensure_session must succeed");
        }

        // Exactly one new-session call wins; the rest see "duplicate session"
        // and recover via the has-session re-check inside ensure_session.
        assert_eq!(
            shared
                .new_session_wins
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "exactly one thread should win new-session"
        );
        // The session must end up created.
        assert!(shared.sessions.lock().unwrap().contains(session));
    }
}
