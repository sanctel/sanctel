// ───────────────────────────────────────────────────────────────────────────
// Domain model
//
// In-app hierarchy (cookies & visual organization):
//   Profile (identity, cookies)
//      └── Space  (organizational, color, tab list)
//             └── Tab  (atomic unit, kind: browser/terminal/chat)
//
// Filesystem entities (orthogonal to the above):
//   Project  (a git repo on disk)
//      └── Worktree  (one per active branch, may include the main checkout)
//
//   AgentSession  (a Claude/Codex transcript, keyed by cwd path)
//   TmuxSession   (a tmux server-side session, outlives the app)
//
// Tabs BRIDGE the two worlds:
//   - tab.spaceId       → Space   (visual grouping)
//   - tab.worktreeId?   → Worktree (filesystem cwd; optional)
//   - tab.sessionId?    → AgentSession or TmuxSession handle
//
// The deeper insight: Tabs are ephemeral. Profiles, Worktrees, transcripts,
// and tmux sessions are durable. On app launch, Tabs are reconstructed by
// their references to the durable entities. See CONTEXT.md §3 + §4.
// ───────────────────────────────────────────────────────────────────────────

// "file" and "diff" are planned (see CONTEXT.md §7); not implemented yet.
// They share the unification insight: each is just a webview loading a
// bundled local page that knows how to render that content type.
export type TabKind = "browser" | "terminal" | "chat" | "file" | "diff";

// Profile = the cookie-isolation boundary. Maps 1:1 to Tauri's
// `WebviewBuilder::with_profile_name`. Most users have one ("Default");
// power users have 2-3 ("Work", "Personal", maybe "Client X").
export interface Profile {
  id: string;        // ← passed to Tauri as profile_name
  name: string;      // user-facing label
  color?: string;    // optional accent color on the profile badge
  isDefault: boolean;
}

// Space = the organizational layer (Arc's "Space"). Belongs to exactly one
// Profile. Switching Spaces may implicitly switch Profiles if the destination
// Space is on a different Profile.
export interface Space {
  id: string;
  name: string;
  color: string;            // sidebar accent (visible)
  profileId: Profile["id"]; // ← which Profile this Space's tabs inherit cookies from
  activeTabId: string | null;
}

// Project = a git repo on disk. Orthogonal to Profile/Space — a Project is
// not "inside" a Profile; it's a filesystem entity that any Space's tabs
// can reference.
export interface Project {
  id: string;
  name: string;          // e.g., "maze-monorepo"
  path: string;          // absolute filesystem path to the main checkout
  defaultBranch?: string;
}

// Worktree = a git worktree on disk. One per active branch (including the
// main checkout). Also a filesystem entity, orthogonal to Profile/Space.
//
// Tab.worktreeId optionally references a Worktree:
//   - terminal tabs: usually attached (their cwd IS the worktree path)
//   - browser tabs: usually null
//   - chat tabs: optional (chat may be about a specific task/worktree)
export interface Worktree {
  id: string;
  projectId: Project["id"];
  branch: string;
  path: string;          // absolute filesystem path to the worktree dir
  status?: "active" | "merged" | "removed" | "stale";
}

export interface Tab {
  id: string;                  // also used as the Tauri webview label
  kind: TabKind;
  title: string;
  // For browser tabs: the URL the webview is showing.
  // For terminal/chat tabs: the local page identifier.
  url: string;
  spaceId: Space["id"];        // visual grouping; cookie isolation via space.profileId
  // Filesystem attachment. Independent of spaceId — a Space can hold tabs
  // for many worktrees; a worktree can have tabs in many Spaces.
  worktreeId?: Worktree["id"];
  // For terminal/chat: handle to a backend session (tmux session name,
  // agent run ID). Reconstructable from worktreeId + tab kind in many cases.
  sessionId?: string;
  loading: boolean;
  favicon?: string;
}

// Request sent to Rust when creating a tab. The frontend computes the
// profileId (via space.profileId) and passes it directly. Rust doesn't
// need to know about Space; it just needs the profile_name and (for
// terminal tabs) the cwd from the worktree.
export interface CreateTabRequest {
  id: string;
  kind: TabKind;
  url: string;
  profileId: string;     // ← Rust uses this for WebviewBuilder::with_profile_name
  cwd?: string;          // ← worktree.path, for terminal tabs (Rust spawns PTY here)
  sessionId?: string;
}

// Layout signal from frontend → Rust telling it where to position the active
// webview. The React shell measures the content area and reports its rect.
export interface ContentRect {
  x: number;
  y: number;
  w: number;
  h: number;
}
