import { describe, expect, it } from "vitest";
import { setupScreenCopy } from "./setupScreenCopy";

describe("setupScreenCopy", () => {
  // Issue #27 acceptance criterion: when SANCTEL_BACKEND=zellij is active
  // and a zellij setup step fails, the setup screen shows zellij-flavoured
  // copy with zellij install instructions — not tmux's.
  it("returns zellij heading and install commands when backend is zellij", () => {
    const copy = setupScreenCopy("zellij");
    expect(copy.heading.toLowerCase()).toContain("zellij");
    expect(copy.intro.toLowerCase()).toContain("zellij");
    expect(copy.install).toContain("brew install zellij");
    expect(copy.install).not.toContain("brew install tmux");
  });

  // The default-stays-tmux invariant: with SANCTEL_BACKEND unset, the
  // setup screen must keep showing the existing tmux copy unchanged.
  it("returns tmux heading and install commands when backend is tmux", () => {
    const copy = setupScreenCopy("tmux");
    expect(copy.heading).toBe("Sanctel needs tmux");
    expect(copy.install).toContain("brew install tmux");
    expect(copy.install).toContain("sudo apt install tmux");
    expect(copy.install).not.toContain("zellij");
  });

  // Defensive fallback per the issue's acceptance list: if the backend
  // value is missing or malformed coming off the wire, the frontend must
  // render the existing tmux copy rather than going blank.
  it("falls back to tmux copy when backend is missing or unknown", () => {
    expect(setupScreenCopy(undefined).heading).toBe("Sanctel needs tmux");
    expect(setupScreenCopy("garbage").heading).toBe("Sanctel needs tmux");
    expect(setupScreenCopy("").heading).toBe("Sanctel needs tmux");
  });
});
