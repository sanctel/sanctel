// Tauri-backed wiring for AgentSession capture. The polling logic lives in
// agent-session-capture.ts so tests can exercise it without a Tauri host.

import { homeDir } from "@tauri-apps/api/path";
import { exists, open, readDir, stat } from "@tauri-apps/plugin-fs";

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
  async readHeader(path) {
    // Bounded read: 16 KiB is plenty to cover claude's jsonl header
    // (permission-mode + file-history-snapshot + first user message,
    // typically <2 KiB total). Avoids pulling a multi-MB transcript
    // into memory just to extract one cwd field.
    const file = await open(path, { read: true });
    try {
      const buf = new Uint8Array(16 * 1024);
      const n = await file.read(buf);
      const slice = n === null ? new Uint8Array(0) : buf.slice(0, n);
      return new TextDecoder().decode(slice);
    } finally {
      await file.close();
    }
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
