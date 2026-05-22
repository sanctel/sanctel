import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import Sidebar from "./core/components/Sidebar";
import ContentArea from "./core/components/ContentArea";
import TmuxSetupScreen from "./core/components/TmuxSetupScreen";
import { useTabStore } from "./core/store/tabStore";
import { useTmuxStatus } from "./core/store/tmuxStatusStore";
import { SqlPersistence } from "./core/persistence/sql-persistence";
import "./styles/app.css";

export default function App() {
  const hydrate = useTmuxStatus((s) => s.hydrate);
  const loaded = useTmuxStatus((s) => s.loaded);
  const tmuxAvailable = useTmuxStatus((s) => s.status.available);
  const [showHooksPrompt, setShowHooksPrompt] = useState(false);

  useEffect(() => {
    hydrate();
  }, [hydrate]);

  useEffect(() => {
    let cancelled = false;
    invoke<HooksStatusReport>("hooks_status")
      .then((status) => {
        if (!cancelled && shouldShowHooksInstallPrompt(status)) {
          setShowHooksPrompt(true);
        }
      })
      .catch((e) => console.error("hooks_status failed", e));
    return () => {
      cancelled = true;
    };
  }, []);

  // Hydrate the tab store from SQLite on launch. The store reads the
  // persisted profiles/spaces/tabs, paints the sidebar, then replays
  // `create_tab` per row so each Tauri webview reattaches to its
  // server-held identity (tmux window / agent session).
  useEffect(() => {
    const persistence = new SqlPersistence();
    useTabStore.getState().hydrate(persistence).catch((e) => {
      console.error("tabStore hydrate failed", e);
    });
  }, []);

  // Terminal/chat webviews emit `sanctel://open-url` when a user clicks a URL
  // detected by xterm's web-links addon. Route it through the same `newTab`
  // path as the sidebar "+" button so the new browser tab inherits the
  // active Space's profile.
  useEffect(() => {
    const unlistenP = listen<{ url: string }>("sanctel://open-url", (e) => {
      useTabStore.getState().newTab("browser", e.payload.url).catch((err) => {
        console.error("open-url newTab failed", err);
      });
    });
    return () => {
      unlistenP.then((u) => u()).catch(() => {});
    };
  }, []);

  // Terminal/chat webviews emit `sanctel://close-tab` when their broken-tab
  // UI asks to remove itself. Route through Core's close lifecycle so state,
  // persistence, and best-effort backend cleanup stay in one path.
  useEffect(() => {
    return listenForTabLifecycleClose("sanctel://close-tab");
  }, []);

  // Rust emits `sanctel://tab-exited` when a terminal-like Tab's backing
  // TmuxSession is confirmed gone. Core still owns Tab removal.
  useEffect(() => {
    return listenForTabLifecycleClose("sanctel://tab-exited");
  }, []);

  // While the probe result is still in flight, show nothing — first paint
  // is fast and a transient empty screen beats flashing the setup UI then
  // hiding it.
  if (!loaded) return <div className="app" />;

  if (!tmuxAvailable) {
    return <TmuxSetupScreen />;
  }

  const app = (
    <div className="app">
      <Sidebar />
      <ContentArea />
    </div>
  );
  if (!showHooksPrompt) return app;

  return (
    <>
      {app}
      <HooksInstallPrompt onDone={() => setShowHooksPrompt(false)} />
    </>
  );
}

interface HookFileStatus {
  agent: string;
  path: string;
  installed: boolean;
  error: string | null;
}

export interface HooksStatusReport {
  agents: HookFileStatus[];
  anyInstalled: boolean;
  allInstalled: boolean;
  promptDeclined: boolean;
  promptSkipped: boolean;
}

export function shouldShowHooksInstallPrompt(status: HooksStatusReport): boolean {
  return !status.promptSkipped && !status.promptDeclined && !status.anyInstalled;
}

function HooksInstallPrompt({ onDone }: { onDone: () => void }) {
  const [busy, setBusy] = useState(false);
  const [capturedTabId] = useState(
    () => useTabStore.getState().activeTab()?.id ?? null,
  );
  const activeTabId = useTabStore(
    (state) => state.activeSpace()?.activeTabId ?? null,
  );

  useEffect(() => {
    hideTabWebviewsForHooksPrompt().catch((e) =>
      console.error("hide hook prompt webviews failed", e),
    );
  }, [activeTabId]);

  const done = () => {
    onDone();
    window.setTimeout(() => {
      restoreTabWebviewAfterHooksPrompt(capturedTabId).catch((e) =>
        console.error("restore hook prompt webview failed", e),
      );
    }, 0);
  };

  const install = async () => {
    setBusy(true);
    try {
      await invoke("install_hooks");
      done();
    } catch (e) {
      console.error("install_hooks failed", e);
      setBusy(false);
    }
  };

  const decline = async () => {
    setBusy(true);
    try {
      await invoke("decline_hooks_install");
    } catch (e) {
      console.error("decline_hooks_install failed", e);
    } finally {
      done();
    }
  };

  return (
    <div className="hooks-consent-backdrop" role="dialog" aria-modal="true">
      <div className="hooks-consent">
        <h2>Install agent hooks?</h2>
        <p>
          Sanctel can add SessionStart hooks for Claude, Codex, and Gemini so
          agent sessions can be captured for restore.
        </p>
        <div className="hooks-consent-actions">
          <button type="button" onClick={decline} disabled={busy}>
            Not now
          </button>
          <button type="button" onClick={install} disabled={busy}>
            Install
          </button>
        </div>
      </div>
    </div>
  );
}

export async function hideTabWebviewsForHooksPrompt(): Promise<void> {
  await invoke("hide_all");
}

export async function restoreTabWebviewAfterHooksPrompt(
  capturedTabId: string | null,
): Promise<void> {
  const state = useTabStore.getState();
  const tabId = tabIdToShowAfterHooksPrompt(capturedTabId, {
    activeSpaceId: state.activeSpaceId,
    tabs: state.tabs,
  });

  if (tabId) {
    await invoke("show_tab", { id: tabId });
  } else {
    await invoke("hide_all");
  }
}

export function tabIdToShowAfterHooksPrompt(
  capturedTabId: string | null,
  state: Pick<
    ReturnType<typeof useTabStore.getState>,
    "activeSpaceId" | "tabs"
  >,
): string | null {
  if (capturedTabId && state.tabs.some((tab) => tab.id === capturedTabId)) {
    return capturedTabId;
  }

  return (
    state.tabs.find((tab) => tab.spaceId === state.activeSpaceId)?.id ?? null
  );
}

type TabLifecycleCloseEventName =
  | "sanctel://close-tab"
  | "sanctel://tab-exited";

export function listenForTabLifecycleClose(
  eventName: TabLifecycleCloseEventName,
): () => void {
  const logContext =
    eventName === "sanctel://close-tab" ? "close-tab" : "tab-exited";

  const unlistenP = listen<unknown>(eventName, (e) => {
    const id = closeTabIdFromPayload(e.payload);
    if (!id) return;

    const state = useTabStore.getState();
    if (!state.tabs.some((t) => t.id === id)) return;

    state.closeTab(id).catch((err) => {
      console.error(`${logContext} closeTab failed`, err);
    });
  });
  return () => {
    unlistenP.then((u) => u()).catch(() => {});
  };
}

function closeTabIdFromPayload(payload: unknown): string | null {
  if (!payload || typeof payload !== "object") return null;
  if (!("id" in payload)) return null;

  const id = payload.id;
  if (typeof id !== "string" || !id) return null;

  return id;
}
