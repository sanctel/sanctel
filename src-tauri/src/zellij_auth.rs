// ───────────────────────────────────────────────────────────────────────────
// zellij_auth — token-mint and HTTP login exchange against `zellij web`.
//
// `zellij web --start` boots an HTTP/WebSocket daemon whose auth middleware
// requires a `session_token` cookie on every request. The flow this module
// implements:
//
//   1. Mint an auth token: `zellij web --create-token` (no extra args).
//      zellij is responsible for naming the token — it auto-generates a
//      `token_<N>` name where N increments across the daemon's lifetime
//      and prints `token_<N>: <UUID>` on stdout. The token persists
//      across daemon restarts (it lives in zellij's on-disk auth store).
//      `--create-token` is `exclusive(true)` in zellij's clap config —
//      combining it with any other flag (including the docstring-
//      suggested "optional token name" flag) is rejected at runtime,
//      so the call is intentionally argument-free.
//
//   2. Exchange the auth token for a `session_token` cookie:
//      POST http://127.0.0.1:<port>/command/login
//        Content-Type: application/json
//        body: {"auth_token":"<UUID>","remember_me":false}
//      Server replies 200 OK with `Set-Cookie: session_token=<UUID>; ...`.
//      The session_token is per-daemon-process — restart the daemon and
//      the old session_token is invalidated.
//
//   3. Register a web client:
//      POST http://127.0.0.1:<port>/session
//        Cookie: session_token=<UUID>
//        Content-Type: application/json
//        body: {}
//      Server replies 200 OK with
//        {"web_client_id":"<UUID>","is_read_only":false}.
//      The id is per-attached-client and must accompany every WebSocket
//      open as a `?web_client_id=<UUID>` query parameter; zellij's
//      `ws_handler_terminal` / `ws_handler_control` look the client up
//      in an in-process connection table by that id and reject the
//      handshake with HTTP 400 if it's missing or unknown.
//
//   4. Carry the cookie AND the web_client_id on every WebSocket open
//      (`/ws/control?web_client_id=<id>`,
//      `/ws/terminal/<session>?web_client_id=<id>`).
//
//   5. On clean shutdown: `zellij web --revoke-token <name>` so the
//      user's `zellij web --list-tokens` doesn't accumulate sanctel
//      tokens across runs. The name passed here is the auto-generated
//      `token_<N>` from step 1, NOT a caller-supplied string.
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
    /// `zellij web --create-token` exited non-zero (missing binary, daemon
    /// not running yet, etc.) — stderr from the failed subprocess is
    /// included for diagnostics.
    TokenMintFailed { stderr: String },
    /// `zellij web --create-token` exited 0 but stdout didn't carry the
    /// expected `token_<N>: <UUID>` line. Distinct from TokenMintFailed
    /// because the failure mode is a zellij version that changed its
    /// output format, not a subprocess error — the diagnostic the user
    /// needs is the raw stdout we couldn't parse.
    MalformedTokenOutput { stdout: String },
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
    /// `POST /session` (the web_client_id registration step) failed —
    /// either non-2xx status, malformed JSON, or missing the
    /// `web_client_id` field. The status + body excerpt are included
    /// so the user can see what zellij returned.
    ClientRegistrationFailed { status: u16, body: String },
    /// `POST /session` answered 2xx but the body didn't parse as the
    /// expected `{"web_client_id":"<UUID>",...}` shape. Distinct from
    /// `ClientRegistrationFailed` so the diagnostic message names the
    /// version-drift failure mode (zellij renamed/removed the field)
    /// rather than implying an HTTP failure.
    ClientRegistrationInvalid { body: String },
}

impl fmt::Display for ZellijAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ZellijAuthError::TokenMintFailed { stderr } => {
                write!(f, "zellij auth: token mint failed: {stderr}")
            }
            ZellijAuthError::MalformedTokenOutput { stdout } => {
                write!(
                    f,
                    "zellij auth: --create-token stdout did not match expected \
                     `token_<N>: <UUID>` shape: {stdout:?}",
                )
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
            ZellijAuthError::ClientRegistrationFailed { status, body } => {
                write!(
                    f,
                    "zellij auth: POST /session failed (HTTP {status}): {body}",
                )
            }
            ZellijAuthError::ClientRegistrationInvalid { body } => {
                write!(
                    f,
                    "zellij auth: POST /session body did not match expected \
                     `{{\"web_client_id\":\"...\"}}` shape: {body}",
                )
            }
        }
    }
}

impl std::error::Error for ZellijAuthError {}

/// Output of one full mint+login round-trip. `auth_token_name` is the
/// auto-generated `token_<N>` zellij assigned to the new token — kept so
/// we can revoke it at sanctel shutdown via `zellij web --revoke-token
/// <name>`. `session_token` is per-daemon-process and gets re-minted
/// whenever the daemon restarts.
#[derive(Debug, Clone)]
pub struct TokenPair {
    pub auth_token_name: String,
    pub session_token: String,
}

/// Run the full mint → login exchange against a running `zellij web` daemon
/// on `port`. The token's name is auto-generated by zellij (`token_<N>`)
/// and returned in the `TokenPair` so the caller can revoke it later.
///
/// Retries the connect step up to a handful of times — the daemon may have
/// spawned but not yet bound the port when this is called from
/// `ZellijDaemon::start`'s tight init sequence.
pub fn authenticate(port: u16) -> Result<TokenPair, ZellijAuthError> {
    let (auth_token_name, auth_token_uuid) = mint_token()?;
    let session_token = exchange_login(port, &auth_token_uuid)?;
    Ok(TokenPair {
        auth_token_name,
        session_token,
    })
}

/// `zellij web --create-token` with no extra args. The flag is
/// `exclusive(true)` in zellij's clap config — combining it with anything
/// else is rejected at runtime — and zellij auto-generates the token name
/// itself. Returns the `(name, uuid)` pair parsed from stdout.
pub(crate) fn mint_token() -> Result<(String, String), ZellijAuthError> {
    let output = Command::new("zellij")
        .args(["web", "--create-token"])
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
    parse_token_line(&stdout).ok_or_else(|| ZellijAuthError::MalformedTokenOutput {
        stdout: stdout.into_owned(),
    })
}

/// Best-effort revoke. Runs synchronously but failure is ignored — the
/// caller is on the Drop path and a hung subprocess would block sanctel's
/// exit. Logging the failure would also be invisible on shutdown.
pub fn revoke_token(token_name: &str) {
    let _ = Command::new("zellij")
        .args(["web", "--revoke-token", token_name])
        .output();
}

/// Parse `zellij web --create-token` stdout for the `(name, uuid)` pair.
/// Expected shape:
///
///   token_1: <UUID>
///
/// We require the left side to start with `token_` (so a stray header line
/// containing a UUID-shaped substring doesn't get picked up) but tolerate
/// surrounding whitespace and preamble lines like "Created token
/// successfully". Returns `None` if no matching line is found — the caller
/// translates that into `MalformedTokenOutput` with the original stdout
/// so the user sees what we couldn't parse.
pub fn parse_token_line(stdout: &str) -> Option<(String, String)> {
    for line in stdout.lines() {
        let trimmed = line.trim();
        let Some((name, value)) = trimmed.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if !name.starts_with("token_") {
            continue;
        }
        let uuid = value.trim();
        if !looks_like_uuid(uuid) {
            continue;
        }
        return Some((name.to_string(), uuid.to_string()));
    }
    None
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

/// Build the bytes of the POST /session request. Mirrors
/// [`build_login_request_bytes`] — same hand-rolled HTTP/1.1 shape with
/// the `session_token` cookie carried on the wire so the auth middleware
/// authorises the request. Body is an empty JSON object (the endpoint
/// accepts `{}`).
pub fn build_register_client_request_bytes(port: u16, session_token: &str) -> Vec<u8> {
    let body = "{}";
    let request = format!(
        "POST /session HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Cookie: session_token={session_token}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        len = body.len(),
    );
    request.into_bytes()
}

/// Parse the response bytes from POST /session. On 2xx, extracts the
/// `web_client_id` field from the JSON body. Errors are routed into
/// `ClientRegistrationFailed` (non-2xx / malformed HTTP) vs
/// `ClientRegistrationInvalid` (2xx but missing field) so the user sees
/// the right diagnostic for the failure mode.
pub fn parse_create_client_response(bytes: &[u8]) -> Result<String, ZellijAuthError> {
    let text = std::str::from_utf8(bytes).map_err(|e| ZellijAuthError::ClientRegistrationInvalid {
        body: format!("non-UTF-8 response: {e}"),
    })?;
    let (headers, body) = text.split_once("\r\n\r\n").unwrap_or((text, ""));
    let status_line = headers.split_once("\r\n").map(|(s, _)| s).unwrap_or(headers);
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| ZellijAuthError::ClientRegistrationInvalid {
            body: format!("bad status line: {status_line}"),
        })?;
    if !(200..300).contains(&status_code) {
        let excerpt: String = body.chars().take(200).collect();
        return Err(ZellijAuthError::ClientRegistrationFailed {
            status: status_code,
            body: excerpt,
        });
    }
    extract_web_client_id(body).ok_or_else(|| ZellijAuthError::ClientRegistrationInvalid {
        body: body.chars().take(200).collect(),
    })
}

/// Pure JSON field extractor for `{"web_client_id":"<UUID>", ...}`.
/// Tolerates surrounding fields (e.g., `is_read_only`) and whitespace.
/// Returns None on malformed input — the caller surfaces it as
/// `ClientRegistrationInvalid` with the original body for diagnostics.
fn extract_web_client_id(body: &str) -> Option<String> {
    // Walk past the `"web_client_id"` key, the colon, and the opening
    // quote; everything up to the next quote is the value. A small ad-hoc
    // parser is fine here — the response shape is fixed and a serde_json
    // dependency for one field would be overkill alongside the rest of
    // this module's hand-rolled HTTP.
    let (_, after_key) = body.split_once("\"web_client_id\"")?;
    let (_, after_colon) = after_key.split_once(':')?;
    let (_, after_open_quote) = after_colon.split_once('"')?;
    let (value, _) = after_open_quote.split_once('"')?;
    if value.is_empty() {
        return None;
    }
    Some(value.to_string())
}

/// Open a TCP connection to the daemon, POST /session with the cookie,
/// read the response, parse the `web_client_id`. The id must accompany
/// every WebSocket open as `?web_client_id=<id>` — zellij's WS handlers
/// reject the handshake with HTTP 400 if it's missing.
pub fn register_client(port: u16, session_token: &str) -> Result<String, ZellijAuthError> {
    let request = build_register_client_request_bytes(port, session_token);
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| {
        ZellijAuthError::ClientRegistrationFailed {
            status: 0,
            body: format!("connect to 127.0.0.1:{port}: {e}"),
        }
    })?;
    let _ = stream.set_read_timeout(Some(LOGIN_TIMEOUT));
    let _ = stream.set_write_timeout(Some(LOGIN_TIMEOUT));
    stream
        .write_all(&request)
        .map_err(|e| ZellijAuthError::ClientRegistrationFailed {
            status: 0,
            body: format!("write: {e}"),
        })?;
    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .map_err(|e| ZellijAuthError::ClientRegistrationFailed {
            status: 0,
            body: format!("read: {e}"),
        })?;
    parse_create_client_response(&buf)
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

    /// Standard zellij 0.44.3 stdout — one `token_<N>: <UUID>` line. We
    /// extract both halves so the caller can later revoke by name.
    #[test]
    fn parse_token_line_extracts_name_and_uuid() {
        let stdout = "token_1: 11111111-2222-3333-4444-555555555555\n";
        let (name, uuid) = parse_token_line(stdout).expect("parse succeeds");
        assert_eq!(name, "token_1");
        assert_eq!(uuid, "11111111-2222-3333-4444-555555555555");
    }

    /// Real-world zellij 0.44.3 output starts with a "Created token
    /// successfully" preamble and a blank line; the parser must skip
    /// those and pick the `token_<N>: <UUID>` line.
    #[test]
    fn parse_token_line_skips_created_token_preamble() {
        let stdout = "\
Created token successfully\n\
\n\
token_1: 9c6718ad-2621-46cc-895a-b12bec301c27\n";
        let (name, uuid) = parse_token_line(stdout).expect("parse succeeds");
        assert_eq!(name, "token_1");
        assert_eq!(uuid, "9c6718ad-2621-46cc-895a-b12bec301c27");
    }

    /// Surrounding whitespace on the matching line (indentation, trailing
    /// CR from a Windows-style line ending) must not block the match.
    #[test]
    fn parse_token_line_tolerates_surrounding_whitespace() {
        let stdout = "   token_42: aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee   \r\n";
        let (name, uuid) = parse_token_line(stdout).expect("parse succeeds");
        assert_eq!(name, "token_42");
        assert_eq!(uuid, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
    }

    /// No colon anywhere → None (the caller surfaces this as
    /// `MalformedTokenOutput` with the original stdout for diagnostics).
    #[test]
    fn parse_token_line_returns_none_without_colon() {
        assert_eq!(parse_token_line("nothing useful here\n"), None);
    }

    /// Colon-bearing lines with non-`token_` left side and non-UUID right
    /// side are skipped. Guards against a future zellij version that
    /// prints, say, "Expires: 2026-01-01" on a line we'd otherwise eat.
    #[test]
    fn parse_token_line_rejects_non_token_lines() {
        let stdout = "Created: yes\nExpires: 2026-01-01\n";
        assert_eq!(parse_token_line(stdout), None);
    }

    /// Empty stdout (`--create-token` exited 0 but printed nothing — would
    /// be a zellij regression) → None. The caller surfaces it through
    /// `MalformedTokenOutput` with the empty stdout.
    #[test]
    fn parse_token_line_returns_none_on_empty() {
        assert_eq!(parse_token_line(""), None);
    }

    /// A colon-bearing line whose left side doesn't start with `token_`
    /// even when the right side IS a UUID is skipped. Guards against a
    /// stray line like "id: <UUID>" being misinterpreted as our token.
    #[test]
    fn parse_token_line_requires_token_prefix_on_name() {
        let stdout = "id: 11111111-2222-3333-4444-555555555555\n";
        assert_eq!(parse_token_line(stdout), None);
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

    /// End-to-end regression on the real `zellij web --create-token` CLI:
    /// the pre-fix code passed an unrecognized `--name <X>` flag, so the
    /// subprocess exited non-zero before parsing could even start. This
    /// test exercises the actual subprocess against the user's installed
    /// zellij — if zellij ever reintroduces the historical flag rejection
    /// (or renames `--create-token`'s output format), this fires first.
    ///
    /// Gated on `zellij --version` succeeding — same pattern as the
    /// existing `_against_real_zellij` tests in `zellij_cli`. Cleans up
    /// after itself by revoking the minted token; without that, the
    /// user's `zellij web --list-tokens` would accumulate one entry per
    /// test run.
    #[test]
    fn mint_token_against_real_zellij_returns_parseable_name_and_uuid() {
        if !zellij_installed() {
            eprintln!("skipping: zellij not installed");
            return;
        }
        // No ZELLIJ_SOCKET_DIR setup: `zellij web --create-token` is a
        // one-shot CLI that writes to zellij's on-disk auth store; it
        // doesn't bind the IPC socket that ZELLIJ_SOCKET_DIR governs.

        let (name, uuid) = mint_token().expect("mint succeeds against real zellij");
        // Best-effort cleanup *before* the assertions panic, so a future
        // assertion regression doesn't strand the token in the user's
        // auth store. Revoke is best-effort already; running it twice
        // (here + a hypothetical Drop) is harmless.
        revoke_token(&name);

        assert!(
            name.starts_with("token_"),
            "auto-generated name must start with `token_`, got: {name:?}",
        );
        assert!(
            looks_like_uuid(&uuid),
            "parsed UUID must be UUID-shaped, got: {uuid:?}",
        );
    }

    /// Standard zellij response shape from the empirical curl trace in
    /// issue #28: `{"web_client_id":"<UUID>","is_read_only":false}`.
    /// The parser extracts the id and ignores the trailing field.
    #[test]
    fn parse_create_client_response_extracts_web_client_id() {
        let raw = "\
HTTP/1.1 200 OK\r\n\
Content-Type: application/json\r\n\
Content-Length: 60\r\n\
\r\n\
{\"web_client_id\":\"0b37aed9-4d1d-442a-a500-b34221c1c653\",\"is_read_only\":false}";
        let id = parse_create_client_response(raw.as_bytes()).expect("parse");
        assert_eq!(id, "0b37aed9-4d1d-442a-a500-b34221c1c653");
    }

    /// Field-order tolerance: zellij could change the JSON encoder and
    /// emit `is_read_only` first; the extractor must still find the
    /// `web_client_id` value.
    #[test]
    fn parse_create_client_response_tolerates_field_reordering() {
        let raw = "\
HTTP/1.1 200 OK\r\n\
\r\n\
{\"is_read_only\":false,\"web_client_id\":\"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\"}";
        let id = parse_create_client_response(raw.as_bytes()).expect("parse");
        assert_eq!(id, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
    }

    /// 2xx but missing the `web_client_id` field → `ClientRegistrationInvalid`.
    /// This is the version-drift failure mode (zellij renamed the field).
    #[test]
    fn parse_create_client_response_fails_when_field_is_absent() {
        let raw = "\
HTTP/1.1 200 OK\r\n\
\r\n\
{\"is_read_only\":false}";
        match parse_create_client_response(raw.as_bytes()) {
            Err(ZellijAuthError::ClientRegistrationInvalid { body }) => {
                assert!(body.contains("is_read_only"), "body in error: {body}");
            }
            other => panic!("expected ClientRegistrationInvalid, got {other:?}"),
        }
    }

    /// Empty `web_client_id` value → invalid (otherwise we'd build a
    /// `?web_client_id=` URL that zellij would still reject).
    #[test]
    fn parse_create_client_response_fails_on_empty_id() {
        let raw = "\
HTTP/1.1 200 OK\r\n\
\r\n\
{\"web_client_id\":\"\",\"is_read_only\":false}";
        match parse_create_client_response(raw.as_bytes()) {
            Err(ZellijAuthError::ClientRegistrationInvalid { .. }) => {}
            other => panic!("expected ClientRegistrationInvalid, got {other:?}"),
        }
    }

    /// 4xx response → `ClientRegistrationFailed` with the status code.
    /// Most likely cause is a stale `session_token` cookie (daemon
    /// restarted between login and register), surfaced verbatim so the
    /// user can re-trigger an attach.
    #[test]
    fn parse_create_client_response_surfaces_401_with_status() {
        let raw = "\
HTTP/1.1 401 Unauthorized\r\n\
Content-Length: 0\r\n\
\r\n";
        match parse_create_client_response(raw.as_bytes()) {
            Err(ZellijAuthError::ClientRegistrationFailed { status, .. }) => {
                assert_eq!(status, 401);
            }
            other => panic!("expected ClientRegistrationFailed, got {other:?}"),
        }
    }

    /// Wire-shape pin on the POST body and headers. A regression that
    /// dropped the `session_token` cookie or renamed the path would land
    /// here as a failing assertion before any real-zellij run.
    #[test]
    fn build_register_client_request_bytes_carries_cookie_and_path() {
        let bytes = build_register_client_request_bytes(54321, "the-session-token");
        let text = std::str::from_utf8(&bytes).expect("ascii request");
        assert!(text.starts_with("POST /session HTTP/1.1\r\n"), "got: {text}");
        assert!(text.contains("Host: 127.0.0.1:54321\r\n"), "got: {text}");
        assert!(
            text.contains("Cookie: session_token=the-session-token\r\n"),
            "got: {text}",
        );
        assert!(text.contains("Content-Type: application/json\r\n"), "got: {text}");
        // Empty JSON body — `{}` is two bytes, must be reflected in
        // Content-Length so zellij doesn't hang reading more.
        assert!(text.contains("Content-Length: 2\r\n"), "got: {text}");
        assert!(text.ends_with("{}"), "got: {text}");
    }

    /// End-to-end: spin a tiny TCP server that mimics zellij's /session
    /// endpoint, drive `register_client` against it, assert the parsed
    /// id. Mirrors the `exchange_login_round_trips_through_loopback`
    /// pattern.
    #[test]
    fn register_client_round_trips_through_loopback() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();

        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            // Drain headers; assert the session_token cookie is present
            // on the wire so a regression dropping it lands here.
            let mut saw_cookie = false;
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                if line.to_ascii_lowercase().contains("cookie:")
                    && line.contains("session_token=the-cookie")
                {
                    saw_cookie = true;
                }
                if line == "\r\n" {
                    break;
                }
                line.clear();
            }
            assert!(saw_cookie, "register_client must carry session_token cookie");
            let response = "\
HTTP/1.1 200 OK\r\n\
Content-Type: application/json\r\n\
\r\n\
{\"web_client_id\":\"fixture-client-id-7777\",\"is_read_only\":false}";
            let _ = stream.write_all(response.as_bytes());
        });

        let id = register_client(port, "the-cookie").expect("register succeeds");
        assert_eq!(id, "fixture-client-id-7777");
    }

    /// End-to-end against a real `zellij web` daemon: spawn it on a free
    /// port, run mint+login+register_client, assert the returned
    /// `web_client_id` is UUID-shaped. This pins the issue #28 bug fix —
    /// pre-fix, sanctel skipped this step entirely and every subsequent
    /// WebSocket handshake answered HTTP 400 because zellij's WS handlers
    /// require `?web_client_id=<id>` from a registered client.
    ///
    /// Gated on `zellij --version` like the other `_against_real_zellij`
    /// tests; skips cleanly in environments without zellij installed.
    /// Cleans up by revoking the minted token and killing the daemon
    /// regardless of test outcome — leaving a stranded daemon would
    /// hold the port and break subsequent runs.
    #[test]
    fn register_client_against_real_zellij_returns_uuid() {
        use std::path::PathBuf;
        use std::process::{Command, Stdio};
        use std::time::Instant;

        if !zellij_installed() {
            eprintln!("skipping: zellij not installed");
            return;
        }

        // Short ZELLIJ_SOCKET_DIR for the same macOS sun_path reason
        // covered in issue #25. The path is set via the production
        // helper so this test's environment matches what sanctel sets
        // at startup.
        let socket_dir = PathBuf::from(format!(
            "/tmp/sanctel-zellij-auth-test-{}",
            std::process::id(),
        ));
        crate::zellij_cli::set_zellij_socket_dir(&socket_dir);

        // Pick a free loopback port — bind+drop, same shape as the
        // helper in zellij_daemon.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let mut daemon = Command::new("zellij")
            .args(["web", "--start", "--port", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn zellij web");

        // Wait for the daemon to bind the port (cold-spawn budget: 5s).
        // The login retry loop also has a budget, but a hung daemon
        // here means the test panic message names the right failure
        // mode rather than blaming the auth flow.
        let bound = (|| {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(5) {
                if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            false
        })();

        let run = || -> Result<(String, String), String> {
            if !bound {
                return Err(format!("zellij web did not bind port {port} within 5s"));
            }
            let pair = authenticate(port).map_err(|e| e.to_string())?;
            let id = register_client(port, &pair.session_token).map_err(|e| e.to_string())?;
            Ok((pair.auth_token_name, id))
        };
        let result = run();

        // Cleanup, regardless of result. Revoke before kill so the
        // running daemon is still around to honor the request; the
        // best-effort revoke would otherwise hit a dead port.
        if let Ok((ref token_name, _)) = result {
            revoke_token(token_name);
        }
        let _ = daemon.kill();
        let _ = daemon.wait();

        match result {
            Ok((_, id)) => {
                assert!(
                    looks_like_uuid(&id),
                    "web_client_id must be UUID-shaped, got: {id:?}",
                );
            }
            Err(msg) => panic!("{msg}"),
        }
    }

    fn zellij_installed() -> bool {
        std::process::Command::new("zellij")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}
