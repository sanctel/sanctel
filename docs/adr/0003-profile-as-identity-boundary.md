# 0003 — Profile is the identity boundary, not Space

**Status:** Accepted

**Decision:** Cookies / localStorage / IndexedDB / history / passwords are
scoped to **Profile**, never to Space. A Space inherits its identity from
its Profile via `Profile.id` → Tauri's
`WebviewBuilder::with_profile_name(profile_id)`. This is the Arc model.

## Considered options

- **Per-Space isolation (Bushido's model)** — every Space is its own cookie
  jar. Forces re-login when reorganizing tabs across Spaces.
- **Shared cookie store (Aizen's current state)** — everything sees
  everything; no identity separation. Wrong for our users (Work vs Personal
  GitHub).
- **Per-window apps** — one OS window per identity; clunky and breaks
  Cmd+K-style cross-Space navigation.

## Consequences

- A Space belongs to exactly one Profile. Many Spaces can share one Profile
  (intended: e.g., five Work-context Spaces share the Work cookies).
- `Tab.profileId` is **derived** from `Space.profileId` — never stored on
  Tab directly. Review rejects denormalized `profileId` on Tab.
- 90% of users have one Profile forever. UI surfaces the Profile concept
  only when the user creates a second one (Arc-style hide-when-trivial).
- Switching Spaces may implicitly switch Profiles when the destination
  Space is on a different Profile.
