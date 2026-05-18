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
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
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

/// JSON-tagged-enum control messages exchanged on `/ws/control`. Mirrors the
/// shape used by zellij's `web_client/control_message.rs` — internally
/// tagged on a `type` discriminator, snake_case variants. Only the two
/// variants this slice actually emits are modeled; the read side accepts
/// unknown variants via `serde(other)` would be nice but isn't necessary
/// here (we don't act on inbound control messages in slice 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    /// Tells zellij the embedded terminal is now `rows`x`cols`. Sent on
    /// every xterm.js resize event so the PTY pane size matches.
    TerminalResize { rows: u16, cols: u16 },
    /// Pushes a config blob to zellij at attach time. The body is opaque
    /// to sanctel — we treat it as a string so the field is forward-compat
    /// with whatever shape zellij accepts.
    SetConfig { config: String },
}

impl ControlMessage {
    /// Serialize to the JSON string that goes on the wire as a text frame.
    pub fn encode(&self) -> Result<String, ZellijWsError> {
        serde_json::to_string(self).map_err(|e| ZellijWsError::Encode(e.to_string()))
    }

    /// Parse a text frame received from `/ws/control`. Used by tests pinning
    /// the round-trip; production code in slice 3 only sends control
    /// messages — inbound control frames are ignored.
    #[allow(dead_code)]
    pub fn decode(s: &str) -> Result<Self, ZellijWsError> {
        serde_json::from_str(s).map_err(|e| ZellijWsError::Encode(e.to_string()))
    }
}

/// Wrap raw PTY bytes for the `/ws/terminal/<session>` endpoint. Pure
/// function so the binary-frame contract is unit-testable without a real
/// WebSocket.
pub fn encode_binary_frame(bytes: Vec<u8>) -> Message {
    Message::Binary(bytes.into())
}

/// Extract bytes from an inbound `/ws/terminal/<session>` frame. Returns
/// None for non-binary frames (text, ping/pong, close, etc.) — the data
/// path is binary-only by design (no UTF-8 transcoding on PTY output).
pub fn decode_binary_frame(msg: &Message) -> Option<Vec<u8>> {
    match msg {
        Message::Binary(b) => Some(b.to_vec()),
        _ => None,
    }
}

/// Per-tab handle returned by `mount`. Owns the send-side of the outgoing
/// mpsc channels; drop closes them and the I/O threads exit cleanly.
pub struct ZellijWsHandle {
    terminal_tx: Sender<Message>,
    control_tx: Sender<Message>,
    // JoinHandles are kept so the threads are observable in tests / future
    // diagnostics. They're joined implicitly when the handle drops by way
    // of the senders closing; we don't explicitly `.join()` on Drop because
    // the I/O threads sleep up to a few ms between iterations and we don't
    // want to block the calling thread for that.
    _terminal_thread: Option<JoinHandle<()>>,
    _control_thread: Option<JoinHandle<()>>,
}

impl ZellijWsHandle {
    /// Send raw PTY input bytes to zellij. Goes out as a binary frame on
    /// the terminal endpoint.
    pub fn write_bytes(&self, bytes: Vec<u8>) -> Result<(), ZellijWsError> {
        self.terminal_tx
            .send(encode_binary_frame(bytes))
            .map_err(|e| ZellijWsError::Send(e.to_string()))
    }

    /// Notify zellij that the pane is now `cols`x`rows`. Goes out as a text
    /// frame with a `TerminalResize` JSON body on the control endpoint.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), ZellijWsError> {
        let msg = ControlMessage::TerminalResize { rows, cols };
        self.control_tx
            .send(Message::Text(msg.encode()?.into()))
            .map_err(|e| ZellijWsError::Send(e.to_string()))
    }
}

/// Open the two WebSocket connections for one attached tab and start the
/// I/O threads. Binary frames from `/ws/terminal/<session>` are forwarded
/// to `on_output`; bytes pushed through the returned handle's
/// `write_bytes` flow back the other way. The control endpoint carries
/// resize / set-config messages.
pub fn mount(
    session_name: &str,
    port: u16,
    on_output: Channel<Vec<u8>>,
) -> Result<ZellijWsHandle, ZellijWsError> {
    let terminal_url = format!("ws://127.0.0.1:{port}/ws/terminal/{session_name}");
    let control_url = format!("ws://127.0.0.1:{port}/ws/control");

    let terminal_ws = connect_plain(&terminal_url)?;
    let control_ws = connect_plain(&control_url)?;

    let (terminal_tx, terminal_rx) = mpsc::channel::<Message>();
    let (control_tx, control_rx) = mpsc::channel::<Message>();

    let terminal_thread = thread::spawn(move || {
        io_loop(terminal_ws, terminal_rx, move |msg| {
            if let Some(bytes) = decode_binary_frame(&msg) {
                // Channel closed = webview gone; the I/O loop exits next
                // iteration when it next tries to send.
                let _ = on_output.send(bytes);
            }
        });
    });
    let control_thread = thread::spawn(move || {
        io_loop(control_ws, control_rx, |_| {
            // Inbound control messages are ignored in this slice. Future
            // slices may want to dispatch on them (e.g., daemon-side
            // notifications about session state changes).
        });
    });

    Ok(ZellijWsHandle {
        terminal_tx,
        control_tx,
        _terminal_thread: Some(terminal_thread),
        _control_thread: Some(control_thread),
    })
}

fn connect_plain(url: &str) -> Result<WebSocket<MaybeTlsStream<TcpStream>>, ZellijWsError> {
    let (ws, _resp) =
        tungstenite::connect(url).map_err(|e| ZellijWsError::Connect(e.to_string()))?;
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

    /// `TerminalResize` round-trips through serde_json. The JSON shape is
    /// the wire contract with zellij web's `/ws/control` endpoint — a
    /// silent change to the tag name or field names would land here as a
    /// failing assertion before it reached a manual acceptance run.
    #[test]
    fn terminal_resize_round_trips_through_json() {
        let msg = ControlMessage::TerminalResize { rows: 24, cols: 80 };
        let encoded = msg.encode().expect("encode");
        // Tag + field shape pin: `{"type":"terminal_resize","rows":24,"cols":80}`.
        assert!(encoded.contains("\"type\":\"terminal_resize\""), "got: {encoded}");
        assert!(encoded.contains("\"rows\":24"), "got: {encoded}");
        assert!(encoded.contains("\"cols\":80"), "got: {encoded}");
        let decoded = ControlMessage::decode(&encoded).expect("decode");
        assert_eq!(decoded, msg);
    }

    /// `SetConfig` round-trips through serde_json. The body is opaque to
    /// sanctel — modeled as a string so the field shape matches whatever
    /// zellij accepts without sanctel having to mirror its config struct.
    #[test]
    fn set_config_round_trips_through_json() {
        let msg = ControlMessage::SetConfig {
            config: "scroll_buffer_size = 10000".into(),
        };
        let encoded = msg.encode().expect("encode");
        assert!(encoded.contains("\"type\":\"set_config\""), "got: {encoded}");
        assert!(encoded.contains("\"config\":"), "got: {encoded}");
        let decoded = ControlMessage::decode(&encoded).expect("decode");
        assert_eq!(decoded, msg);
    }

    /// A malformed JSON body surfaces as `Encode`, not a panic. The
    /// production control loop ignores inbound messages in this slice
    /// (see `mount`), but the decode helper is also called from tests
    /// and any future inbound-message handling.
    #[test]
    fn decode_malformed_json_surfaces_as_encode_error() {
        match ControlMessage::decode("{not json") {
            Err(ZellijWsError::Encode(_)) => {}
            other => panic!("expected Encode error, got {other:?}"),
        }
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

    /// Non-binary frames decode to `None` so the production reader can
    /// ignore them without misforwarding text or control frames into the
    /// byte channel.
    #[test]
    fn non_binary_frames_decode_to_none() {
        assert!(decode_binary_frame(&Message::Text("hi".into())).is_none());
        assert!(decode_binary_frame(&Message::Ping(vec![].into())).is_none());
        assert!(decode_binary_frame(&Message::Pong(vec![].into())).is_none());
    }
}
