// Agent-session discovery (Slice 6 / issue #7).
//
// Given a Worktree's cwd, find the newest Claude session id for it by
// scanning `~/.claude/projects/<encoded-cwd>/` for *.jsonl files and
// returning the basename of the newest one. The result is plugged into
// `initialCommand = "claude --resume <agentSessionId>"` so a chat tab
// rehydrates the same conversation after a tmux server restart.
//
// Claude encodes a cwd into the projects dir name by replacing every `/`
// with `-`. We mirror that exactly — keeping the leading `-` from the
// absolute path's leading slash.
//
// Filesystem access is injected so the module is testable with a fixture
// fs adapter; the production wiring uses `@tauri-apps/plugin-fs`.

export interface AgentSessionFsEntry {
  name: string;
  mtime: number;
}

export interface AgentSessionFs {
  exists(path: string): Promise<boolean>;
  readDir(path: string): Promise<AgentSessionFsEntry[]>;
}

export function encodeCwd(cwd: string): string {
  const trimmed = cwd.endsWith("/") && cwd.length > 1 ? cwd.slice(0, -1) : cwd;
  return trimmed.replace(/\//g, "-");
}

const JSONL_RE = /\.jsonl$/;

export function pickNewestSessionId(
  entries: readonly AgentSessionFsEntry[],
): string | null {
  let best: AgentSessionFsEntry | null = null;
  for (const e of entries) {
    if (!JSONL_RE.test(e.name)) continue;
    if (!best || e.mtime > best.mtime) best = e;
  }
  if (!best) return null;
  return best.name.replace(JSONL_RE, "");
}

export interface DiscoverOptions {
  cwd: string;
  home: string;
  fs: AgentSessionFs;
}

export async function discoverAgentSession(
  opts: DiscoverOptions,
): Promise<string | null> {
  const dir = `${opts.home}/.claude/projects/${encodeCwd(opts.cwd)}`;
  try {
    if (!(await opts.fs.exists(dir))) return null;
    const entries = await opts.fs.readDir(dir);
    return pickNewestSessionId(entries);
  } catch {
    // A failed read (permission, transient FS error) must not block chat
    // tab creation — fall back to plain `claude` and let the user start
    // a fresh conversation.
    return null;
  }
}
