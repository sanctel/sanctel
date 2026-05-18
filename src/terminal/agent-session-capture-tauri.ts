// Tauri-backed wiring for AgentSession capture. The polling logic lives in
// agent-session-capture.ts so tests can exercise it without a Tauri host.

import { homeDir } from "@tauri-apps/api/path";
import { exists, readDir, stat } from "@tauri-apps/plugin-fs";

import {
  startAgentSessionCapture as startCapture,
  type AgentSessionCapture,
  type AgentSessionCaptureStarter,
  type AgentSessionFs,
  type AgentSessionFsEntry,
} from "./agent-session-capture";

export type { AgentSessionCaptureStarter } from "./agent-session-capture";

const tauriFs: AgentSessionFs = {
  exists,
  async readDir(path) {
    const entries = await readDir(path);
    const out: AgentSessionFsEntry[] = [];
    for (const e of entries) {
      if (!e.isFile) continue;
      try {
        const s = await stat(`${path}/${e.name}`);
        out.push({ name: e.name, mtime: s.mtime?.getTime() ?? 0 });
      } catch {
        // Skip entries that disappear between readDir and stat.
      }
    }
    return out;
  },
};

export const startAgentSessionCapture: AgentSessionCaptureStarter = (opts) => {
  let stopped = false;
  let inner: AgentSessionCapture | null = null;

  homeDir()
    .then((home) => {
      if (stopped) return;
      inner = startCapture({
        ...opts,
        home: stripTrailingSlash(home),
        fs: tauriFs,
      });
    })
    .catch(() => {});

  return {
    stop() {
      stopped = true;
      inner?.stop();
    },
  };
};

function stripTrailingSlash(p: string): string {
  return p.endsWith("/") && p.length > 1 ? p.slice(0, -1) : p;
}
