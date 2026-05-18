// ───────────────────────────────────────────────────────────────────────────
// zellij_cli — small typed wrapper over the `zellij` subprocess CLI.
//
// Slice 2 (issue #18) ships the session-lifecycle methods on top of slice 1's
// `version()` probe:
//   - `new_session(name, cwd, initial_command)`
//   - `has_session(name)`
//   - `kill_session(name)` (idempotent on missing)
//   - `list_sessions()`
//
// Shape mirrors `tmux_cli` deliberately so a contributor who knows one knows
// the other. The `CommandRunner` trait is reused so the same mock-runner test
// pattern works for both backends; no duplicate abstraction.
//
// Zellij quirks encoded here (the only place they're allowed to live):
//   - `zellij` has no `-c <cwd>` flag analogous to tmux's `new-session -c`.
//     `new_session` sets cwd via `CommandRunner::run_in_dir` (which the real
//     runner threads to `Command::current_dir`).
//   - The non-interactive create path is `zellij attach --create-background
//     <name>` — `zellij --session <name>` attaches the current process,
//     `attach --create-background` does not. Quirk discovered: this command
//     is idempotent on already-existing sessions (zellij's "create if not
//     exists" semantics), so the race-retry pattern from tmux's
//     `new-session` rarely fires; we still surface the "session already
//     exists" stderr as `SessionAlreadyExists` defensively for edge cases
//     (e.g., zellij's resurrectable-exited-session path).
//   - `zellij list-sessions -s` (`--short`) prints one session name per line
//     with no decoration. The default format adds `[Created N mins ago]`
//     and decorates EXITED sessions; `-s` is the parseable form, mirroring
//     `tmux list-sessions -F '#{session_name}'`.
//   - On a freshly-started zellij with no sessions, `list-sessions` exits
//     non-zero with stderr "No active zellij sessions found." — handled as
//     `Ok(Vec::new())` so callers don't special-case the cold-start path.
//   - `kill-session <name>` errors with "No session named ... found." when
//     the session is missing; that case maps to `Ok(())` so `close_tab`-
//     style cleanup paths are safe to call without a `has_session` probe
//     first. Same idempotency contract as `tmux_cli::kill_session`.
// ───────────────────────────────────────────────────────────────────────────

use std::fmt;

use crate::tmux_cli::{CommandRunner, RealCommandRunner};

/// Errors surfaced from the zellij wrapper. Shape mirrors `TmuxError` —
/// callers either retry (Race) or surface to the user (everything else).
#[derive(Debug, Clone)]
pub enum ZellijError {
    /// `zellij` failed to spawn (zellij likely not installed).
    NotFound(String),
    /// A zellij invocation exited non-zero with a parseable stderr.
    Command { command: String, stderr: String },
    /// `new_session` lost a race with a concurrent caller — caller should
    /// retry. See module docs for why this rarely fires under
    /// `attach --create-background` semantics. Unused until slice 3 wires
    /// the per-Worktree allocator for the zellij path.
    #[allow(dead_code)]
    SessionAlreadyExists(String),
}

impl fmt::Display for ZellijError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ZellijError::NotFound(msg) => write!(f, "zellij not found: {msg}"),
            ZellijError::Command { command, stderr } => {
                write!(f, "zellij {command} failed: {stderr}")
            }
            ZellijError::SessionAlreadyExists(name) => {
                write!(f, "zellij session already exists: {name}")
            }
        }
    }
}

impl std::error::Error for ZellijError {}

/// zellij wrapper. Constructed with a runner (mock in tests, real shell-out
/// in production via the `Default` impl).
pub struct ZellijCli<R: CommandRunner = RealCommandRunner> {
    runner: R,
}

impl Default for ZellijCli<RealCommandRunner> {
    fn default() -> Self {
        ZellijCli::new(RealCommandRunner)
    }
}

impl<R: CommandRunner> ZellijCli<R> {
    pub fn new(runner: R) -> Self {
        ZellijCli { runner }
    }

    /// `zellij --version` → "zellij 0.42.0" (or similar). Used by the
    /// startup probe when `SANCTEL_BACKEND=zellij`. Same role as the tmux
    /// probe — if this errors, sanctel surfaces a setup-screen via the
    /// existing status channel.
    pub fn version(&self) -> Result<String, ZellijError> {
        let out = self
            .runner
            .run("zellij", &["--version"])
            .map_err(|e| ZellijError::NotFound(e.to_string()))?;
        if out.status != 0 {
            return Err(ZellijError::Command {
                command: "--version".into(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// `zellij attach --create-background <name>` — creates the named
    /// session if it doesn't exist and immediately returns without
    /// attaching. This is the closest zellij equivalent to
    /// `tmux new-session -d`; see module docs for the quirk note.
    ///
    /// `cwd` is threaded via `run_in_dir` (zellij has no CLI flag for it).
    ///
    /// `initial_command` is accepted for API symmetry with
    /// `tmux_cli::ensure_session_window`. The spike does NOT transmit it
    /// to zellij at this layer — zellij has no clean CLI to start a
    /// session with a command running (would require a generated layout
    /// file). The WebSocket attach path (slice 3) writes the command into
    /// the pane after the session exists.
    ///
    /// Returns `Err(SessionAlreadyExists)` if zellij's stderr signals a
    /// duplicate-session collision (rare given `--create-background`'s
    /// idempotent semantics, but kept as a defensive recovery path).
    #[allow(dead_code)] // Wired up by slice 3 (`attach_tab_to_zellij`).
    pub fn new_session(
        &self,
        name: &str,
        cwd: &str,
        initial_command: Option<&str>,
    ) -> Result<(), ZellijError> {
        let _ = initial_command;
        let args = ["attach", "--create-background", name];
        let out = self
            .runner
            .run_in_dir("zellij", &args, cwd)
            .map_err(|e| ZellijError::NotFound(e.to_string()))?;
        if out.status == 0 {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("session already exists")
            || stderr.contains("session with this name already exists")
            || stderr.contains("already exists")
        {
            return Err(ZellijError::SessionAlreadyExists(name.into()));
        }
        Err(ZellijError::Command {
            command: format!("attach --create-background {name}"),
            stderr: stderr.into_owned(),
        })
    }

    /// Returns true if a session with the given name is present in
    /// `list_sessions()`. Zellij has no dedicated `has-session` subcommand
    /// (tmux does), so this is one `list-sessions -s` call plus membership
    /// check. Cheap enough for sanctel's call patterns.
    #[allow(dead_code)] // Wired up by slice 3.
    pub fn has_session(&self, name: &str) -> Result<bool, ZellijError> {
        Ok(self.list_sessions()?.iter().any(|s| s == name))
    }

    /// `zellij kill-session <name>`. Idempotent on a session that doesn't
    /// exist — zellij's "No session named ... found." error is swallowed
    /// so `close_tab`-style cleanups are safe to call without a
    /// has-session probe first. Same contract as `tmux_cli::kill_session`.
    #[allow(dead_code)] // Wired up by slice 3 (`close_tab` zellij path).
    pub fn kill_session(&self, name: &str) -> Result<(), ZellijError> {
        let out = self
            .runner
            .run("zellij", &["kill-session", name])
            .map_err(|e| ZellijError::NotFound(e.to_string()))?;
        if out.status == 0 {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("No session named")
            || stderr.contains("not found")
            || stderr.contains("no active zellij sessions")
            || stderr.contains("No active zellij sessions")
        {
            return Ok(());
        }
        Err(ZellijError::Command {
            command: format!("kill-session {name}"),
            stderr: stderr.into_owned(),
        })
    }

    /// `zellij list-sessions -s` — one session name per line, no
    /// decoration. The allocator (lib.rs `allocate_session_for_tab`-
    /// analog for the zellij path) will call this and scan for the
    /// per-Worktree-base prefix, mirroring the tmux pattern.
    ///
    /// Returns an empty Vec (not an error) when there are no sessions:
    /// zellij exits non-zero with "No active zellij sessions found." on a
    /// freshly-started server. The allocator calls this before any
    /// session exists, so the empty case is the hot path on first launch.
    #[allow(dead_code)] // Wired up by slice 3 (allocator + has_session).
    pub fn list_sessions(&self) -> Result<Vec<String>, ZellijError> {
        let out = self
            .runner
            .run("zellij", &["list-sessions", "-s"])
            .map_err(|e| ZellijError::NotFound(e.to_string()))?;
        if out.status != 0 {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            // zellij prints the "no sessions" notice on either stream
            // depending on version — handle both.
            if stderr.contains("No active zellij sessions")
                || stderr.contains("no active zellij sessions")
                || stdout.contains("No active zellij sessions")
                || stdout.contains("no active zellij sessions")
            {
                return Ok(Vec::new());
            }
            return Err(ZellijError::Command {
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
    use crate::tmux_cli::CommandOutput;
    use std::sync::Mutex;

    /// Scripted runner: each `run`/`run_in_dir` call shifts the front of an
    /// expectation queue and returns its canned output. Records every call's
    /// program, args, and cwd (if any).
    struct MockRunner {
        scripted: Mutex<Vec<MockCall>>,
        seen: Mutex<Vec<SeenCall>>,
    }

    #[derive(Debug, Clone)]
    struct SeenCall {
        program: String,
        args: Vec<String>,
        cwd: Option<String>,
    }

    struct MockCall {
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

        fn record(&self, program: &str, args: &[&str], cwd: Option<&str>) {
            self.seen.lock().unwrap().push(SeenCall {
                program: program.to_string(),
                args: args.iter().map(|s| s.to_string()).collect(),
                cwd: cwd.map(str::to_string),
            });
        }

        fn next(&self, args: &[&str]) -> std::io::Result<CommandOutput> {
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

        fn seen(&self) -> Vec<SeenCall> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl CommandRunner for MockRunner {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            self.record(program, args, None);
            self.next(args)
        }
        fn run_in_dir(
            &self,
            program: &str,
            args: &[&str],
            cwd: &str,
        ) -> std::io::Result<CommandOutput> {
            self.record(program, args, Some(cwd));
            self.next(args)
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

    // ─── version (slice 1, unchanged) ────────────────────────────────────

    /// The probe shells out to exactly `zellij --version` — that's the
    /// command the version-probe acceptance criterion calls out and it's the
    /// shape zellij has carried since 0.x.
    #[test]
    fn version_invokes_zellij_with_version_flag() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["--version"]),
            result: ok("zellij 0.42.0\n"),
        }]);
        let cli = ZellijCli::new(mock);
        let v = cli.version().expect("version succeeds");
        assert_eq!(v, "zellij 0.42.0");
        let seen = cli.runner.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].program, "zellij");
        assert_eq!(seen[0].args, vec!["--version"]);
    }

    /// stdout is trimmed (trailing newline from the CLI is normalised away).
    #[test]
    fn version_trims_trailing_whitespace() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["--version"]),
            result: ok("  zellij 0.42.0  \n\n"),
        }]);
        let cli = ZellijCli::new(mock);
        assert_eq!(cli.version().unwrap(), "zellij 0.42.0");
    }

    /// Spawn failure (executable not on PATH) maps to `NotFound`. This is
    /// the path that triggers the setup-screen flow when
    /// `SANCTEL_BACKEND=zellij` is set but zellij isn't installed
    /// (acceptance criterion: missing zellij produces a clear error rather
    /// than a confusing tab-by-tab failure).
    #[test]
    fn missing_zellij_surfaces_as_not_found() {
        struct FailingRunner;
        impl CommandRunner for FailingRunner {
            fn run(&self, _: &str, _: &[&str]) -> std::io::Result<CommandOutput> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no such file",
                ))
            }
        }
        let cli = ZellijCli::new(FailingRunner);
        match cli.version() {
            Err(ZellijError::NotFound(_)) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    /// `zellij --version` exiting non-zero (unlikely in practice, but
    /// possible on a broken install) maps to `Command` with the stderr
    /// captured so the user sees what went wrong.
    #[test]
    fn nonzero_exit_surfaces_as_command_error() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["--version"]),
            result: Ok(CommandOutput {
                status: 1,
                stdout: vec![],
                stderr: b"broken zellij binary".to_vec(),
            }),
        }]);
        let cli = ZellijCli::new(mock);
        match cli.version() {
            Err(ZellijError::Command { command, stderr }) => {
                assert!(command.contains("--version"));
                assert!(stderr.contains("broken zellij binary"));
            }
            other => panic!("expected Command error, got {other:?}"),
        }
    }

    // ─── new_session ─────────────────────────────────────────────────────

    /// `new_session` invokes `zellij attach --create-background <name>` and
    /// threads the requested cwd through `run_in_dir` (the production
    /// runner translates this to `Command::current_dir`). The argv shape is
    /// the load-bearing piece this test pins: a regression that drops
    /// `--create-background` would silently attach the parent and hang.
    #[test]
    fn new_session_invokes_attach_create_background_with_cwd() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["attach", "--create-background", "foo"]),
            result: ok(""),
        }]);
        let cli = ZellijCli::new(mock);
        cli.new_session("foo", "/home/me/wt", None).unwrap();
        let seen = cli.runner.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].program, "zellij");
        assert_eq!(
            seen[0].args,
            vec!["attach", "--create-background", "foo"]
        );
        assert_eq!(seen[0].cwd.as_deref(), Some("/home/me/wt"));
    }

    /// `initial_command` is accepted for API symmetry but is NOT spliced
    /// into the CLI argv — zellij has no clean CLI flag to start a session
    /// with a command running. The byte-stream reaches the pane via the
    /// attach path instead. This test catches a regression that silently
    /// appends to argv (which zellij would ignore or misinterpret as a
    /// session-name fragment depending on the version).
    #[test]
    fn new_session_does_not_splice_initial_command_into_argv() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["attach", "--create-background", "foo"]),
            result: ok(""),
        }]);
        let cli = ZellijCli::new(mock);
        cli.new_session("foo", "/tmp", Some("claude --resume abc"))
            .unwrap();
        let seen = cli.runner.seen();
        assert_eq!(seen.len(), 1);
        assert!(
            !seen[0].args.iter().any(|a| a.contains("claude")),
            "initial_command must not appear in argv: {:?}",
            seen[0].args,
        );
    }

    /// Race-retry contract: when zellij's stderr signals a duplicate-
    /// session collision, `new_session` returns `SessionAlreadyExists` so
    /// callers can recover (mirrors tmux's `new-session` race-retry path).
    /// Rare in practice under `attach --create-background`'s idempotent
    /// semantics, but kept defensively for edge cases.
    #[test]
    fn new_session_surfaces_session_already_exists_for_race_retry() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["attach", "--create-background"]),
            result: err("session already exists: foo"),
        }]);
        let cli = ZellijCli::new(mock);
        match cli.new_session("foo", "/tmp", None) {
            Err(ZellijError::SessionAlreadyExists(name)) => assert_eq!(name, "foo"),
            other => panic!("expected SessionAlreadyExists, got {other:?}"),
        }
    }

    /// Any other non-zero exit surfaces as `Command` with stderr captured —
    /// the same shape `version` uses on a broken install. This is the path
    /// a stale layout file, permission denial, or zellij internal error
    /// takes.
    #[test]
    fn new_session_surfaces_unexpected_errors() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["attach"]),
            result: err("zellij: panic in plugin loader"),
        }]);
        let cli = ZellijCli::new(mock);
        match cli.new_session("foo", "/tmp", None) {
            Err(ZellijError::Command { command, stderr }) => {
                assert!(command.contains("foo"));
                assert!(stderr.contains("panic"));
            }
            other => panic!("expected Command error, got {other:?}"),
        }
    }

    // ─── has_session ─────────────────────────────────────────────────────

    /// has_session delegates to list_sessions + membership; one `zellij
    /// list-sessions -s` invocation, returns true when the name is present.
    #[test]
    fn has_session_is_true_when_listed() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["list-sessions", "-s"]),
            result: ok("alpha\nfoo\nbeta\n"),
        }]);
        let cli = ZellijCli::new(mock);
        assert!(cli.has_session("foo").unwrap());
    }

    #[test]
    fn has_session_is_false_when_absent() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["list-sessions"]),
            result: ok("alpha\nbeta\n"),
        }]);
        let cli = ZellijCli::new(mock);
        assert!(!cli.has_session("foo").unwrap());
    }

    /// On a freshly-started zellij with no sessions, has_session must
    /// return false (not error) — same cold-start invariant tmux has.
    #[test]
    fn has_session_is_false_on_empty_server() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["list-sessions"]),
            result: err("No active zellij sessions found."),
        }]);
        let cli = ZellijCli::new(mock);
        assert!(!cli.has_session("foo").unwrap());
    }

    // ─── kill_session ────────────────────────────────────────────────────

    /// kill_session argv shape: `zellij kill-session <name>`. The
    /// session-name is positional; no `-t` flag (zellij differs from tmux
    /// here).
    #[test]
    fn kill_session_targets_session_by_positional_name() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["kill-session", "sanctel_wt_x__term-1"]),
            result: ok(""),
        }]);
        let cli = ZellijCli::new(mock);
        cli.kill_session("sanctel_wt_x__term-1").unwrap();
        let seen = cli.runner.seen();
        assert_eq!(seen[0].args, vec!["kill-session", "sanctel_wt_x__term-1"]);
    }

    /// Missing-session is swallowed — close_tab cleanups call this
    /// without a has-session probe. The "No session named ... found."
    /// stderr is the exact string zellij prints for this case.
    #[test]
    fn kill_session_is_idempotent_on_missing_session() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["kill-session"]),
            result: err("No session named \"nope\" found."),
        }]);
        let cli = ZellijCli::new(mock);
        cli.kill_session("nope").unwrap();
    }

    /// Idempotent also when zellij has no sessions at all (the empty-
    /// server case). Some zellij versions answer with the "no active
    /// sessions" message on kill rather than the per-name message.
    #[test]
    fn kill_session_is_idempotent_on_empty_server() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["kill-session"]),
            result: err("No active zellij sessions found."),
        }]);
        let cli = ZellijCli::new(mock);
        cli.kill_session("nope").unwrap();
    }

    /// Anything that isn't the missing-session class surfaces as
    /// `Command` — e.g., a permission denial reading the session socket.
    #[test]
    fn kill_session_surfaces_unexpected_errors() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["kill-session"]),
            result: err("Permission denied on socket"),
        }]);
        let cli = ZellijCli::new(mock);
        assert!(matches!(
            cli.kill_session("x"),
            Err(ZellijError::Command { .. })
        ));
    }

    // ─── list_sessions ───────────────────────────────────────────────────

    /// Happy path: `-s` (`--short`) prints one name per line.
    #[test]
    fn list_sessions_parses_one_name_per_line() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["list-sessions", "-s"]),
            result: ok("sanctel_wt_a__term-1\nsanctel_wt_a__term-2\nsanctel_wt_b__term-1\n"),
        }]);
        let cli = ZellijCli::new(mock);
        assert_eq!(
            cli.list_sessions().unwrap(),
            vec![
                "sanctel_wt_a__term-1",
                "sanctel_wt_a__term-2",
                "sanctel_wt_b__term-1"
            ],
        );
    }

    #[test]
    fn list_sessions_ignores_blank_lines() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["list-sessions"]),
            result: ok("alpha\n\nbeta\n\n"),
        }]);
        let cli = ZellijCli::new(mock);
        assert_eq!(cli.list_sessions().unwrap(), vec!["alpha", "beta"]);
    }

    /// On a fresh zellij with no sessions, list-sessions exits non-zero
    /// with "No active zellij sessions found." on stderr. The allocator
    /// (zellij path) calls list_sessions before any session exists, so
    /// this MUST translate to `Ok(empty vec)` — otherwise the very first
    /// create_tab on a fresh launch fails.
    #[test]
    fn list_sessions_returns_empty_when_no_sessions_on_stderr() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["list-sessions"]),
            result: err("No active zellij sessions found."),
        }]);
        let cli = ZellijCli::new(mock);
        assert_eq!(cli.list_sessions().unwrap(), Vec::<String>::new());
    }

    /// Some zellij versions print the "no active sessions" notice on
    /// stdout while still exiting non-zero. Handle both streams.
    #[test]
    fn list_sessions_returns_empty_when_no_sessions_on_stdout() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["list-sessions"]),
            result: Ok(CommandOutput {
                status: 1,
                stdout: b"No active zellij sessions found.\n".to_vec(),
                stderr: vec![],
            }),
        }]);
        let cli = ZellijCli::new(mock);
        assert_eq!(cli.list_sessions().unwrap(), Vec::<String>::new());
    }

    /// Any other non-zero exit surfaces — broken zellij socket, etc.
    #[test]
    fn list_sessions_surfaces_unexpected_errors() {
        let mock = MockRunner::new(vec![MockCall {
            expect_args_contain: Some(vec!["list-sessions"]),
            result: err("Could not connect to zellij socket"),
        }]);
        let cli = ZellijCli::new(mock);
        assert!(matches!(
            cli.list_sessions(),
            Err(ZellijError::Command { .. })
        ));
    }

    // ─── real-zellij integration ─────────────────────────────────────────

    /// One real-zellij end-to-end: create → has → list → kill → verify
    /// removal → kill again (idempotent). Mirrors
    /// `idempotent_attach_against_real_tmux` in shape and gating.
    ///
    /// Skips when zellij isn't installed (sandcastle CI doesn't ship it),
    /// matching the `tmux_available()` pattern. On a zellij-bearing dev
    /// box this exercises the full CLI roundtrip and surfaces any quirks
    /// (output format drift, unexpected stderr strings) the mock tests
    /// can't predict.
    #[test]
    fn idempotent_lifecycle_against_real_zellij() {
        if !zellij_available() {
            eprintln!("skipping: zellij not installed");
            return;
        }

        let cli = ZellijCli::default();
        let session = format!("sanctel-cli-test-{}", std::process::id());
        let cwd = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());

        // Belt-and-braces cleanup if a prior run leaked.
        let _ = cli.kill_session(&session);

        // 1. Create the session.
        cli.new_session(&session, &cwd, None).expect("new_session");

        // 2. has_session sees it; list_sessions contains it.
        assert!(cli.has_session(&session).unwrap(), "session must be visible after creation");
        assert!(
            cli.list_sessions().unwrap().iter().any(|s| s == &session),
            "list_sessions must contain the created session",
        );

        // 3. Kill removes it.
        cli.kill_session(&session).expect("kill_session");

        // Zellij keeps the session resurrectable on disk by default —
        // post-kill, has_session may still return true with status
        // "EXITED". The acceptance criterion is "kill terminates the
        // session"; a more aggressive `delete-session` is out of scope
        // for this slice. We only assert the kill itself succeeded and
        // is idempotent on a second call.

        // 4. Second kill is idempotent (the close_tab contract).
        cli.kill_session(&session).expect("kill_session is idempotent");
    }

    fn zellij_available() -> bool {
        std::process::Command::new("zellij")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}
