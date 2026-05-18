# Windows support

**Decision:** Not on the roadmap. Sanctel is macOS and Linux only for the
foreseeable future.

**Reason:** Sanctel's target user base — AI-coding-enthusiast developers
working in Tauri-shaped desktop apps — is overwhelmingly on macOS and
Linux. The engineering cost of native Windows support (ConPTY backend,
Windows-specific path handling, codesigning, MSI installer, separate
Windows CI lane, Windows-specific edge cases in Tauri's webview, etc.)
is not justified by the current user demand.

**Architecturally-relevant consequences:**

- Tmux is acceptable as a PTY backend even though it requires WSL on
  Windows. WSL is not a path sanctel needs to support.
- Path handling in Rust can assume Unix semantics (forward slashes,
  POSIX permissions, etc.) for the data paths sanctel owns.
- See `.out-of-scope/zellij-backend.md` — the decisive argument for
  zellij was Windows. Without Windows on the roadmap, the verdict flips
  to tmux.

**Prior requests:** None explicit yet. This file pre-empts the question
so future architecture decisions (especially around backend choice,
PTY libraries, and filesystem APIs) can reference it directly.

**When to reconsider:** A concrete user-demand signal — multiple
external requests, a strategic partnership requiring Windows, or
evidence that sanctel's target market is more Windows-heavy than
assumed. Until then, treat "let's pick X because it works on Windows"
as a yellow flag in technical decisions.
