// Tauri-backed wiring for agent-session-discovery. Lives in its own file so
// the pure module (`agent-session-discovery.ts`) stays free of Tauri imports
// and can be unit-tested with a fixture fs.

import { homeDir } from "@tauri-apps/api/path";
import { exists, readDir, stat } from "@tauri-apps/plugin-fs";

import {
  discoverAgentSession,
  type AgentSessionFs,
  type AgentSessionFsEntry,
} from "./agent-session-discovery";

const tauriFs: AgentSessionFs = {
  async exists(path) {
    return exists(path);
  },
  async readDir(path) {
    const entries = await readDir(path);
    const out: AgentSessionFsEntry[] = [];
    for (const e of entries) {
      try {
        const s = await stat(`${path}/${e.name}`);
        // mtime can be null on platforms that don't expose it; treat as 0
        // so the entry still ranks below files with a real mtime.
        const t =
          s.mtime instanceof Date ? s.mtime.getTime() : Number(s.mtime ?? 0);
        out.push({ name: e.name, mtime: t });
      } catch {
        // Skip unreadable entries — they shouldn't block discovery.
      }
    }
    return out;
  },
};

export async function discoverAgentSessionForWorktree(
  worktreePath: string,
): Promise<string | null> {
  try {
    const home = await homeDir();
    return await discoverAgentSession({
      cwd: worktreePath,
      home: stripTrailingSlash(home),
      fs: tauriFs,
    });
  } catch {
    return null;
  }
}

function stripTrailingSlash(p: string): string {
  return p.endsWith("/") && p.length > 1 ? p.slice(0, -1) : p;
}
