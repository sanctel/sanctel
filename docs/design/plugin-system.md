# Plugin system (design)

> **Status**: planned, not yet implemented. Phased rollout starting at v0.5.
> Decision recorded in
> [ADR-0008](../adr/0008-tuicommander-style-plugin-system.md). When this
> ships, this document moves alongside the code (e.g.,
> `src-tauri/src/plugin/DESIGN.md`) and the corresponding `CONTEXT.md` is
> extracted.

## The chosen archetype

A **hybrid in-webview JS + Rust capability gates** model, modeled directly
on TUICommander's plugin system (which has the most thoughtful design among
the references we surveyed).

```
webview (in-process JS)              Rust backend (capability gates)
─────────────────────────            ───────────────────────────────
plugin/main.js
   │
   │ host.registerOutputWatcher(...) ◄── handled in JS, no Rust call
   │ host.addItem(...)
   │
   │ host.fs.read("...")           ───►  plugin_fs.rs:
   │                                     manifest declares "fs:read"?
   │                                     path inside sandbox?
   │
   │ host.http.fetch("https://...") ───►  plugin_http.rs:
   │                                       matches manifest.allowedUrls?
   │
   │ host.exec.run(["rtk",...])    ───►  plugin_exec.rs:
   │                                      binary in manifest.binaries?
   │
   │ host.pty.read(sessionId)      ───►  plugin_pty.rs: "pty:read"?
```

**Why this hybrid**: same-process JS gives Phase-1 productivity (hot reload,
trivial debugging, full API access for UI extension). Manifest-declared
capabilities checked in Rust make the dangerous operations (FS, network,
exec, PTY) safe — the manifest on disk is the source of truth, and the
frontend cannot lie about it.

The four archetypes we rejected and why:

- **VS Code separate-process**: too heavy for our scale; defer the IPC cost
  until we have a public marketplace with untrusted plugins.
- **Pure WASM (Zed)**: narrow API ceiling — Zed has had an open RFC for
  custom UI panels for 18+ months and the ecosystem can't grow past
  languages and themes.
- **In-process without capability gates (Obsidian / Hyper / Sublime)**:
  works but every plugin can do anything; we want a clearer trust boundary
  for v1.
- **Config-only (tmux)**: too limited for tab kinds, agent integrations,
  output watchers.

## Phased rollout

```
v0.1   no plugin system — themes / agents / keybindings loaded from JSON
       config files in ~/.config/sanctel/
v0.5   Phase 1 plugins: in-webview JS + Rust capability gates (this spec)
v0.6   community registry — "Browse plugins" UI, signed updates from a
       curated index
v∞     Phase 2 WASM — optional sandboxed runtime for untrusted plugins;
       first-party / trusted plugins remain JS
```

Don't build Phase 1 until v0.5 — the core (terminal tabs, worktrees, mobile
bridge) must ship first. Plugins extend a working app; they don't substitute
for one.

## Capability tiers

```
Tier 0  always-on             logging
Tier 1  always-on             commands (Cmd+K palette)
                              sidebar widgets
                              tab decorations (status dots, badges)
                              status bar segments
                              output watchers (regex on PTY output)
                              event subscribers (tab/agent/worktree events)
                              theme contributions
                              read-only state queries (tabs, spaces, …)
                              notify (toast)
Tier 2  always-on             register new TabKind  (e.g., "kanban", "music")
                              per-plugin sandboxed KV storage
Tier 3  manifest-declared     fs:read / fs:list / fs:watch
                              pty:read
Tier 4  manifest-declared     net:http  (URL allowlist via manifest.allowedUrls)
                              exec:cli  (binary allowlist via manifest.binaries)
                              worktree:create / worktree:remove
                              spawn:agent  (declared agent types)
```

Tier 3 and Tier 4 each correspond to a Rust file (`plugin_fs.rs`,
`plugin_pty.rs`, `plugin_http.rs`, `plugin_exec.rs`, `plugin_worktree.rs`,
`plugin_agent.rs`) that enforces the manifest's declared scope. JS cannot
bypass these — Rust holds the gate.

## Extension surface — what plugins actually do

The four "very high value" extension points (what plugins should be best
at):

1. **Register new TabKinds** — beyond browser/terminal/chat. A "kanban"
   plugin adds `kind: "kanban"` with a bundled HTML page; the rest of the
   app treats it like any other tab.
2. **Agent integrations** — new agent CLIs, ACP adapters, slash commands,
   per-agent status patterns.
3. **Output watchers** — regex against PTY output → custom actions (the
   pattern that powers Claude Squad's status detection, TUICommander's
   activity center).
4. **Worktree hooks** — pre/post-create, pre/post-finish handlers (env
   file copying, dependency installation, branch labeling).

Other capabilities (commands, decorations, themes, notifications, settings
panels) are tablestakes and should ship in Tier 1.

## Manifest format

```jsonc
// ~/.config/sanctel/plugins/<plugin-id>/manifest.json
{
  "id": "@you/cool-plugin",        // must match the directory name
  "name": "Cool Plugin",
  "version": "1.0.0",
  "minAppVersion": "0.5.0",
  "main": "main.js",                // ES module entry point
  "description": "Adds a kanban tab and watches for TODO comments.",
  "author": "you",

  // Tier 3/4 capabilities — must be declared explicitly
  "capabilities": ["fs:read", "pty:read", "net:http"],
  "allowedUrls": ["https://api.linear.app/*"],   // required if net:http
  "binaries":   ["rtk", "mdkb"],                  // required if exec:cli
  "agentTypes": ["claude", "codex"],              // scope plugin to certain agents

  "contributes": {
    "tabKinds":   [{ "id": "kanban", "label": "Kanban",
                     "entry": "kanban.html", "icon": "..." }],
    "commands":   [{ "id": "kanban.new", "title": "Kanban: New board" }],
    "themes":     [{ "id": "cyberpunk", "path": "themes/cyberpunk.json" }]
  }
}
```

All manifest fields use **camelCase** (matches Rust serde defaults).

## Plugin interface

```typescript
// What every plugin's main.js exports
export default {
  id: "plugin-id",
  onload(host: PluginHost): void { /* register your contributions */ },
  onunload(): void { /* optional cleanup; auto-disposers handle the rest */ },
};
```

## PluginHost API (the surface plugins can use)

```typescript
interface PluginHost {
  // Tier 0
  log(level: "debug"|"info"|"warn"|"error", msg: string, data?: unknown): void;

  // Tier 1 — UI extension
  registerCommand(cmd: Command): Disposable;
  registerSidebarWidget(widget: SidebarWidget): Disposable;
  registerTabDecoration(decorator: TabDecorator): Disposable;
  registerStatusBarItem(item: StatusItem): Disposable;
  registerOutputWatcher(w: { pattern: RegExp; onMatch(m, ctx): void }): Disposable;
  registerThemeContribution(theme: Theme): Disposable;
  notify(toast: { title: string; level: "info"|"warn"|"error" }): void;

  // Tier 1 — read-only state queries
  state: {
    activeProfile(): Profile;
    activeSpace(): Space;
    tabs(filter?: TabFilter): Tab[];
    worktrees(filter?: WorktreeFilter): Worktree[];
    agentSessions(): AgentSession[];
  };

  // Tier 1 — events (cleanup auto-handled by the registry on unload)
  on(event:
      | "tab:created"  | "tab:closed"  | "tab:focused"
      | "agent:status-changed" | "agent:permission-request"
      | "worktree:created"     | "worktree:finished"
      | "profile:switched"     | "space:switched",
    handler: (payload) => void
  ): Disposable;

  // Tier 1 — actions
  tabs: {
    create(req: CreateTabRequest): Promise<Tab>;
    close(id: string): Promise<void>;
    focus(id: string): Promise<void>;
  };
  spaces: { switch(id): Promise<void>; create(...): Promise<Space>; };

  // Tier 2 — per-plugin sandboxed KV storage
  storage: {
    get(key: string): Promise<unknown>;
    set(key: string, value: unknown): Promise<void>;
    delete(key: string): Promise<void>;
  };

  // Tier 3/4 — privileged; only present if declared in manifest.capabilities
  fs?:       { read; list; watch; };
  pty?:      { read(sessionId): Promise<string>; };
  http?:     { fetch(url, init): Promise<Response>; };
  exec?:     { run(binary, args): Promise<{stdout, stderr, code}>; };
  worktree?: { create(...); remove(...); };
  agent?:    { spawn(type, cwd, prompt): Promise<AgentSession>; };
}
```

## Lifecycle + crash safety (TUICommander's pattern)

```
1. Discovery     Rust scans ~/.config/sanctel/plugins/<id>/manifest.json
2. Validation    manifest schema + minAppVersion + capability declarations
3. Import        await import("plugin://<id>/main.js") via custom URI scheme
4. Module check  default export has id, onload, onunload
5. Register      pluginRegistry.register(plugin) → plugin.onload(host)
6. Active        receives events, output, structured events
7. Hot reload    file watcher → unregister + re-import
8. Unload        plugin.onunload() → auto-dispose all registrations
```

Every boundary is wrapped in try/catch. A broken plugin logs to its own
ring buffer, gets a red error badge in Settings → Plugins, and is skipped.
The app continues. **Plugin failures must never crash the app.**

## Distribution

Phase 1: **filesystem-only**. Users drop directories into
`~/.config/sanctel/plugins/<id>/`. Power-user shape; intentional friction
keeps the early ecosystem trusted.

Phase 2 (~v0.6): a **community registry**. Pattern options:

- a GitHub repo with `plugins.json` index (Zed / TPM style — simplest)
- a hosted `plugins.sanctel.app` site with search + reviews (Obsidian /
  VS Code style — bigger lift)

Settings → Plugins → "Browse" reads the registry and installs to the same
filesystem path. Auto-update via signed manifests.

## What plugins do NOT get

These are deliberate non-features in Phase 1:

- **No direct DOM access** to the main app's webview. Plugins can render
  inside their own sidebar widgets / tab kinds / settings panels — but
  can't reach into the main React tree.
- **No access to other plugins' storage.** Each plugin has its own KV
  store keyed by plugin id.
- **No raw access to other plugins' webviews.** Inter-plugin
  communication must go through registered events.
- **No `eval`-style escape hatches** like `registerRustFunction` — every
  privileged op must be a typed Rust command behind a capability.

## References to study

The plugin system is essentially a port of TUICommander's, with tweaks for
our domain model:

| File | What you'll learn |
|---|---|
| `../tuicommander/docs/plugins.md` | the user-facing authoring guide — read first, it's the spec |
| `../tuicommander/src-tauri/src/plugins.rs` | discovery, validation, `plugin://` URI protocol, hot reload, capability check |
| `../tuicommander/src-tauri/src/plugin_fs.rs` | sandboxed filesystem capability |
| `../tuicommander/src-tauri/src/plugin_pty.rs` | scoped PTY read |
| `../tuicommander/src-tauri/src/plugin_http.rs` | URL-allowlisted HTTP w/ SSRF protection |
| `../tuicommander/src-tauri/src/plugin_exec.rs` | binary-allowlisted CLI exec |
| `../tuicommander/examples/plugins/hello-world/` | the simplest possible plugin (Tier 1 only) |
| `../tuicommander/examples/plugins/claude-status/` | agent-scoped plugin pattern |
| `../tuicommander/src/stores/keybindings.ts:140` | how dynamic actions appear in Keyboard Shortcuts UI |
| `../waveterm/schema/widgets.json` | alternative declarative model — widgets as JSON, no JS at all |

The pattern-recognition shortcut: when in doubt, do what TUICommander does.
Their plugin system is the closest existing thing to what we want.
