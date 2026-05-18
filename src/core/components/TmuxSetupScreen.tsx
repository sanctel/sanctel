import { useTmuxStatus } from "../store/tmuxStatusStore";
import { setupScreenCopy } from "./setupScreenCopy";

// Shown when the Rust-side startup probe could not find a usable backend.
// Issue #8: replaces tab-by-tab failures with one clear setup screen at app
// launch. The copy and install instructions follow which backend actually
// failed (`status.backend`) — a zellij failure must not tell the user to
// install tmux. React stays in this state until the user installs the
// right backend and restarts sanctel — the probe runs once per launch.
export default function TmuxSetupScreen() {
  const backend = useTmuxStatus((s) => s.status.backend);
  const error = useTmuxStatus((s) => s.status.error);
  const copy = setupScreenCopy(backend);

  return (
    <div
      role="alert"
      style={{
        position: "absolute",
        inset: 0,
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        gap: 16,
        padding: 32,
        textAlign: "center",
        background: "#0e0e10",
        color: "#e4e4e7",
        fontFamily: "ui-sans-serif, system-ui, sans-serif",
      }}
    >
      <h1 style={{ fontSize: 20, margin: 0, color: "#fca5a5" }}>
        {copy.heading}
      </h1>
      <p style={{ maxWidth: 520, lineHeight: 1.5, color: "#a1a1aa", margin: 0 }}>
        {copy.intro}
      </p>
      <pre
        style={{
          background: "#18181b",
          border: "1px solid #27272a",
          padding: "10px 14px",
          borderRadius: 6,
          fontSize: 13,
          color: "#e4e4e7",
          margin: 0,
        }}
      >
        {copy.install}
      </pre>
      {error && (
        <details style={{ color: "#71717a", fontSize: 12 }}>
          <summary style={{ cursor: "pointer" }}>Diagnostic detail</summary>
          <pre style={{ marginTop: 8 }}>{error}</pre>
        </details>
      )}
    </div>
  );
}
