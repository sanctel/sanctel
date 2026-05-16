// Shared terminal-runtime module imported by both terminal.html and
// chat.html. v0.3 Slice 1 just renders an empty xterm; later slices wire
// the Tauri IPC channel, addons, clipboard, and resize observer here.
// See docs/design/terminal-runtime.md.

import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";

export interface MountOptions {
  // Reserved for later slices (worktreeId etc. flow through Rust at
  // create_tab time, not through this entry point — identity stays
  // server-held per the design doc).
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
  term.open(container);

  return {
    term,
    dispose: () => term.dispose(),
  };
}
