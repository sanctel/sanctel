// Copy for the startup setup screen when the tmux probe failed.
// Kept in its own module so React rendering is decoupled from the copy.

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

export function setupScreenCopy(_backend?: string | null): SetupScreenCopy {
  return TMUX_COPY;
}
