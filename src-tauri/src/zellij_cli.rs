// ───────────────────────────────────────────────────────────────────────────
// zellij_cli — small typed wrapper over the `zellij` subprocess CLI.
//
// Slice 1 (this file) ships only `version()`, the startup probe analogous to
// `tmux_cli::version()`. Slice 2 (issue #18) will grow it with session
// lifecycle commands (`new-session`, `has-session`, `kill-session`,
// `list-sessions`). The shape mirrors `tmux_cli` deliberately so a contributor
// who knows one knows the other.
//
// `CommandRunner` is reused from `tmux_cli` so the same mock-runner test
// pattern works for both backends; no need to duplicate that abstraction.
// ───────────────────────────────────────────────────────────────────────────

use std::fmt;

use crate::tmux_cli::{CommandRunner, RealCommandRunner};

/// Errors surfaced from the zellij wrapper. Shape mirrors `TmuxError` —
/// callers either retry (none of those today) or surface to the user
/// (everything else).
#[derive(Debug, Clone)]
pub enum ZellijError {
    /// `zellij --version` failed to spawn (zellij likely not installed).
    NotFound(String),
    /// A zellij invocation exited non-zero with a parseable stderr.
    Command { command: String, stderr: String },
}

impl fmt::Display for ZellijError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ZellijError::NotFound(msg) => write!(f, "zellij not found: {msg}"),
            ZellijError::Command { command, stderr } => {
                write!(f, "zellij {command} failed: {stderr}")
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
}

// ─── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux_cli::CommandOutput;
    use std::sync::Mutex;

    struct MockRunner {
        program: Mutex<Option<String>>,
        args: Mutex<Option<Vec<String>>>,
        result: Mutex<Option<std::io::Result<CommandOutput>>>,
    }

    impl MockRunner {
        fn new(result: std::io::Result<CommandOutput>) -> Self {
            MockRunner {
                program: Mutex::new(None),
                args: Mutex::new(None),
                result: Mutex::new(Some(result)),
            }
        }
    }

    impl CommandRunner for MockRunner {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            *self.program.lock().unwrap() = Some(program.to_string());
            *self.args.lock().unwrap() =
                Some(args.iter().map(|s| s.to_string()).collect());
            self.result.lock().unwrap().take().expect("MockRunner used twice")
        }
    }

    fn ok(stdout: &str) -> std::io::Result<CommandOutput> {
        Ok(CommandOutput {
            status: 0,
            stdout: stdout.as_bytes().to_vec(),
            stderr: vec![],
        })
    }

    /// The probe shells out to exactly `zellij --version` — that's the
    /// command the version-probe acceptance criterion calls out and it's the
    /// shape zellij has carried since 0.x.
    #[test]
    fn version_invokes_zellij_with_version_flag() {
        let mock = MockRunner::new(ok("zellij 0.42.0\n"));
        let cli = ZellijCli::new(mock);
        let v = cli.version().expect("version succeeds");
        assert_eq!(v, "zellij 0.42.0");
        assert_eq!(cli.runner.program.lock().unwrap().as_deref(), Some("zellij"));
        assert_eq!(
            cli.runner.args.lock().unwrap().as_deref(),
            Some(["--version".to_string()].as_slice()),
        );
    }

    /// stdout is trimmed (trailing newline from the CLI is normalised away).
    #[test]
    fn version_trims_trailing_whitespace() {
        let mock = MockRunner::new(ok("  zellij 0.42.0  \n\n"));
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
        let mock = MockRunner::new(Ok(CommandOutput {
            status: 1,
            stdout: vec![],
            stderr: b"broken zellij binary".to_vec(),
        }));
        let cli = ZellijCli::new(mock);
        match cli.version() {
            Err(ZellijError::Command { command, stderr }) => {
                assert!(command.contains("--version"));
                assert!(stderr.contains("broken zellij binary"));
            }
            other => panic!("expected Command error, got {other:?}"),
        }
    }
}
