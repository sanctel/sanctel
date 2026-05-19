// Shared terminal-runtime module imported by both terminal.html and
// chat.html. Slice 2 wires xterm.js end-to-end against the Rust
// `terminal_attach` / `terminal_write` / `terminal_resize` commands; output
// flows over a Tauri Channel as raw bytes (no UTF-8 transcoding on the data
// path — see docs/design/terminal-runtime.md §"IPC contract").
//
// Slice 3 adds the polish layer:
//   - ResizeObserver on the container → fit.fit() → terminal_resize.
//   - Optional clipboard glue (Cmd/Ctrl+C copy, Cmd/Ctrl+V paste).
//   - Optional web-links addon firing an injected link handler so the
//     runtime stays decoupled from the rest of the app.
//
// Per-tab identity (worktreeId, windowName, initialCommand) is server-held
// in the Rust TabRecord, so mount() takes no arguments beyond the container.
// The webview's label IS the tabId.

import { Channel, invoke } from "@tauri-apps/api/core";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { parseAttachError } from "./attach-error";

export { parseAttachError } from "./attach-error";
export type { ParsedAttachError } from "./attach-error";

import { type ClipboardApi, installClipboard } from "./clipboard";

export interface MountOptions {
  /** Wired by the host page to call `create_tab` for the URL. When omitted,
   * the web-links addon is not loaded and URLs render as plain text. */
  linkHandler?: (event: MouseEvent, url: string) => void;
  /** Clipboard plugin bridge. Omit to disable copy/paste shortcuts. */
  clipboard?: ClipboardApi;
  /** Wired by the host page to ask Core to close this Tab. */
  closeTabHandler?: () => void;
}

export interface MountedTerminal {
  term: Terminal;
  dispose: () => void;
}

export function mount(
  container: HTMLElement,
  options: MountOptions = {},
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

  if (options.linkHandler) {
    term.loadAddon(new WebLinksAddon(options.linkHandler));
  }

  term.open(container);

  // WebGL renderer for throughput. Falls back to canvas if WebGL is not
  // available (older WebKit). On context loss (can happen when WKWebView
  // releases GPU resources on macOS), dispose the addon so xterm falls
  // back to canvas rendering for the rest of the tab's lifetime.
  try {
    const webgl = new WebglAddon();
    webgl.onContextLoss(() => {
      webgl.dispose();
    });
    term.loadAddon(webgl);
  } catch {
    // No WebGL — xterm will use its canvas renderer.
  }

  if (options.clipboard) {
    installClipboard(term, { clipboard: options.clipboard });
  }

  // Output channel: raw bytes from the PTY → xterm.write. xterm accepts
  // Uint8Array directly; Tauri's Vec<u8> arrives as a number[] over the
  // wire, so coerce to Uint8Array. No UTF-8 decoding here — xterm handles
  // it correctly (including partial multi-byte chunks).
  //
  // The channel is installed BEFORE terminal_attach is invoked so the
  // initial screen dump (for chat tabs reattaching to a surviving tmux
  // session) is never missed.
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

  // Forward xterm size changes (driven by fit.fit() below) to tmux via Rust.
  // term.onResize fires whenever the cell grid actually changes — duplicate
  // ResizeObserver callbacks at the same dimensions don't cost an IPC.
  // Filter degenerate resizes before they reach tmux. Anything with rows < 4
  // or cols < 20 is almost certainly a transient layout state, not a real
  // user intent — and forwarding it would call `tmux resize-window` with
  // tiny dimensions, which truncates scrollback non-recoverably. The
  // upstream guard in ContentArea blocks the most common cause (near-zero
  // content rect during React layout commits); this is belt-and-braces.
  const MIN_COLS = 20;
  const MIN_ROWS = 4;
  const onResizeDisposable = term.onResize(({ cols, rows }) => {
    if (cols < MIN_COLS || rows < MIN_ROWS) return;
    invoke("terminal_resize", { cols, rows }).catch((e) =>
      console.error("terminal_resize failed", e),
    );
  });

  // Reflow on container size changes. fit.fit() re-measures the container,
  // computes cell dimensions, and resizes the terminal (which fires the
  // term.onResize above).
  //
  // CRITICAL: filter degenerate container sizes BEFORE fit.fit() runs.
  // When the webview is moved off-screen (sanctel's hide_webview), WebKit
  // collapses the body layout, which fires a ResizeObserver tick reporting
  // a near-zero container. `fit.fit()` would compute rows=1, call
  // term.resize(127, 1), and xterm would internally truncate the buffer —
  // the term.onResize callback fires AFTER the resize is committed, so
  // skipping the IPC there is too late to undo the damage. The guard has
  // to be here, before fit runs.
  const MIN_CONTAINER_PX = 40;
  const resizeObserver = new ResizeObserver(() => {
    const r = container.getBoundingClientRect();
    if (r.width < MIN_CONTAINER_PX || r.height < MIN_CONTAINER_PX) return;
    try {
      fit.fit();
    } catch {
      // fit can throw if xterm's renderer measurements aren't ready yet.
    }
  });
  resizeObserver.observe(container);

  // Defer terminal_attach until the container has a real (non-trivial)
  // size. During hydrate, sanctel creates webviews while content_rect is
  // still (0,0,0,0) (React hasn't reported its layout yet), so the
  // container is clamped to 1×1 and `fit.fit()` would yield 0/1 cols.
  // Attaching at that size tells tmux the grid is degenerate; wait for
  // the show_webview pass to apply real dimensions before talking to Rust.
  waitForRealContainerSize(container, fit, term).then(() => {
    invoke("terminal_attach", {
      cols: term.cols,
      rows: term.rows,
      onOutput,
    }).catch((e) => {
      const parsed = parseAttachError(e);
      if (parsed.kind === "worktree-missing") {
        renderBrokenTab(container, parsed.path, options.closeTabHandler);
        onDataDisposable.dispose();
        term.dispose();
        return;
      }
      // Other failures (tmux missing, spawn errors) — render inline in the
      // terminal so the user sees what went wrong. tmux-missing should not
      // reach here in practice because React gates create_tab on the startup
      // probe, but defensive surfacing is cheap.
      term.write(
        `\r\n\x1b[31mterminal_attach failed: ${parsed.message}\x1b[0m\r\n`,
      );
    });
  });

  return {
    term,
    dispose: () => {
      resizeObserver.disconnect();
      onResizeDisposable.dispose();
      onDataDisposable.dispose();
      term.dispose();
    },
  };
}

// Inline broken-tab panel for the worktree-missing case. Replaces the xterm
// canvas inside the container, leaves the sidebar entry untouched. "Recreate
// from main" is a v0.3.x follow-up — for now the button is wired to a no-op
// handler so the UI is complete and the click target is real.
function renderBrokenTab(
  container: HTMLElement,
  path: string,
  closeTabHandler?: () => void,
): void {
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
    closeTabHandler?.();
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

// Resolve when the container has a layout size large enough for a useful
// xterm grid (≥ one cell each direction). Runs `fit.fit()` on every
// candidate size change so `term.cols` / `term.rows` reflect the final
// dimensions by the time the caller invokes `terminal_attach`. Returns
// immediately if the container is already sized at call time.
//
// We observe via ResizeObserver rather than polling so the resolution
// fires on the same animation frame as the host's layout commit.
function waitForRealContainerSize(
  container: HTMLElement,
  fit: FitAddon,
  term: Terminal,
): Promise<void> {
  const tryFit = (): boolean => {
    const { width, height } = container.getBoundingClientRect();
    if (width < 2 || height < 2) return false;
    try {
      fit.fit();
    } catch {
      return false;
    }
    return term.cols > 1 && term.rows > 1;
  };

  if (tryFit()) return Promise.resolve();

  return new Promise((resolve) => {
    const ro = new ResizeObserver(() => {
      if (tryFit()) {
        ro.disconnect();
        resolve();
      }
    });
    ro.observe(container);
  });
}
