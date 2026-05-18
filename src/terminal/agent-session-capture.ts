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
}

export interface AgentSessionCaptureOptions {
  tabId: string;
  worktreePath: string;
  startedAt: number;
  home: string;
  fs: AgentSessionFs;
  onSession: (sessionId: string) => Promise<void> | void;
  intervalMs?: number;
}

export interface AgentSessionCapture {
  stop(): void;
}

export type AgentSessionCaptureStarter = (
  opts: Omit<AgentSessionCaptureOptions, "home" | "fs">,
) => AgentSessionCapture;

const JSONL_RE = /\.jsonl$/;

export function encodeCwd(cwd: string): string {
  const trimmed = cwd.endsWith("/") && cwd.length > 1 ? cwd.slice(0, -1) : cwd;
  return trimmed.replace(/\//g, "-");
}

export function pickFirstSessionAfter(
  entries: readonly AgentSessionFsEntry[],
  startedAt: number,
): string | null {
  let best: AgentSessionFsEntry | null = null;
  for (const e of entries) {
    if (!JSONL_RE.test(e.name)) continue;
    if (e.mtime < startedAt) continue;
    if (!best || e.mtime < best.mtime) best = e;
  }
  if (!best) return null;
  return best.name.replace(JSONL_RE, "");
}

export async function discoverCapturedAgentSession(
  opts: Pick<AgentSessionCaptureOptions, "worktreePath" | "startedAt" | "home" | "fs">,
): Promise<string | null> {
  const dir = `${opts.home}/.claude/projects/${encodeCwd(opts.worktreePath)}`;
  try {
    if (!(await opts.fs.exists(dir))) return null;
    const entries = await opts.fs.readDir(dir);
    return pickFirstSessionAfter(entries, opts.startedAt);
  } catch {
    return null;
  }
}

export function startAgentSessionCapture(
  opts: AgentSessionCaptureOptions,
): AgentSessionCapture {
  const intervalMs = opts.intervalMs ?? 5000;
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | null = null;

  const tick = async () => {
    const sessionId = await discoverCapturedAgentSession(opts);
    if (stopped) return;
    if (sessionId) {
      try {
        await opts.onSession(sessionId);
        stopped = true;
        return;
      } catch {
        // Persistence can fail transiently; keep polling so the verified
        // transcript can be recorded on a later pass.
      }
    }
    timer = setTimeout(tick, intervalMs);
  };

  timer = setTimeout(tick, intervalMs);

  return {
    stop() {
      stopped = true;
      if (timer) clearTimeout(timer);
    },
  };
}
