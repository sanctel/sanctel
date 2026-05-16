import { describe, expect, it, vi } from "vitest";

import { createLinkHandler } from "./link-handler";

describe("createLinkHandler", () => {
  it("invokes openBrowserTab with the clicked URL", () => {
    const openBrowserTab = vi.fn();
    const handler = createLinkHandler({ openBrowserTab });

    handler({} as MouseEvent, "https://example.com/path?q=1");

    expect(openBrowserTab).toHaveBeenCalledTimes(1);
    expect(openBrowserTab).toHaveBeenCalledWith("https://example.com/path?q=1");
  });

  it("logs but does not throw when openBrowserTab rejects", async () => {
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const openBrowserTab = vi.fn(() => Promise.reject(new Error("boom")));
    const handler = createLinkHandler({ openBrowserTab });

    handler({} as MouseEvent, "https://example.com");
    // Let the rejected promise settle through the catch.
    await Promise.resolve();
    await Promise.resolve();

    expect(errSpy).toHaveBeenCalled();
    errSpy.mockRestore();
  });
});
