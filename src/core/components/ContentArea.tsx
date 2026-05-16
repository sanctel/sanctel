import { useEffect, useRef } from "react";
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
  const activeTab = useTabStore((s) => s.activeTab());

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
