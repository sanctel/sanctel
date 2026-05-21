// Production Persistence implementation: sql.js (wasm SQLite) +
// `@tauri-apps/plugin-fs` for durable storage. Rust never touches the
// .db file — every per-Tab fact reaches Rust through `create_tab`
// arguments, satisfying the matching acceptance criterion.
//
// The DB file lives at `sanctel.db` under Tauri's `AppLocalData` base
// directory. On launch we read the bytes, hand them to sql.js, and run
// the schema (CREATE IF NOT EXISTS is idempotent). On every mutation we
// serialize the in-memory DB back to disk so a crash mid-session never
// loses more than the last write.
//
// Writing the full DB on every mutation is fine for sanctel's scale —
// dozens of tabs, kilobytes total. If state grows we can swap to
// debounced or journal-mode writes; the Persistence interface hides it.

import initSqlJs, { type Database, type SqlJsStatic } from "sql.js";
import sqlWasmUrl from "sql.js/dist/sql-wasm.wasm?url";
import {
  BaseDirectory,
  exists,
  mkdir,
  readFile,
  writeFile,
} from "@tauri-apps/plugin-fs";

import { SCHEMA_V1 } from "./schema";
import type {
  PersistedProfile,
  PersistedSpace,
  PersistedTab,
  Persistence,
  Snapshot,
} from "./persistence";

const DB_FILE = "sanctel.db";
const DB_DIR: BaseDirectory = BaseDirectory.AppLocalData;

export class SqlPersistence implements Persistence {
  private db!: Database;
  private SQL!: SqlJsStatic;

  async init(): Promise<void> {
    this.SQL = await initSqlJs({ locateFile: () => sqlWasmUrl });

    if (await exists(DB_FILE, { baseDir: DB_DIR })) {
      const bytes = await readFile(DB_FILE, { baseDir: DB_DIR });
      this.db = new this.SQL.Database(bytes);
    } else {
      // First run: ensure the AppLocalData dir exists, then create an
      // empty DB. mkdir with recursive is a no-op when the dir is there.
      await mkdir("", { baseDir: DB_DIR, recursive: true }).catch(() => {});
      this.db = new this.SQL.Database();
    }
    this.db.run(SCHEMA_V1);
    await this.flush();
  }

  async loadAll(): Promise<Snapshot> {
    const profiles = this.selectAll<PersistedProfile>(
      "SELECT id, name, color, is_default AS isDefault FROM profiles",
      (r) => ({
        id: r.id as string,
        name: r.name as string,
        color: (r.color as string | null) ?? null,
        isDefault: !!r.isDefault,
      }),
    );
    const spaces = this.selectAll<PersistedSpace>(
      "SELECT id, profile_id AS profileId, name, color, sort_order AS sortOrder " +
        "FROM spaces ORDER BY sort_order ASC",
      (r) => ({
        id: r.id as string,
        profileId: r.profileId as string,
        name: r.name as string,
        color: r.color as string,
        sortOrder: r.sortOrder as number,
      }),
    );
    const tabs = this.selectAll<PersistedTab>(
      "SELECT id, space_id AS spaceId, kind, title, sort_order AS sortOrder, " +
        "url, worktree_id AS worktreeId, window_name AS windowName, " +
        "initial_command AS initialCommand, agent_session_id AS agentSessionId " +
        "FROM tabs ORDER BY sort_order ASC",
      (r) => ({
        id: r.id as string,
        spaceId: r.spaceId as string,
        kind: r.kind as PersistedTab["kind"],
        title: r.title as string,
        sortOrder: r.sortOrder as number,
        url: (r.url as string | null) ?? null,
        worktreeId: (r.worktreeId as string | null) ?? null,
        windowName: (r.windowName as string | null) ?? null,
        initialCommand: (r.initialCommand as string | null) ?? null,
        agentSessionId: (r.agentSessionId as string | null) ?? null,
      }),
    );
    return { profiles, spaces, tabs };
  }

  async saveProfile(p: PersistedProfile): Promise<void> {
    this.db.run(
      "INSERT INTO profiles (id, name, color, is_default) VALUES (?, ?, ?, ?) " +
        "ON CONFLICT(id) DO UPDATE SET name = excluded.name, color = excluded.color, is_default = excluded.is_default",
      [p.id, p.name, p.color, p.isDefault ? 1 : 0],
    );
    await this.flush();
  }

  async saveSpace(s: PersistedSpace): Promise<void> {
    this.db.run(
      "INSERT INTO spaces (id, profile_id, name, color, sort_order) VALUES (?, ?, ?, ?, ?) " +
        "ON CONFLICT(id) DO UPDATE SET profile_id = excluded.profile_id, name = excluded.name, color = excluded.color, sort_order = excluded.sort_order",
      [s.id, s.profileId, s.name, s.color, s.sortOrder],
    );
    await this.flush();
  }

  async saveTab(t: PersistedTab): Promise<void> {
    this.db.run(
      "INSERT INTO tabs (id, space_id, kind, title, sort_order, url, worktree_id, window_name, initial_command, agent_session_id) " +
        "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) " +
        "ON CONFLICT(id) DO UPDATE SET " +
        "  space_id = excluded.space_id, kind = excluded.kind, title = excluded.title, sort_order = excluded.sort_order, " +
        "  url = excluded.url, worktree_id = excluded.worktree_id, window_name = excluded.window_name, " +
        "  initial_command = excluded.initial_command, agent_session_id = excluded.agent_session_id",
      [
        t.id,
        t.spaceId,
        t.kind,
        t.title,
        t.sortOrder,
        t.url,
        t.worktreeId,
        t.windowName,
        t.initialCommand,
        t.agentSessionId,
      ],
    );
    await this.flush();
  }

  async renameTab(id: string, title: string): Promise<void> {
    this.db.run("UPDATE tabs SET title = ? WHERE id = ?", [title, id]);
    await this.flush();
  }

  async updateTabAgentSession(
    id: string,
    agentSessionId: string,
  ): Promise<void> {
    this.db.run(
      "UPDATE tabs SET agent_session_id = ? WHERE id = ?",
      [agentSessionId, id],
    );
    await this.flush();
  }

  async removeTab(id: string): Promise<void> {
    this.db.run("DELETE FROM tabs WHERE id = ?", [id]);
    await this.flush();
  }

  async reorderTabs(spaceId: string, orderedIds: string[]): Promise<void> {
    this.db.run("BEGIN");
    try {
      orderedIds.forEach((tabId, i) => {
        this.db.run(
          "UPDATE tabs SET sort_order = ? WHERE id = ? AND space_id = ?",
          [i, tabId, spaceId],
        );
      });
      this.db.run("COMMIT");
    } catch (e) {
      this.db.run("ROLLBACK");
      throw e;
    }
    await this.flush();
  }

  private selectAll<T>(
    sql: string,
    map: (row: Record<string, unknown>) => T,
  ): T[] {
    const stmt = this.db.prepare(sql);
    const out: T[] = [];
    while (stmt.step()) {
      out.push(map(stmt.getAsObject()));
    }
    stmt.free();
    return out;
  }

  private async flush(): Promise<void> {
    const bytes = this.db.export();
    await writeFile(DB_FILE, bytes, { baseDir: DB_DIR });
  }
}
