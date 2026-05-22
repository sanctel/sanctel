// macOS terminal keybinding translation table.
//
// xterm.js doesn't translate the macOS chord conventions that users coming
// from iTerm2 / Terminal.app expect (cmd+backspace → kill-line, cmd+arrow
// → line nav, alt+arrow → word nav). WKWebView doesn't surface them as
// useful keystrokes either, so without this layer typing those chords does
// nothing.
//
// The table is the iTerm2 "Natural Text Editing" preset, cross-checked
// against VS Code's workbench-terminal keybinding contribution. Both
// agree on every binding except alt+backspace; we follow iTerm2 there.
//
//   - iTerm2: plists/PresetKeyMappings.plist key "Natural Text Editing"
//   - VS Code: src/vs/workbench/contrib/terminalContrib/sendSequence/
//              browser/terminal.sendSequence.contribution.ts
//              (lines 185–253 at commit 1fdf66f8)

export interface MacTerminalKeybinding {
  /** KeyboardEvent.key value (e.g. "Backspace", "ArrowLeft", "Delete"). */
  key: string;
  /** Modifier set that MUST be active. Anything not in the set must be inactive. */
  mods: ReadonlyArray<"cmd" | "alt" | "ctrl" | "shift">;
  /** Bytes to write to the PTY when this binding matches. */
  bytes: ReadonlyArray<number>;
  /** Human-readable label for debugging / future settings UI. */
  description: string;
}

const CTRL_A = 0x01;
const CTRL_E = 0x05;
const CTRL_U = 0x15;
const ESC = 0x1b;
const DEL = 0x7f;

// ESC <letter> is the readline meta-prefix encoding. ESC b = meta-b (word
// back), ESC f = meta-f (word forward), ESC d = meta-d (kill word right).
const META = (letter: string): number[] => [ESC, letter.charCodeAt(0)];

export const MAC_TERMINAL_KEYBINDINGS: ReadonlyArray<MacTerminalKeybinding> = [
  // Line navigation
  {
    key: "ArrowLeft",
    mods: ["cmd"],
    bytes: [CTRL_A],
    description: "Move cursor to beginning of line",
  },
  {
    key: "ArrowRight",
    mods: ["cmd"],
    bytes: [CTRL_E],
    description: "Move cursor to end of line",
  },
  // Word navigation
  {
    key: "ArrowLeft",
    mods: ["alt"],
    bytes: META("b"),
    description: "Move cursor one word back",
  },
  {
    key: "ArrowRight",
    mods: ["alt"],
    bytes: META("f"),
    description: "Move cursor one word forward",
  },
  // Line / word kill
  {
    key: "Backspace",
    mods: ["cmd"],
    bytes: [CTRL_U],
    description: "Kill from cursor to start of line",
  },
  {
    key: "Backspace",
    mods: ["alt"],
    // iTerm2's "Natural Text Editing" uses ESC + DEL — readline's
    // `backward-kill-word`. More granular than ^W (which is
    // `unix-word-rubout` and uses whitespace-only boundaries). Users
    // expecting iTerm2 muscle memory get the right thing here.
    bytes: [ESC, DEL],
    description: "Kill previous word (readline backward-kill-word)",
  },
  {
    key: "Delete",
    mods: ["alt"],
    bytes: META("d"),
    description: "Kill next word",
  },
];

/**
 * Check whether `ev` matches `kb` exactly (key + the precise set of mods).
 * Extra modifiers on `ev` not in `kb.mods` are disqualifying so users can
 * still get vanilla behavior with e.g. `cmd+shift+backspace`.
 */
export function matches(ev: KeyboardEvent, kb: MacTerminalKeybinding): boolean {
  if (ev.key !== kb.key) return false;
  const want = (mod: MacTerminalKeybinding["mods"][number]) =>
    kb.mods.includes(mod);
  if (ev.metaKey !== want("cmd")) return false;
  if (ev.altKey !== want("alt")) return false;
  if (ev.ctrlKey !== want("ctrl")) return false;
  if (ev.shiftKey !== want("shift")) return false;
  return true;
}

/**
 * Try to handle a keyboard event through the macOS terminal keybinding
 * table. Returns the bytes to send to the PTY if a binding matched, or
 * `null` to indicate xterm.js should process the event normally.
 *
 * Only `keydown` events are considered. `keypress` / `keyup` always pass
 * through.
 */
export function lookupMacTerminalKeybinding(
  ev: KeyboardEvent,
): ReadonlyArray<number> | null {
  if (ev.type !== "keydown") return null;
  for (const kb of MAC_TERMINAL_KEYBINDINGS) {
    if (matches(ev, kb)) return kb.bytes;
  }
  return null;
}
