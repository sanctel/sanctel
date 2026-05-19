# Review Guardrails

These are quick review rules. Source-of-truth details live in `src/core/CONTEXT.md` and `docs/adr/`.

## Domain vocabulary

| Use | Not |
|---|---|
| `Profile` | `Workspace`, `Account` |
| `Space` | `Workspace`, `Group`, `Project` |
| `Tab` | `Pane`, `Window` |
| `TabKind` | `TabType` |
| `Worktree` | `Branch dir`, `Workspace dir` |
| `AgentSession` | `ChatSession`, `Conversation` |
| `TmuxSession` | `Shell`, `Pty` |

The three overloaded words to disambiguate:

- **"Session"** must be qualified: `TmuxSession`, `AgentSession`, or a UI `Tab`.
- **"Workspace"** must not appear in type names. Use `Space` or `Profile`.
- **"Window"** = OS / Tauri window only. tmux windows map to our `Tab`.

## Review-enforced invariants

These are not preferences. They are architectural commitments captured as ADRs; violations are blockers.

- **Persistence Anchor** — [ADR-0004](../adr/0004-persistence-anchor-pattern.md). Tabs are ephemeral; durable state lives in the filesystem and tmux server.
- **Profile-as-identity-boundary** — [ADR-0003](../adr/0003-profile-as-identity-boundary.md). `WebviewBuilder::with_profile_name(...)` takes the profile id, never the space id. `Tab.profileId` is derived from `Space.profileId`, never stored separately.
- **Worktree orthogonality** — [ADR-0005](../adr/0005-worktree-orthogonality.md). Don't tie Worktree to Profile in code.
- **TabKind unification** — [ADR-0006](../adr/0006-tabkind-unification.md). New tab kinds add a `kind` enum variant + a bundled HTML page or external URL. They do not add new tab-specific data structures.
- **Plugin capability tiers** — [ADR-0008](../adr/0008-tuicommander-style-plugin-system.md). New plugin APIs must declare a capability tier (1/2/3/4). Tier 3/4 operations require manifest-declared capabilities, enforced in Rust.
- **tmux session/window mapping** — [ADR-0012](../adr/0012-tmux-session-per-worktree-window-per-tab.md). One tmux session per Tab (`sanctel_wt_<worktreeId>__<windowName>`), one window per session, Worktree as name prefix. Sanctel runs on a dedicated tmux server (`-L sanctel`).

## When you add something new

| Change | What else updates |
|---|---|
| New architectural decision | New ADR in `docs/adr/NNNN-slug.md`. Reference from `CONTEXT-MAP.md` if it is a cross-cutting invariant. |
| New context | New `<dir>/CONTEXT.md`. Add a row to `CONTEXT-MAP.md`. |
| New term in Core | Add to `src/core/CONTEXT.md` Language + Relationships. |
| New `TabKind` | Update ADR-0006 examples + add the bundled HTML page or external URL pattern. |
| New plugin capability | Add the constant + Rust enforcement module + update `docs/design/plugin-system.md`. |
| New planned subsystem | Add `docs/design/<sub>.md`. Add a row to `CONTEXT-MAP.md`. |

## When you start a specialized subsystem

When code for a specialized context first lands:

1. Create `src/<sub>/` and/or `src-tauri/src/<sub>/`.
2. Write `<sub>/CONTEXT.md`.
3. Move `docs/design/<sub>.md` to `<sub>/DESIGN.md`.
4. Update `CONTEXT-MAP.md` to point at the new locations.

The first file in a new subsystem directory is always its `CONTEXT.md`.
