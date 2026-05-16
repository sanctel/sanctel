// Hardcoded list of Worktrees for the Slice 4 demo. A real Worktree-
// management UI (discover projects, add/remove worktrees, prune merged
// branches) is out of scope for the v0.3 terminal-runtime PRD and is
// tracked separately. The values here are pointers to real on-disk
// directories so `tmux -c <path>` lands somewhere shells can actually
// chdir into; tabs in the same Worktree share a `sanctel-wt:<id>` tmux
// session per ADR-0012.

import type { Worktree } from "./types";

const HOME = "/home/agent";

export const DEMO_WORKTREES: readonly Worktree[] = [
  {
    id: "sanctel-main",
    projectId: "sanctel",
    branch: "main",
    path: `${HOME}/workspace`,
    status: "active",
  },
  {
    id: "sanctel-scratch",
    projectId: "sanctel",
    branch: "scratch",
    path: HOME,
    status: "active",
  },
];

export function findWorktree(id: string): Worktree | undefined {
  return DEMO_WORKTREES.find((w) => w.id === id);
}
