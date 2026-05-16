import { useTabStore } from "../store/tabStore";
import type { Tab, TabKind } from "../types";

const kindGlyph: Record<TabKind, string> = {
  browser: "○",
  terminal: ">",
  chat: "✱",
  file: "▤",
  diff: "±",
};

export default function Sidebar() {
  const profiles = useTabStore((s) => s.profiles);
  const spaces = useTabStore((s) => s.spaces);
  const activeSpaceId = useTabStore((s) => s.activeSpaceId);
  const activeProfile = useTabStore((s) => s.activeProfile());
  const visibleTabs = useTabStore((s) => s.visibleTabs());
  const activeTab = useTabStore((s) => s.activeTab());
  const spacesForProfile = useTabStore((s) => s.spacesForProfile);

  const newTab = useTabStore((s) => s.newTab);
  const closeTab = useTabStore((s) => s.closeTab);
  const activateTab = useTabStore((s) => s.activateTab);
  const switchSpace = useTabStore((s) => s.switchSpace);
  const addSpace = useTabStore((s) => s.addSpace);
  const addProfile = useTabStore((s) => s.addProfile);

  // Only show profile UI when there's more than one profile (Arc-style hide-when-trivial).
  const showProfiles = profiles.length > 1;

  // Show only the spaces belonging to the active profile (Arc invariant:
  // a Space belongs to exactly one Profile; switching Spaces may switch Profiles).
  const currentProfileSpaces = activeProfile
    ? spacesForProfile(activeProfile.id)
    : spaces;

  return (
    <aside className="sidebar">
      {showProfiles && (
        <div className="profile-bar">
          {profiles.map((p) => {
            const firstSpace = spacesForProfile(p.id)[0];
            const isActive = activeProfile?.id === p.id;
            return (
              <button
                key={p.id}
                className={`profile-pill ${isActive ? "active" : ""}`}
                style={p.color ? { background: p.color } : undefined}
                onClick={() => firstSpace && switchSpace(firstSpace.id)}
                title={p.name}
              >
                {p.name[0]?.toUpperCase() ?? "?"}
              </button>
            );
          })}
          <button
            className="profile-add"
            onClick={() => addProfile(`Profile ${profiles.length + 1}`)}
            title="Add profile"
          >
            +
          </button>
        </div>
      )}

      <div className="space-list">
        {currentProfileSpaces.map((sp) => (
          <button
            key={sp.id}
            className={`space-pill ${sp.id === activeSpaceId ? "active" : ""}`}
            style={{ background: sp.color }}
            onClick={() => switchSpace(sp.id)}
            title={sp.name}
          >
            {sp.name[0]?.toUpperCase()}
          </button>
        ))}
        <button
          className="space-add"
          onClick={() => addSpace(`Space ${currentProfileSpaces.length + 1}`)}
          title="Add space"
        >
          +
        </button>
      </div>

      {!showProfiles && (
        <button
          className="add-profile-hint"
          onClick={() => addProfile(`Profile 2`)}
          title="Add a second profile"
        >
          + new profile
        </button>
      )}

      <div className="new-buttons">
        <button onClick={() => newTab("browser", "")}>+ Browser</button>
        <button onClick={() => newTab("terminal", "")}>+ Terminal</button>
        <button onClick={() => newTab("chat", "")}>+ Chat</button>
      </div>

      <ul className="tab-list">
        {visibleTabs.map((t) => (
          <TabRow
            key={t.id}
            tab={t}
            active={activeTab?.id === t.id}
            onActivate={() => activateTab(t.id)}
            onClose={() => closeTab(t.id)}
          />
        ))}
      </ul>
    </aside>
  );
}

function TabRow({
  tab,
  active,
  onActivate,
  onClose,
}: {
  tab: Tab;
  active: boolean;
  onActivate: () => void;
  onClose: () => void;
}) {
  return (
    <li
      className={`tab-row ${active ? "active" : ""} kind-${tab.kind}`}
      onClick={onActivate}
    >
      <span className="kind-glyph">{kindGlyph[tab.kind]}</span>
      <span className="title">{tab.title}</span>
      <button
        className="close"
        onClick={(e) => {
          e.stopPropagation();
          onClose();
        }}
      >
        ×
      </button>
    </li>
  );
}
