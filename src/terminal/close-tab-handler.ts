export interface CreateCloseTabHandlerOptions {
  currentTabId: () => string | null;
  requestCloseTab: (id: string) => Promise<void> | void;
}

export function createCloseTabHandler(
  opts: CreateCloseTabHandlerOptions,
): () => void {
  return () => {
    const id = opts.currentTabId();
    if (!id) return;

    try {
      const result = opts.requestCloseTab(id);
      if (result instanceof Promise) {
        result.catch((err) => console.error("requestCloseTab failed", err));
      }
    } catch (err) {
      console.error("requestCloseTab failed", err);
    }
  };
}
