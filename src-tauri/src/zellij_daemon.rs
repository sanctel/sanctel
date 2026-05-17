// ───────────────────────────────────────────────────────────────────────────
// zellij_daemon — supervises the `zellij web --start --port <free-port>`
// child process for the spike backend (issue #16, slice 1 / issue #17).
//
// Lifecycle:
//   1. `ZellijDaemon::start(launcher)` picks a free loopback TCP port,
//      synchronously spawns the daemon once (so a missing binary is
//      observed at startup, not asynchronously), and hands ownership of
//      the child to a supervisor thread.
//   2. The supervisor polls `try_wait` every 50ms while watching a
//      shutdown channel. On child exit (crash, external `pkill`,
//      anything) the supervisor sleeps a backoff and respawns; the
//      backoff caps at the issue's 2-second acceptance criterion.
//   3. `shutdown()` (also called by `Drop`) signals the supervisor
//      through the channel; the supervisor kills the live child and
//      returns. This is the path that fires when Tauri tears down
//      AppState at app exit.
//
// Abstractions for testability: the `Launcher` and `ChildProc` traits let
// unit tests script the lifecycle without spawning a real zellij. The real
// implementations (`RealLauncher`, `RealChild`) are thin wrappers over
// `std::process::{Command, Child}`.
// ───────────────────────────────────────────────────────────────────────────

use std::io;
use std::net::TcpListener;
use std::process::{Child, Command};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Ceiling on the restart backoff. Issue #17 requires the supervisor to
/// restart the daemon within ~2 seconds when it dies externally; capping
/// the backoff at this value satisfies that even after a long crash burst.
pub const BACKOFF_CAP: Duration = Duration::from_millis(2000);

/// Poll cadence for `try_wait` while the daemon is alive. Small enough to
/// notice a crash quickly (well under the 2s acceptance criterion) and
/// large enough to keep idle CPU negligible.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Exponential backoff schedule: 100ms, 200ms, 400ms, 800ms, 1600ms, then
/// `BACKOFF_CAP` for every attempt after that. Pure function so tests can
/// assert the schedule without timing flakes.
pub fn next_backoff(attempt: u32) -> Duration {
    let shift = attempt.min(10);
    let ms = 100u64.saturating_mul(1u64 << shift);
    Duration::from_millis(ms).min(BACKOFF_CAP)
}

/// Find an unused TCP port on the loopback interface by binding to port 0
/// and immediately releasing it. There is a TOCTOU window between this
/// function returning and `zellij web` binding the port — fine for the
/// spike (a 1-in-a-million collision is a manual rerun away).
pub fn find_free_loopback_port() -> io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Anything the supervisor can spawn — the production impl runs a real
/// `zellij web` command, tests substitute a scripted mock.
pub trait Launcher: Send + 'static {
    fn launch(&self, port: u16) -> io::Result<Box<dyn ChildProc>>;
}

/// Minimum surface the supervisor needs on a spawned child: poll for exit,
/// kill on shutdown. Intentionally narrower than `std::process::Child` so
/// mocks stay small.
pub trait ChildProc: Send {
    /// Non-blocking exit check. `Ok(Some(_))` means the child exited;
    /// `Ok(None)` means it's still running.
    fn try_wait(&mut self) -> io::Result<Option<i32>>;
    /// Best-effort SIGKILL (or equivalent). Idempotent on a child that's
    /// already exited.
    fn kill(&mut self) -> io::Result<()>;
}

/// Production launcher: shells out to `zellij web --start --port <port>`.
pub struct RealLauncher;

impl Launcher for RealLauncher {
    fn launch(&self, port: u16) -> io::Result<Box<dyn ChildProc>> {
        let child = Command::new("zellij")
            .args(["web", "--start", "--port", &port.to_string()])
            .spawn()?;
        Ok(Box::new(RealChild(child)))
    }
}

struct RealChild(Child);

impl ChildProc for RealChild {
    fn try_wait(&mut self) -> io::Result<Option<i32>> {
        match self.0.try_wait()? {
            Some(status) => Ok(Some(status.code().unwrap_or(-1))),
            None => Ok(None),
        }
    }

    fn kill(&mut self) -> io::Result<()> {
        // SIGKILL on Unix, TerminateProcess on Windows. Already-exited
        // children return InvalidInput; swallow that so callers don't have
        // to special-case it.
        match self.0.kill() {
            Ok(()) => {
                let _ = self.0.wait();
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::InvalidInput => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// The supervisor handle returned to the rest of the codebase. Hold it
/// alive for the duration of the daemon's lifetime; drop it (or call
/// `shutdown`) to tear the daemon down.
pub struct ZellijDaemon {
    // The picked loopback port. Consumed by `zellij_ws` in slice 3+ to
    // open WebSocket connections to the running daemon; the field is
    // already wired through here so the supervisor and the future ws
    // client agree on a single source of truth. Marked dead_code for
    // the slice-1 build where only the daemon supervision contract is
    // exercised.
    #[allow(dead_code)]
    port: u16,
    shutdown_tx: Option<Sender<()>>,
    supervisor: Option<JoinHandle<()>>,
}

impl ZellijDaemon {
    /// Pick a free port, spawn the daemon once synchronously, and hand the
    /// child to a supervisor thread. Returns immediately on spawn success;
    /// returns an error if the initial spawn fails (zellij not on PATH,
    /// port couldn't be bound, etc.). Subsequent crashes are handled by
    /// the supervisor and are NOT surfaced through this Result.
    pub fn start<L: Launcher>(launcher: L) -> io::Result<Self> {
        let port = find_free_loopback_port()?;
        let initial = launcher.launch(port)?;
        let (tx, rx) = mpsc::channel();
        let supervisor = thread::spawn(move || {
            supervisor_loop(launcher, port, initial, rx);
        });
        Ok(ZellijDaemon {
            port,
            shutdown_tx: Some(tx),
            supervisor: Some(supervisor),
        })
    }

    /// The port the daemon was started on. Stable for the daemon's
    /// lifetime — respawns reuse it so `zellij_ws` clients (slice 3+) can
    /// keep their connection target.
    #[allow(dead_code)]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Tell the supervisor to kill the current child and exit. Idempotent;
    /// calling twice is fine.
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.supervisor.take() {
            let _ = h.join();
        }
    }
}

impl Drop for ZellijDaemon {
    fn drop(&mut self) {
        // AppState owns the daemon; when Tauri tears down state at app
        // exit, this Drop fires and kills the zellij web process. That's
        // the path acceptance criterion #3 calls out ("On clean sanctel
        // shutdown, the spawned `zellij web` process is terminated").
        self.shutdown();
    }
}

/// The supervisor's body. Owns the launcher (so respawns are possible),
/// the port (stable across respawns), the currently-live child, and the
/// shutdown channel. Returns only when shutdown is signaled.
fn supervisor_loop<L: Launcher>(
    launcher: L,
    port: u16,
    initial_child: Box<dyn ChildProc>,
    shutdown_rx: Receiver<()>,
) {
    let mut child = initial_child;
    let mut attempt: u32 = 0;

    loop {
        // Watch the current child. Returns either because the child
        // exited (rebackoff/respawn) or because shutdown was signaled
        // (kill + exit).
        match watch_child(child.as_mut(), &shutdown_rx) {
            WatchOutcome::ChildExited => {
                // fall through to backoff + respawn
            }
            WatchOutcome::Shutdown => {
                let _ = child.kill();
                return;
            }
        }

        // Backoff before respawn. The recv_timeout doubles as a
        // shutdown-aware sleep — if shutdown arrives mid-backoff, return
        // immediately rather than running an unwanted respawn.
        let delay = next_backoff(attempt);
        if shutdown_rx.recv_timeout(delay).is_ok() {
            return;
        }
        attempt = attempt.saturating_add(1);

        // Respawn. A spawn failure (e.g., transient PATH issue) loops
        // back to backoff with an incremented attempt — the supervisor
        // never gives up, matching `tmux` daemon orchestration that's
        // similarly stubborn.
        match launcher.launch(port) {
            Ok(new_child) => {
                child = new_child;
                attempt = 0; // healthy spawn — reset the schedule
            }
            Err(_) => {
                // Substitute a zombie placeholder so the next loop iteration
                // re-enters watch+backoff without special-casing the
                // spawn-failed path.
                child = Box::new(ZombieChild);
            }
        }
    }
}

enum WatchOutcome {
    ChildExited,
    Shutdown,
}

fn watch_child(child: &mut dyn ChildProc, shutdown_rx: &Receiver<()>) -> WatchOutcome {
    loop {
        match shutdown_rx.try_recv() {
            Ok(()) | Err(TryRecvError::Disconnected) => return WatchOutcome::Shutdown,
            Err(TryRecvError::Empty) => {}
        }
        match child.try_wait() {
            Ok(Some(_)) => return WatchOutcome::ChildExited,
            Ok(None) => {}
            Err(_) => return WatchOutcome::ChildExited,
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Placeholder child used when a respawn fails — try_wait reports it as
/// already-exited so the supervisor loop falls through to another backoff.
struct ZombieChild;

impl ChildProc for ZombieChild {
    fn try_wait(&mut self) -> io::Result<Option<i32>> {
        Ok(Some(-1))
    }
    fn kill(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// ─── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    /// 100ms, 200ms, 400ms, 800ms, 1600ms, then capped at 2000ms forever.
    /// This is the schedule that satisfies the "supervisor restarts the
    /// daemon within ~2 seconds" acceptance criterion: even after many
    /// crashes, the next attempt fires within `BACKOFF_CAP` of the exit.
    #[test]
    fn next_backoff_schedule_doubles_until_cap() {
        assert_eq!(next_backoff(0), Duration::from_millis(100));
        assert_eq!(next_backoff(1), Duration::from_millis(200));
        assert_eq!(next_backoff(2), Duration::from_millis(400));
        assert_eq!(next_backoff(3), Duration::from_millis(800));
        assert_eq!(next_backoff(4), Duration::from_millis(1600));
        assert_eq!(next_backoff(5), BACKOFF_CAP);
        assert_eq!(next_backoff(99), BACKOFF_CAP);
    }

    /// The free-port helper must return a port we can actually bind to
    /// right after it returns (modulo the TOCTOU window). This is a
    /// smoke test that loopback works in the test environment, which the
    /// supervisor relies on for real spawns.
    #[test]
    fn find_free_loopback_port_returns_usable_port() {
        let port = find_free_loopback_port().expect("loopback bind works");
        assert!(port > 0, "port must be a real value, got {port}");
        // Confirm we can bind again — the port is genuinely free at this
        // instant. (Subsequent runs may not be free, that's fine.)
        let _again = TcpListener::bind(("127.0.0.1", port))
            .expect("free port reported by helper must be bindable");
    }

    /// Mock launcher whose every launch records a count and returns a
    /// `ScriptedChild` whose alive/dead state is driven by an
    /// `AtomicBool`. Tests use the count to assert how many spawns the
    /// supervisor performed.
    struct MockLauncher {
        launches: Arc<AtomicUsize>,
        live_handles: Arc<Mutex<Vec<Arc<AtomicBool>>>>,
    }

    impl MockLauncher {
        fn new() -> Self {
            MockLauncher {
                launches: Arc::new(AtomicUsize::new(0)),
                live_handles: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    /// Mark the most recent scripted child as dead so the supervisor
    /// observes the "external kill" condition. Free function (not a method
    /// on MockLauncher) because the launcher is moved into
    /// `ZellijDaemon::start` and the tests hold the handles Arc separately.
    fn kill_latest(handles: &Mutex<Vec<Arc<AtomicBool>>>) {
        if let Some(h) = handles.lock().unwrap().last() {
            h.store(false, Ordering::SeqCst);
        }
    }

    impl Launcher for MockLauncher {
        fn launch(&self, _port: u16) -> io::Result<Box<dyn ChildProc>> {
            self.launches.fetch_add(1, Ordering::SeqCst);
            let alive = Arc::new(AtomicBool::new(true));
            self.live_handles.lock().unwrap().push(Arc::clone(&alive));
            Ok(Box::new(ScriptedChild { alive }))
        }
    }

    struct ScriptedChild {
        alive: Arc<AtomicBool>,
    }

    impl ChildProc for ScriptedChild {
        fn try_wait(&mut self) -> io::Result<Option<i32>> {
            Ok(if self.alive.load(Ordering::SeqCst) {
                None
            } else {
                Some(0)
            })
        }
        fn kill(&mut self) -> io::Result<()> {
            self.alive.store(false, Ordering::SeqCst);
            Ok(())
        }
    }

    /// `ZellijDaemon::start` must spawn the daemon exactly once
    /// synchronously and return a handle whose `port()` is the picked
    /// loopback port. The single spawn matches acceptance criterion #2.
    #[test]
    fn start_spawns_daemon_once_synchronously() {
        let launcher = MockLauncher::new();
        let launch_count = Arc::clone(&launcher.launches);
        let daemon = ZellijDaemon::start(launcher).expect("start succeeds");
        assert!(daemon.port() > 0);
        assert_eq!(launch_count.load(Ordering::SeqCst), 1);
        drop(daemon);
    }

    /// External-kill recovery: simulate `pkill -f 'zellij web'` by
    /// flipping the alive flag, then observe the supervisor spawn a
    /// replacement within the BACKOFF_CAP window (acceptance criterion
    /// #5: "supervisor restarts it within ~2 seconds").
    #[test]
    fn supervisor_restarts_on_external_kill_within_cap() {
        let launcher = MockLauncher::new();
        let launches = Arc::clone(&launcher.launches);
        let handles = Arc::clone(&launcher.live_handles);
        let daemon = ZellijDaemon::start(launcher).expect("start succeeds");
        assert_eq!(launches.load(Ordering::SeqCst), 1);

        kill_latest(&handles);

        // First respawn fires after a ~100ms backoff. Allow generous
        // headroom; we're asserting "within the acceptance window", not
        // a tight bound. Cap is 2s; we wait up to 1s here.
        let start = Instant::now();
        while launches.load(Ordering::SeqCst) < 2 {
            if start.elapsed() > Duration::from_millis(1000) {
                panic!(
                    "supervisor did not respawn within 1s; launches={}",
                    launches.load(Ordering::SeqCst)
                );
            }
            thread::sleep(Duration::from_millis(20));
        }
        drop(daemon);
    }

    /// `shutdown()` must stop the supervisor so subsequent crashes don't
    /// trigger respawns. Without this, Drop wouldn't be a clean teardown
    /// and ghost daemons would survive sanctel quit.
    #[test]
    fn shutdown_stops_supervisor_and_no_further_respawns() {
        let launcher = MockLauncher::new();
        let launches = Arc::clone(&launcher.launches);
        let handles = Arc::clone(&launcher.live_handles);
        let mut daemon = ZellijDaemon::start(launcher).expect("start succeeds");

        daemon.shutdown();
        let after_shutdown = launches.load(Ordering::SeqCst);

        // Try to provoke a respawn by killing the (now-already-dead)
        // latest child. If the supervisor is still running, it would
        // observe this and spawn again. Wait a generous window covering
        // the longest possible initial backoff to confirm it doesn't.
        kill_latest(&handles);
        thread::sleep(Duration::from_millis(300));
        assert_eq!(launches.load(Ordering::SeqCst), after_shutdown);
    }

    /// Drop is a path the rest of the codebase relies on (AppState owns
    /// the daemon; Tauri shutdown drops AppState). It must call shutdown,
    /// joining the supervisor before the test thread exits — otherwise
    /// the supervisor would leak across tests in the same process.
    #[test]
    fn drop_kills_supervisor() {
        let launcher = MockLauncher::new();
        let launches = Arc::clone(&launcher.launches);
        let handles = Arc::clone(&launcher.live_handles);
        let daemon = ZellijDaemon::start(launcher).expect("start succeeds");
        let after_start = launches.load(Ordering::SeqCst);
        drop(daemon);
        kill_latest(&handles);
        thread::sleep(Duration::from_millis(200));
        assert_eq!(launches.load(Ordering::SeqCst), after_start);
    }
}
