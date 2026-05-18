// ───────────────────────────────────────────────────────────────────────────
// zellij_ws — WebSocket client for the `zellij web` daemon. Spike slice 3
// (issue #19, PRD #16).
//
// Architecture:
//   - Two endpoints per attached tab: `ws://localhost:<port>/ws/control`
//     (JSON-tagged-enum control messages — resize, set-config) and
//     `ws://localhost:<port>/ws/terminal/<session>` (raw bytes both ways).
//   - One std::thread per connection runs a small non-blocking I/O loop:
//     try-read from the WebSocket, try-recv outgoing mpsc, sleep briefly
//     if neither produced work. The underlying TcpStream is set to
//     non-blocking so blocking reads can't lock out writes.
//   - The returned `ZellijWsHandle` exposes `write_bytes` (terminal input)
//     and `resize` (sent on the control endpoint). Drop closes both mpsc
//     senders; the I/O threads observe the disconnect and exit.
//
// Tests cover the load-bearing pieces unit-tests can reach:
//   - `ControlMessage` JSON round-trip for the two variants this slice
//     uses (TerminalResize, SetConfig). The argv shape of the JSON is the
//     wire contract with `zellij web`; a regression that changes the tag
//     name silently would land here.
//   - Binary frame round-trip helpers — bytes in, same bytes out — pinned
//     so the data path is byte-clean (no transcoding on PTY output).
//
// The real-zellij integration is exercised by the manual acceptance
// criteria in the PRD (sandcastle CI doesn't ship a zellij binary).
// ───────────────────────────────────────────────────────────────────────────

use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::client::Request;
use tungstenite::http::HeaderValue;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// Errors surfaced by the WebSocket client.
#[derive(Debug)]
pub enum ZellijWsError {
    /// Could not connect to the zellij web daemon (port closed, handshake
    /// failed, etc.).
    Connect(String),
    /// Outgoing message could not be enqueued (I/O thread exited).
    Send(String),
    /// JSON encode/decode of a control message failed.
    Encode(String),
}

impl std::fmt::Display for ZellijWsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ZellijWsError::Connect(msg) => write!(f, "zellij_ws connect: {msg}"),
            ZellijWsError::Send(msg) => write!(f, "zellij_ws send: {msg}"),
            ZellijWsError::Encode(msg) => write!(f, "zellij_ws encode: {msg}"),
        }
    }
}

impl std::error::Error for ZellijWsError {}

/// Outbound payloads on `/ws/control`. Wire shape mirrors zellij's
/// `WebClientToWebServerControlMessagePayload` enum (internally tagged on
/// `type`, PascalCase variant names — `#[serde(tag = "type")]` with no
/// `rename_all` matches zellij's Rust idents byte-for-byte). The inner
/// fields stay snake_case via Rust naming.
///
/// Verified against `zellij-client/src/web_client/control_message.rs`
/// and `zellij-client/assets/websockets.js` (issue #29 audit).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ControlPayload {
    /// Grid dimensions in character cells. Server uses this to size the
    /// PTY. Sent on connect and on every xterm resize.
    TerminalResize { rows: u16, cols: u16 },
    /// Pixel-precise display geometry. Zellij forwards these to apps that
    /// query screen size via CSI 14t / 16t / OSC 11. Sent immediately
    /// after TerminalResize on connect and on resize, mirroring zellij's
    /// own client.
    TerminalMetrics {
        cell_pixel_width: u32,
        cell_pixel_height: u32,
        text_area_pixel_width: u32,
        text_area_pixel_height: u32,
    },
}

/// Outer envelope wrapping every control message sanctel sends. Mirrors
/// zellij's `WebClientToWebServerControlMessage`. The `web_client_id`
/// field is what triggers zellij's server-side `add_client_control_tx`
/// on first receipt — without an envelope, the message fails JSON
/// validation server-side and the connection eventually closes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlEnvelope {
    pub web_client_id: String,
    pub payload: ControlPayload,
}

impl ControlEnvelope {
    /// Build an envelope wrapping `payload` with the given client id. Pure
    /// so the wire shape is unit-testable.
    pub fn new(web_client_id: &str, payload: ControlPayload) -> Self {
        Self {
            web_client_id: web_client_id.to_string(),
            payload,
        }
    }

    /// Serialize to the JSON text frame that goes on the wire.
    pub fn encode(&self) -> Result<String, ZellijWsError> {
        serde_json::to_string(self).map_err(|e| ZellijWsError::Encode(e.to_string()))
    }
}

/// Inbound messages from `/ws/control`. Mirrors zellij's
/// `WebServerToWebClientControlMessage`. We only act on
/// `QueryTerminalSize` (re-emit a size update); the rest are accepted
/// silently so unknown future variants don't break the WS read loop.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ServerControlMessage {
    /// Zellij asks the client for current grid + pixel dimensions. We
    /// respond with the latest `TerminalResize` + `TerminalMetrics`.
    QueryTerminalSize,
    /// Theme / font / cursor config. Sanctel doesn't apply these (xterm
    /// is styled by the webview), so this is informational.
    SetConfig(serde_json::Value),
    Log { #[allow(dead_code)] lines: Vec<String> },
    LogError { #[allow(dead_code)] lines: Vec<String> },
    SwitchedSession { #[allow(dead_code)] new_session_name: String },
    /// Forward-compat: any variant we don't yet model. Keeps the read
    /// loop alive across zellij version bumps that introduce new message
    /// types.
    #[serde(other)]
    Other,
}

/// Default cell pixel dimensions used when the frontend hasn't reported
/// real measurements yet. 7×14 is a common monospace cell on a 1× display
/// at the font sizes terminal apps typically use. The values are only
/// load-bearing for OSC-11 / CSI-14t queries (screen size in pixels);
/// the grid dimensions in `TerminalResize` are what actually drive the
/// PTY size. Refined when xterm emits a real resize event.
const DEFAULT_CELL_PIXEL_WIDTH: u32 = 7;
const DEFAULT_CELL_PIXEL_HEIGHT: u32 = 14;
const DEFAULT_ROWS: u16 = 24;
const DEFAULT_COLS: u16 = 80;

/// Wrap raw PTY bytes for outbound on `/ws/terminal/<session>`. We send
/// as `Message::Binary` rather than `Message::Text` because keystrokes
/// from xterm.js can include non-UTF-8 byte sequences (binary pastes,
/// raw escape codes) and `Message::Text` would force a UTF-8
/// validation that those payloads would fail. zellij's terminal-WS
/// handler accepts both Binary and Text inbound and routes both
/// through the same `parse_stdin` helper, so binary outbound is the
/// safer choice.
pub fn encode_binary_frame(bytes: Vec<u8>) -> Message {
    Message::Binary(bytes.into())
}

/// Extract PTY output bytes from an inbound `/ws/terminal/<session>`
/// frame. **zellij sends terminal output as `Message::Text` frames**
/// (verified empirically via the issue #29 audit — sanctel's earlier
/// binary-only acceptance was the load-bearing data-path failure
/// behind the closed-channel symptom). Accepts both text and binary so
/// a future zellij version that switches frame types doesn't silently
/// stop forwarding output again.
pub fn decode_binary_frame(msg: &Message) -> Option<Vec<u8>> {
    match msg {
        Message::Binary(b) => Some(b.to_vec()),
        Message::Text(t) => Some(t.as_bytes().to_vec()),
        _ => None,
    }
}

/// Per-tab handle returned by `mount`. Owns the send-side of the outgoing
/// mpsc channels; drop closes them and the I/O threads observe the
/// disconnect and exit. The threads detach (the `JoinHandle`s from
/// `thread::spawn` are discarded) — we don't `.join()` on Drop because
/// the I/O threads sleep up to a few ms between iterations and we don't
/// want to block the caller for that.
pub struct ZellijWsHandle {
    web_client_id: String,
    terminal_tx: Sender<Message>,
    control_tx: Sender<Message>,
    /// Last known grid + cell dimensions. Updated on `resize()` and read
    /// by the control inbound handler when responding to
    /// `QueryTerminalSize`. Shared with the io_loop thread.
    last_size: Arc<Mutex<SizeState>>,
}

#[derive(Debug, Clone, Copy)]
struct SizeState {
    rows: u16,
    cols: u16,
    cell_pixel_width: u32,
    cell_pixel_height: u32,
}

impl SizeState {
    fn default() -> Self {
        Self {
            rows: DEFAULT_ROWS,
            cols: DEFAULT_COLS,
            cell_pixel_width: DEFAULT_CELL_PIXEL_WIDTH,
            cell_pixel_height: DEFAULT_CELL_PIXEL_HEIGHT,
        }
    }

    fn to_messages(&self, web_client_id: &str) -> Result<[Message; 2], ZellijWsError> {
        let resize = ControlEnvelope::new(
            web_client_id,
            ControlPayload::TerminalResize {
                rows: self.rows,
                cols: self.cols,
            },
        );
        let metrics = ControlEnvelope::new(
            web_client_id,
            ControlPayload::TerminalMetrics {
                cell_pixel_width: self.cell_pixel_width,
                cell_pixel_height: self.cell_pixel_height,
                text_area_pixel_width: u32::from(self.cols) * self.cell_pixel_width,
                text_area_pixel_height: u32::from(self.rows) * self.cell_pixel_height,
            },
        );
        Ok([
            Message::Text(resize.encode()?.into()),
            Message::Text(metrics.encode()?.into()),
        ])
    }
}

impl ZellijWsHandle {
    /// Send raw PTY input bytes to zellij. Goes out as a binary frame on
    /// the terminal endpoint.
    pub fn write_bytes(&self, bytes: Vec<u8>) -> Result<(), ZellijWsError> {
        self.terminal_tx
            .send(encode_binary_frame(bytes))
            .map_err(|e| ZellijWsError::Send(e.to_string()))
    }

    /// Notify zellij that the pane is now `cols`×`rows`. Sends BOTH a
    /// `TerminalResize` and a `TerminalMetrics` envelope on the control
    /// endpoint, mirroring zellij's own client (see
    /// `zellij-client/assets/websockets.js::sendSizeUpdate`). Sending only
    /// `TerminalResize` leaves stale pixel dimensions cached in zellij;
    /// apps querying screen size via CSI 14t / 16t / OSC 11 then get
    /// outdated answers.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), ZellijWsError> {
        let mut size = self.last_size.lock().unwrap();
        size.rows = rows;
        size.cols = cols;
        let msgs = size.to_messages(&self.web_client_id)?;
        drop(size);
        for msg in msgs {
            self.control_tx
                .send(msg)
                .map_err(|e| ZellijWsError::Send(e.to_string()))?;
        }
        Ok(())
    }
}

/// Bytes that bootstrap a freshly-created chat session: command + `\n`.
/// The trailing newline is what makes the receiving shell treat the bytes
/// as a submitted command — without it the shell sees the keystrokes but
/// never runs them, mirroring the `\n` that lands in tmux's PTY when
/// `new-session ... <cmd>` runs the command. Shared between the transient
/// WS path ([`write_initial_command`]) and the persistent WS path
/// (terminal_runtime's attach) so the wire shape can't drift between them.
pub fn initial_command_bytes(command: &str) -> Vec<u8> {
    format!("{command}\n").into_bytes()
}

/// `ws://127.0.0.1:<port>/ws/terminal/<session>?web_client_id=<id>`. Pure
/// so the URL shape is unit-testable without opening a real socket.
/// The id is URL-encoded so a future zellij version that swaps the UUID
/// for an opaque token containing reserved characters doesn't silently
/// corrupt the handshake.
pub fn terminal_ws_url(port: u16, session_name: &str, web_client_id: &str) -> String {
    format!(
        "ws://127.0.0.1:{port}/ws/terminal/{session_name}?web_client_id={}",
        url_encode(web_client_id),
    )
}

/// `ws://127.0.0.1:<port>/ws/control?web_client_id=<id>`. Same query-
/// parameter rule as the terminal endpoint — zellij's `ws_handler_control`
/// also requires the id.
pub fn control_ws_url(port: u16, web_client_id: &str) -> String {
    format!(
        "ws://127.0.0.1:{port}/ws/control?web_client_id={}",
        url_encode(web_client_id),
    )
}

/// Minimal percent-encoder covering the characters that would otherwise
/// be interpreted as URL syntax in a query value. Everything outside
/// `[A-Za-z0-9-._~]` is encoded as `%HH`. Adequate for the UUIDs zellij
/// currently emits (which need no encoding) and forward-compat with a
/// less constrained id format.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let is_unreserved = b.is_ascii_alphanumeric()
            || b == b'-'
            || b == b'.'
            || b == b'_'
            || b == b'~';
        if is_unreserved {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Build a tungstenite client request for `url` carrying a
/// `Cookie: session_token=<value>` header. zellij web's auth middleware
/// reads that cookie to authorize the handshake; without it every
/// handshake answers HTTP 401.
///
/// Public so the cookie-injection contract is unit-testable without
/// opening a real WebSocket.
pub fn build_ws_request(url: &str, session_token: &str) -> Result<Request, ZellijWsError> {
    let mut req = url
        .into_client_request()
        .map_err(|e| ZellijWsError::Connect(e.to_string()))?;
    let cookie = format!("session_token={session_token}");
    let value = HeaderValue::from_str(&cookie)
        .map_err(|e| ZellijWsError::Connect(format!("invalid cookie value: {e}")))?;
    req.headers_mut().insert("Cookie", value);
    Ok(req)
}

/// Open the two WebSocket connections for one attached tab and start the
/// I/O threads. Binary frames from `/ws/terminal/<session>` are forwarded
/// to `on_output`; bytes pushed through the returned handle's
/// `write_bytes` flow back the other way. The control endpoint carries
/// resize / set-config messages.
///
/// `session_token` is the cookie minted by `zellij_auth` — see
/// `build_ws_request` for the auth-middleware contract. `web_client_id`
/// is minted by `zellij_auth::register_client` and rides on both URLs as
/// a `?web_client_id=<id>` query parameter; zellij's WS handlers reject
/// the handshake with HTTP 400 if it's missing.
pub fn mount(
    session_name: &str,
    port: u16,
    session_token: &str,
    web_client_id: &str,
    on_output: Channel<Vec<u8>>,
) -> Result<ZellijWsHandle, ZellijWsError> {
    let terminal_url = terminal_ws_url(port, session_name, web_client_id);
    let control_url = control_ws_url(port, web_client_id);

    // Connect order is load-bearing. **Control first, with its initial
    // envelopes written directly to the socket BEFORE the terminal WS
    // opens.**
    //
    // zellij's server registers the per-client control_tx in its
    // connection table on the FIRST envelope message received on
    // `/ws/control`. The listener thread spawned by `handle_ws_terminal`
    // calls `send_control(SwitchedSession{...})` shortly after
    // attachment completes; if control_tx isn't registered yet, that
    // send is dropped silently and the subsequent pipeline stalls.
    // Verified empirically via the issue #29 audit: opening both
    // sockets in parallel produced clean WS closes within milliseconds
    // (the symptom).
    //
    // Writing the initial envelopes via the mpsc → io_loop chain
    // doesn't work either, because the io_loop thread hasn't been
    // spawned yet when we open terminal; queued mpsc messages never
    // reach the wire in time. The envelopes have to be written
    // **directly to the WebSocket in blocking mode** before nonblocking
    // is enabled and before terminal opens.
    let req = build_ws_request(&control_url, session_token)?;
    let (mut control_ws, _) =
        tungstenite::connect(req).map_err(|e| ZellijWsError::Connect(e.to_string()))?;
    let initial_size = SizeState::default();
    let initial_msgs = initial_size.to_messages(web_client_id)?;
    for msg in initial_msgs {
        control_ws
            .send(msg)
            .map_err(|e| ZellijWsError::Send(e.to_string()))?;
    }
    // Flush to ensure bytes are actually on the wire before we open
    // the terminal WS. tungstenite::send in blocking mode usually
    // writes synchronously, but flush forces it explicitly.
    control_ws
        .flush()
        .map_err(|e| ZellijWsError::Send(e.to_string()))?;
    // Now switch to nonblocking for the io_loop's polling pattern.
    if let MaybeTlsStream::Plain(s) = control_ws.get_ref() {
        let _ = s.set_nonblocking(true);
    }
    let (control_tx, control_rx) = mpsc::channel::<Message>();

    let terminal_ws = connect_with_session(&terminal_url, session_token)?;
    let (terminal_tx, terminal_rx) = mpsc::channel::<Message>();

    let last_size = Arc::new(Mutex::new(initial_size));

    thread::spawn(move || {
        io_loop(terminal_ws, terminal_rx, move |msg| {
            if let Some(bytes) = decode_binary_frame(&msg) {
                // Channel closed = webview gone; the I/O loop exits next
                // iteration when it next tries to send.
                let _ = on_output.send(bytes);
            }
        });
    });

    // Control inbound dispatcher: parse incoming text frames as
    // `ServerControlMessage` and respond to `QueryTerminalSize` with the
    // latest cached size. All other variants are accepted silently —
    // dropping unknown variants on the floor is forward-compatible with
    // zellij version bumps that add new server→client messages.
    let control_tx_for_inbound = control_tx.clone();
    let web_client_id_for_inbound = web_client_id.to_string();
    let last_size_for_inbound = last_size.clone();
    thread::spawn(move || {
        io_loop(control_ws, control_rx, move |msg| {
            let Message::Text(text) = msg else {
                return;
            };
            let parsed: Result<ServerControlMessage, _> = serde_json::from_str(&text);
            if let Ok(ServerControlMessage::QueryTerminalSize) = parsed {
                let size = *last_size_for_inbound.lock().unwrap();
                let Ok(reply_msgs) = size.to_messages(&web_client_id_for_inbound) else {
                    return;
                };
                for reply in reply_msgs {
                    if control_tx_for_inbound.send(reply).is_err() {
                        return;
                    }
                }
            }
        });
    });

    Ok(ZellijWsHandle {
        web_client_id: web_client_id.to_string(),
        terminal_tx,
        control_tx,
        last_size,
    })
}

fn connect_with_session(
    url: &str,
    session_token: &str,
) -> Result<WebSocket<MaybeTlsStream<TcpStream>>, ZellijWsError> {
    let req = build_ws_request(url, session_token)?;
    let (ws, _resp) =
        tungstenite::connect(req).map_err(|e| ZellijWsError::Connect(e.to_string()))?;
    if let MaybeTlsStream::Plain(s) = ws.get_ref() {
        let _ = s.set_nonblocking(true);
    }
    Ok(ws)
}

/// Body of the per-connection I/O thread. Alternates non-blocking reads
/// against the WebSocket with non-blocking recvs from the outgoing mpsc.
/// Returns when the underlying socket closes, a fatal protocol error
/// occurs, or the outgoing channel disconnects (handle was dropped).
fn io_loop(
    mut ws: WebSocket<MaybeTlsStream<TcpStream>>,
    outgoing: Receiver<Message>,
    mut on_message: impl FnMut(Message),
) {
    const IDLE_SLEEP: Duration = Duration::from_millis(2);
    loop {
        let mut did_work = false;

        match ws.read() {
            Ok(Message::Close(_)) => return,
            Ok(msg) => {
                on_message(msg);
                did_work = true;
            }
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
            Err(_) => return,
        }

        match outgoing.try_recv() {
            Ok(msg) => match ws.send(msg) {
                Ok(()) => did_work = true,
                Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {
                    // Outgoing buffer full; spin briefly. The message has
                    // already been written to tungstenite's internal queue
                    // and a future `read()` / `send()` will retry the
                    // socket write. Treat as work to avoid backing off.
                    did_work = true;
                }
                Err(_) => return,
            },
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => return,
        }

        if !did_work {
            thread::sleep(IDLE_SLEEP);
        }
    }
}

// ─── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Outbound `TerminalResize` envelope matches zellij's wire shape
    /// byte-for-byte. Pinned against the format `{web_client_id, payload:
    /// {type: "TerminalResize", rows, cols}}` — the PascalCase tag,
    /// outer envelope, and `payload`-keyed wrapping are all load-bearing
    /// per `zellij-client/src/web_client/control_message.rs` and
    /// `zellij-client/assets/websockets.js`. The previous shape sanctel
    /// emitted (`{type:"terminal_resize",rows,cols}` — flat, snake_case,
    /// no envelope) was the load-bearing failure mode behind issue #29.
    #[test]
    fn terminal_resize_envelope_matches_zellij_wire_shape() {
        let env = ControlEnvelope::new(
            "abc-123",
            ControlPayload::TerminalResize { rows: 24, cols: 80 },
        );
        let encoded = env.encode().expect("encode");
        assert!(
            encoded.contains("\"web_client_id\":\"abc-123\""),
            "outer envelope missing web_client_id: {encoded}"
        );
        assert!(
            encoded.contains("\"payload\":"),
            "outer envelope missing payload wrapper: {encoded}"
        );
        // Tag and field shape pin: `type:"TerminalResize"` (PascalCase),
        // snake_case `rows` / `cols` fields nested under payload.
        assert!(
            encoded.contains("\"type\":\"TerminalResize\""),
            "PascalCase tag missing: {encoded}"
        );
        assert!(encoded.contains("\"rows\":24"), "rows missing: {encoded}");
        assert!(encoded.contains("\"cols\":80"), "cols missing: {encoded}");
    }

    /// Outbound `TerminalMetrics` envelope matches zellij's wire shape
    /// per `zellij-client/assets/websockets.js::sendSizeUpdate`. zellij
    /// sends `TerminalResize` and `TerminalMetrics` as a pair on every
    /// connect + resize; both must use the same envelope shape.
    #[test]
    fn terminal_metrics_envelope_matches_zellij_wire_shape() {
        let env = ControlEnvelope::new(
            "abc-123",
            ControlPayload::TerminalMetrics {
                cell_pixel_width: 7,
                cell_pixel_height: 14,
                text_area_pixel_width: 560,
                text_area_pixel_height: 336,
            },
        );
        let encoded = env.encode().expect("encode");
        assert!(
            encoded.contains("\"type\":\"TerminalMetrics\""),
            "PascalCase tag missing: {encoded}"
        );
        assert!(
            encoded.contains("\"cell_pixel_width\":7"),
            "cell_pixel_width missing: {encoded}"
        );
        assert!(
            encoded.contains("\"text_area_pixel_height\":336"),
            "text_area_pixel_height missing: {encoded}"
        );
    }

    /// `SizeState::to_messages` produces the exact pair zellij expects on
    /// connect: TerminalResize first, then TerminalMetrics. Order matters
    /// because zellij's server registers the control channel on the
    /// FIRST message received; if Metrics arrived before Resize, the
    /// server might cache a stale grid size that subsequent resizes
    /// would have to overwrite.
    #[test]
    fn size_state_emits_resize_then_metrics_in_order() {
        let state = SizeState {
            rows: 30,
            cols: 100,
            cell_pixel_width: 8,
            cell_pixel_height: 16,
        };
        let [msg1, msg2] = state.to_messages("client-id").expect("to_messages");
        let Message::Text(text1) = msg1 else {
            panic!("first message must be text frame");
        };
        let Message::Text(text2) = msg2 else {
            panic!("second message must be text frame");
        };
        assert!(
            text1.contains("\"type\":\"TerminalResize\""),
            "first must be TerminalResize: {text1}"
        );
        assert!(
            text2.contains("\"type\":\"TerminalMetrics\""),
            "second must be TerminalMetrics: {text2}"
        );
        // text_area_pixel_* are derived from grid * cell pixel dims.
        // Pin the math so a refactor that drops the multiplication
        // surfaces here.
        assert!(
            text2.contains("\"text_area_pixel_width\":800"),
            "100 cols × 8px = 800: {text2}"
        );
        assert!(
            text2.contains("\"text_area_pixel_height\":480"),
            "30 rows × 16px = 480: {text2}"
        );
    }

    /// Inbound `QueryTerminalSize` parses into the matching variant.
    /// Production code in `mount`'s control inbound dispatcher uses this
    /// parse to decide whether to re-emit a size update.
    #[test]
    fn server_control_message_parses_query_terminal_size() {
        let text = r#"{"type":"QueryTerminalSize"}"#;
        let parsed: ServerControlMessage = serde_json::from_str(text).expect("parse");
        assert!(matches!(parsed, ServerControlMessage::QueryTerminalSize));
    }

    /// Unknown server message types fall into the `Other` variant rather
    /// than failing the parse. Forward-compat with future zellij version
    /// bumps that add new server→client message types.
    #[test]
    fn server_control_message_falls_back_to_other_on_unknown() {
        let text = r#"{"type":"SomeFutureMessage","field":42}"#;
        let parsed: ServerControlMessage = serde_json::from_str(text).expect("parse");
        assert!(matches!(parsed, ServerControlMessage::Other));
    }

    /// Inbound text frames decode to bytes. **Verified empirically:
    /// zellij's `/ws/terminal/<session>` endpoint sends PTY output as
    /// `Message::Text` frames, not binary** (see issue #29 audit). A
    /// regression that drops the Text arm from `decode_binary_frame`
    /// would silently break every terminal tab; pin it here.
    #[test]
    fn decode_accepts_text_frame_from_zellij_output() {
        let zellij_init = "\x1b[?1l\x1b=\x1b[r\x1b[?1000l\x1b[?1002l";
        let text_frame = Message::Text(zellij_init.into());
        let decoded = decode_binary_frame(&text_frame).expect("text frame decodes");
        assert_eq!(decoded.as_slice(), zellij_init.as_bytes());
    }

    /// Binary frame round-trip: bytes go in via `encode_binary_frame`,
    /// come back unchanged via `decode_binary_frame`. The data path is
    /// raw bytes by design (no UTF-8 transcoding), so this proves the
    /// helpers don't accidentally mangle non-text payloads — escape
    /// sequences, control characters, NULs, high-bit bytes all survive.
    #[test]
    fn binary_frame_round_trips_arbitrary_bytes() {
        let payloads: &[&[u8]] = &[
            b"",
            b"hello\n",
            b"\x1b[31mred\x1b[0m",  // ANSI escape
            b"\x00\x01\x02\x03\xff", // NUL + high bytes
            &[0u8; 4096],            // larger payload
        ];
        for bytes in payloads {
            let frame = encode_binary_frame(bytes.to_vec());
            let decoded = decode_binary_frame(&frame).expect("binary frame decodes");
            assert_eq!(decoded.as_slice(), *bytes, "payload survived round-trip");
        }
    }

    /// The initial-command frame appends a trailing newline (so the
    /// receiving shell submits the line) and rides as a binary frame
    /// over `/ws/terminal/<session>` — same byte path as user keystrokes.
    /// A chat tab's `claude --resume <id>` becomes `claude --resume <id>\n`
    /// on the wire; the regression a contributor might land — dropping the
    /// newline, or sending as text — would leave claude waiting for input
    /// or the daemon refusing the frame, both manual-acceptance regressions
    /// we'd rather catch in CI.
    #[test]
    fn initial_command_bytes_appends_newline() {
        assert_eq!(
            initial_command_bytes("claude --resume abc-123"),
            b"claude --resume abc-123\n"
        );
    }

    /// And the plain `claude` (no `--resume`) shape — the first-chat
    /// acceptance criterion path (no prior .jsonl for the Worktree).
    #[test]
    fn initial_command_bytes_handles_plain_claude() {
        assert_eq!(initial_command_bytes("claude"), b"claude\n");
    }

    /// High-throughput byte path sanity check: a 10 MiB payload (the size
    /// the spike's stress criterion #5 calls out, `cat /dev/urandom |
    /// head -c 10M`) survives `encode_binary_frame` → `decode_binary_frame`
    /// byte-identical. The full criterion is a manual acceptance run on a
    /// dev box (sandcastle has no zellij, no xterm.js to receive the
    /// output); this CI artefact rules out a silent length cap in the
    /// in-process byte helpers as the failure mode. A regression that
    /// silently truncated the frame at 1 MB / 16 MB / a u16 length field
    /// would surface here as a length mismatch before reaching a manual
    /// acceptance run.
    #[test]
    fn binary_frame_round_trips_ten_megabytes() {
        let payload: Vec<u8> = (0..10 * 1024 * 1024).map(|i| (i % 256) as u8).collect();
        let frame = encode_binary_frame(payload.clone());
        let decoded = decode_binary_frame(&frame).expect("binary frame decodes");
        // Length check first so a silent cap regression (the failure mode
        // this test exists for) panics with two numbers, not a 10 MB diff.
        assert_eq!(decoded.len(), payload.len(), "frame size cap regression?");
        assert_eq!(decoded, payload);
    }

    /// Non-byte frames (ping/pong/close) decode to `None` so the
    /// production reader can ignore them. **Text frames are accepted**
    /// because zellij sends terminal output as text — see
    /// `decode_accepts_text_frame_from_zellij_output` for the pinned
    /// rationale.
    #[test]
    fn non_byte_frames_decode_to_none() {
        assert!(decode_binary_frame(&Message::Ping(vec![].into())).is_none());
        assert!(decode_binary_frame(&Message::Pong(vec![].into())).is_none());
        assert!(decode_binary_frame(&Message::Close(None)).is_none());
    }

    /// `build_ws_request` must put the session_token cookie on every
    /// request. zellij web's auth middleware reads `Cookie:
    /// session_token=<value>` to authorize the handshake; a regression
    /// that dropped the cookie would re-introduce HTTP 401 across every
    /// terminal tab on the zellij backend.
    #[test]
    fn build_ws_request_carries_session_token_cookie() {
        let req = build_ws_request("ws://127.0.0.1:9123/ws/terminal/foo", "test-session-token")
            .expect("request builds");
        let cookie = req
            .headers()
            .get("Cookie")
            .expect("Cookie header must be present");
        assert_eq!(cookie.to_str().unwrap(), "session_token=test-session-token");
    }

    /// Same cookie shape applies to the control endpoint — both URLs hit
    /// the same auth middleware so both must carry the cookie.
    #[test]
    fn build_ws_request_carries_cookie_on_control_endpoint_too() {
        let req = build_ws_request("ws://127.0.0.1:9123/ws/control", "abc-123")
            .expect("request builds");
        let cookie = req.headers().get("Cookie").expect("Cookie present");
        assert_eq!(cookie.to_str().unwrap(), "session_token=abc-123");
    }

    /// `terminal_ws_url` must append `?web_client_id=<id>` to the path.
    /// zellij's `ws_handler_terminal` deserialises a `TerminalParams` from
    /// the query string at handshake time — a missing parameter rejects
    /// the upgrade with HTTP 400. This is the load-bearing piece of the
    /// fix for issue #28.
    #[test]
    fn terminal_ws_url_carries_web_client_id_query() {
        let url = terminal_ws_url(9123, "sanctel_wt_x__term-1", "client-abc");
        assert_eq!(
            url,
            "ws://127.0.0.1:9123/ws/terminal/sanctel_wt_x__term-1?web_client_id=client-abc",
        );
    }

    /// `control_ws_url` must also carry the query parameter — the same
    /// middleware gates both endpoints.
    #[test]
    fn control_ws_url_carries_web_client_id_query() {
        let url = control_ws_url(9123, "client-abc");
        assert_eq!(url, "ws://127.0.0.1:9123/ws/control?web_client_id=client-abc");
    }

    /// Reserved characters in the id are percent-encoded so a future
    /// zellij version that swaps the UUID for an opaque token containing
    /// `&`, `=`, `/`, etc. doesn't silently corrupt the URL parse. UUIDs
    /// today need no encoding — the test pins forward-compat.
    #[test]
    fn terminal_ws_url_percent_encodes_reserved_chars() {
        let url = terminal_ws_url(9123, "sess", "a&b=c/d");
        assert_eq!(
            url,
            "ws://127.0.0.1:9123/ws/terminal/sess?web_client_id=a%26b%3Dc%2Fd",
        );
    }

    /// Sanity: a canonical UUID id round-trips unchanged through the
    /// encoder. Guards against an over-eager encoder mangling the common
    /// case.
    #[test]
    fn url_encode_leaves_uuids_unchanged() {
        let id = "0b37aed9-4d1d-442a-a500-b34221c1c653";
        assert_eq!(url_encode(id), id);
    }
}
