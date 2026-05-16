# 0001 — Tauri 2 + React + Zustand for the desktop shell

**Status:** Accepted

**Decision:** Sanctel's desktop shell is a Tauri 2 application with a React +
Zustand frontend. Cross-platform from day one (macOS / Windows / Linux); the
shell is HTML and Tauri webviews are positioned over an empty
ContentArea div by Rust.

## Considered options

- **Electron** — heavier (~150 MB bundles, ~3–10× RAM), but mature plugin
  ecosystem.
- **Native AppKit / SwiftUI** — best-in-class on macOS, but mac-only and
  ties us to one team.
- **TUI (Bubble Tea / ratatui)** — small and fast, but rules out a mobile
  companion UI and a polished browser-tab story.

## Consequences

- We accept Tauri 2's smaller ecosystem in exchange for the small-binary,
  cross-platform, web-stack-on-Rust profile that matches a workspace app.
- Tab logic is JS-driven; capability gates and PTY ownership stay in Rust
  (see [ADR-0009](./0009-tuicommander-style-plugin-system.md)).
- WebView platform differences (WKWebView / WebView2 / WebKitGTK) become
  ours to bridge for any agent-browser integration
  (see [ADR-0010](./0010-architecture-b-browser-control.md)).
