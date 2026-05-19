# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Layout

This is a multi-context repo.

- Read `CONTEXT-MAP.md` first. It points at the relevant context glossaries and explains how contexts relate.
- Read `src/core/CONTEXT.md` for Core terms: Profile, Space, Tab, TabKind, Worktree, Project, AgentSession, and TmuxSession.
- Read ADRs under `docs/adr/` that touch the area being changed.
- Specialized contexts create their own `CONTEXT.md` lazily when code lands in their directories.

## Use the glossary's vocabulary

When output names a domain concept, use the term as defined in the relevant `CONTEXT.md`. Do not drift to synonyms the glossary explicitly avoids.

If the needed concept is missing from the glossary, either reconsider the language or use `grill-with-docs` to resolve the term before adding it.

## Flag ADR conflicts

If output contradicts an existing ADR, surface it explicitly rather than silently overriding it.
