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
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { parseAttachError } from "./attach-error";

export { parseAttachError } from "./attach-error";
export type { ParsedAttachError } from "./attach-error";

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
    const parsed = parseAttachError(e);
    if (parsed.kind === "worktree-missing") {
      renderBrokenTab(container, parsed.path);
      onDataDisposable.dispose();
      term.dispose();
      return;
    }
    // Other failures (tmux missing, spawn errors) — render inline in the
    // terminal so the user sees what went wrong. tmux-missing should not
    // reach here in practice because React gates create_tab on the startup
    // probe, but defensive surfacing is cheap.
    term.write(`\r\n\x1b[31mterminal_attach failed: ${parsed.message}\x1b[0m\r\n`);
  });

  return {
    term,
    dispose: () => {
      onDataDisposable.dispose();
      term.dispose();
    },
  };
}

/// Inline broken-tab panel for the worktree-missing case. Replaces the
/// xterm canvas inside the container, leaves the sidebar entry untouched.
/// "Recreate from main" is a v0.3.x follow-up — for now the button is wired
/// to a no-op handler so the UI is complete and the click target is real.
/// "Remove this tab" invokes close_tab and lets React clean up the row.
function renderBrokenTab(container: HTMLElement, path: string): void {
  // Wipe xterm's DOM. The Terminal.dispose() call from the caller releases
  // the addon/renderer; this just clears the visual residue.
  container.innerHTML = "";
  const panel = document.createElement("div");
  panel.className = "broken-tab";
  panel.setAttribute("role", "alert");
  panel.style.cssText = [
    "position: absolute",
    "inset: 0",
    "display: flex",
    "flex-direction: column",
    "align-items: center",
    "justify-content: center",
    "gap: 12px",
    "padding: 24px",
    "color: #e4e4e7",
    "background: #0e0e10",
    "font-family: ui-sans-serif, system-ui, sans-serif",
    "text-align: center",
  ].join(";");

  const heading = document.createElement("div");
  heading.style.cssText = "font-size: 15px; font-weight: 600; color: #fca5a5;";
  heading.textContent = "Worktree no longer exists";

  const detail = document.createElement("div");
  detail.style.cssText = "font-size: 13px; color: #a1a1aa; max-width: 480px;";
  detail.textContent = path;

  const buttons = document.createElement("div");
  buttons.style.cssText = "display: flex; gap: 8px;";

  const recreate = document.createElement("button");
  recreate.type = "button";
  recreate.className = "recreate";
  recreate.textContent = "Recreate from main";
  recreate.style.cssText = buttonCss();
  // Recreate-from-main needs a Worktree manager (planned v0.3.x). Until
  // then the button is wired but no-ops with a tooltip rather than silently
  // doing nothing.
  recreate.title = "Worktree recreation lands in the Worktree manager";
  recreate.addEventListener("click", () => {
    recreate.disabled = true;
    recreate.textContent = "Not yet available";
  });

  const remove = document.createElement("button");
  remove.type = "button";
  remove.className = "remove";
  remove.textContent = "Remove this tab";
  remove.style.cssText = buttonCss();
  remove.addEventListener("click", () => {
    // close_tab takes the webview label as id; the webview's label IS the
    // tabId by construction (see src-tauri/src/lib.rs `create_tab`).
    const id = labelFromWebview();
    if (id) {
      invoke("close_tab", { id }).catch((err) =>
        console.error("close_tab failed", err),
      );
    }
  });

  buttons.append(recreate, remove);
  panel.append(heading, detail, buttons);
  container.append(panel);
}

function buttonCss(): string {
  return [
    "padding: 6px 12px",
    "background: #27272a",
    "color: #e4e4e7",
    "border: 1px solid #3f3f46",
    "border-radius: 4px",
    "font-size: 13px",
    "cursor: pointer",
  ].join(";");
}

/// The webview's label IS the tabId by construction (see
/// src-tauri/src/lib.rs `create_tab`). Tauri 2 exposes it via
/// getCurrentWebview().label. Returns null if Tauri isn't reachable (e.g.,
/// running under Vitest), which lets unit tests render the broken-tab UI
/// without invoking close_tab.
function labelFromWebview(): string | null {
  try {
    return getCurrentWebview().label;
  } catch {
    return null;
  }
}
