// Backend-aware copy for the startup setup screen. Pure so the branching
// is unit-testable without rendering React. Defensive default: any value
// other than `"zellij"` falls back to the tmux copy so a missing or
// malformed `backend` field off the wire renders the existing UI rather
// than blank.

export interface SetupScreenCopy {
  heading: string;
  intro: string;
  install: string;
}

const TMUX_COPY: SetupScreenCopy = {
  heading: "Sanctel needs tmux",
  intro:
    "Sanctel's terminal and chat tabs are backed by tmux. Install it from your package manager and relaunch:",
  install:
    "# macOS\nbrew install tmux\n\n# Debian / Ubuntu\nsudo apt install tmux",
};

const ZELLIJ_COPY: SetupScreenCopy = {
  heading: "Sanctel needs zellij",
  intro:
    "Sanctel's terminal and chat tabs are backed by zellij. Install it and relaunch:",
  install:
    "# macOS\nbrew install zellij\n\n# Linux\ncargo install --locked zellij\n# or grab a prebuilt release tarball from\n# https://github.com/zellij-org/zellij/releases",
};

export function setupScreenCopy(backend: string | undefined | null): SetupScreenCopy {
  return backend === "zellij" ? ZELLIJ_COPY : TMUX_COPY;
}
