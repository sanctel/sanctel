# 0009 — File editor scope: levels 1+2+5 (view, light edit, diff); CodeMirror, not Monaco

**Status:** Accepted (architecture); **Implementation:** deferred to v0.4

**Decision:** Sanctel ships **viewer + light editor + diff viewer** only —
no LSP, no completion, no go-to-definition. Two new TabKinds (`file`,
`diff`) host **CodeMirror 6** + `@codemirror/merge`. Real code editing is
delegated to the user's external IDE via an "Open in IDE" pill.

## Considered options

- **Monaco** — ~10 MB core, language packs always bundled, no mobile story.
  Only right if we were building a level-3+ IDE.
- **Full level-3 IDE (LSP / completion / refactor)** — months of work that
  doesn't differentiate an agent orchestrator from Cursor. Every Arc-shaped
  agent orchestrator (Aizen, TUICommander, superset.sh, Wave) takes the
  light-edit path.
- **No editing at all** — forces external IDE every time; UX friction is
  too high for "agent wrote this, glance at it" workflows.

## Consequences

- CodeMirror 6 (~50 KB core, lazy language packs) keeps the bundle small
  and works on mobile.
- The agent ↔ editor ↔ file triangle needs **file watching** (`notify`
  crate; reload buffer on change) and **optimistic concurrency on save**
  (stat mtime, reject stale writes). Both are v1 requirements.
- Cross-boundary undo, diff-before-write, multi-cursor power features are
  explicit non-v1.
- Per-format viewers (image, notebook, CSV) ship as plugins via
  [ADR-0008](./0008-tuicommander-style-plugin-system.md)'s
  `host.registerFileKind` API.
- Full design (TabKind URLs, Rust commands, recovery, references) in
  [docs/design/file-editor.md](../design/file-editor.md).
