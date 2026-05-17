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

/// Sanitize a string for safe inclusion in a tmux session/window name.
///
/// tmux interprets `:` and `.` in target specs as the session/window/pane
/// separators (e.g., `tmux list-windows -t foo:bar` parses as
/// `session=foo, window=bar`). Sanctel must never pass `:` or `.` to tmux
/// inside a name we own — and tmux itself silently rewrites `:` to `_` on
/// `new-session`, so a session like `sanctel-wt:test` is created on disk as
/// `sanctel-wt_test` but then unreachable by its construction-time name.
///
/// Replaces any character outside `[A-Za-z0-9_-]` with `_`. Idempotent:
/// `tmux_safe(tmux_safe(x)) == tmux_safe(x)`.
pub fn tmux_safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

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

    /// Like `run`, but sets the spawned process's working directory. Used by
    /// `zellij_cli::new_session` because zellij has no `-c <cwd>` CLI flag
    /// (tmux does). Default impl delegates to `run` and ignores cwd — fine
    /// for mock runners that don't actually spawn processes; the production
    /// `RealCommandRunner` overrides to call `Command::current_dir`.
    #[allow(dead_code)] // Production caller (`new_session`) lands in slice 3.
    fn run_in_dir(
        &self,
        program: &str,
        args: &[&str],
        _cwd: &str,
    ) -> std::io::Result<CommandOutput> {
        self.run(program, args)
    }
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

    fn run_in_dir(
        &self,
        program: &str,
        args: &[&str],
        cwd: &str,
    ) -> std::io::Result<CommandOutput> {
        std::process::Command::new(program)
            .args(args)
            .current_dir(cwd)
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

    /// `tmux new-session -d -s <session> -n <window_name> -c <cwd> [shell-cmd]`.
    ///
    /// **Always** passes `-n <window_name>` so the session's initial window IS
    /// the one sanctel wants — without `-n`, tmux silently creates a phantom
    /// `zsh-` (or `$SHELL-`) window first and the session's lifecycle is then
    /// pinned by that phantom rather than the term-N sanctel will later kill.
    /// See ADR-0012 and issue #14.
    ///
    /// Returns `Err(SessionAlreadyExists)` if tmux reports a name collision
    /// (a concurrent caller won the race). The sole caller,
    /// `ensure_session_window`, recovers by falling through to its
    /// session-exists branch.
    fn new_session_with_window(
        &self,
        session: &str,
        window_name: &str,
        cwd: &str,
        initial_command: Option<&str>,
    ) -> Result<(), TmuxError> {
        let mut args: Vec<&str> = vec![
            "new-session", "-d", "-s", session, "-n", window_name, "-c", cwd,
        ];
        if let Some(cmd) = initial_command {
            args.push(cmd);
        }
        let out = self.run(&args)?;
        if out.status == 0 {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("duplicate session") || stderr.contains("already exists") {
            return Err(TmuxError::SessionAlreadyExists(session.into()));
        }
        Err(TmuxError::Command {
            command: format!("new-session -s {session} -n {window_name}"),
            stderr: stderr.into_owned(),
        })
    }

    /// Atomic "session contains this named window, nothing else implicit"
    /// primitive. This is the only method `create_tab` / `attach_tab_to_tmux`
    /// use to bring a (session, window) into existence.
    ///
    /// Algorithm:
    ///   - `has-session`?
    ///     - **Yes**: list windows; if `window_name` is already there, return
    ///       Ok (idempotent reattach). Otherwise `new-window -n`.
    ///     - **No**: `new-session -d -s <session> -n <window_name> -c <cwd>
    ///       [initial_command]`. Race-retry once on `duplicate session`: the
    ///       winner's session now exists, so re-check `has-session` and fall
    ///       through to the session-exists branch.
    ///
    /// Why a single primitive: doing `new-session` without `-n` leaves a
    /// phantom `zsh-` window in the session (tmux's default initial window).
    /// That phantom keeps the session alive forever after sanctel kills its
    /// `term-N`, breaking ADR-0012's "tmux destroys the session when its
    /// last window dies" promise. Merging the two operations into one
    /// `new-session -n` call is the fix and is encoded here so no caller
    /// can forget. See issue #14.
    pub fn ensure_session_window(
        &self,
        session: &str,
        window_name: &str,
        cwd: &str,
        initial_command: Option<&str>,
    ) -> Result<(), TmuxError> {
        if self.has_session(session)? {
            return self.add_window_if_absent(session, window_name, cwd, initial_command);
        }
        match self.new_session_with_window(session, window_name, cwd, initial_command) {
            Ok(()) => Ok(()),
            Err(TmuxError::SessionAlreadyExists(_)) => {
                // Race winner created the session between our has-session and
                // new-session. The winner may or may not have created the same
                // window_name; fall through to the session-exists branch.
                if !self.has_session(session)? {
                    return Err(TmuxError::Command {
                        command: format!("new-session -s {session} -n {window_name}"),
                        stderr: "session reported as duplicate but does not exist".into(),
                    });
                }
                self.add_window_if_absent(session, window_name, cwd, initial_command)
            }
            Err(e) => Err(e),
        }
    }

    /// Internal: list windows in `session`, add `window_name` only if it's
    /// not already there. Idempotent. The caller must have already confirmed
    /// the session exists.
    fn add_window_if_absent(
        &self,
        session: &str,
        window_name: &str,
        cwd: &str,
        initial_command: Option<&str>,
    ) -> Result<(), TmuxError> {
        let existing = self.list_windows(session)?;
        if existing.iter().any(|w| w == window_name) {
            return Ok(());
        }
        self.new_window(session, window_name, cwd, initial_command)
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

    /// `tmux kill-session -t <session>`. Used by `close_tab` for
    /// terminal/chat tabs: each tab owns its own session
    /// (`sanctel_wt_<wt>__term-N`), so killing the session is the one-shot
    /// cleanup. Idempotent on a session that doesn't exist — `tmux`'s
    /// "can't find session" error is swallowed so reattach/cleanup paths
    /// remain safe to call without first probing `has-session`. See
    /// ADR-0012 / issue #15.
    pub fn kill_session(&self, session: &str) -> Result<(), TmuxError> {
        let target = format!("={session}");
        let out = self.run(&["kill-session", "-t", &target])?;
        if out.status == 0 {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("can't find session") || stderr.contains("session not found") {
            return Ok(());
        }
        Err(TmuxError::Command {
            command: format!("kill-session -t {session}"),
            stderr: stderr.into_owned(),
        })
    }

    /// Returns every existing session name on this socket, parsed from
    /// `tmux list-sessions -F '#{session_name}'`. Used by the per-Worktree
    /// windowName allocator (issue #15) to compute the next `term-N`: each
    /// Tab is its own session, so the allocator scans sessions whose name
    /// starts with the Worktree prefix.
    ///
    /// Returns an empty Vec (not an error) when the tmux server has no
    /// sessions — list-sessions exits non-zero with "no server running on …"
    /// or "no sessions" in that case.
    pub fn list_sessions(&self) -> Result<Vec<String>, TmuxError> {
        let out = self.run(&["list-sessions", "-F", "#{session_name}"])?;
        if out.status != 0 {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("no server running")
                || stderr.contains("no sessions")
                || stderr.contains("error connecting")
            {
                return Ok(Vec::new());
            }
            return Err(TmuxError::Command {
                command: "list-sessions".into(),
                stderr: stderr.into_owned(),
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
    fn tmux_safe_passes_through_alphanumeric_underscore_hyphen() {
        assert_eq!(tmux_safe("term-1"), "term-1");
        assert_eq!(tmux_safe("sanctel_wt_main"), "sanctel_wt_main");
        assert_eq!(tmux_safe("AZaz09_-"), "AZaz09_-");
    }

    #[test]
    fn tmux_safe_replaces_colon_dot_and_space() {
        // The three characters that matter most for tmux target-spec parsing
        // (`:` and `.`) plus whitespace (a common worktreeId-from-branch issue).
        assert_eq!(tmux_safe("a:b"), "a_b");
        assert_eq!(tmux_safe("a.b"), "a_b");
        assert_eq!(tmux_safe("a b"), "a_b");
        assert_eq!(tmux_safe("sanctel-wt:test-wt"), "sanctel-wt_test-wt");
    }

    #[test]
    fn tmux_safe_replaces_other_punctuation() {
        assert_eq!(tmux_safe("a/b"), "a_b");
        assert_eq!(tmux_safe("a\\b"), "a_b");
        assert_eq!(tmux_safe("a@b"), "a_b");
        assert_eq!(tmux_safe("\u{00E9}"), "_"); // non-ASCII collapses
    }

    #[test]
    fn tmux_safe_is_idempotent() {
        for s in ["", "term-1", "a:b", "feature/branch", "a..b::c"] {
            assert_eq!(tmux_safe(s), tmux_safe(&tmux_safe(s)));
        }
    }

    #[test]
    fn tmux_safe_handles_empty_string() {
        assert_eq!(tmux_safe(""), "");
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

    /// Session is missing. The fix for issue #14: `new-session` must carry
    /// `-n <window_name>` so the session's initial window is the one sanctel
    /// asked for — no phantom `zsh-` window left in the session. Crucially,
    /// no separate `new-window` call follows.
    #[test]
    fn ensure_session_window_creates_session_with_n_flag_when_missing() {
        let mock = MockRunner::new(vec![
            MockCall {
                expect_args_contain: Some(vec!["has-session"]),
                result: err("no session"),
            },
            MockCall {
                // Verify `-n term-1` is in the new-session call. This is the
                // assertion that prevents the phantom-window regression.
                expect_args_contain: Some(vec![
                    "new-session", "-d", "-s", "foo", "-n", "term-1", "-c", "/tmp",
                ]),
                result: ok(""),
            },
        ]);
        let cli = TmuxCli::new("s", mock);
        cli.ensure_session_window("foo", "term-1", "/tmp", None).unwrap();
        let seen = cli.runner.seen_args();
        // Exactly two tmux invocations: has-session + new-session. No
        // trailing new-window. The phantom-window bug was created precisely
        // by an extra new-window call after a bare `new-session`.
        assert_eq!(seen.len(), 2, "expected has-session + new-session only, got: {seen:?}");
        assert!(
            !seen[1].iter().any(|a| a == "new-window"),
            "must not invoke new-window when session is created with -n",
        );
    }

    /// The initial-command pass-through still works in the new primitive:
    /// `new-session -d -s … -n … -c … <cmd>`.
    #[test]
    fn ensure_session_window_creates_session_with_initial_command() {
        let mock = MockRunner::new(vec![
            MockCall {
                expect_args_contain: Some(vec!["has-session"]),
                result: err("no session"),
            },
            MockCall {
                expect_args_contain: Some(vec![
                    "new-session", "-n", "term-1", "claude --resume abc",
                ]),
                result: ok(""),
            },
        ]);
        let cli = TmuxCli::new("s", mock);
        cli.ensure_session_window("foo", "term-1", "/tmp", Some("claude --resume abc"))
            .unwrap();
    }

    /// Session exists, window absent. Must call `new-window -n` and only
    /// one `new-window` call.
    #[test]
    fn ensure_session_window_adds_window_when_session_exists_without_it() {
        let mock = MockRunner::new(vec![
            MockCall {
                expect_args_contain: Some(vec!["has-session"]),
                result: ok(""),
            },
            MockCall {
                expect_args_contain: Some(vec!["list-windows"]),
                result: ok("term-1\n"),
            },
            MockCall {
                expect_args_contain: Some(vec!["new-window", "-n", "term-2", "-c", "/tmp"]),
                result: ok(""),
            },
        ]);
        let cli = TmuxCli::new("s", mock);
        cli.ensure_session_window("foo", "term-2", "/tmp", None).unwrap();
        assert_eq!(cli.runner.seen_args().len(), 3);
    }

    /// Session exists, window present. Pure no-op aside from the
    /// has-session + list-windows probe.
    #[test]
    fn ensure_session_window_is_noop_when_window_already_exists() {
        let mock = MockRunner::new(vec![
            MockCall {
                expect_args_contain: Some(vec!["has-session"]),
                result: ok(""),
            },
            MockCall {
                expect_args_contain: Some(vec!["list-windows"]),
                result: ok("term-1\nterm-2\n"),
            },
        ]);
        let cli = TmuxCli::new("s", mock);
        cli.ensure_session_window("foo", "term-1", "/tmp", None).unwrap();
        let seen = cli.runner.seen_args();
        assert_eq!(seen.len(), 2, "no new-window expected; got: {seen:?}");
        assert!(!seen.iter().any(|args| args.iter().any(|a| a == "new-window")));
    }

    /// Race: `has-session` says no, `new-session` loses to a concurrent
    /// caller, we re-check `has-session` (now yes), list windows, and add
    /// our window since the race winner created a different one.
    #[test]
    fn ensure_session_window_recovers_from_lost_race_by_adding_window() {
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
            MockCall {
                expect_args_contain: Some(vec!["list-windows"]),
                result: ok("term-1\n"),
            },
            MockCall {
                expect_args_contain: Some(vec!["new-window", "-n", "term-2"]),
                result: ok(""),
            },
        ]);
        let cli = TmuxCli::new("s", mock);
        cli.ensure_session_window("foo", "term-2", "/tmp", None).unwrap();
        assert_eq!(cli.runner.seen_args().len(), 5);
    }

    /// Race with same-window winner: the race winner asked for the same
    /// window name as we did. After the duplicate-session error, the
    /// list-windows shows our target already present and we return Ok
    /// without a new-window call.
    #[test]
    fn ensure_session_window_recovers_from_lost_race_when_winner_created_same_window() {
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
            MockCall {
                expect_args_contain: Some(vec!["list-windows"]),
                result: ok("term-1\n"),
            },
        ]);
        let cli = TmuxCli::new("s", mock);
        cli.ensure_session_window("foo", "term-1", "/tmp", None).unwrap();
        let seen = cli.runner.seen_args();
        assert_eq!(seen.len(), 4);
        assert!(!seen.iter().any(|args| args.iter().any(|a| a == "new-window")));
    }

    /// kill_session targets the session with the `=` exact-match prefix so
    /// tmux doesn't fuzzy-match into a sibling session. The kill is
    /// idempotent — a missing session is success, not error, so the
    /// close_tab path is safe to call from cleanup contexts that might
    /// race a previous attempt.
    #[test]
    fn kill_session_targets_session_exact_match() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["kill-session", "=sanctel_wt_x__term-1"]),
            result: ok(""),
        }]);
        let cli = TmuxCli::new("s", mock);
        cli.kill_session("sanctel_wt_x__term-1").unwrap();
    }

    #[test]
    fn kill_session_is_idempotent_on_missing_session() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["kill-session"]),
            result: err("can't find session: nope"),
        }]);
        let cli = TmuxCli::new("s", mock);
        // Must succeed — close_tab cleanups call this without a
        // has-session probe.
        cli.kill_session("nope").unwrap();
    }

    #[test]
    fn kill_session_surfaces_unexpected_errors() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["kill-session"]),
            result: err("some other tmux failure"),
        }]);
        let cli = TmuxCli::new("s", mock);
        assert!(matches!(
            cli.kill_session("x"),
            Err(TmuxError::Command { .. })
        ));
    }

    #[test]
    fn list_sessions_parses_one_name_per_line() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["list-sessions", "#{session_name}"]),
            result: ok("sanctel_wt_a__term-1\nsanctel_wt_a__term-2\nsanctel_wt_b__term-1\n"),
        }]);
        let cli = TmuxCli::new("s", mock);
        let names = cli.list_sessions().unwrap();
        assert_eq!(
            names,
            vec![
                "sanctel_wt_a__term-1",
                "sanctel_wt_a__term-2",
                "sanctel_wt_b__term-1"
            ]
        );
    }

    /// `tmux list-sessions` on a freshly-started server with no sessions
    /// exits non-zero with "no server running on …". The allocator calls
    /// list_sessions before any session has been created, so this MUST
    /// translate to Ok(empty vec) — otherwise the very first
    /// create_tab on a fresh launch fails.
    #[test]
    fn list_sessions_returns_empty_when_no_server() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["list-sessions"]),
            result: err("no server running on /tmp/tmux-1000/sanctel"),
        }]);
        let cli = TmuxCli::new("s", mock);
        assert_eq!(cli.list_sessions().unwrap(), Vec::<String>::new());
    }

    /// A runner that models a tiny piece of "real tmux": a shared map from
    /// session name to its window-name list. has-session / list-windows
    /// read it; new-session (with -n) and new-window write it atomically
    /// (the new-session compare-and-set returns duplicate-session if the
    /// session already exists). Lets us write thread-based concurrency
    /// tests without spawning real tmux.
    struct StateRunner {
        windows: Arc<Mutex<std::collections::HashMap<String, Vec<String>>>>,
        new_session_wins: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl StateRunner {
        fn new() -> Self {
            StateRunner {
                windows: Arc::new(Mutex::new(std::collections::HashMap::new())),
                new_session_wins: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }
        fn clone_shared(&self) -> Self {
            StateRunner {
                windows: Arc::clone(&self.windows),
                new_session_wins: Arc::clone(&self.new_session_wins),
            }
        }
        fn windows_for(&self, session: &str) -> Vec<String> {
            self.windows.lock().unwrap().get(session).cloned().unwrap_or_default()
        }
    }

    fn arg_after<'a>(args: &'a [&'a str], flag: &str) -> Option<&'a str> {
        args.iter().position(|a| *a == flag).and_then(|i| args.get(i + 1)).copied()
    }

    impl CommandRunner for StateRunner {
        fn run(&self, _: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            let sub = args
                .iter()
                .find(|a| {
                    matches!(
                        **a,
                        "has-session"
                            | "new-session"
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
                    let exists = self.windows.lock().unwrap().contains_key(&target);
                    Ok(CommandOutput {
                        status: if exists { 0 } else { 1 },
                        stdout: vec![],
                        stderr: if exists { vec![] } else { b"can't find session".to_vec() },
                    })
                }
                "new-session" => {
                    let name = arg_after(args, "-s").unwrap_or_default().to_string();
                    // The fix for issue #14: the new primitive ALWAYS passes
                    // `-n <window_name>`. Capture it so the simulated session
                    // starts with exactly that one window, never a phantom.
                    let window_name = arg_after(args, "-n").unwrap_or_default().to_string();
                    let mut map = self.windows.lock().unwrap();
                    if let std::collections::hash_map::Entry::Vacant(e) = map.entry(name.clone()) {
                        let initial = if window_name.is_empty() {
                            vec![]
                        } else {
                            vec![window_name]
                        };
                        e.insert(initial);
                        self.new_session_wins
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok(CommandOutput { status: 0, stdout: vec![], stderr: vec![] })
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
                        .unwrap()
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
                    let mut map = self.windows.lock().unwrap();
                    let windows = map.entry(target).or_default();
                    windows.push(name);
                    Ok(CommandOutput { status: 0, stdout: vec![], stderr: vec![] })
                }
                _ => Ok(CommandOutput { status: 0, stdout: vec![], stderr: vec![] }),
            }
        }
    }

    /// Multiple threads calling `ensure_session_window` for the same
    /// (session, window) must all succeed; exactly one `new-session` call
    /// wins the race, and the session ends up with exactly one window
    /// matching the requested name — no phantom, no duplicate.
    #[test]
    fn ensure_session_window_is_concurrent_safe_under_real_race() {
        let shared = StateRunner::new();
        let session = "sanctel_wt_race-test";
        let window = "term-1";
        let cwd = "/tmp";
        const THREADS: usize = 16;

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let runner = shared.clone_shared();
                let session = session.to_string();
                let window = window.to_string();
                let cwd = cwd.to_string();
                std::thread::spawn(move || {
                    let cli = TmuxCli::new("s", runner);
                    cli.ensure_session_window(&session, &window, &cwd, None)
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap().expect("ensure_session_window must succeed");
        }

        assert_eq!(
            shared.new_session_wins.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "exactly one thread should win new-session"
        );
        // The session must end up with exactly one window: the one we asked
        // for. The phantom-window regression would show up here as a second
        // entry; the duplicate-window regression as two `term-1`s.
        let windows = shared.windows_for(session);
        assert_eq!(
            windows,
            vec![window.to_string()],
            "session must contain exactly the requested window — no phantom, no duplicate",
        );
    }
}
