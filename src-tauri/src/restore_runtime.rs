use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::tmux_cli::{CommandRunner, TmuxCli, TmuxError};
use tokio::sync::oneshot;

const ANCHOR_SESSION: &str = "_sanctel_anchor";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreOutcome {
    Restored { sessions: usize },
    NoSnapshot,
}

#[derive(Debug)]
pub enum RestoreError {
    Io(String),
    Tmux(TmuxError),
    HomeNotSet,
}

impl fmt::Display for RestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RestoreError::Io(msg) => write!(f, "restore I/O error: {msg}"),
            RestoreError::Tmux(e) => write!(f, "restore tmux error: {e}"),
            RestoreError::HomeNotSet => write!(f, "HOME not set"),
        }
    }
}

impl std::error::Error for RestoreError {}

impl From<TmuxError> for RestoreError {
    fn from(value: TmuxError) -> Self {
        RestoreError::Tmux(value)
    }
}

pub trait RestoreRuntime: Send + Sync {
    fn restore_on_launch(&self) -> Result<RestoreOutcome, RestoreError>;
    fn save_now(&self) -> Result<(), RestoreError>;
}

#[derive(Clone, Debug)]
pub struct RestorePaths {
    resurrect_dir: PathBuf,
    restore_script: PathBuf,
    save_script: PathBuf,
}

impl RestorePaths {
    pub fn new(
        resurrect_dir: impl Into<PathBuf>,
        restore_script: impl Into<PathBuf>,
        save_script: impl Into<PathBuf>,
    ) -> Self {
        Self {
            resurrect_dir: resurrect_dir.into(),
            restore_script: restore_script.into(),
            save_script: save_script.into(),
        }
    }
}

pub struct ResurrectRuntime<R: CommandRunner = crate::tmux_cli::RealCommandRunner> {
    tmux: Arc<TmuxCli<R>>,
    paths: RestorePaths,
}

pub struct SaveTimerHandle {
    cancel: Option<oneshot::Sender<()>>,
    _task: tokio::task::JoinHandle<()>,
}

impl Drop for SaveTimerHandle {
    fn drop(&mut self) {
        drop(self.cancel.take());
    }
}

impl<R: CommandRunner> ResurrectRuntime<R> {
    pub fn new(tmux: Arc<TmuxCli<R>>, paths: RestorePaths) -> Self {
        Self { tmux, paths }
    }

    fn snapshot_exists(&self) -> Result<bool, RestoreError> {
        std::fs::create_dir_all(&self.paths.resurrect_dir)
            .map_err(|e| RestoreError::Io(e.to_string()))?;

        let entries = std::fs::read_dir(&self.paths.resurrect_dir)
            .map_err(|e| RestoreError::Io(e.to_string()))?;

        for entry in entries {
            let entry = entry.map_err(|e| RestoreError::Io(e.to_string()))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("tmux_resurrect_") && name.ends_with(".txt") {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl<R: CommandRunner + 'static> ResurrectRuntime<R> {
    pub fn start_periodic_save(&self, interval: Duration) -> SaveTimerHandle {
        let tmux = Arc::clone(&self.tmux);
        let save_script = self.paths.save_script.to_string_lossy().into_owned();
        let (cancel, mut cancelled) = oneshot::channel::<()>();

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {
                        match tmux.run_shell(&save_script) {
                            Ok(()) => eprintln!("periodic tmux save succeeded"),
                            Err(e) => eprintln!("periodic tmux save failed: {e}"),
                        }
                    }
                    _ = &mut cancelled => break,
                }
            }
        });

        SaveTimerHandle {
            cancel: Some(cancel),
            _task: task,
        }
    }
}

impl<R: CommandRunner> RestoreRuntime for ResurrectRuntime<R> {
    fn restore_on_launch(&self) -> Result<RestoreOutcome, RestoreError> {
        let had_snapshot = self.snapshot_exists()?;
        let home = std::env::var("HOME").map_err(|_| RestoreError::HomeNotSet)?;
        let restore_script = self.paths.restore_script.to_string_lossy().into_owned();

        self.tmux.new_anchor_session(ANCHOR_SESSION, &home)?;
        let restore_result = self.tmux.run_shell(&restore_script);
        let cleanup_result = self.tmux.kill_session(ANCHOR_SESSION);

        match (restore_result, cleanup_result) {
            (Err(e), _) => return Err(e.into()),
            (Ok(()), Err(e)) => return Err(e.into()),
            (Ok(()), Ok(())) => {}
        }

        if !had_snapshot {
            return Ok(RestoreOutcome::NoSnapshot);
        }

        let sessions = self
            .tmux
            .list_sessions()?
            .into_iter()
            .filter(|session| session != ANCHOR_SESSION)
            .count();
        Ok(RestoreOutcome::Restored { sessions })
    }

    fn save_now(&self) -> Result<(), RestoreError> {
        let save_script = self.paths.save_script.to_string_lossy().into_owned();
        self.tmux.run_shell(&save_script).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux_cli::{CommandOutput, DEFAULT_CONF_PATH};
    use parking_lot::Mutex;
    use std::path::Path;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    type RecordedCalls = Arc<Mutex<Vec<Vec<String>>>>;

    #[derive(Clone)]
    struct RecordingRunner {
        calls: RecordedCalls,
        restore_status: i32,
        restore_stderr: Vec<u8>,
        save_status: i32,
        save_stderr: Vec<u8>,
        list_sessions_stdout: Vec<u8>,
    }

    impl RecordingRunner {
        fn new() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                restore_status: 0,
                restore_stderr: Vec::new(),
                save_status: 0,
                save_stderr: Vec::new(),
                list_sessions_stdout: Vec::new(),
            }
        }

        fn failing_restore() -> Self {
            Self {
                restore_status: 1,
                restore_stderr: b"restore failed".to_vec(),
                ..Self::new()
            }
        }

        fn failing_save() -> Self {
            Self {
                save_status: 1,
                save_stderr: b"save failed".to_vec(),
                ..Self::new()
            }
        }

        fn with_sessions(stdout: &str) -> Self {
            Self {
                list_sessions_stdout: stdout.as_bytes().to_vec(),
                ..Self::new()
            }
        }
    }

    impl CommandRunner for RecordingRunner {
        fn run(&self, _: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            self.calls
                .lock()
                .push(args.iter().map(|arg| arg.to_string()).collect());
            let subcommand = args
                .iter()
                .find(|arg| {
                    matches!(
                        **arg,
                        "new-session" | "run-shell" | "kill-session" | "list-sessions"
                    )
                })
                .copied()
                .unwrap_or("");

            let output = match subcommand {
                "run-shell" => {
                    let script = args.last().copied().unwrap_or_default();
                    if script.ends_with("/save.sh") {
                        CommandOutput {
                            status: self.save_status,
                            stdout: Vec::new(),
                            stderr: self.save_stderr.clone(),
                        }
                    } else {
                        CommandOutput {
                            status: self.restore_status,
                            stdout: Vec::new(),
                            stderr: self.restore_stderr.clone(),
                        }
                    }
                }
                "list-sessions" => CommandOutput {
                    status: 0,
                    stdout: self.list_sessions_stdout.clone(),
                    stderr: Vec::new(),
                },
                _ => CommandOutput {
                    status: 0,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                },
            };
            Ok(output)
        }
    }

    fn temp_restore_dir(test_name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "sanctel-restore-runtime-{test_name}-{}-{unique}",
            std::process::id()
        ))
    }

    fn runtime(
        runner: RecordingRunner,
        resurrect_dir: &Path,
    ) -> (ResurrectRuntime<RecordingRunner>, RecordedCalls) {
        let calls = Arc::clone(&runner.calls);
        let tmux = Arc::new(TmuxCli::new("test", DEFAULT_CONF_PATH, runner));
        let paths = RestorePaths::new(
            resurrect_dir,
            "/bundle/resurrect/scripts/restore.sh",
            "/bundle/resurrect/scripts/save.sh",
        );
        (ResurrectRuntime::new(tmux, paths), calls)
    }

    fn subcommands(calls: &[Vec<String>]) -> Vec<String> {
        calls
            .iter()
            .filter_map(|args| {
                args.iter()
                    .find(|arg| {
                        matches!(
                            arg.as_str(),
                            "new-session" | "run-shell" | "kill-session" | "list-sessions"
                        )
                    })
                    .cloned()
            })
            .collect()
    }

    fn save_call_count(calls: &RecordedCalls) -> usize {
        calls
            .lock()
            .iter()
            .filter(|call| {
                call.iter()
                    .any(|arg| arg == "/bundle/resurrect/scripts/save.sh")
            })
            .count()
    }

    async fn wait_for_save_calls(calls: &RecordedCalls, min: usize) {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        loop {
            if save_call_count(calls) >= min {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "expected at least {min} save calls, got {}",
                    save_call_count(calls)
                );
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[test]
    fn restore_on_launch_runs_anchor_restore_cleanup_and_counts_sessions() {
        let dir = temp_restore_dir("restored");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tmux_resurrect_20260521T120000.txt"), "").unwrap();
        let runner = RecordingRunner::with_sessions(
            "_sanctel_anchor\nsanctel_wt_main__term-1\nsanctel_wt_main__term-2\n",
        );
        let (runtime, calls) = runtime(runner, &dir);

        let outcome = runtime.restore_on_launch().unwrap();

        assert_eq!(outcome, RestoreOutcome::Restored { sessions: 2 });
        let calls = calls.lock().clone();
        assert_eq!(
            subcommands(&calls),
            vec!["new-session", "run-shell", "kill-session", "list-sessions"],
        );
        assert!(calls[1]
            .iter()
            .any(|arg| arg == "/bundle/resurrect/scripts/restore.sh"));
    }

    #[test]
    fn restore_on_launch_returns_no_snapshot_after_noop_restore() {
        let dir = temp_restore_dir("empty");
        let (runtime, calls) = runtime(RecordingRunner::new(), &dir);

        let outcome = runtime.restore_on_launch().unwrap();

        assert_eq!(outcome, RestoreOutcome::NoSnapshot);
        assert_eq!(
            subcommands(&calls.lock()),
            vec!["new-session", "run-shell", "kill-session"],
        );
    }

    #[test]
    fn restore_on_launch_kills_anchor_when_restore_fails() {
        let dir = temp_restore_dir("failure");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tmux_resurrect_20260521T120000.txt"), "").unwrap();
        let (runtime, calls) = runtime(RecordingRunner::failing_restore(), &dir);

        let result = runtime.restore_on_launch();

        assert!(matches!(result, Err(RestoreError::Tmux(_))));
        assert_eq!(
            subcommands(&calls.lock()),
            vec!["new-session", "run-shell", "kill-session"],
        );
    }

    #[test]
    fn save_now_runs_bundled_save_script_once() {
        let dir = temp_restore_dir("save");
        let (runtime, calls) = runtime(RecordingRunner::new(), &dir);

        runtime.save_now().unwrap();

        let calls = calls.lock().clone();
        assert_eq!(subcommands(&calls), vec!["run-shell"]);
        assert!(calls[0]
            .iter()
            .any(|arg| arg == "/bundle/resurrect/scripts/save.sh"));
    }

    #[test]
    fn save_now_returns_restore_error_when_save_script_fails() {
        let dir = temp_restore_dir("save-failure");
        let (runtime, calls) = runtime(RecordingRunner::failing_save(), &dir);

        let result = runtime.save_now();

        assert!(matches!(result, Err(RestoreError::Tmux(_))));
        assert_eq!(subcommands(&calls.lock()), vec!["run-shell"]);
    }

    #[tokio::test]
    async fn periodic_save_invokes_save_at_interval() {
        let dir = temp_restore_dir("periodic-save");
        let (runtime, calls) = runtime(RecordingRunner::new(), &dir);

        let _handle = runtime.start_periodic_save(Duration::from_millis(20));
        wait_for_save_calls(&calls, 2).await;

        let save_calls = save_call_count(&calls);
        assert!(
            save_calls >= 2,
            "expected at least two periodic saves, got {save_calls}"
        );
    }

    #[tokio::test]
    async fn dropping_periodic_save_handle_stops_timer() {
        let dir = temp_restore_dir("periodic-save-cancel");
        let (runtime, calls) = runtime(RecordingRunner::new(), &dir);

        let handle = runtime.start_periodic_save(Duration::from_millis(20));
        wait_for_save_calls(&calls, 1).await;
        drop(handle);
        let calls_after_drop = save_call_count(&calls);

        tokio::time::sleep(Duration::from_millis(75)).await;

        assert_eq!(save_call_count(&calls), calls_after_drop);
    }
}
