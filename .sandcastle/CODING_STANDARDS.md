# Coding Standards

Loaded by the reviewer agent during code review (via `@.sandcastle/CODING_STANDARDS.md`),
so these standards are enforced without costing tokens during implementation.

**Read `/CONTEXT.md` first** for domain vocabulary (Profile/Space/Tab/Worktree)
and the planned architecture (plugin system §6, file editor §7, agent↔browser §8).
The standards below assume that vocabulary.

## Domain vocabulary (use these names — NOT alternatives)

| Use | Not | Reason |
|---|---|---|
| `Profile` | `Workspace`, `Account` | identity / cookie boundary (Arc model, CONTEXT §3) |
| `Space` | `Workspace`, `Group`, `Project` | organizational grouping inside a Profile (CONTEXT §3) |
| `Tab` | `Pane`, `Window` | atomic sidebar entry, one webview each |
| `TabKind` | `TabType` | enum: `browser | terminal | chat | file | diff` |
| `Worktree` | `Branch dir`, `Workspace dir` | a real `git worktree`; filesystem entity |
| `AgentSession` | `ChatSession`, `Conversation` | a Claude/Codex thread, keyed by cwd |
| `TmuxSession` | `Shell`, `Pty` | server-side tmux session for persistence |

Three names that meant different things across our references and which we
have explicitly disambiguated — review will reject misuse:

- "Session" must be qualified: `TmuxSession`, `AgentSession`, or a UI `Tab`.
- "Workspace" must not appear in type names — use `Space` or `Profile`.
- "Window" = OS / Tauri window only. tmux windows map to our `Tab`.

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
  (This is the Arc-model invariant from CONTEXT §3.)

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
Per CONTEXT §6: any privileged plugin call (`fs:*`, `pty:*`, `net:http`,
`exec:cli`, `worktree:*`, `tab:control`) **must** go through a Rust function
that checks the plugin's manifest-declared capabilities. **Frontend cannot lie
about capabilities** — the on-disk manifest is the source of truth.

If you add a new capability:
1. Add the constant to `plugin_capabilities.rs` (or similar).
2. Add a `plugin_<name>.rs` enforcement module.
3. Update CONTEXT §6 to document the new tier.

### Testing
- `cargo test` for unit + integration. `#[cfg(test)]` modules co-located with
  source.
- Test boundary contracts (does this command accept what the manifest says it
  accepts? Does it reject what's not declared?).

## Architecture invariants (review will enforce)

Drawn from CONTEXT.md. Violations are blockers, not nits.

### Persistence Anchor (CONTEXT §3)
- Tabs are ephemeral pointers. Worktrees, transcripts, profile data dirs, and
  tmux sessions are durable. Code that tries to make Tab the source of truth
  for something durable is wrong.
- On launch, the app reconstructs Tabs from disk; it does NOT save Tab
  in-memory state to disk during normal operation.

### Profile invariants (CONTEXT §3)
- A Space belongs to exactly one Profile. `Tab.profileId` is **derived from
  Space.profileId**, never stored separately. Reviewer rejects denormalized
  `profileId` on Tab.
- `WebviewBuilder::with_profile_name(...)` takes the **profile id**, never
  the space id. Test this with a "two spaces share one profile, cookies are
  shared" assertion when feasible.

### TabKind unification (CONTEXT §3 + §7)
- New tab kinds add a `kind` enum variant + a bundled HTML page (or external
  URL for browser). They do NOT add new tab-specific data structures unless
  truly necessary.

### Worktree orthogonality (CONTEXT §3)
- A Worktree is a filesystem entity. Don't tie it to Profile in code. Don't
  store `profileId` on Worktree.

### TUICommander-style plugin capabilities (CONTEXT §6)
- New plugin APIs MUST declare a capability tier (1/2/3/4).
- Tier 3/4 privileged operations require manifest-declared capabilities,
  checked at the Rust boundary.

## Architecture/code "smells" the reviewer will flag

- `Workspace` in a type or variable name (use `Profile` or `Space`).
- `Tab.profileId` stored directly (it's derived; should be `space.profileId`).
- `with_profile_name(space.id)` (it should be `with_profile_name(profile.id)`).
- Unchecked `unwrap()` / `expect()` in command handlers.
- `any` in TypeScript without a comment justification.
- Components reaching into Zustand state with `getState()` outside actions —
  use the hook + selector.
- Tauri commands taking positional args.
- Long blocking work in a Tauri command (should spawn a task and stream
  events).
- New plugin capability without a matching Rust enforcement module.
- New TabKind without updating CONTEXT §3 table.

## Documentation discipline

- New architectural decision → CONTEXT.md §2 decisions table (one row, with
  rejected alternatives in the right column).
- New planned subsystem → its own CONTEXT.md section with the same shape as
  §6/§7/§8: status banner, decision, alternatives rejected, implementation
  order, what's not in v1, references to study.
- README.md is the build guide / extension plan; CONTEXT.md is the domain.
- Don't duplicate. README points at CONTEXT, never re-states.
