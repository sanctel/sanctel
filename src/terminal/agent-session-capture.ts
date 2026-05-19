// Capture a chat Tab's own Claude AgentSession after `claude` starts.
//
// Claude stores transcripts under `~/.claude/projects/<encoded-cwd>/*.jsonl`.
// A fresh chat Tab starts plain `claude`; once a jsonl appears after that
// start time, the filename is the verified resume target for that Tab.

export interface AgentSessionFsEntry {
  name: string;
  mtime: number;
}

export interface AgentSessionFs {
  exists(path: string): Promise<boolean>;
  readDir(path: string): Promise<AgentSessionFsEntry[]>;
  readFirstLine(path: string): Promise<string>;
}

export interface AgentSessionCaptureOptions {
  tabId: string;
  worktreePath: string;
  startedAt: number;
  home: string;
  fs: AgentSessionFs;
  onSession: (sessionId: string) => Promise<void> | void;
  intervalMs?: number;
  maxDurationMs?: number;
}

export interface AgentSessionCapture {
  stop(): void;
}

export type AgentSessionCaptureStarter = (
  opts: Omit<AgentSessionCaptureOptions, "home" | "fs">,
) => AgentSessionCapture;

type AgentSessionDiscoveryOptions = Pick<
  AgentSessionCaptureOptions,
  "worktreePath" | "startedAt" | "home" | "fs"
>;

const JSONL_RE = /\.jsonl$/;
const DEFAULT_MAX_DURATION_MS = 30 * 60 * 1000;

export function encodeCwd(cwd: string): string {
  const trimmed = cwd.endsWith("/") && cwd.length > 1 ? cwd.slice(0, -1) : cwd;
  return trimmed.replace(/\//g, "-");
}

export function pickFirstSessionAfter(
  entries: readonly AgentSessionFsEntry[],
  startedAt: number,
): string | null {
  let earliest: AgentSessionFsEntry | null = null;
  for (const e of entries) {
    if (!JSONL_RE.test(e.name)) continue;
    if (e.mtime < startedAt) continue;
    if (!earliest || e.mtime < earliest.mtime) earliest = e;
  }
  if (!earliest) return null;
  return earliest.name.replace(JSONL_RE, "");
}

export async function discoverCapturedAgentSession(
  opts: AgentSessionDiscoveryOptions,
): Promise<string | null> {
  const dir = `${opts.home}/.claude/projects/${encodeCwd(opts.worktreePath)}`;
  try {
    if (!(await opts.fs.exists(dir))) return null;
    const entries = await opts.fs.readDir(dir);
    const candidates = entries
      .filter((e) => JSONL_RE.test(e.name) && e.mtime >= opts.startedAt)
      .sort((a, b) => a.mtime - b.mtime);
    for (const entry of candidates) {
      const path = `${dir}/${entry.name}`;
      if (await jsonlBelongsToWorktree(opts.fs, path, opts.worktreePath)) {
        return entry.name.replace(JSONL_RE, "");
      }
    }
    return null;
  } catch {
    return null;
  }
}

async function jsonlBelongsToWorktree(
  fs: AgentSessionFs,
  path: string,
  worktreePath: string,
): Promise<boolean> {
  try {
    const firstLine = await fs.readFirstLine(path);
    const cwd = JSON.parse(firstLine) as { cwd?: unknown };
    if (typeof cwd.cwd !== "string") return false;
    return normalizeCwd(cwd.cwd) === normalizeCwd(worktreePath);
  } catch {
    return false;
  }
}

function normalizeCwd(cwd: string): string {
  const trimmed = cwd.endsWith("/") && cwd.length > 1 ? cwd.slice(0, -1) : cwd;
  return trimmed.toLocaleLowerCase();
}

export function startAgentSessionCapture(
  opts: AgentSessionCaptureOptions,
): AgentSessionCapture {
  const intervalMs = opts.intervalMs ?? 5000;
  const deadlineAt = Date.now() + (opts.maxDurationMs ?? DEFAULT_MAX_DURATION_MS);
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | null = null;

  const stop = () => {
    stopped = true;
    if (timer) clearTimeout(timer);
    timer = null;
  };

  const scheduleNext = () => {
    if (stopped) return;
    if (Date.now() >= deadlineAt) {
      stop();
      return;
    }
    timer = setTimeout(tick, intervalMs);
  };

  const tick = async () => {
    if (Date.now() >= deadlineAt) {
      stop();
      return;
    }
    const sessionId = await discoverCapturedAgentSession(opts);
    if (stopped) return;
    if (sessionId) {
      try {
        await opts.onSession(sessionId);
        stop();
        return;
      } catch {
        // Persistence can fail transiently; keep polling so the verified
        // transcript can be recorded on a later pass.
      }
    }
    scheduleNext();
  };

  scheduleNext();

  return {
    stop,
  };
}
