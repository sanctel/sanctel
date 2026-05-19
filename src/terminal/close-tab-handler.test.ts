import { describe, expect, it, vi } from "vitest";

import { createCloseTabHandler } from "./close-tab-handler";

describe("createCloseTabHandler", () => {
  it("requests Core close for the current Tab id", () => {
    const requestCloseTab = vi.fn();
    const handler = createCloseTabHandler({
      currentTabId: () => "tab-123",
      requestCloseTab,
    });

    handler();

    expect(requestCloseTab).toHaveBeenCalledTimes(1);
    expect(requestCloseTab).toHaveBeenCalledWith("tab-123");
  });

  it("does not request Core close without a current Tab id", () => {
    const requestCloseTab = vi.fn();

    createCloseTabHandler({
      currentTabId: () => null,
      requestCloseTab,
    })();
    createCloseTabHandler({
      currentTabId: () => "",
      requestCloseTab,
    })();

    expect(requestCloseTab).not.toHaveBeenCalled();
  });
});
