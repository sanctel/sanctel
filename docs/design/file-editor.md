# File editor (design)

> **Status**: planned, not yet implemented. Two new TabKinds + a few Rust
> commands; ships in v0.4 alongside (or just before) the plugin system.
> Decision recorded in
> [ADR-0009](../adr/0009-codemirror-light-edit-only.md). When this ships,
> this document moves alongside the code (e.g.,
> `src-tauri/src/files/DESIGN.md` and `src/files/CONTEXT.md`).

## Scope: levels 1+2+5, not 3-4

"File editor" bundles five distinct products. The scope must be clear:

```
                Read-only    Light edit    Code intel    Full IDE
                ─────────    ──────────    ──────────    ────────
1. File viewer       ✓
2. Quick editor      ✓           ✓
3. Code editor       ✓           ✓           ✓
4. IDE               ✓           ✓           ✓             ✓
5. Diff viewer       ✓
```

**We ship levels 1, 2, and 5. We delegate 3-4 to the user's external IDE.**

Why: every Arc-shaped agent orchestrator (Aizen, TUICommander, superset.sh,
Wave) follows this pattern. Real code editing is months of LSP / treesitter
/ debugger work that doesn't differentiate an agent orchestrator from
Cursor. Users have strong editor preferences; the "Open in IDE" pill is the
canonical solution.

## The two new TabKinds

```ts
type TabKind = "browser" | "terminal" | "chat" | "file" | "diff";
```

**`file` tab** — view + light edit of a single file:

```
url:  tauri://localhost/file.html?path=<abs-path>&worktree=<id>
worktreeId: optional (gives file git context)
```

**`diff` tab** — side-by-side diff for a worktree's branch vs base:

```
url:  tauri://localhost/diff.html?worktree=<id>&base=main
worktreeId: required (diff is always worktree-anchored)
```

One tab per file (Arc model), not one editor area hosting many tabs (VS
Code model). Matches the existing one-webview-per-tab pattern. Lots-of-tabs
problem is mitigated by Space grouping.

## Library: CodeMirror 6, not Monaco

| | CodeMirror 6 | Monaco |
|---|---|---|
| Core size | ~50 KB | ~10 MB |
| Language packs | lazy, 10-30 KB each | bundled always |
| Diff support | `@codemirror/merge` | built-in |
| Mobile-friendly | yes | no |
| Used by | Obsidian, Sourcegraph, Jupyter | VS Code, GitHub web, Wave Terminal |

For "view + light edit + diff," CodeMirror wins on every axis. Monaco would
only be right if we were building level 3-4 (LSP + completion + go-to-def).
We aren't.

Obsidian uses CodeMirror; Obsidian is the closest mental-model match to our
product. Adopt their choice.

## The agent ↔ editor ↔ file triangle

Most editors solve "user edits file." Our editor solves
"user **and** agents both edit the same file."

```
                     Agent (in tmux pane)
                       │
              writes   │   reads
                       ▼
                     File ◄──── reads ──── Editor (in webview tab)
                       ▲                     │
                       └──── writes ─────────┘
```

Requirements unique to this triangle:

1. **File watcher non-negotiable**. Agent writes → editor refreshes.
   Use `notify` crate; emit `file:changed`; reload editor buffer.
2. **Optimistic concurrency on save**. Editor stores mtime at read.
   On save: re-stat; if mtime moved, prompt user (overwrite? merge?
   reload?). `file_write(path, content, expected_mtime)` enforces this.
3. **Audit trail of file edits**. Each write logs source: `user`,
   `agent:<type>:<session>`, `external`. Powers future
   "who wrote this line?"
4. **Cross-boundary undo** *(advanced, defer)*. Editor's Cmd+Z can undo
   recent agent edits if a change journal is kept.
5. **Diff-before-write for agents** *(advanced, defer)*. Cursor-style
   "review each agent edit before commit." Big feature; out of scope v1.

For v1, ship items 1 and 2. That's 80% of the safety with 20% of the work.

## Rust commands

```rust
// src-tauri/src/files.rs (new module)
#[tauri::command] fn file_read(path: String) -> Result<FileContents, String>;
#[tauri::command] fn file_write(
    path: String,
    content: String,
    expected_mtime: i64,            // ← optimistic concurrency
) -> Result<i64, String>;            // returns new mtime
#[tauri::command] fn file_watch(path: String) -> Result<(), String>;
                                     // emits "file:changed" events

#[tauri::command] fn git_diff(
    worktree: String,
    base: String,
) -> Result<DiffResult, String>;     // git diff <base>...HEAD
```

`FileContents` = `{ content, mtime, encoding }`. `DiffResult` =
`{ files: [{path, hunks: [...]}] }` — a structured diff the diff page can
render.

## Bundled pages

```
public/
  file.html       CodeMirror 6 editor, reads ?path&worktree
  diff.html       CodeMirror merge view, reads ?worktree&base
```

Each page mounts CodeMirror, subscribes to backend events for live updates,
calls the appropriate Tauri commands.

## File-tree sidebar widget (v1.1)

For browsing files in the active worktree:

```
Sidebar:
  [profile pills]
  [space pills]
  + Browser  + Terminal  + Chat  + File

  ▾ tour (main)
     ▸ src/
     ▸ public/
       package.json

  ─── tabs ───
  …
```

Click file → open file tab (or focus existing).

Architecturally a sidebar widget. Ships as core but built as if it were a
plugin — so it can become a plugin example later. The first canonical
"plugin pattern" for the registry.

## Unsaved buffer recovery

Files on disk are durable; editor buffers are not. If the app crashes with
unsaved edits, we recover from a per-tab journal:

```
~/.sanctel/recovery/<tab-id>.json
   { path, content, baseMtime, lastEditedAt }
```

Auto-saved every 2 s while a buffer is dirty. Cleared on save or discard.
On launch: each file tab checks for a matching recovery file; if found and
newer than the file's mtime, offer "restore unsaved changes."

## Plugin extension points (Phase 1+)

Once the plugin system ships (see
[docs/design/plugin-system.md](./plugin-system.md)), file editing exposes:

```typescript
host.registerFileKind({
  id: "csv",
  extensions: [".csv", ".tsv"],
  entry: "csv-viewer.html",
  icon: "...",
});

host.registerEditorCommand({
  id: "format",
  title: "Format Document",
  shortcut: "Cmd+Shift+F",
  run: ({ path, content, save }) => { /* format + save */ },
});

host.on("file:changed", ({ path, source }) => {
  // source: "user" | "external" | "agent:claude:<session>"
});
```

Enables plugins to add:

- Image / video viewers (`.png`, `.mp4` → custom view)
- Notebook viewer (`.ipynb`)
- Spreadsheet viewer (`.csv`)
- Format-on-save (prettier, black, gofmt)
- Vim / emacs modes (`@replit/codemirror-vim`)
- Per-language LSP bridges *(eventually)*

## What's deliberately NOT in v1

Each of these is a real feature we can defer:

| Feature | Why defer |
|---|---|
| **LSP / completion / go-to-def** | Months of work. External IDE handles. |
| **Multi-cursor advanced editing** | Review use case doesn't need it. |
| **Vim/emacs modes** | Ship as plugins. |
| **Find across files** | External IDE. |
| **Refactoring (rename, extract)** | LSP-dependent. |
| **Inline AI edit-suggest (Cursor)** | Agents are external (tmux). Don't compete. |
| **Notebook editing** | Plugin territory. |
| **Image / video preview** | Plugin territory. |
| **Project-wide formatter** | Plugin or external command. |
| **Settings.json-style schema validation** | Plugin territory. |

Discipline: if it requires LSP, defer. If it requires real code
intelligence, defer. If it's view + light edit + diff, ship.

## How this maps to the Persistence Anchor

Same pattern as terminal tabs (see
[ADR-0004](../adr/0004-persistence-anchor-pattern.md)):

```
Ephemeral (recreated on launch)   Durable
─────────────────────────────     ─────────────────────────────
Tab (kind: file | diff)           File on disk
Editor's CodeMirror state         Git history (for diffs)
Open file path                    Unsaved buffer recovery
                                     (~/.sanctel/recovery/<tab-id>.json)
```

App restart: file/diff tabs replay their `url` (encodes path + worktree),
editor reopens, reads file fresh. Unsaved edits restore from recovery.

**Files are the durable layer. Editors are pure views.** Closing a file
tab and reopening it should be lossless.

## Implementation order (when you tackle v0.4)

```
1. (½ day) New TabKind "file"; types.ts + tabStore
2. (1 day) Rust: file_read / file_write / file_watch with optimistic concurrency
3. (1 day) public/file.html with CodeMirror 6 + basic syntax highlight
4. (½ day) Sidebar: + File button + native file picker dialog
5. (½ day) Dirty indicator, save UI, file:changed event handling
6. (1 day) Rust: git_diff using libgit2 or shelling to git
7. (1 day) public/diff.html with @codemirror/merge view
8. (1 day) Sidebar: + Diff button (creates diff tab for active worktree)
9. (1 day) Unsaved buffer recovery (auto-save + restore on launch)
10. (½ day) File-tree sidebar widget (basic; v1.1 polish)
```

Total: ~8 days of focused work for v1.0 file editing. Defer item 10 to v1.1
if you want to ship faster.

## References to study

| File | What you'll learn |
|---|---|
| `../waveterm/frontend/app/view/preview/` | embedding Monaco in a Tauri-like webview (worth studying even though we use CodeMirror — same plumbing pattern) |
| `../waveterm/frontend/app/monaco/` | their Monaco wrapper components |
| `../aizen/aizen/Features/Files/` | native Swift file browser + editor; different architecture but same UX problem |
| `../tuicommander/src-tauri/src/plugin_fs.rs` | sandboxed filesystem patterns for the file_read/write/watch commands |
| `../min/js/findinpage.js` | classic find-in-page pattern (for CodeMirror in-file search) |
| Obsidian source (not cloned; on GitHub) | the canonical "CodeMirror in a workspace app" reference — Obsidian *is* what we're building, minus agents |
| CodeMirror 6 docs (codemirror.net/docs/) | the API |
| `@codemirror/merge` package | diff view |
