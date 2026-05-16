// Clipboard glue between xterm.js and the system clipboard.
//
// xterm.js owns selection and key events; the system clipboard is owned by
// the platform. This module bridges the two with a single
// `attachCustomKeyEventHandler` hook:
//
//   Cmd/Ctrl+C with a selection → write selection to clipboard, swallow.
//   Cmd/Ctrl+C with no selection → let xterm send Ctrl+C (SIGINT).
//   Cmd/Ctrl+V                   → read clipboard, term.paste(text).
//
// `term.paste(text)` routes through xterm's existing onData callback, which
// the runtime already forwards to `terminal_write`. So paste flows through
// the same byte path as keystrokes — no separate IPC entry needed.

import type { Terminal } from "@xterm/xterm";

// Injected dependency so tests can run without Tauri.
export interface ClipboardApi {
  readText(): Promise<string>;
  writeText(text: string): Promise<void>;
}

export interface InstallClipboardOptions {
  clipboard: ClipboardApi;
}

export function installClipboard(
  term: Terminal,
  opts: InstallClipboardOptions,
): void {
  term.attachCustomKeyEventHandler((e: KeyboardEvent): boolean => {
    if (e.type !== "keydown") return true;

    // Only handle the bare Cmd/Ctrl combos. Shift+Cmd+C, Alt+Cmd+V, etc.
    // are reserved for xterm / shell shortcuts.
    const mod = e.metaKey || e.ctrlKey;
    if (!mod || e.shiftKey || e.altKey) return true;

    const key = e.key.toLowerCase();

    if (key === "c") {
      if (term.hasSelection()) {
        const text = term.getSelection();
        opts.clipboard
          .writeText(text)
          .catch((err) => console.error("clipboard writeText failed", err));
        return false;
      }
      // No selection — let xterm send the Ctrl+C through to the shell.
      return true;
    }

    if (key === "v") {
      opts.clipboard
        .readText()
        .then((text) => {
          if (text) term.paste(text);
        })
        .catch((err) => console.error("clipboard readText failed", err));
      return false;
    }

    return true;
  });
}
