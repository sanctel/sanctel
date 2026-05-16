import { describe, expect, it, vi } from "vitest";

import { installClipboard } from "./clipboard";

// Plain duck-typed KeyboardEvent — node's default test env has no DOM
// globals, and the clipboard module only reads a handful of fields. We
// don't run the events through any real DOM dispatch, so a struct is
// enough.
interface FakeKeyEvent {
  type: string;
  key: string;
  metaKey?: boolean;
  ctrlKey?: boolean;
  shiftKey?: boolean;
  altKey?: boolean;
}

function keydown(key: string, mods: Partial<FakeKeyEvent> = {}): FakeKeyEvent {
  return { type: "keydown", key, ...mods };
}

// Minimal xterm.js stand-in. We only care about the slice of the Terminal
// API that the clipboard module actually touches.
function makeFakeTerm(initialSelection = "") {
  let handler: ((e: FakeKeyEvent) => boolean) | null = null;
  return {
    selection: initialSelection,
    pasted: [] as string[],
    hasSelection() {
      return this.selection.length > 0;
    },
    getSelection() {
      return this.selection;
    },
    paste(text: string) {
      this.pasted.push(text);
    },
    attachCustomKeyEventHandler(h: (e: FakeKeyEvent) => boolean) {
      handler = h;
    },
    fire(e: FakeKeyEvent) {
      if (!handler) throw new Error("no handler attached");
      return handler(e);
    },
  };
}

describe("installClipboard", () => {
  it("copies selection on Cmd+C and swallows the event", () => {
    const term = makeFakeTerm("hello world");
    const writeText = vi.fn(async () => {});
    installClipboard(term as never, {
      clipboard: { readText: async () => "", writeText },
    });

    const result = term.fire(keydown("c", { metaKey: true }));

    expect(result).toBe(false);
    expect(writeText).toHaveBeenCalledWith("hello world");
  });

  it("copies selection on Ctrl+C and swallows the event", () => {
    const term = makeFakeTerm("hello");
    const writeText = vi.fn(async () => {});
    installClipboard(term as never, {
      clipboard: { readText: async () => "", writeText },
    });

    const result = term.fire(keydown("c", { ctrlKey: true }));

    expect(result).toBe(false);
    expect(writeText).toHaveBeenCalledWith("hello");
  });

  it("lets Ctrl+C through as SIGINT when there is no selection", () => {
    const term = makeFakeTerm("");
    const writeText = vi.fn(async () => {});
    installClipboard(term as never, {
      clipboard: { readText: async () => "", writeText },
    });

    const result = term.fire(keydown("c", { ctrlKey: true }));

    expect(result).toBe(true);
    expect(writeText).not.toHaveBeenCalled();
  });

  it("pastes clipboard text via term.paste on Cmd+V", async () => {
    const term = makeFakeTerm();
    const readText = vi.fn(async () => "pasted-bytes");
    installClipboard(term as never, {
      clipboard: { readText, writeText: async () => {} },
    });

    const result = term.fire(keydown("v", { metaKey: true }));
    // Resolve the in-flight readText promise.
    await Promise.resolve();
    await Promise.resolve();

    expect(result).toBe(false);
    expect(readText).toHaveBeenCalled();
    expect(term.pasted).toEqual(["pasted-bytes"]);
  });

  it("pastes clipboard text via term.paste on Ctrl+V", async () => {
    const term = makeFakeTerm();
    const readText = vi.fn(async () => "abc");
    installClipboard(term as never, {
      clipboard: { readText, writeText: async () => {} },
    });

    const result = term.fire(keydown("v", { ctrlKey: true }));
    await Promise.resolve();
    await Promise.resolve();

    expect(result).toBe(false);
    expect(term.pasted).toEqual(["abc"]);
  });

  it("ignores keyup events (handler only acts on keydown)", () => {
    const term = makeFakeTerm("hello");
    const writeText = vi.fn(async () => {});
    installClipboard(term as never, {
      clipboard: { readText: async () => "", writeText },
    });

    const result = term.fire({
      type: "keyup",
      key: "c",
      metaKey: true,
    });

    expect(result).toBe(true);
    expect(writeText).not.toHaveBeenCalled();
  });

  it("ignores Cmd+Shift+C / other modifier combos so xterm sees them as-is", () => {
    const term = makeFakeTerm("hello");
    const writeText = vi.fn(async () => {});
    installClipboard(term as never, {
      clipboard: { readText: async () => "", writeText },
    });

    const result = term.fire(
      keydown("c", { metaKey: true, shiftKey: true }),
    );

    expect(result).toBe(true);
    expect(writeText).not.toHaveBeenCalled();
  });
});
