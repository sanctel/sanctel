# 0010 — Agent ↔ browser integration: Architecture B (drive the user's tabs in-place via MCP)

**Status:** Accepted (architecture); **Implementation:** deferred to v0.6–v0.8

**Decision:** Agents drive **the user's existing browser tabs in-place**.
A Rust **MCP server** exposes browser-control tools; each tool dispatches
to the per-platform WebView API (`WKWebView.evaluateJavaScript`,
`WebView2.ExecuteScriptAsync`, `webkit_web_view_run_javascript_async`).
Profile-inheritance is automatic — the agent drives the webview that
already has the right cookies.

## Considered options

- **A: Agent has its own headless browser (Playwright + bundled Chromium)** —
  doesn't work for our stack. Playwright drives Chromium / Firefox /
  its-own-WebKit in separate processes, not the OS WebView inside Tauri.
  Going Playwright means shipping a second 300 MB browser; no Profile
  sharing; user can't watch live.
- **C: Hybrid shadow tabs** — extra complexity for marginal gain; defer
  until a real use case appears.
- **D: Computer Use vision loop** — slow, expensive; v2 fallback for
  workflows where DOM access fails.

## Consequences

- Profile-aware automation for free: the agent operates inside the
  user's existing webview, which already has the right
  `with_profile_name`. The agent cannot cross profiles by accident.
- Real-time observability: the user watches navigations, scrolling, and
  cursor live.
- Per-platform glue (`browser_control_{mac,win,linux}.rs`) is ours to write
  — ~300–500 LOC each.
- Agent-browser is delivered as a **plugin** (per
  [ADR-0008](./0008-tuicommander-style-plugin-system.md)); core ships the
  primitives and the capability gates only.
- Trust UX (visual indicator, one-click pause, action log, user-input
  auto-pause, approval gates) is **mandatory in v1** of this feature.
- Full design (tool inventory, capability tiers, per-platform glue, trust
  UX, phased rollout) in
  [docs/design/agent-browser-control.md](../design/agent-browser-control.md).
