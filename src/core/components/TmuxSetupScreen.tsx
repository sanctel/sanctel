import { useTmuxStatus } from "../store/tmuxStatusStore";

/// Shown when the Rust-side `tmux -V` startup probe could not find a usable
/// tmux. Issue #8: replaces tab-by-tab failures with one clear setup screen
/// at app launch. React stays in this state until the user installs tmux
/// and restarts sanctel — the probe runs once per app launch.
export default function TmuxSetupScreen() {
  const error = useTmuxStatus((s) => s.status.error);

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
        Sanctel needs tmux
      </h1>
      <p style={{ maxWidth: 520, lineHeight: 1.5, color: "#a1a1aa", margin: 0 }}>
        Sanctel's terminal and chat tabs are backed by tmux. Install it from
        your package manager and relaunch:
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
        {"# macOS\nbrew install tmux\n\n# Debian / Ubuntu\nsudo apt install tmux"}
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
