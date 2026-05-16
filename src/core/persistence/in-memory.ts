// In-memory Persistence implementation used by tests. Behaves like a
// SQLite-backed store: stable identifiers, sort-order semantics, and the
// same surface as `SqlPersistence`. Production never imports this.

import type {
  PersistedProfile,
  PersistedSpace,
  PersistedTab,
  Persistence,
  Snapshot,
} from "./persistence";

export class InMemoryPersistence implements Persistence {
  private profiles = new Map<string, PersistedProfile>();
  private spaces = new Map<string, PersistedSpace>();
  private tabs = new Map<string, PersistedTab>();

  async init(): Promise<void> {
    // No-op — schema is implicit in the typed maps.
  }

  async loadAll(): Promise<Snapshot> {
    return {
      profiles: [...this.profiles.values()],
      spaces: [...this.spaces.values()].sort(
        (a, b) => a.sortOrder - b.sortOrder,
      ),
      tabs: [...this.tabs.values()].sort(
        (a, b) => a.sortOrder - b.sortOrder,
      ),
    };
  }

  async saveProfile(p: PersistedProfile): Promise<void> {
    this.profiles.set(p.id, { ...p });
  }

  async saveSpace(s: PersistedSpace): Promise<void> {
    this.spaces.set(s.id, { ...s });
  }

  async saveTab(t: PersistedTab): Promise<void> {
    this.tabs.set(t.id, { ...t });
  }

  async renameTab(id: string, title: string): Promise<void> {
    const t = this.tabs.get(id);
    if (t) this.tabs.set(id, { ...t, title });
  }

  async removeTab(id: string): Promise<void> {
    this.tabs.delete(id);
  }

  async reorderTabs(spaceId: string, orderedIds: string[]): Promise<void> {
    orderedIds.forEach((tabId, i) => {
      const t = this.tabs.get(tabId);
      if (t && t.spaceId === spaceId) {
        this.tabs.set(tabId, { ...t, sortOrder: i });
      }
    });
  }
}
