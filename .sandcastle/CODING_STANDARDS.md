# Coding Standards

Loaded by the reviewer agent during code review (via `@.sandcastle/CODING_STANDARDS.md`),
so these standards are enforced without costing tokens during implementation.

Read `docs/agents/review-guardrails.md` for domain vocabulary, architecture
invariants, and documentation update rules. Read `CONTEXT-MAP.md` and relevant
ADRs for the source-of-truth domain context behind those guardrails.

## TypeScript / React (frontend)

### Style
- **Strict mode**, no `any` without an explicit-cast comment explaining why.
- **Named exports only** for components and utilities; default exports only for
  top-level page entry points if any.
- **Function components + hooks**; no class components.
- Files are PascalCase for components (`Sidebar.tsx`), camelCase for utilities
  (`tabStore.ts`), kebab-case for assets (`app.css`).

### State management
- **Zustand only** for cross-cutting state. No Redux, no Context-as-store, no
  Recoil. If a piece of state is truly local, use `useState`.
- Stores live in `src/store/<name>Store.ts` and export a single hook
  (`useTabStore`, `useProfileStore`).
- Mutations are functions on the store object, NEVER `set(...)` from outside.
- Async work goes inside store methods (they may call `invoke(...)`), so
  components stay declarative.

### Tauri IPC
- Frontend never builds Rust state assumptions. Call `invoke("...", { ... })`
  with named args; let Rust own state.
- Frontend computes `profileId` from `space.profileId` before sending to
  `create_tab` — Rust receives the profile name, not space/workspace IDs.
  See ADR-0003 and `docs/agents/review-guardrails.md`.

### Testing
- Vitest for unit + integration. `<name>.test.ts(x)` next to the file under test.
- Test **observable behavior**, not implementation details. Component tests use
  Testing Library; assert on rendered output and store changes, not on hook
  internals.
- New behavior ships with at least one test that fails without the change.

## Rust / Tauri (backend)

### Style
- `cargo fmt` clean. `cargo clippy --all-targets -- -D warnings` clean. The
  reviewer rejects clippy warnings; treat them as errors.
- **No `unwrap()` or `expect()` in non-test code** unless preceded by a
  documented invariant comment explaining why panic is correct.
- Use `?` for error propagation; map to `String` at the Tauri command boundary
  (Tauri's serialization expects this) — internal code uses richer error types.
- Module names singular, type names PascalCase, function names snake_case
  (Rust convention).

### Tauri commands
- All `#[tauri::command]` functions take **named** struct arguments — never
  positional `(a: String, b: String, c: String)` since serde can confuse them.
- Commands must validate inputs at the boundary (URLs parse, paths absolute,
  IDs in registry) — internal helpers can trust their callers.
- Long-running work runs on a separate task; commands return quickly.
- Per-platform code (WKWebView/WebView2/WebKitGTK) lives in
  `src-tauri/src/<feature>_{mac,win,linux}.rs` with a shared `<feature>/mod.rs`
  dispatcher.

### Plugin capability gates
Per ADR-0008 and `docs/design/plugin-system.md`: any privileged plugin call (`fs:*`, `pty:*`, `net:http`,
`exec:cli`, `worktree:*`, `tab:control`) **must** go through a Rust function
that checks the plugin's manifest-declared capabilities. **Frontend cannot lie
about capabilities** — the on-disk manifest is the source of truth.

If you add a new capability:
1. Add the constant to `plugin_capabilities.rs` (or similar).
2. Add a `plugin_<name>.rs` enforcement module.
3. Update `docs/design/plugin-system.md` to document the new tier.

### Testing
- `cargo test` for unit + integration. `#[cfg(test)]` modules co-located with
  source.
- Test boundary contracts (does this command accept what the manifest says it
  accepts? Does it reject what's not declared?).

## Code smells the reviewer will flag

- Unchecked `unwrap()` / `expect()` in command handlers.
- `any` in TypeScript without a comment justification.
- Components reaching into Zustand state with `getState()` outside actions —
  use the hook + selector.
- Tauri commands taking positional args.
- Long blocking work in a Tauri command (should spawn a task and stream
  events).
