import { describe, expect, it } from "vitest";

import { allocateWindowName } from "./window-name-allocator";

describe("allocateWindowName", () => {
  it("returns term-1 for an empty list", () => {
    expect(allocateWindowName([])).toBe("term-1");
  });

  it("advances past a sequential list", () => {
    expect(allocateWindowName(["term-1", "term-2"])).toBe("term-3");
  });

  it("tolerates gaps by picking max + 1", () => {
    expect(allocateWindowName(["term-2", "term-5"])).toBe("term-6");
  });

  it("ignores non-numeric names", () => {
    expect(allocateWindowName(["bash", "build-watcher"])).toBe("term-1");
  });

  it("ignores mixed non-numeric and term-N names", () => {
    expect(allocateWindowName(["term-3", "bash", "term-1", "deploy"])).toBe(
      "term-4",
    );
  });

  it("ignores malformed term-N entries (term-, term-abc)", () => {
    expect(allocateWindowName(["term-", "term-abc", "term-2"])).toBe("term-3");
  });
});
