# 0003 — Profile is the identity boundary, not Space

**Status:** Accepted

**Decision:** Cookies / localStorage / IndexedDB / history / passwords are
scoped to **Profile**, never to Space. A Space inherits its identity from
its Profile via `Profile.id` → a per-platform Tauri 2.11 WebView API,
applied in one place (`profile_isolation::apply_profile_isolation`):

- **Windows (WebView2) / Linux (WebKitGTK):**
  `WebviewBuilder::data_directory(<app-local>/profiles/<profile_id>)`.
  Both runtimes honor `data_directory` for cookies, IndexedDB, and
  localStorage — same path → same store.
- **macOS (WKWebView) / iOS (≥ 17):**
  `WebviewBuilder::data_store_identifier([u8; 16])`. WKWebView ignores
  `data_directory` for cookie / localStorage isolation in Tauri 2.11,
  so a separate API is required. The 16-byte identifier is derived
  deterministically as `UUIDv5(SANCTEL_PROFILE_NAMESPACE, profile_id)`
  so same `profile_id` always produces the same `WKWebsiteDataStore`
  and cookies survive sanctel restart.

This is the Arc model.

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
- The per-platform branching lives in exactly one helper. `create_tab`
  itself contains no `cfg(target_os)` attributes — adding a new platform
  is a change to `profile_isolation`, not a sprinkle across the codebase.
- The macOS `SANCTEL_PROFILE_NAMESPACE` UUID is load-bearing: changing
  its 16 bytes silently invalidates every existing user's WKWebsiteDataStore.
  Treat it as a forever-constant; any migration needs an explicit data-store
  migration step.
