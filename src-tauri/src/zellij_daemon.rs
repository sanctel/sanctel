// ───────────────────────────────────────────────────────────────────────────
// zellij_daemon — supervises the `zellij web --start --port <free-port>`
// child process for the spike backend (issue #16, slice 1 / issue #17), plus
// the cookie-auth flow that `zellij web` requires (issue #23).
//
// Lifecycle:
//   1. `ZellijDaemon::start(launcher, authenticator)` picks a free loopback
//      TCP port, synchronously spawns the daemon, runs the authenticator's
//      mint-token + login exchange against the daemon, and hands ownership
//      of the child + auth state to a supervisor thread.
//   2. The supervisor polls `try_wait` every 50ms while watching a
//      shutdown channel. On child exit (crash, external `pkill`,
//      anything) the supervisor sleeps a backoff, respawns, and re-runs
//      the authenticator against the fresh daemon — the session_token
//      from the dead process is invalidated, so any subsequent
//      WebSocket open must use the freshly-minted one.
//   3. `shutdown()` (also called by `Drop`) signals the supervisor
//      through the channel; the supervisor kills the live child,
//      revokes the auth token best-effort, and returns.
//
// Auth state is shared through `Arc<Mutex<String>>` so the rest of the
// codebase (zellij_ws::mount, zellij_ws::write_initial_command) can read
// the *current* session_token at connection time without worrying about
// whether a respawn has happened in the meantime.
// ───────────────────────────────────────────────────────────────────────────

use std::fmt;
use std::io;
use std::net::TcpListener;
use std::process::{Child, Command};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::zellij_auth::{self, TokenPair, ZellijAuthError};

/// Ceiling on the restart backoff. Issue #17 requires the supervisor to
/// restart the daemon within ~2 seconds when it dies externally; capping
/// the backoff at this value satisfies that even after a long crash burst.
pub const BACKOFF_CAP: Duration = Duration::from_millis(2000);

/// Poll cadence for `try_wait` while the daemon is alive. Small enough to
/// notice a crash quickly (well under the 2s acceptance criterion) and
/// large enough to keep idle CPU negligible.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Fixed sanctel auth token name. Deterministic per machine so re-runs
/// replace rather than accumulate entries in `zellij web --list-tokens`.
/// One sanctel instance per user is assumed; two concurrent instances
/// would still share the same token (auth tokens are per-machine, not
/// per-process — the session_token is what's per-process).
pub const DEFAULT_TOKEN_NAME: &str = "sanctel";

/// Errors the daemon's start path can surface. `Io` wraps the existing
/// spawn failures; the new variants surface auth-flow failures so the
/// setup-screen message can name the specific failure mode rather than
/// dumping an opaque HTTP 401 into the tab.
#[derive(Debug)]
pub enum ZellijDaemonError {
    Io(io::Error),
    Auth(ZellijAuthError),
}

impl fmt::Display for ZellijDaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ZellijDaemonError::Io(e) => write!(f, "zellij daemon: {e}"),
            ZellijDaemonError::Auth(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ZellijDaemonError {}

impl From<io::Error> for ZellijDaemonError {
    fn from(e: io::Error) -> Self {
        ZellijDaemonError::Io(e)
    }
}

impl From<ZellijAuthError> for ZellijDaemonError {
    fn from(e: ZellijAuthError) -> Self {
        ZellijDaemonError::Auth(e)
    }
}

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

/// The auth flow as a trait so tests don't need to spawn `zellij web` to
/// drive the supervisor — production uses `RealAuthenticator` which
/// shells out to `zellij web --create-token` + POSTs the login exchange.
pub trait Authenticator: Send + 'static {
    /// Mint + login against the daemon listening on `port`. Returns a
    /// freshly-issued (token_name, session_token) pair.
    fn authenticate(&self, port: u16) -> Result<TokenPair, ZellijAuthError>;
    /// Best-effort revoke at shutdown. Failures are swallowed by the caller
    /// (Drop path doesn't have a place to surface errors).
    fn revoke(&self, token_name: &str);
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

/// Production authenticator: shells out to `zellij web --create-token` and
/// POSTs the login exchange. See `zellij_auth` for the wire-shape details.
pub struct RealAuthenticator {
    pub token_name: String,
}

impl Default for RealAuthenticator {
    fn default() -> Self {
        RealAuthenticator {
            token_name: DEFAULT_TOKEN_NAME.into(),
        }
    }
}

impl Authenticator for RealAuthenticator {
    fn authenticate(&self, port: u16) -> Result<TokenPair, ZellijAuthError> {
        zellij_auth::authenticate(port, &self.token_name)
    }
    fn revoke(&self, token_name: &str) {
        zellij_auth::revoke_token(token_name);
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
    port: u16,
    shutdown_tx: Option<Sender<()>>,
    supervisor: Option<JoinHandle<()>>,
    /// Current session_token cookie value, refreshed on every (re)auth by
    /// the supervisor. Cloned by `zellij_ws` callers at WebSocket-open
    /// time so a freshly-restarted daemon's cookie reaches new mounts
    /// without any extra plumbing.
    session_token: Arc<Mutex<String>>,
}

impl ZellijDaemon {
    /// Pick a free port, spawn the daemon once synchronously, authenticate
    /// against it, and hand the child + auth state to a supervisor thread.
    /// Returns immediately on success; returns an error if the initial
    /// spawn fails OR if the auth flow against the freshly-started daemon
    /// fails (so a setup-screen surfaces the failure rather than letting
    /// `terminal_attach` hit an opaque 401 per tab).
    pub fn start<L: Launcher, A: Authenticator>(
        launcher: L,
        authenticator: A,
    ) -> Result<Self, ZellijDaemonError> {
        let port = find_free_loopback_port()?;
        let initial = launcher.launch(port)?;
        let pair = authenticator.authenticate(port)?;
        let session_token = Arc::new(Mutex::new(pair.session_token));
        let token_name = pair.token_name;
        let session_token_for_supervisor = Arc::clone(&session_token);
        let (tx, rx) = mpsc::channel();
        let supervisor = thread::spawn(move || {
            supervisor_loop(
                launcher,
                authenticator,
                port,
                initial,
                session_token_for_supervisor,
                token_name,
                rx,
            );
        });
        Ok(ZellijDaemon {
            port,
            shutdown_tx: Some(tx),
            supervisor: Some(supervisor),
            session_token,
        })
    }

    /// The port the daemon was started on. Stable for the daemon's
    /// lifetime — respawns reuse it so `zellij_ws` clients can keep their
    /// connection target.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Snapshot of the current session_token cookie. Callers should grab
    /// this immediately before opening a WebSocket; if the daemon respawns
    /// between snapshot and connect, the connect will fail with 401 and
    /// the user (or frontend) re-triggers an attach, which picks up the
    /// fresh token.
    pub fn session_token(&self) -> String {
        self.session_token.lock().unwrap().clone()
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
        // exit, this Drop fires and kills the zellij web process. The
        // supervisor's exit branch handles the token revoke before
        // returning, so by the time shutdown() resolves the revoke has
        // already run (or been attempted).
        self.shutdown();
    }
}

/// The supervisor's body. Owns the launcher (so respawns are possible),
/// the port (stable across respawns), the currently-live child, and the
/// shutdown channel. Returns only when shutdown is signaled.
fn supervisor_loop<L: Launcher, A: Authenticator>(
    launcher: L,
    authenticator: A,
    port: u16,
    initial_child: Box<dyn ChildProc>,
    session_token: Arc<Mutex<String>>,
    token_name: String,
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
                // Best-effort revoke before returning. Failure is silent
                // because Drop has no place to surface it.
                authenticator.revoke(&token_name);
                return;
            }
        }

        // Backoff before respawn. The recv_timeout doubles as a
        // shutdown-aware sleep — if shutdown arrives mid-backoff, return
        // immediately rather than running an unwanted respawn.
        let delay = next_backoff(attempt);
        if shutdown_rx.recv_timeout(delay).is_ok() {
            authenticator.revoke(&token_name);
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
                // Re-authenticate against the fresh daemon — the old
                // session_token died with the previous process. If auth
                // fails (e.g., the daemon crashed mid-restart) the loop
                // falls back through backoff and tries again, which is
                // the same shape we use for spawn failures.
                match authenticator.authenticate(port) {
                    Ok(pair) => {
                        *session_token.lock().unwrap() = pair.session_token;
                    }
                    Err(_) => {
                        // Treat as a degenerate exit: kill the child and
                        // loop back through backoff.
                        let _ = child.kill();
                        child = Box::new(ZombieChild);
                    }
                }
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
    use std::sync::Mutex as StdMutex;
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
        live_handles: Arc<StdMutex<Vec<Arc<AtomicBool>>>>,
    }

    impl MockLauncher {
        fn new() -> Self {
            MockLauncher {
                launches: Arc::new(AtomicUsize::new(0)),
                live_handles: Arc::new(StdMutex::new(Vec::new())),
            }
        }
    }

    /// Mark the most recent scripted child as dead so the supervisor
    /// observes the "external kill" condition. Free function (not a method
    /// on MockLauncher) because the launcher is moved into
    /// `ZellijDaemon::start` and the tests hold the handles Arc separately.
    fn kill_latest(handles: &StdMutex<Vec<Arc<AtomicBool>>>) {
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

    /// In-memory authenticator. Records every authenticate / revoke call so
    /// tests can assert "auth ran once per (re)launch" and "revoke ran on
    /// clean shutdown". Returns a synthetic token shaped like
    /// `session-<N>` where N is the call ordinal — so tests can also
    /// assert the daemon's `session_token()` advanced after a respawn.
    struct CountingAuth {
        auth_calls: Arc<AtomicUsize>,
        revoke_calls: Arc<AtomicUsize>,
    }

    impl CountingAuth {
        fn new() -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
            let auth_calls = Arc::new(AtomicUsize::new(0));
            let revoke_calls = Arc::new(AtomicUsize::new(0));
            (
                CountingAuth {
                    auth_calls: Arc::clone(&auth_calls),
                    revoke_calls: Arc::clone(&revoke_calls),
                },
                auth_calls,
                revoke_calls,
            )
        }
    }

    impl Authenticator for CountingAuth {
        fn authenticate(&self, _port: u16) -> Result<TokenPair, ZellijAuthError> {
            let n = self.auth_calls.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(TokenPair {
                token_name: "test-token".into(),
                session_token: format!("session-{n}"),
            })
        }
        fn revoke(&self, _token_name: &str) {
            self.revoke_calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A no-op authenticator used by tests that don't care about auth
    /// (the existing shutdown / drop / backoff schedule tests).
    struct NoopAuth;
    impl Authenticator for NoopAuth {
        fn authenticate(&self, _port: u16) -> Result<TokenPair, ZellijAuthError> {
            Ok(TokenPair {
                token_name: "test-token".into(),
                session_token: "session-noop".into(),
            })
        }
        fn revoke(&self, _token_name: &str) {}
    }

    /// `ZellijDaemon::start` must spawn the daemon exactly once
    /// synchronously and return a handle whose `port()` is the picked
    /// loopback port. Auth runs exactly once on start.
    #[test]
    fn start_spawns_daemon_once_synchronously_and_authenticates() {
        let launcher = MockLauncher::new();
        let launch_count = Arc::clone(&launcher.launches);
        let (auth, auth_calls, _revoke_calls) = CountingAuth::new();
        let daemon = ZellijDaemon::start(launcher, auth).expect("start succeeds");
        assert!(daemon.port() > 0);
        assert_eq!(launch_count.load(Ordering::SeqCst), 1);
        assert_eq!(auth_calls.load(Ordering::SeqCst), 1);
        // Initial token is observable via the accessor.
        assert_eq!(daemon.session_token(), "session-1");
        drop(daemon);
    }

    /// Auth failure on initial start is a hard error (not a silent retry).
    /// This is the path the setup-screen catches when `zellij web` is
    /// installed but the daemon crashed between spawn and our login POST.
    #[test]
    fn start_surfaces_initial_auth_failure() {
        struct FailingAuth;
        impl Authenticator for FailingAuth {
            fn authenticate(&self, _port: u16) -> Result<TokenPair, ZellijAuthError> {
                Err(ZellijAuthError::TokenMintFailed {
                    stderr: "synthetic".into(),
                })
            }
            fn revoke(&self, _: &str) {}
        }
        let launcher = MockLauncher::new();
        match ZellijDaemon::start(launcher, FailingAuth) {
            Err(ZellijDaemonError::Auth(ZellijAuthError::TokenMintFailed { .. })) => {}
            Err(other) => panic!("expected Auth(TokenMintFailed), got {other:?}"),
            Ok(_) => panic!("expected Auth(TokenMintFailed), got Ok(daemon)"),
        }
    }

    /// External-kill recovery: simulate `pkill -f 'zellij web'` by
    /// flipping the alive flag, then observe the supervisor spawn a
    /// replacement AND re-authenticate against the fresh daemon. The new
    /// session_token replaces the old one in the Arc<Mutex<String>>.
    #[test]
    fn supervisor_reauthenticates_on_external_kill() {
        let launcher = MockLauncher::new();
        let launches = Arc::clone(&launcher.launches);
        let handles = Arc::clone(&launcher.live_handles);
        let (auth, auth_calls, _revoke_calls) = CountingAuth::new();
        let daemon = ZellijDaemon::start(launcher, auth).expect("start succeeds");
        assert_eq!(launches.load(Ordering::SeqCst), 1);
        assert_eq!(auth_calls.load(Ordering::SeqCst), 1);
        assert_eq!(daemon.session_token(), "session-1");

        kill_latest(&handles);

        // Wait for the supervisor to respawn AND re-authenticate. Both
        // counts must reach 2 within the acceptance window.
        let start = Instant::now();
        loop {
            if launches.load(Ordering::SeqCst) >= 2 && auth_calls.load(Ordering::SeqCst) >= 2 {
                break;
            }
            if start.elapsed() > Duration::from_millis(1500) {
                panic!(
                    "respawn/reauth did not happen: launches={}, auth_calls={}",
                    launches.load(Ordering::SeqCst),
                    auth_calls.load(Ordering::SeqCst),
                );
            }
            thread::sleep(Duration::from_millis(20));
        }
        // session_token advanced — proves the daemon-state mutex is wired
        // to the supervisor's auth path. A regression that updated some
        // *other* string (e.g., a copy) would still see "session-1".
        assert_eq!(daemon.session_token(), "session-2");
        drop(daemon);
    }

    /// External-kill recovery (legacy contract): supervisor restarts within
    /// the `BACKOFF_CAP` window. Kept as a separate assertion so a
    /// regression on the timing path is distinguishable from a regression
    /// on the auth path.
    #[test]
    fn supervisor_restarts_on_external_kill_within_cap() {
        let launcher = MockLauncher::new();
        let launches = Arc::clone(&launcher.launches);
        let handles = Arc::clone(&launcher.live_handles);
        let daemon = ZellijDaemon::start(launcher, NoopAuth).expect("start succeeds");
        assert_eq!(launches.load(Ordering::SeqCst), 1);

        kill_latest(&handles);

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
    /// trigger respawns, AND fire the revoke best-effort so the user's
    /// `zellij web --list-tokens` doesn't accumulate sanctel entries.
    #[test]
    fn shutdown_stops_supervisor_and_revokes_token() {
        let launcher = MockLauncher::new();
        let launches = Arc::clone(&launcher.launches);
        let handles = Arc::clone(&launcher.live_handles);
        let (auth, _auth_calls, revoke_calls) = CountingAuth::new();
        let mut daemon = ZellijDaemon::start(launcher, auth).expect("start succeeds");

        daemon.shutdown();
        let after_shutdown = launches.load(Ordering::SeqCst);
        assert_eq!(
            revoke_calls.load(Ordering::SeqCst),
            1,
            "shutdown must run revoke exactly once",
        );

        // Try to provoke a respawn by killing the (now-already-dead)
        // latest child. If the supervisor is still running, it would
        // observe this and spawn again. Wait a window covering the
        // initial backoff to confirm it doesn't.
        kill_latest(&handles);
        thread::sleep(Duration::from_millis(300));
        assert_eq!(launches.load(Ordering::SeqCst), after_shutdown);
    }

    /// Repeated-kill stress: five back-to-back external kills, each timed
    /// for a respawn within `BACKOFF_CAP`. The single-kill test proves the
    /// first recovery; this one proves the recovery loop doesn't degrade
    /// under sustained pressure (the spike's criterion #7 asks for the
    /// supervisor to survive ongoing crashes).
    #[test]
    fn supervisor_recovers_from_repeated_external_kills() {
        const KILLS: usize = 5;
        let launcher = MockLauncher::new();
        let launches = Arc::clone(&launcher.launches);
        let handles = Arc::clone(&launcher.live_handles);
        let daemon = ZellijDaemon::start(launcher, NoopAuth).expect("start succeeds");

        for n in 1..=KILLS {
            let before = launches.load(Ordering::SeqCst);
            kill_latest(&handles);
            let start = Instant::now();
            while launches.load(Ordering::SeqCst) <= before {
                if start.elapsed() > BACKOFF_CAP + Duration::from_millis(500) {
                    panic!(
                        "kill #{n}: supervisor did not respawn within BACKOFF_CAP+500ms; launches={}",
                        launches.load(Ordering::SeqCst)
                    );
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
        // Total launches = 1 initial + KILLS respawns.
        assert_eq!(launches.load(Ordering::SeqCst), 1 + KILLS);
        drop(daemon);
    }

    /// Drop is a path the rest of the codebase relies on (AppState owns
    /// the daemon; Tauri shutdown drops AppState). It must call shutdown,
    /// joining the supervisor before the test thread exits.
    #[test]
    fn drop_kills_supervisor() {
        let launcher = MockLauncher::new();
        let launches = Arc::clone(&launcher.launches);
        let handles = Arc::clone(&launcher.live_handles);
        let daemon = ZellijDaemon::start(launcher, NoopAuth).expect("start succeeds");
        let after_start = launches.load(Ordering::SeqCst);
        drop(daemon);
        kill_latest(&handles);
        thread::sleep(Duration::from_millis(200));
        assert_eq!(launches.load(Ordering::SeqCst), after_start);
    }
}
