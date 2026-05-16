import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";

import Sidebar from "./core/components/Sidebar";
import ContentArea from "./core/components/ContentArea";
import { useTabStore } from "./core/store/tabStore";
import "./styles/app.css";

export default function App() {
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

  return (
    <div className="app">
      <Sidebar />
      <ContentArea />
    </div>
  );
}
