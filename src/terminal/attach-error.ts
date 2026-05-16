// Wire-shape parsing for the `terminal_attach` Rust command's error
// payload. Lives in its own module so it can be unit-tested without
// pulling in xterm (which references `self` and won't load under Node).
//
// The Rust side (`src-tauri/src/terminal_runtime.rs::AttachError::Display`)
// emits `worktree-missing: <path>` for the broken-tab case. The prefix is
// the wire contract — the broken-tab UI routes off it.

export type ParsedAttachError =
  | { kind: "worktree-missing"; path: string }
  | { kind: "other"; message: string };

export function parseAttachError(raw: unknown): ParsedAttachError {
  const message =
    typeof raw === "string"
      ? raw
      : raw instanceof Error
        ? raw.message
        : String(raw);
  const prefix = "worktree-missing:";
  if (message.startsWith(prefix)) {
    return { kind: "worktree-missing", path: message.slice(prefix.length).trim() };
  }
  return { kind: "other", message };
}
