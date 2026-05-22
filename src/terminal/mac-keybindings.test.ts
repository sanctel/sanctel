import { describe, expect, it } from "vitest";

import {
  MAC_TERMINAL_KEYBINDINGS,
  lookupMacTerminalKeybinding,
  matches,
} from "./mac-keybindings";

interface FakeKeyboardEvent {
  type: string;
  key: string;
  metaKey: boolean;
  altKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
}

function evt(opts: Partial<FakeKeyboardEvent>): FakeKeyboardEvent {
  return {
    type: "keydown",
    key: "",
    metaKey: false,
    altKey: false,
    ctrlKey: false,
    shiftKey: false,
    ...opts,
  };
}

describe("matches", () => {
  it("requires exactly the binding's modifiers", () => {
    const cmdBackspace = MAC_TERMINAL_KEYBINDINGS.find(
      (kb) => kb.key === "Backspace" && kb.mods.length === 1 && kb.mods[0] === "cmd",
    )!;
    expect(matches(evt({ key: "Backspace", metaKey: true }) as KeyboardEvent, cmdBackspace)).toBe(true);
    // Adding shift disqualifies — user might want vanilla backspace behavior with the chord.
    expect(matches(evt({ key: "Backspace", metaKey: true, shiftKey: true }) as KeyboardEvent, cmdBackspace)).toBe(false);
    // Wrong key
    expect(matches(evt({ key: "Delete", metaKey: true }) as KeyboardEvent, cmdBackspace)).toBe(false);
    // No modifiers
    expect(matches(evt({ key: "Backspace" }) as KeyboardEvent, cmdBackspace)).toBe(false);
  });

  it("requires the absence of unwanted modifiers", () => {
    const altBackspace = MAC_TERMINAL_KEYBINDINGS.find(
      (kb) => kb.key === "Backspace" && kb.mods.length === 1 && kb.mods[0] === "alt",
    )!;
    // alt+backspace alone matches
    expect(matches(evt({ key: "Backspace", altKey: true }) as KeyboardEvent, altBackspace)).toBe(true);
    // alt+cmd+backspace must NOT match alt-only binding (cmd-only binding handles the alt+cmd case differently)
    expect(matches(evt({ key: "Backspace", altKey: true, metaKey: true }) as KeyboardEvent, altBackspace)).toBe(false);
  });
});

describe("lookupMacTerminalKeybinding", () => {
  it("ignores non-keydown events", () => {
    expect(
      lookupMacTerminalKeybinding(
        evt({ type: "keyup", key: "Backspace", metaKey: true }) as KeyboardEvent,
      ),
    ).toBeNull();
    expect(
      lookupMacTerminalKeybinding(
        evt({ type: "keypress", key: "Backspace", metaKey: true }) as KeyboardEvent,
      ),
    ).toBeNull();
  });

  it("returns null for unbound combos", () => {
    expect(
      lookupMacTerminalKeybinding(evt({ key: "a" }) as KeyboardEvent),
    ).toBeNull();
    expect(
      lookupMacTerminalKeybinding(
        evt({ key: "F", metaKey: true }) as KeyboardEvent,
      ),
    ).toBeNull();
  });

  it("maps cmd+backspace to ^U", () => {
    expect(
      lookupMacTerminalKeybinding(
        evt({ key: "Backspace", metaKey: true }) as KeyboardEvent,
      ),
    ).toEqual([0x15]);
  });

  it("maps alt+backspace to ^W", () => {
    expect(
      lookupMacTerminalKeybinding(
        evt({ key: "Backspace", altKey: true }) as KeyboardEvent,
      ),
    ).toEqual([0x17]);
  });

  it("maps cmd+left to ^A and cmd+right to ^E", () => {
    expect(
      lookupMacTerminalKeybinding(
        evt({ key: "ArrowLeft", metaKey: true }) as KeyboardEvent,
      ),
    ).toEqual([0x01]);
    expect(
      lookupMacTerminalKeybinding(
        evt({ key: "ArrowRight", metaKey: true }) as KeyboardEvent,
      ),
    ).toEqual([0x05]);
  });

  it("maps alt+left to ESC b and alt+right to ESC f", () => {
    expect(
      lookupMacTerminalKeybinding(
        evt({ key: "ArrowLeft", altKey: true }) as KeyboardEvent,
      ),
    ).toEqual([0x1b, 0x62]); // ESC + 'b'
    expect(
      lookupMacTerminalKeybinding(
        evt({ key: "ArrowRight", altKey: true }) as KeyboardEvent,
      ),
    ).toEqual([0x1b, 0x66]); // ESC + 'f'
  });

  it("maps alt+delete to ESC d (kill word right)", () => {
    expect(
      lookupMacTerminalKeybinding(
        evt({ key: "Delete", altKey: true }) as KeyboardEvent,
      ),
    ).toEqual([0x1b, 0x64]); // ESC + 'd'
  });

  it("maps alt+up/down to the ctrl-up/down CSI sequences", () => {
    expect(
      lookupMacTerminalKeybinding(
        evt({ key: "ArrowUp", altKey: true }) as KeyboardEvent,
      ),
    ).toEqual([0x1b, 0x5b, 0x31, 0x3b, 0x35, 0x41]); // ESC [ 1 ; 5 A
    expect(
      lookupMacTerminalKeybinding(
        evt({ key: "ArrowDown", altKey: true }) as KeyboardEvent,
      ),
    ).toEqual([0x1b, 0x5b, 0x31, 0x3b, 0x35, 0x42]); // ESC [ 1 ; 5 B
  });

  it("does not match cmd+shift+backspace (user might want different behavior)", () => {
    expect(
      lookupMacTerminalKeybinding(
        evt({ key: "Backspace", metaKey: true, shiftKey: true }) as KeyboardEvent,
      ),
    ).toBeNull();
  });
});
