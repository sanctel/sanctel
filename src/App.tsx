import { useEffect } from "react";
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

  useEffect(() => {
    hydrate();
  }, [hydrate]);

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

  return (
    <div className="app">
      <Sidebar />
      <ContentArea />
    </div>
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
