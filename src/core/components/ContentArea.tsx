import { useEffect, useMemo, useRef } from "react";
import { useTabStore } from "../store/tabStore";

/**
 * The content area is where Tauri webviews are positioned by Rust.
 * React renders an empty div; Rust positions webviews to overlay it.
 *
 * On every layout change (resize, sidebar toggle), measure the rect and
 * tell Rust where to put the active webview.
 */
export default function ContentArea() {
  const ref = useRef<HTMLDivElement>(null);
  const setContentRect = useTabStore((s) => s.setContentRect);
  // Select stable slices; compute the derived activeTab here. Calling
  // `s.activeTab()` from a zustand selector returns a fresh `find()` result
  // on every snapshot read, which trips React's useSyncExternalStore
  // caching check on some boot orderings.
  const tabs = useTabStore((s) => s.tabs);
  const spaces = useTabStore((s) => s.spaces);
  const activeSpaceId = useTabStore((s) => s.activeSpaceId);
  const activeTab = useMemo(() => {
    const sp = spaces.find((s) => s.id === activeSpaceId);
    return sp?.activeTabId
      ? tabs.find((t) => t.id === sp.activeTabId)
      : undefined;
  }, [tabs, spaces, activeSpaceId]);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    const report = () => {
      const r = el.getBoundingClientRect();
      setContentRect({ x: r.x, y: r.y, w: r.width, h: r.height });
    };
    report();

    const ro = new ResizeObserver(report);
    ro.observe(el);
    window.addEventListener("resize", report);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", report);
    };
  }, [setContentRect, activeTab?.id]);

  return (
    <main ref={ref} className="content-area">
      {!activeTab && (
        <div className="empty-state">
          <p>No tab open. Create one from the sidebar.</p>
        </div>
      )}
    </main>
  );
}
