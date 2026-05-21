use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::snapshot_rewriter::{self, AgentResume};
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
    hooks_dir: Option<PathBuf>,
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
            hooks_dir: None,
        }
    }

    #[cfg(test)]
    pub fn with_hooks_dir(mut self, hooks_dir: impl Into<PathBuf>) -> Self {
        self.hooks_dir = Some(hooks_dir.into());
        self
    }
}

pub struct ResurrectRuntime<R: CommandRunner = crate::tmux_cli::RealCommandRunner> {
    tmux: Arc<TmuxCli<R>>,
    paths: RestorePaths,
}

pub struct SaveTimerHandle {
    cancel: Option<oneshot::Sender<()>>,
    _task: tauri::async_runtime::JoinHandle<()>,
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
        let paths = self.paths.clone();
        let (cancel, mut cancelled) = oneshot::channel::<()>();

        // Use tauri::async_runtime::spawn (not tokio::spawn) so this works
        // when called from `.setup` — there is no ambient tokio runtime at
        // that point, but Tauri's runtime is available. tests pass either way
        // because tauri::async_runtime uses tokio under the hood.
        let task = tauri::async_runtime::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {
                        match save_with_snapshot_rewrite(&tmux, &paths) {
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

fn run_save_script<R: CommandRunner>(
    tmux: &TmuxCli<R>,
    save_script: &str,
) -> Result<(), RestoreError> {
    tmux.run_shell(save_script).map_err(Into::into)
}

fn save_with_snapshot_rewrite<R: CommandRunner>(
    tmux: &TmuxCli<R>,
    paths: &RestorePaths,
) -> Result<(), RestoreError> {
    let save_script = paths.save_script.to_string_lossy().into_owned();
    run_save_script(tmux, &save_script)?;
    rewrite_latest_snapshot(paths)
}

fn rewrite_latest_snapshot(paths: &RestorePaths) -> Result<(), RestoreError> {
    let Some(snapshot_path) = latest_snapshot_path(&paths.resurrect_dir)? else {
        return Ok(());
    };
    let hooks_dir = match &paths.hooks_dir {
        Some(path) => path.clone(),
        None => crate::hook_handler::default_hooks_dir().map_err(RestoreError::Io)?,
    };
    rewrite_snapshot_file(&snapshot_path, &hooks_dir)
}

fn latest_snapshot_path(resurrect_dir: &Path) -> Result<Option<PathBuf>, RestoreError> {
    let last_path = resurrect_dir.join("last");
    if !last_path.exists() {
        return Ok(None);
    }
    Ok(Some(std::fs::canonicalize(&last_path).unwrap_or(last_path)))
}

fn rewrite_snapshot_file(snapshot_path: &Path, hooks_dir: &Path) -> Result<(), RestoreError> {
    let captures = crate::agent_session_watcher::read_agent_session_captures(hooks_dir)
        .map_err(RestoreError::Io)?;
    let capture_map = snapshot_rewriter::capture_map(captures.into_iter().map(|capture| {
        (
            capture.session_name,
            AgentResume {
                agent: capture.agent,
                session_id: capture.session_id,
            },
            capture.ts,
        )
    }));
    if capture_map.is_empty() {
        return Ok(());
    }

    let snapshot = std::fs::read_to_string(snapshot_path)
        .map_err(|e| RestoreError::Io(format!("read snapshot failed: {e}")))?;
    let rewritten = snapshot_rewriter::rewrite_snapshot(&snapshot, &capture_map);
    if rewritten == snapshot {
        return Ok(());
    }

    let tmp_path = tmp_path_for(snapshot_path)?;
    std::fs::write(&tmp_path, rewritten)
        .map_err(|e| RestoreError::Io(format!("write snapshot tmp failed: {e}")))?;
    std::fs::rename(&tmp_path, snapshot_path)
        .map_err(|e| RestoreError::Io(format!("rename snapshot tmp failed: {e}")))
}

fn tmp_path_for(path: &Path) -> Result<PathBuf, RestoreError> {
    let file_name = path.file_name().ok_or_else(|| {
        RestoreError::Io(format!("snapshot path has no filename: {}", path.display()))
    })?;
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(".tmp");
    Ok(path.with_file_name(tmp_name))
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
        save_with_snapshot_rewrite(&self.tmux, &self.paths)
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

    async fn wait_for_file_contains(path: &Path, needle: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        loop {
            if std::fs::read_to_string(path)
                .map(|body| body.contains(needle))
                .unwrap_or(false)
            {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "expected {} to contain {needle:?}, got {:?}",
                    path.display(),
                    std::fs::read_to_string(path),
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

    #[test]
    fn save_now_rewrites_latest_snapshot_after_save_script() {
        let dir = temp_restore_dir("save-rewrite");
        let hooks_dir = dir.join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(
            hooks_dir.join("sanctel_wt_sanctel-main__term-1.json"),
            r#"{"agent":"claude","session_id":"claude-session-1","ts":1779311720}"#,
        )
        .unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let snapshot = dir.join("tmux_resurrect_20260521T120000.txt");
        std::fs::write(
            &snapshot,
            "pane\tsanctel_wt_sanctel-main__term-1\t0\t1\t:*\t0\tclaude\t:/repo\t1\tclaude\t:claude\n",
        )
        .unwrap();
        std::os::unix::fs::symlink("tmux_resurrect_20260521T120000.txt", dir.join("last")).unwrap();
        let runner = RecordingRunner::new();
        let calls = Arc::clone(&runner.calls);
        let tmux = Arc::new(TmuxCli::new("test", DEFAULT_CONF_PATH, runner));
        let paths = RestorePaths::new(
            &dir,
            "/bundle/resurrect/scripts/restore.sh",
            "/bundle/resurrect/scripts/save.sh",
        )
        .with_hooks_dir(&hooks_dir);
        let runtime = ResurrectRuntime::new(tmux, paths);

        runtime.save_now().unwrap();

        assert_eq!(subcommands(&calls.lock()), vec!["run-shell"]);
        assert_eq!(
            std::fs::read_to_string(&snapshot).unwrap(),
            "pane\tsanctel_wt_sanctel-main__term-1\t0\t1\t:*\t0\tclaude\t:/repo\t1\tclaude\t:claude --resume claude-session-1\n",
        );
        assert!(!dir.join("tmux_resurrect_20260521T120000.txt.tmp").exists());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn capture_map_prefers_freshest_capture_for_a_session() {
        let map = crate::snapshot_rewriter::capture_map(vec![
            (
                "sanctel_wt_sanctel-main__term-1".to_string(),
                crate::snapshot_rewriter::AgentResume {
                    agent: "claude".to_string(),
                    session_id: "old-session".to_string(),
                },
                1,
            ),
            (
                "sanctel_wt_sanctel-main__term-1".to_string(),
                crate::snapshot_rewriter::AgentResume {
                    agent: "codex".to_string(),
                    session_id: "new-session".to_string(),
                },
                2,
            ),
        ]);

        assert_eq!(
            map.get("sanctel_wt_sanctel-main__term-1"),
            Some(&crate::snapshot_rewriter::AgentResume {
                agent: "codex".to_string(),
                session_id: "new-session".to_string(),
            }),
        );
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

    #[tokio::test]
    async fn periodic_save_rewrites_latest_snapshot() {
        let dir = temp_restore_dir("periodic-save-rewrite");
        let hooks_dir = dir.join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(
            hooks_dir.join("sanctel_wt_sanctel-main__term-1.json"),
            r#"{"agent":"codex","session_id":"codex-session-1","ts":1779311720}"#,
        )
        .unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let snapshot = dir.join("last");
        std::fs::write(
            &snapshot,
            "pane\tsanctel_wt_sanctel-main__term-1\t0\t1\t:*\t0\tcodex\t:/repo\t1\tcodex\t:codex\n",
        )
        .unwrap();
        let runner = RecordingRunner::new();
        let calls = Arc::clone(&runner.calls);
        let tmux = Arc::new(TmuxCli::new("test", DEFAULT_CONF_PATH, runner));
        let paths = RestorePaths::new(
            &dir,
            "/bundle/resurrect/scripts/restore.sh",
            "/bundle/resurrect/scripts/save.sh",
        )
        .with_hooks_dir(&hooks_dir);
        let runtime = ResurrectRuntime::new(tmux, paths);

        let _handle = runtime.start_periodic_save(Duration::from_millis(20));
        wait_for_save_calls(&calls, 1).await;
        wait_for_file_contains(&snapshot, ":codex resume codex-session-1").await;

        let _ = std::fs::remove_dir_all(dir);
    }
}
