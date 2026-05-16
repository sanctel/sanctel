# 0006 — Every TabKind is a webview; kind is just which URL it loads

**Status:** Accepted

**Decision:** Every Tab is a Tauri webview. The Tab's `kind`
(`browser | terminal | chat | file | diff | plugin-registered`) is the URL
the webview loads — nothing more. Tab itself is one data structure across
all kinds; no per-kind subclasses.

| Kind | URL it loads | Backend |
|---|---|---|
| `browser` | `https://...` (external) | the web; cookies isolated per Profile |
| `terminal` | `tauri://localhost/terminal.html` | tmux runtime via Tauri IPC |
| `chat` | `tauri://localhost/chat.html` | agent runtime (hook files / ACP) |
| `file` | `tauri://localhost/file.html?path=...&worktree=...` | `file_read` / `file_write` / `file_watch` |
| `diff` | `tauri://localhost/diff.html?worktree=...&base=...` | `git_diff` |
| _(plugin-registered)_ | `plugin://<plugin-id>/<entry>.html?...` | plugin's own commands |

## Considered options

- **Per-kind data structures** — `BrowserTab`, `TerminalTab`, `ChatTab` as
  separate types. Lots of boilerplate; every new kind requires schema
  changes; plugin-registered kinds (see
  [docs/design/plugin-system.md](../design/plugin-system.md)) impossible
  without dynamic typing.
- **Different UI primitives per kind** — different sidebar rows, different
  routing. Breaks the "every tab looks the same" sidebar invariant.

## Consequences

- Adding a new kind = adding a string variant to the enum + bundling a new
  HTML page (or pointing at an external URL). No new schema, no new tab
  storage, no new sidebar widget code path.
- Plugins can register new TabKinds dynamically without modifying core.
- Each kind's "backend" lives in its specialized context (terminal, file
  editor, agent runtime, etc.); Core only owns the Tab record itself.
