# Sanctel — agent guidance

Read this whenever you enter the repo. The companion documents are:

- **[CONTEXT-MAP.md](./CONTEXT-MAP.md)** — domain entry point; map of
  contexts and how they relate
- **[src/core/CONTEXT.md](./src/core/CONTEXT.md)** — Core glossary
- **[docs/adr/](./docs/adr/)** — architectural decisions (start here when
  any "why" question comes up)
- **[docs/design/](./docs/design/)** — full design docs for planned
  subsystems
- **[.sandcastle/CODING_STANDARDS.md](./.sandcastle/CODING_STANDARDS.md)** —
  enforced at code review

## Working principles

- **Think before coding.** Surface tradeoffs; don't pick silently. If
  multiple interpretations exist, name them.
- **Simplicity first.** Minimum code that solves the problem. No
  speculative features, no abstractions for single-use code, no error
  handling for impossible scenarios.
- **Surgical changes.** Touch only what you must. Don't "improve" adjacent
  code, comments, or formatting. Match existing style.
- **Goal-driven execution.** Define success criteria; loop until verified.
  Strong success criteria let you loop independently.
- **Concrete and visual.** Code sketches, diagrams, tables when they
  improve understanding. Don't hide moving parts in prose.
- **Co-locate what changes together.** Each context lives in its own
  directory with its `CONTEXT.md` next to the code.
- **Test observable behavior**, not implementation details. New behavior
  ships with at least one test that fails without the change.

## Domain vocabulary (use these names)

| Use | Not |
|---|---|
| `Profile` | `Workspace`, `Account` |
| `Space` | `Workspace`, `Group`, `Project` |
| `Tab` | `Pane`, `Window` |
| `TabKind` | `TabType` |
| `Worktree` | `Branch dir`, `Workspace dir` |
| `AgentSession` | `ChatSession`, `Conversation` |
| `TmuxSession` | `Shell`, `Pty` |

The three overloaded words to disambiguate (review rejects misuse):

- **"Session"** must be qualified: `TmuxSession`, `AgentSession`, or a UI
  `Tab`.
- **"Workspace"** must not appear in type names — use `Space` or `Profile`.
- **"Window"** = OS / Tauri window only. tmux windows map to our `Tab`.

## Invariants that review enforces

These are not preferences. They are architectural commitments captured as
ADRs; violations are blockers.

- **Persistence Anchor** —
  [ADR-0004](./docs/adr/0004-persistence-anchor-pattern.md). Tabs are
  ephemeral; durable state lives in the filesystem and tmux server.
- **Profile-as-identity-boundary** —
  [ADR-0003](./docs/adr/0003-profile-as-identity-boundary.md).
  `WebviewBuilder::with_profile_name(...)` takes the profile id, never the
  space id. `Tab.profileId` is derived from `Space.profileId`, never
  stored separately.
- **Worktree orthogonality** —
  [ADR-0005](./docs/adr/0005-worktree-orthogonality.md). Don't tie
  Worktree to Profile in code.
- **TabKind unification** —
  [ADR-0006](./docs/adr/0006-tabkind-unification.md). New tab kinds add a
  `kind` enum variant + a bundled HTML page (or external URL). They do
  NOT add new tab-specific data structures.
- **Plugin capability tiers** —
  [ADR-0008](./docs/adr/0008-tuicommander-style-plugin-system.md). New
  plugin APIs MUST declare a capability tier (1/2/3/4). Tier 3/4
  operations require manifest-declared capabilities, enforced in Rust.

## When you add something new

| Change | What else updates |
|---|---|
| New architectural decision | New ADR in `docs/adr/NNNN-slug.md`. Reference from CONTEXT-MAP if it's a cross-cutting invariant. |
| New context (specialized subsystem) | New `<dir>/CONTEXT.md`. Add a row to CONTEXT-MAP. |
| New term in Core | Add to `src/core/CONTEXT.md` Language + Relationships. |
| New TabKind | Update [ADR-0006](./docs/adr/0006-tabkind-unification.md) examples + add the bundled HTML page or external URL pattern. |
| New plugin capability | Add the constant + Rust enforcement module + update [docs/design/plugin-system.md](./docs/design/plugin-system.md). |
| New planned subsystem | Add `docs/design/<sub>.md` (full spec). Add a row to CONTEXT-MAP. |

## When you start a specialized subsystem

When code for a specialized context (plugin, file editor, agent runtime,
etc.) first lands:

1. Create `src/<sub>/` and/or `src-tauri/src/<sub>/`.
2. Write `<sub>/CONTEXT.md` (extracted glossary for that context).
3. Move `docs/design/<sub>.md` → `<sub>/DESIGN.md`.
4. Update CONTEXT-MAP's row to point at the new locations.

The first file in a new subsystem directory is always its `CONTEXT.md`.
