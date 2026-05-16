// Per-Worktree monotonic window-name allocator (see ADR-0012).
//
// Given the current tmux window names in a Worktree's session, return the
// next `term-N` to use for a new terminal Tab. Gaps are tolerated (we pick
// max + 1 rather than the lowest free integer) so that closing window N
// never re-uses N for a future tab in the same session — keeps `windowName`
// stable as a tmux handle across the session's lifetime.
//
// Non-numeric names (`bash`, `build-watcher`, etc.) are ignored. The
// `term-` prefix is the only one this allocator owns; users renaming their
// own tmux windows to anything else won't perturb the counter.

const TERM_PREFIX_RE = /^term-(\d+)$/;

export function allocateWindowName(existing: readonly string[]): string {
  let max = 0;
  for (const name of existing) {
    const m = TERM_PREFIX_RE.exec(name);
    if (!m) continue;
    const n = Number.parseInt(m[1], 10);
    if (n > max) max = n;
  }
  return `term-${max + 1}`;
}
