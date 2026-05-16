# Sanctel

## Behavioural guidelines

### 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:

- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

### 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

### 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:

- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:

- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

### 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:

- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:

```text
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make it work") require constant clarification.

## Biases

### Explanation Biases

- Prefer concrete, visual explanations over prose-only explanations when they improve understanding: code sketches, before/after snippets, Mermaid diagrams, tables, dependency maps, and small workflow graphs.
- Show the main moving parts directly when discussing plans, reviews, or tradeoffs. Use compact snippets and diagrams to make the reasoning inspectable.

### Architecture Biases

- Small interfaces, deep modules. Expose narrow APIs; hide complexity behind them.
- Validate at boundaries. Reject invalid input at API, DB, and integration edges.
- Name things by domain, not implementation. Prefer domain terms over technical placeholders.
- Co-locate what changes together. Organize by feature and reason to change.
- Screaming Architecture. Let the structure reflect the product before the framework.

### Testing Biases

- New behavior ships with tests.
- Test observable behavior, not implementation details.
- Refactors preserve behavior. Update tests only when behavior changes.
- Prefer the fastest test that gives confidence. Use integration tests when behavior crosses boundaries.

## Companion documents

- **[CONTEXT-MAP.md](./CONTEXT-MAP.md)** — domain entry point; map of
  contexts and how they relate
- **[src/core/CONTEXT.md](./src/core/CONTEXT.md)** — Core glossary
  (Profile / Space / Tab / Worktree)
- **[docs/adr/](./docs/adr/)** — architectural decisions; start here when
  any "why" question comes up
- **[docs/design/](./docs/design/)** — design docs for planned subsystems
- **[.sandcastle/CODING_STANDARDS.md](./.sandcastle/CODING_STANDARDS.md)** —
  enforced at code review
- **[CONTRIBUTING.md](./CONTRIBUTING.md)** — setup, prerequisites, reading
  order

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
