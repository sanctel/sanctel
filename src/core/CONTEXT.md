# Sanctel — Core Context

The shared kernel referenced by every specialized context in Sanctel. Defines
the entities that span the whole product: identity (Profile), organization
(Space), atomic units (Tab), filesystem attachments (Worktree, Project).

For specialized contexts (plugin runtime, file editor, agent runtime,
agent-browser, etc.) and how they relate to Core, see
[CONTEXT-MAP.md](../../CONTEXT-MAP.md).

## Language

**Profile**: the cookie / storage isolation boundary. Maps 1:1 to Tauri's
`WebviewBuilder::with_profile_name`. A user typically has one ("Default"); power
users have 2–3 ("Work", "Personal"). Owns the cookie jar — all Spaces under one
Profile share logins.
_Avoid_: `Workspace`, `Account`, `Identity`.

**Space**: organizational grouping inside a Profile — color, ordered tab list,
active tab. Belongs to exactly one Profile. Switching Spaces may implicitly
switch Profiles when the destination Space is on a different Profile.
_Avoid_: `Workspace`, `Group`, `Project`.

**Tab**: atomic sidebar entry, one Tauri webview each. Belongs to exactly one
Space. References at most one Worktree (for terminal / file / diff kinds) and
at most one backend session (TmuxSession, AgentSession).
_Avoid_: `Pane`, `Window`.

**TabKind**: enum — `browser | terminal | chat | file | diff`. Determines
which URL the webview loads. New kinds add a variant plus a bundled HTML page
(or external URL for `browser`).
_Avoid_: `TabType`.

**Project**: a git repo on disk. Filesystem entity; exists regardless of
which Profile is active. One Project can be touched by tabs in many Spaces
across multiple Profiles.

**Worktree**: a real `git worktree` on disk; tied to one branch. Filesystem
entity, orthogonal to Profile and Space. A Tab optionally attaches via
`worktreeId`. Many tabs can share one Worktree (separate shells, same cwd);
one Worktree can be referenced from tabs in many Spaces.
_Avoid_: `Branch dir`, `Workspace dir`.

**AgentSession**: a Claude / Codex / Gemini conversation thread. Keyed by
cwd path (e.g., `~/.claude/projects/<encoded-cwd>/<id>.jsonl`), so an
AgentSession is implicitly scoped to a Worktree, not to a Tab. A Tab is a
*viewer* of the AgentSession that exists in its cwd.

**TmuxSession**: a server-side tmux session, the persistence handle for a
Worktree's shells. Named `sanctel_wt_<worktreeId>` — one session per
Worktree. Contains one **tmux window** per terminal **Tab** in that
Worktree; the window name lives in the Tab record (the Tab's only durable
field beyond `worktreeId` and `kind`). Outlives the app. The reason tabs
survive app restart without explicit save logic.

Worktree-less terminal tabs (plain shell, no `worktreeId`) attach to a
fallback session `sanctel_detached_<profileId>`. The separator is `_`
because tmux parses `:` and `.` in target specs as session/window/pane
delimiters; see [ADR-0012](../../docs/adr/0012-tmux-session-per-worktree-window-per-tab.md).

## Relationships

- A **Profile** contains one or more **Spaces**.
- A **Space** belongs to exactly one **Profile**.
- A **Space** contains zero or more **Tabs** (ordered, plus one active).
- A **Tab** has exactly one **TabKind**.
- A **Tab** of kind `terminal | file | diff` typically attaches to one
  **Worktree** via `worktreeId`. Browser tabs usually do not.
- A **Worktree** belongs to exactly one **Project**.
- One **Worktree** can be referenced from Tabs in many Spaces, and across
  many Profiles (though cross-Profile is unusual).
- One **Profile** can hold Tabs referencing Worktrees in many Projects.
- A **TmuxSession** is owned by the tmux server, named by Worktree
  (`sanctel_wt_<worktreeId>`). A terminal **Tab** points at one
  TmuxSession *and* one window within it. Many Tabs on the same Worktree
  share one TmuxSession via separate windows; tmux destroys the session
  when its last window dies.
- An **AgentSession** belongs to one Worktree (key = cwd path), not to a Tab.
  Multiple Tabs in the same Worktree see the same `claude --resume` history.

## The Persistence Anchor invariant

Tabs are ephemeral pointers. Durable state lives in the filesystem and the
tmux server:

```
Ephemeral (recreated on launch)   Durable (outlives app)
─────────────────────────────     ─────────────────────────────────
Tab                               Profile data dir (cookies, etc.)
Space.activeTabId                 Worktree directory (real git wt)
Space (visual state)              AgentSession transcript
                                     (~/.claude/projects/<encoded>/…)
                                  TmuxSession (tmux server outlives app)
```

On launch, Tabs are reconstructed from disk by replaying their references
(Profile id, Space id, Worktree path, kind, url). Sanctel saves almost no
state of its own. See [ADR-0004](../../docs/adr/0004-persistence-anchor-pattern.md).

## Example dialogue

> **Dev:** "When I open a browser tab in the 'Maze monorepo' Space, which
> cookies does it see?"
>
> **Domain expert:** "The cookies of the Space's **Profile**. Spaces don't
> own cookies — Profiles do. If 'Maze monorepo' and 'Maze ops' are both in
> your Work Profile, they share your work GitHub login automatically."
>
> **Dev:** "If I close that tab and reopen tomorrow, what happens to a
> running `claude` in a different terminal tab?"
>
> **Domain expert:** "**Tabs are ephemeral. Worktrees, transcripts, and
> TmuxSessions are durable.** The Tab record is restored from disk on launch,
> then re-attached to the Worktree's TmuxSession. Claude's **AgentSession**
> transcript is keyed by the Worktree's path, so `claude --resume` picks up
> the conversation."
>
> **Dev:** "Can one Space contain Tabs that reference different Worktrees?"
>
> **Domain expert:** "Yes. A Space is purely organizational. One Space —
> 'Maze monorepo' — can have one Tab in the main checkout, another Tab in
> the `fix-auth` worktree, and a third in the `tests` worktree. Different
> Worktrees, shared Space."

## Flagged ambiguities

These four overloaded terms must be qualified in code; reviewer will reject
misuse.

- **"Session"** historically meant tmux session, Claude / Codex / Gemini
  conversation, ACP JSON-RPC handle, or a UI tab. Resolved by qualifying:
  **TmuxSession**, **AgentSession**, **AcpxSession**, or **Tab**. The bare
  word is not used in code.
- **"Workspace"** historically meant Profile, Space, git workspace, or
  "the app itself" depending on speaker. Resolved: not a code term. Use
  **Profile** (identity) or **Space** (organization). Type names containing
  `Workspace` are review-rejected.
- **"Window"** historically meant tmux window or OS window. Resolved:
  **Window** = OS / Tauri window only. tmux "windows" map to our **Tab**.
- **"Worktree"** was almost made a child of **Space** in early drafts.
  Resolved: orthogonal — filesystem entity, no Profile or Space ownership.
  See [ADR-0005](../../docs/adr/0005-worktree-orthogonality.md).
