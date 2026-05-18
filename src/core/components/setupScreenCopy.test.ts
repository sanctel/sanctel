import { describe, expect, it } from "vitest";
import { setupScreenCopy } from "./setupScreenCopy";

describe("setupScreenCopy", () => {
  it("returns tmux heading and install commands", () => {
    const copy = setupScreenCopy("tmux");
    expect(copy.heading).toBe("Sanctel needs tmux");
    expect(copy.install).toContain("brew install tmux");
    expect(copy.install).toContain("sudo apt install tmux");
  });

  it("returns tmux copy regardless of backend argument", () => {
    expect(setupScreenCopy(undefined).heading).toBe("Sanctel needs tmux");
    expect(setupScreenCopy("garbage").heading).toBe("Sanctel needs tmux");
    expect(setupScreenCopy("").heading).toBe("Sanctel needs tmux");
  });
});
