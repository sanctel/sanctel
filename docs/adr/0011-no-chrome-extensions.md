# 0011 — No Chrome extension runtime; OS WebView only; plugin-equivalents for extension features

**Status:** Accepted

**Decision:** Sanctel's browser tabs use the OS WebView (WKWebView /
WebView2 / WebKitGTK). **No Chrome extension runtime in v1.** Extension-
equivalent features (adblock, autofill / vault, userscripts, reader mode,
vim navigation) are delivered as **plugins** via JS injection. An "Open in
real browser" right-click hatch handles cases where a vendor-specific
extension is required.

## Considered options

- **CEF (Chromium Embedded Framework)** — +150–200 MB bundle, weeks of
  Tauri ↔ CEF integration, ongoing Chromium-bump work; loses Tauri's
  "small native app" thesis.
- **Move to Electron** — rewrite off Tauri, 100–150 MB bundle, slower
  startup, 3–10× RAM, abandons stack discipline.
- **Fork Chromium** — team-scale engineering (Arc / Brave / Vivaldi tier);
  not solo-doable.
- **Implement extension runtime ourselves** — many person-years; nobody
  outside Google has built one for MV3 service workers.
- **No extension-equivalent features at all** — loses adblock / autofill /
  Vimium, which are genuine user value in a workspace app.

## Consequences

- We pay the small loss of "no Chrome extensions" to keep the small binary,
  cross-platform parity, and elegant Tauri integration.
- Plugin equivalents (`adblock`, `vault`, `userscripts`, `reader-mode`,
  `vim-nav`) are shipped or referenced via
  [ADR-0008](./0008-tuicommander-style-plugin-system.md)'s plugin system.
- Future escape hatch: a `chromium-tabs` plugin could bundle CEF as an
  optional component, registering a `chromium-browser` TabKind. **v3+
  concern; do not pre-build.**
