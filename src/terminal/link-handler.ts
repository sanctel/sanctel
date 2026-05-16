// Link handler injected into terminal-runtime via `mount(..., {linkHandler})`.
//
// The runtime stays decoupled from the rest of the app: it knows xterm and
// the Rust terminal commands, nothing else. terminal.html and chat.html each
// wire a concrete `openBrowserTab` that knows how to ask the React shell to
// open a browser tab in the current Space — typically by emitting a Tauri
// event the React app listens for.

export interface CreateLinkHandlerOptions {
  openBrowserTab: (url: string) => Promise<void> | void;
}

export function createLinkHandler(
  opts: CreateLinkHandlerOptions,
): (event: MouseEvent, url: string) => void {
  return (_event, url) => {
    try {
      const result = opts.openBrowserTab(url);
      if (result && typeof (result as Promise<void>).catch === "function") {
        (result as Promise<void>).catch((err) =>
          console.error("openBrowserTab failed", err),
        );
      }
    } catch (err) {
      console.error("openBrowserTab threw", err);
    }
  };
}
