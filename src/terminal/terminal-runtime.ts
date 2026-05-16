// Shared terminal-runtime module imported by both terminal.html and
// chat.html. Slice 2 wires xterm.js end-to-end against the Rust
// `terminal_attach` / `terminal_write` / `terminal_resize` commands; output
// flows over a Tauri Channel as raw bytes (no UTF-8 transcoding on the data
// path — see docs/design/terminal-runtime.md §"IPC contract").
//
// Per-tab identity (worktreeId, windowName, initialCommand) is server-held
// in the Rust TabRecord, so mount() takes no arguments beyond the container.
// The webview's label IS the tabId.

import { Channel, invoke } from "@tauri-apps/api/core";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";

export interface MountOptions {
  // Reserved for chat.html to mount with a header above; the data path
  // does not vary by tab kind.
}

export interface MountedTerminal {
  term: Terminal;
  dispose: () => void;
}

export function mount(
  container: HTMLElement,
  _options: MountOptions = {},
): MountedTerminal {
  const term = new Terminal({
    cursorBlink: true,
    fontFamily: "ui-monospace, Menlo, Consolas, monospace",
    fontSize: 13,
    scrollback: 10_000,
    theme: { background: "#0e0e10", foreground: "#e4e4e7" },
  });

  const fit = new FitAddon();
  term.loadAddon(fit);

  term.open(container);

  // WebGL renderer for throughput. Falls back to canvas silently if WebGL
  // isn't available (e.g., older WebKit).
  try {
    const webgl = new WebglAddon();
    term.loadAddon(webgl);
  } catch {
    // No WebGL — xterm will use its canvas renderer.
  }

  // Compute the initial viewport size from the container's actual dimensions
  // before talking to Rust, so the PTY starts at the right size.
  fit.fit();

  // Output channel: raw bytes from the PTY → xterm.write. xterm accepts
  // Uint8Array directly; Tauri's Vec<u8> arrives as a number[] over the
  // wire, so coerce to Uint8Array. No UTF-8 decoding here — xterm handles
  // it correctly (including partial multi-byte chunks).
  const onOutput = new Channel<number[] | Uint8Array>();
  onOutput.onmessage = (data) => {
    const bytes = data instanceof Uint8Array ? data : Uint8Array.from(data);
    term.write(bytes);
  };

  // Encode keystrokes back to bytes. UTF-8 here is fine: the only data we
  // ever encode at this boundary is the string that xterm.onData emits,
  // which is already UTF-8 by xterm's contract.
  const encoder = new TextEncoder();
  const onDataDisposable = term.onData((s) => {
    const bytes = Array.from(encoder.encode(s));
    invoke("terminal_write", { bytes }).catch((e) =>
      console.error("terminal_write failed", e),
    );
  });

  // Attach. Rust looks up worktreeId / windowName / initialCommand by this
  // webview's label from the TabRecord that create_tab stored.
  invoke("terminal_attach", {
    cols: term.cols,
    rows: term.rows,
    onOutput,
  }).catch((e) => {
    // Surface attach failures (e.g., tmux missing, worktree gone) inline.
    // Slice 6 turns this into a proper broken-tab UI; Slice 2 just writes
    // the error into the terminal so the demo still tells you what failed.
    term.write(`\r\n\x1b[31mterminal_attach failed: ${e}\x1b[0m\r\n`);
  });

  return {
    term,
    dispose: () => {
      onDataDisposable.dispose();
      term.dispose();
    },
  };
}
