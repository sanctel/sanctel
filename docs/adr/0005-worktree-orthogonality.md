# 0005 — Worktree is a filesystem entity, orthogonal to Profile and Space

**Status:** Accepted

**Decision:** A **Worktree** is a real `git worktree` on disk. It belongs to
exactly one **Project** (its parent repo). It does **not** belong to a
Profile or a Space. Tabs reference Worktrees via `worktreeId`; the
relationship is many-to-many across Spaces and across Profiles.

## Considered options

- **Worktree as child of Space** — natural for solo-project workflows but
  breaks the moment a Worktree is referenced from two Spaces (e.g., a
  "Tasks" Space and a "Watch" Space both monitoring `fix-auth`). Also wrong
  for filesystem operations (`cd` doesn't see cookies).
- **Worktree as child of Profile** — same problem; the filesystem doesn't
  care about identity. You can `cd ~/code/personal/...` from a Work-profile
  terminal tab.

## Consequences

- `Worktree.profileId` is **forbidden**. Review rejects denormalized profile
  links on Worktree.
- Many Tabs can share one Worktree (separate shells, same cwd, shared
  transcript directory). One Worktree can be referenced from Tabs in many
  Spaces.
- AgentSession transcript files are keyed by cwd (Worktree.path), not by Tab
  or Profile. A chat Tab must still carry its own verified AgentSession id
  before `claude --resume` is a valid startup command.
- Worktree storage strategy (sibling / AppDir / InsideRepo / ClaudeDefault)
  is a separate tactical decision and not part of this ADR.
