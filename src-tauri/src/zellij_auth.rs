// ───────────────────────────────────────────────────────────────────────────
// zellij_auth — token-mint and HTTP login exchange against `zellij web`.
//
// `zellij web --start` boots an HTTP/WebSocket daemon whose auth middleware
// requires a `session_token` cookie on every request. The flow this module
// implements:
//
//   1. Mint an auth token: `zellij web --create-token --name <name>`.
//      The token UUID lives on a `<name>: <UUID>` line on stdout. The
//      token persists across daemon restarts (it lives in zellij's
//      on-disk auth store).
//
//   2. Exchange the auth token for a `session_token` cookie:
//      POST http://127.0.0.1:<port>/command/login
//        Content-Type: application/json
//        body: {"auth_token":"<UUID>","remember_me":false}
//      Server replies 200 OK with `Set-Cookie: session_token=<UUID>; ...`.
//      The session_token is per-daemon-process — restart the daemon and
//      the old session_token is invalidated.
//
//   3. Carry the cookie on every WebSocket open
//      (`/ws/control`, `/ws/terminal/<session>`).
//
//   4. On clean shutdown: `zellij web --revoke-token <name>` so the
//      user's `zellij web --list-tokens` doesn't accumulate sanctel
//      tokens across runs.
//
// Why hand-rolled HTTP rather than a dependency like `ureq`:
//
//   The request is fixed-shape, localhost-only, no TLS, no streaming, no
//   redirects. The hand-rolled path is ~60 LOC; `ureq` (~200KB compiled)
//   would buy ergonomic chaining we don't need for one POST. The parse
//   and shape are exercised by unit tests so a future maintainer reading
//   the wire bytes has the test cases as a reference.
// ───────────────────────────────────────────────────────────────────────────

use std::fmt;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::time::Duration;

/// Network read/write timeout for the localhost login exchange. The
/// daemon is a child process on the same host, so a 5-second budget is
/// generous; if it doesn't answer in this window something is wrong
/// (process crashed mid-spawn, port collision, kernel weirdness) and
/// we'd rather surface the error than block sanctel's startup.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Errors surfaced from the auth flow. Variants are kept distinct so
/// callers can route into setup-screen messaging by the failure mode.
#[derive(Debug, Clone)]
pub enum ZellijAuthError {
    /// `zellij web --create-token` failed (non-zero exit, missing binary,
    /// or stdout that doesn't carry a parseable UUID).
    TokenMintFailed { stderr: String },
    /// Could not reach the login endpoint, or it answered non-2xx, or
    /// the response wasn't parseable as HTTP/1.1.
    LoginRequestFailed { msg: String },
    /// The HTTP response parsed as HTTP/1.1 but didn't carry the data
    /// shape we need (no status line, garbled bytes).
    LoginResponseInvalid { msg: String },
    /// The response was 2xx but no `Set-Cookie: session_token=...`
    /// header was present — likely a zellij version that changed the
    /// cookie name or removed the cookie auth path.
    MissingSessionTokenCookie,
}

impl fmt::Display for ZellijAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ZellijAuthError::TokenMintFailed { stderr } => {
                write!(f, "zellij auth: token mint failed: {stderr}")
            }
            ZellijAuthError::LoginRequestFailed { msg } => {
                write!(f, "zellij auth: login request failed: {msg}")
            }
            ZellijAuthError::LoginResponseInvalid { msg } => {
                write!(f, "zellij auth: login response invalid: {msg}")
            }
            ZellijAuthError::MissingSessionTokenCookie => {
                write!(f, "zellij auth: login response missing session_token cookie")
            }
        }
    }
}

impl std::error::Error for ZellijAuthError {}

/// Output of one full mint+login round-trip. The auth token sticks around
/// in zellij's on-disk store under `token_name` (kept so we can revoke it
/// at sanctel shutdown); the `session_token` is per-process and gets
/// re-minted whenever the daemon restarts.
#[derive(Debug, Clone)]
pub struct TokenPair {
    pub token_name: String,
    pub session_token: String,
}

/// Run the full mint → login exchange against a running `zellij web` daemon
/// on `port`. The `token_name` is what `zellij web --list-tokens` will
/// show; sanctel uses a fixed name so re-runs replace rather than
/// accumulate.
///
/// Retries the connect step up to a handful of times — the daemon may have
/// spawned but not yet bound the port when this is called from
/// `ZellijDaemon::start`'s tight init sequence.
pub fn authenticate(port: u16, token_name: &str) -> Result<TokenPair, ZellijAuthError> {
    let auth_token = mint_token(token_name)?;
    let session_token = exchange_login(port, &auth_token)?;
    Ok(TokenPair {
        token_name: token_name.to_string(),
        session_token,
    })
}

/// `zellij web --create-token --name <name>`. Returns the UUID extracted
/// from stdout. `zellij` prints `<name>: <UUID>` on a line; we tolerate
/// any prefix on the colon's left side so a future format change that
/// drops the literal `token_` prefix still parses.
fn mint_token(token_name: &str) -> Result<String, ZellijAuthError> {
    let output = Command::new("zellij")
        .args(["web", "--create-token", "--name", token_name])
        .output()
        .map_err(|e| ZellijAuthError::TokenMintFailed {
            stderr: format!("spawn failed: {e}"),
        })?;
    if !output.status.success() {
        return Err(ZellijAuthError::TokenMintFailed {
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_token_mint_stdout(&stdout)
}

/// Best-effort revoke. Runs synchronously but failure is ignored — the
/// caller is on the Drop path and a hung subprocess would block sanctel's
/// exit. Logging the failure would also be invisible on shutdown.
pub fn revoke_token(token_name: &str) {
    let _ = Command::new("zellij")
        .args(["web", "--revoke-token", token_name])
        .output();
}

/// Parse `zellij web --create-token` stdout for the UUID. Expected shape:
///
///   token_1: <UUID>
///
/// We accept any text on the left of `:` and look for a UUID-shaped token
/// on the right (hex digits + dashes), so a zellij version that renames
/// the token id doesn't break parsing.
pub fn parse_token_mint_stdout(stdout: &str) -> Result<String, ZellijAuthError> {
    for line in stdout.lines() {
        let Some((_, value)) = line.split_once(':') else {
            continue;
        };
        let candidate = value.trim();
        if looks_like_uuid(candidate) {
            return Ok(candidate.to_string());
        }
    }
    Err(ZellijAuthError::TokenMintFailed {
        stderr: format!("could not parse UUID from stdout: {stdout:?}"),
    })
}

/// Loose UUID shape check — hex digits and dashes, between 32 and 40
/// characters. Tighter than "any non-empty string" but lenient about
/// canonical 8-4-4-4-12 grouping in case zellij changes formatting.
fn looks_like_uuid(s: &str) -> bool {
    let trimmed = s.trim();
    let len = trimmed.len();
    (32..=40).contains(&len)
        && trimmed
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Build the bytes of the POST /command/login request. Public so tests can
/// pin the wire shape — a contributor changing the Content-Type, the path,
/// or the JSON field names would land here as a failing assertion.
pub fn build_login_request_bytes(port: u16, auth_token: &str) -> Vec<u8> {
    let body = format!(r#"{{"auth_token":"{auth_token}","remember_me":false}}"#);
    let request = format!(
        "POST /command/login HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        len = body.len(),
    );
    request.into_bytes()
}

/// Parse the response bytes from POST /command/login. On 2xx, extracts the
/// `session_token` cookie value from a `Set-Cookie` header. On any other
/// status, surfaces the error with the response excerpt so the user can
/// see what went wrong.
pub fn parse_login_response(bytes: &[u8]) -> Result<String, ZellijAuthError> {
    let text = std::str::from_utf8(bytes).map_err(|e| ZellijAuthError::LoginResponseInvalid {
        msg: format!("non-UTF-8 response: {e}"),
    })?;
    let (status_line, rest) =
        text.split_once("\r\n")
            .ok_or_else(|| ZellijAuthError::LoginResponseInvalid {
                msg: "no status line".into(),
            })?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| ZellijAuthError::LoginResponseInvalid {
            msg: format!("bad status line: {status_line}"),
        })?;
    if !(200..300).contains(&status_code) {
        let excerpt: String = text.chars().take(200).collect();
        return Err(ZellijAuthError::LoginRequestFailed {
            msg: format!("HTTP {status_code}: {excerpt}"),
        });
    }
    let header_end = rest.find("\r\n\r\n").unwrap_or(rest.len());
    for line in rest[..header_end].split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("set-cookie") {
            continue;
        }
        let value = value.trim();
        // Cookie header may carry multiple attrs separated by `;`. The
        // session_token=<value> attr is the one we need; everything else
        // (HttpOnly, SameSite=Strict, Path=/) is ignored.
        for attr in value.split(';') {
            let attr = attr.trim();
            if let Some(token) = attr.strip_prefix("session_token=") {
                if token.is_empty() {
                    return Err(ZellijAuthError::LoginResponseInvalid {
                        msg: "session_token cookie was empty".into(),
                    });
                }
                return Ok(token.to_string());
            }
        }
    }
    Err(ZellijAuthError::MissingSessionTokenCookie)
}

/// Open a TCP connection to the daemon, write the login request, read the
/// response, parse the session_token cookie. Retries the connect step a
/// few times because `start()`'s caller invokes this immediately after
/// spawning the daemon and the port may not be bound yet.
pub fn exchange_login(port: u16, auth_token: &str) -> Result<String, ZellijAuthError> {
    let request = build_login_request_bytes(port, auth_token);
    let mut last_err: Option<String> = None;
    for attempt in 0..10 {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(mut stream) => {
                let _ = stream.set_read_timeout(Some(LOGIN_TIMEOUT));
                let _ = stream.set_write_timeout(Some(LOGIN_TIMEOUT));
                stream.write_all(&request).map_err(|e| {
                    ZellijAuthError::LoginRequestFailed {
                        msg: format!("write: {e}"),
                    }
                })?;
                let mut buf = Vec::new();
                stream.read_to_end(&mut buf).map_err(|e| {
                    ZellijAuthError::LoginRequestFailed {
                        msg: format!("read: {e}"),
                    }
                })?;
                return parse_login_response(&buf);
            }
            Err(e) => {
                last_err = Some(e.to_string());
                std::thread::sleep(Duration::from_millis(100 * (attempt + 1)));
            }
        }
    }
    Err(ZellijAuthError::LoginRequestFailed {
        msg: format!(
            "connect to 127.0.0.1:{port} after 10 attempts: {}",
            last_err.unwrap_or_else(|| "no error captured".into()),
        ),
    })
}

// ─── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;

    /// Standard zellij stdout: `token_1: <UUID>`. We extract the UUID.
    #[test]
    fn parse_token_mint_stdout_extracts_uuid_from_token_1_line() {
        let stdout = "token_1: 11111111-2222-3333-4444-555555555555\n";
        let uuid = parse_token_mint_stdout(stdout).expect("parse succeeds");
        assert_eq!(uuid, "11111111-2222-3333-4444-555555555555");
    }

    /// Multi-line stdout (zellij version that prints a header) still picks
    /// the UUID line. Tolerant on what precedes the colon.
    #[test]
    fn parse_token_mint_stdout_skips_header_lines() {
        let stdout = "\
Token created.\n\
token_42: aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\n\
Run `zellij web --list-tokens` to view.\n";
        let uuid = parse_token_mint_stdout(stdout).expect("parse succeeds");
        assert_eq!(uuid, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
    }

    /// No colon anywhere → TokenMintFailed (so the user sees something
    /// actionable rather than a panic).
    #[test]
    fn parse_token_mint_stdout_fails_without_colon() {
        match parse_token_mint_stdout("nothing useful here\n") {
            Err(ZellijAuthError::TokenMintFailed { .. }) => {}
            other => panic!("expected TokenMintFailed, got {other:?}"),
        }
    }

    /// Colon-bearing line but the value isn't UUID-shaped → TokenMintFailed.
    /// Guards against a future zellij version that prints, say, "expires:
    /// 2026-01-01" on a line we'd otherwise eat.
    #[test]
    fn parse_token_mint_stdout_rejects_non_uuid_values() {
        match parse_token_mint_stdout("Created: yes\nExpires: 2026-01-01\n") {
            Err(ZellijAuthError::TokenMintFailed { .. }) => {}
            other => panic!("expected TokenMintFailed, got {other:?}"),
        }
    }

    /// Empty stdout (daemon crashed between spawn and our subprocess
    /// running) → TokenMintFailed with a useful error message.
    #[test]
    fn parse_token_mint_stdout_fails_on_empty() {
        match parse_token_mint_stdout("") {
            Err(ZellijAuthError::TokenMintFailed { stderr }) => {
                assert!(stderr.contains("could not parse"), "got: {stderr}");
            }
            other => panic!("expected TokenMintFailed, got {other:?}"),
        }
    }

    /// Wire-shape pin on the POST body: contains the auth_token verbatim,
    /// JSON-encoded with the documented field names. A regression that
    /// renamed `remember_me` to `rememberMe` would land here.
    #[test]
    fn build_login_request_bytes_carries_auth_token_and_json_shape() {
        let bytes = build_login_request_bytes(54321, "deadbeef-0000-0000-0000-cafebabecafe");
        let text = std::str::from_utf8(&bytes).expect("ascii request");
        assert!(text.starts_with("POST /command/login HTTP/1.1\r\n"), "got: {text}");
        assert!(text.contains("Host: 127.0.0.1:54321\r\n"), "got: {text}");
        assert!(text.contains("Content-Type: application/json\r\n"), "got: {text}");
        assert!(
            text.contains(r#"{"auth_token":"deadbeef-0000-0000-0000-cafebabecafe","remember_me":false}"#),
            "got: {text}",
        );
    }

    /// Happy path: HTTP 200 + Set-Cookie with session_token attr. Returns
    /// the cookie value, stripped of trailing attrs (HttpOnly etc.).
    #[test]
    fn parse_login_response_extracts_session_token_on_200() {
        let raw = "\
HTTP/1.1 200 OK\r\n\
Content-Type: application/json\r\n\
Set-Cookie: session_token=11111111-2222-3333-4444-555555555555; HttpOnly; SameSite=Strict; Path=/\r\n\
Content-Length: 0\r\n\
\r\n";
        let token = parse_login_response(raw.as_bytes()).expect("parse");
        assert_eq!(token, "11111111-2222-3333-4444-555555555555");
    }

    /// 4xx response → LoginRequestFailed with HTTP status in the message.
    #[test]
    fn parse_login_response_surfaces_4xx_with_status_code() {
        let raw = "\
HTTP/1.1 401 Unauthorized\r\n\
Content-Length: 0\r\n\
\r\n";
        match parse_login_response(raw.as_bytes()) {
            Err(ZellijAuthError::LoginRequestFailed { msg }) => {
                assert!(msg.contains("401"), "got: {msg}");
            }
            other => panic!("expected LoginRequestFailed, got {other:?}"),
        }
    }

    /// 200 OK but no Set-Cookie session_token → MissingSessionTokenCookie.
    /// This is the version-drift failure mode (zellij renamed the cookie).
    #[test]
    fn parse_login_response_fails_when_set_cookie_is_absent() {
        let raw = "\
HTTP/1.1 200 OK\r\n\
Content-Type: application/json\r\n\
Content-Length: 0\r\n\
\r\n";
        match parse_login_response(raw.as_bytes()) {
            Err(ZellijAuthError::MissingSessionTokenCookie) => {}
            other => panic!("expected MissingSessionTokenCookie, got {other:?}"),
        }
    }

    /// 200 with a Set-Cookie that has no `session_token` attr (e.g., zellij
    /// version sent only `csrf=...`). Falls through to MissingSessionTokenCookie.
    #[test]
    fn parse_login_response_fails_when_session_token_attr_is_absent() {
        let raw = "\
HTTP/1.1 200 OK\r\n\
Set-Cookie: csrf=abc123; HttpOnly\r\n\
\r\n";
        match parse_login_response(raw.as_bytes()) {
            Err(ZellijAuthError::MissingSessionTokenCookie) => {}
            other => panic!("expected MissingSessionTokenCookie, got {other:?}"),
        }
    }

    /// Empty session_token value (Set-Cookie: session_token=;) → invalid.
    /// Distinct from absent so the user sees a more specific message.
    #[test]
    fn parse_login_response_fails_when_session_token_is_empty() {
        let raw = "\
HTTP/1.1 200 OK\r\n\
Set-Cookie: session_token=; HttpOnly\r\n\
\r\n";
        match parse_login_response(raw.as_bytes()) {
            Err(ZellijAuthError::LoginResponseInvalid { msg }) => {
                assert!(msg.contains("empty"), "got: {msg}");
            }
            other => panic!("expected LoginResponseInvalid, got {other:?}"),
        }
    }

    /// Garbled bytes → LoginResponseInvalid (no panic, no silent default).
    #[test]
    fn parse_login_response_fails_on_garbled_input() {
        match parse_login_response(b"this is not http\r\n\r\n") {
            Err(ZellijAuthError::LoginResponseInvalid { .. }) => {}
            other => panic!("expected LoginResponseInvalid, got {other:?}"),
        }
    }

    /// End-to-end: spin a tiny TCP server that mimics the zellij login
    /// endpoint, drive `exchange_login` against it, assert the returned
    /// session_token. This pins the full request-write → response-read
    /// → parse roundtrip without depending on a real `zellij web`.
    #[test]
    fn exchange_login_round_trips_through_loopback() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();

        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            // Drain headers (until blank line). Body is short enough that
            // we don't need to read it — the test only cares the server
            // responds with the expected Set-Cookie.
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                if line == "\r\n" {
                    break;
                }
                line.clear();
            }
            let response = "\
HTTP/1.1 200 OK\r\n\
Content-Type: application/json\r\n\
Set-Cookie: session_token=fixture-token-9999; HttpOnly; SameSite=Strict; Path=/\r\n\
Content-Length: 0\r\n\
\r\n";
            let _ = stream.write_all(response.as_bytes());
        });

        let token = exchange_login(port, "auth-token-uuid").expect("exchange succeeds");
        assert_eq!(token, "fixture-token-9999");
    }

    /// When the server returns 401, `exchange_login` surfaces it as
    /// LoginRequestFailed with the status code in the message.
    #[test]
    fn exchange_login_surfaces_401_from_server() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();

        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                if line == "\r\n" {
                    break;
                }
                line.clear();
            }
            let response = "\
HTTP/1.1 401 Unauthorized\r\n\
Content-Length: 0\r\n\
\r\n";
            let _ = stream.write_all(response.as_bytes());
        });

        match exchange_login(port, "bad-token") {
            Err(ZellijAuthError::LoginRequestFailed { msg }) => {
                assert!(msg.contains("401"), "got: {msg}");
            }
            other => panic!("expected LoginRequestFailed, got {other:?}"),
        }
    }
}
