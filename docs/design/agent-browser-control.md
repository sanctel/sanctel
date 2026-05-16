# Agent ↔ browser integration (design)

> **Status**: planned, not yet implemented. Ships in v0.6–v0.8, after the
> plugin system. Architecture is settled (see
> [ADR-0010](../adr/0010-architecture-b-browser-control.md)); implementation
> deferred. When this ships, this document moves alongside the code (e.g.,
> `src-tauri/src/mcp/browser/DESIGN.md`).

## The decision: Architecture B (agent drives the user's tabs in-place)

Four candidate architectures, with the chosen one in bold:

```
A.  Agent has its own headless browser    (Playwright + bundled Chromium)
B.  Agent drives the USER's tabs ★ chosen (in-place WebView APIs)
C.  Hybrid — shadow tabs for agent work   (extra complexity, defer)
D.  Computer Use vision loop              (slow, expensive; v2 fallback)
```

Why B and not A: the obvious answer ("just use Playwright") doesn't work
for our stack. Playwright drives Chromium / Firefox / its-own-WebKit-build
in separate processes, not the WKWebView / WebView2 / WebKitGTK webviews
inside a Tauri app. Going with Playwright means **shipping a second
browser** alongside the user's — 300 MB bundle, separate session, no
Profile sharing, user can't watch live.

Architecture B gives us Profile-aware automation for free (the webview
already has the right cookies), real-time observability (user watches the
cursor, navigations, scrolling), and a small bundle.

## The protocol: MCP

We don't invent a protocol. The agent ↔ browser bridge speaks MCP (Model
Context Protocol). Already supported by Claude Desktop, Cursor, Cline, Zed,
and most modern agent CLIs.

```
Agent (Claude / Cursor / etc.)
   │
   │  tools/list, tools/call   (MCP over stdio or socket)
   ▼
Rust MCP server in src-tauri/src/mcp/browser.rs
   │
   │  Tauri commands
   ▼
Per-platform WebView APIs
   ├─ macOS:    WKWebView.evaluateJavaScript(...)
   ├─ Windows:  WebView2.ExecuteScriptAsync(...)
   └─ Linux:    webkit_web_view_run_javascript_async(...)
   │
   ▼
The user's tab (same webview the user sees)
```

## Tool inventory (the MCP surface)

Eleven tools, grouped by capability tier:

```
Tier tab:read           tabs.list                  → [{ id, url, title, spaceId, profileId }]
                        tabs.read(id)              → { url, title, visible_text, html }
                        tabs.screenshot(id)        → PNG bytes

Tier tab:control        tabs.navigate(id, url)
                        tabs.eval(id, code)        → return value
                        tabs.click(id, selector)
                        tabs.type(id, selector, text)
                        tabs.scroll(id, ...)
                        tabs.wait_for(id, condition)
                        tabs.focus(id)

Tier tab:create         tabs.open(profileId?, url) → id
                        tabs.close(id)
```

Each MCP tool is a thin Rust function that dispatches to the right platform
WebView API. ~500–800 LOC total for the bridge.

## Per-platform glue (Rust modules)

```
src-tauri/src/browser_control/
   ├── mod.rs                  // dispatch + shared types
   ├── browser_control_mac.rs  // WKWebView via objc/cocoa
   ├── browser_control_win.rs  // WebView2 via webview2-com
   └── browser_control_linux.rs // WebKitGTK via gtk-rs
```

`mod.rs` exposes a unified API; platform files implement it. Same pattern
Tauri itself uses internally.

## Capability tiers (manifest-declared)

A plugin or built-in caller must declare the tiers it uses in
`manifest.json` (see
[docs/design/plugin-system.md](./plugin-system.md#capability-tiers)):

| Capability | What's possible | Risk |
|---|---|---|
| `tab:read` | observe URL/title/text/screenshot | low — read-only |
| `tab:control` | navigate, click, type, eval JS | high — can do anything on logged-in sites |
| `tab:create` | spawn or close tabs | medium — can flood the user |

Plus optional `allowedDomains: ["github.com/*", ...]` in the manifest to
restrict which URLs a plugin can drive — a Tier-3-style allowlist mirroring
`net:http`'s `allowedUrls`.

## Profile-inheritance invariant

```
Tab in Profile "Work"
   → agent navigates the tab to github.com
   → agent sees the Work GitHub login (Work profile's cookies)
   → NOT the Personal profile's login
```

This is **automatic**: the agent drives the existing webview, which already
has the correct `with_profile_name` from the Core context's Profile
invariants. The agent cannot cross profiles by accident.

A hypothetical `tab:cross-profile` capability would let plugins move tabs
between Profiles. Treat this as a red flag — almost never needed; explicit
opt-in only.

## Trust + visibility UX (mandatory in v1 of this feature)

Without these, agent-controlled browser tabs feel like spyware. Required
from v0.7:

1. **Visual indicator on agent-driven tabs** — small "agent: claude" badge
   in the tab title; dimmed background tint; optional live cursor overlay.
2. **One-click pause** — user freezes agent control; subsequent agent
   calls return an "agent paused" error.
3. **Action log per tab** — every agent action (navigate, click, eval) is
   recorded with timestamp. Viewable in a tab side panel.
4. **Auto-pause on user input** — if user clicks/types in a tab the agent
   is driving, the agent yields. Resume requires user action.
5. **Approval gates for destructive actions** — patterns like clicking
   "Delete", "Confirm", "Send", or URLs matching destructive heuristics
   require user approval. Configurable per plugin via manifest.

## Implementation order (v0.6 → v0.8)

```
v0.6  (3 days)   MCP server scaffolding + read-only tools
                 tabs.list, tabs.read, tabs.screenshot
                 — useful immediately for "agent looks at what I'm
                 viewing" workflows; minimal trust risk

v0.7  (~1 week)  Write tools per platform
                 tabs.navigate, tabs.eval, tabs.click, tabs.type,
                 tabs.scroll, tabs.wait_for, tabs.focus
                 — each platform's evaluateJavaScript /
                 ExecuteScriptAsync / run_javascript_async wired

v0.8  (~1 week)  Trust + visibility UX
                 indicator, pause button, action log,
                 user-input auto-pause, destructive-action gates

v1.0+ (later)    tabs.open / tabs.close for new-tab control;
                 cross-tab orchestration; opt-in headless tabs
```

## Architecture as a plugin, not core

Per the plugin system design, agent-browser integration is delivered as a
**plugin**, not core. Sanctel ships the underlying browser-control Tauri
commands and the capability gates; a `browser-agent-bridge` plugin spawns
an MCP server and exposes the tools to MCP-aware clients.

This means:

- Plugins compete (Cursor-flavored, Claude-Desktop-flavored, custom)
- You don't carry the agent-integration maintenance in core
- Users explicitly opt in by installing a bridge plugin
- New agent UIs ship by writing new bridge plugins, not by patching core

## What's NOT in v1

| Feature | Defer to | Why |
|---|---|---|
| Anthropic Computer Use (vision loop) | v2+ | slow + token-heavy; rarely needed if DOM access works |
| Cross-browser fingerprint spoofing | never | not our problem |
| Recording macros / replay | v2+ | nice but big work |
| Headless browser pool | only on demand | bundle cost; most workflows don't need it |
| Multi-tab parallel orchestration | v2+ | sequence first, parallel later |
| AI-decided element targeting (no selectors) | v2+ | Stagehand pattern; needs more reasoning |
| Form filling from credential vault | v2+ | sensitive; needs Profile-aware vault first |
| OAuth / SSO flows mediated by agent | never | leave to user; security minefield |

## How this composes with worktrees

Browser tabs in Spaces tied to a Profile don't have `worktreeId` (browser
tabs are URL-shaped, not filesystem-shaped). But agent workflows often span
both:

```
Space "Maze: fix-auth" (Profile "Work")
   ├── Terminal tab    cwd=worktree/fix-auth, running claude
   ├── Browser tab     linear.app/issue/123  (Work cookies)
   └── Browser tab     github.com/maze/pr/456  (Work cookies)

A "browser-agent-bridge" plugin could expose to claude (running in the
terminal tab) MCP tools to read/navigate the two browser tabs. Claude can
say "check the linear issue, then read the PR comments, then update the
PR title to match the issue title" — all driving the user's actual tabs,
respecting Profile cookies.
```

This is the **killer workflow** the architecture unlocks. The agent has
shared state with the human: same logins, same view, same context.

## What we reuse vs build

```
Reuse (don't build):
   • MCP protocol                  (already a standard)
   • WKWebView.evaluateJavaScript  (in macOS)
   • WebView2.ExecuteScriptAsync   (in Windows Edge runtime)
   • webkit_web_view_run_javascript (in WebKitGTK)
   • TUICommander's MCP server bootstrap pattern (mcp_http/, desktop-mcp/)
   • Microsoft Playwright MCP — for tool-name conventions (apache-2.0)
   • Stagehand's act/extract/observe naming (apache-2.0)

Build (must write):
   • Per-platform browser_control_*.rs glue        (~300–500 LOC each)
   • Rust MCP server registering browser tools     (~300 LOC)
   • Trust UX (indicator, pause, log, gates)       (~500 LOC frontend)
   • Bridge plugin (the one users install)         (~200 LOC; ships as example)

Don't reuse:
   • Playwright (drives separate browsers, not in-place webviews)
   • Browser-Use, Stagehand (Python or Playwright-bound)
   • Browserbase (cloud-only)
```

## References to study

| File / project | What you'll learn |
|---|---|
| `../tuicommander/src-tauri/src/mcp_http/` | how Tauri exposes MCP tools |
| `../tuicommander/packages/desktop-mcp/` | the MCP-server-as-sidecar pattern |
| Microsoft Playwright MCP ([github.com/microsoft/playwright-mcp](https://github.com/microsoft/playwright-mcp)) | tool naming conventions; argument shapes |
| Browser-Use ([github.com/browser-use/browser-use](https://github.com/browser-use/browser-use)) | high-level agent API design (what Claude "thinks in") |
| Stagehand ([github.com/browserbase/stagehand](https://github.com/browserbase/stagehand)) | act/extract/observe API ergonomics |
| Anthropic Computer Use docs | the vision-loop fallback pattern |
| VS Code's integrated browser ([github.com/microsoft/vscode](https://github.com/microsoft/vscode), `src/vs/workbench/contrib/webview/`) | how an open-source editor lets agents inspect webview content |
| Cursor (closed) | observe the UX: badges, pause, action history — adopt the patterns |
| Dia by Browser Company (closed) | observe the agent-driven browser-tab UX |

The pattern-recognition shortcut: TUICommander's MCP plumbing is the
closest local reference for "Tauri app exposes MCP tools that drive
internal state." Their browser-tab story is empty, but the MCP scaffolding
is reusable.
