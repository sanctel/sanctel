use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use crate::agent_cli::AgentCli;

const SANCTEL_HOOK_MARKER: &str = "sanctel hook-handler";
const HOOK_DECLINED_FILE: &str = "hooks-install-declined";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookFileStatus {
    pub agent: String,
    pub path: String,
    pub installed: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HooksStatusReport {
    pub agents: Vec<HookFileStatus>,
    pub any_installed: bool,
    pub all_installed: bool,
    pub prompt_declined: bool,
    pub prompt_skipped: bool,
}

struct HookTarget {
    agent: AgentCli,
    path: PathBuf,
}

pub fn install_hook_settings(input: &str, command: &str) -> Result<String, String> {
    let mut root = parse_settings(input)?;
    let session_start = session_start_hooks_mut(&mut root)?;
    // Always remove any prior sanctel entry (including legacy bare-shape
    // entries from earlier sanctel builds) so install is self-healing
    // across schema migrations. Idempotency-by-count is preserved: after
    // install there is exactly one sanctel entry in the correct shape.
    session_start.retain(|entry| !is_sanctel_hook_entry(entry));
    // Claude's settings schema expects each event-array entry to be a
    // wrapper object containing a `hooks` array of command entries.
    // {"hooks": [{"type": "command", "command": "..."}]} — see
    // https://code.claude.com/docs/en/hooks
    session_start.push(json!({
        "hooks": [{ "type": "command", "command": command }]
    }));
    stringify_settings(&root)
}

pub fn uninstall_hook_settings(input: &str) -> Result<String, String> {
    let mut root = parse_settings(input)?;
    if let Some(session_start) = existing_session_start_hooks_mut(&mut root)? {
        session_start.retain(|entry| !is_sanctel_hook_entry(entry));
    }
    stringify_settings(&root)
}

pub fn has_sanctel_hook(input: &str) -> Result<bool, String> {
    let mut root = parse_settings(input)?;
    let Some(session_start) = existing_session_start_hooks_mut(&mut root)? else {
        return Ok(false);
    };
    Ok(session_start.iter().any(is_sanctel_hook_entry))
}

fn parse_settings(input: &str) -> Result<Value, String> {
    let root: Value =
        serde_json::from_str(input).map_err(|e| format!("parse settings failed: {e}"))?;
    if !root.is_object() {
        return Err("settings root must be a JSON object".to_string());
    }
    Ok(root)
}

fn stringify_settings(root: &Value) -> Result<String, String> {
    serde_json::to_string_pretty(root).map_err(|e| format!("serialize settings failed: {e}"))
}

fn session_start_hooks_mut(root: &mut Value) -> Result<&mut Vec<Value>, String> {
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| "settings root must be a JSON object".to_string())?;
    let hooks = root_obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| "settings hooks must be a JSON object".to_string())?;
    let session_start = hooks_obj.entry("SessionStart").or_insert_with(|| json!([]));
    session_start
        .as_array_mut()
        .ok_or_else(|| "hooks.SessionStart must be a JSON array".to_string())
}

fn existing_session_start_hooks_mut(root: &mut Value) -> Result<Option<&mut Vec<Value>>, String> {
    let Some(hooks) = root.as_object_mut().and_then(|obj| obj.get_mut("hooks")) else {
        return Ok(None);
    };
    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| "settings hooks must be a JSON object".to_string())?;
    let Some(session_start) = hooks_obj.get_mut("SessionStart") else {
        return Ok(None);
    };
    Ok(Some(session_start.as_array_mut().ok_or_else(|| {
        "hooks.SessionStart must be a JSON array".to_string()
    })?))
}

fn is_sanctel_hook_entry(entry: &Value) -> bool {
    // Correct shape (post-fix and what every modern CLI requires):
    //   {"hooks": [{"type": "command", "command": "...<marker>..."}]}
    if let Some(hooks) = entry.get("hooks").and_then(Value::as_array) {
        if hooks.iter().any(|inner| {
            inner
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|cmd| cmd.contains(SANCTEL_HOOK_MARKER))
        }) {
            return true;
        }
    }
    // Legacy bare shape from sanctel pre-fix:
    //   {"type": "command", "command": "...<marker>..."}
    // Detection is retained so uninstall (and install's retain-then-push)
    // can clean up entries written by older builds during migration.
    entry
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(SANCTEL_HOOK_MARKER))
}

pub fn install_all_hooks() -> Result<HooksStatusReport, String> {
    let home = home_dir()?;
    refresh_sanctel_symlink_for_home(&home, &std::env::current_exe().map_err(|e| e.to_string())?)?;
    for target in hook_targets(&home) {
        let input = read_settings_or_empty(&target.path)?;
        let command = hook_command(&home, target.agent);
        let output = install_hook_settings(&input, &command)?;
        atomic_write(&target.path, &output)?;
    }
    hooks_status()
}

pub fn uninstall_all_hooks() -> Result<HooksStatusReport, String> {
    let home = home_dir()?;
    for target in hook_targets(&home) {
        if !target.path.exists() {
            continue;
        }
        let input = std::fs::read_to_string(&target.path)
            .map_err(|e| format!("read {} failed: {e}", target.path.display()))?;
        let output = uninstall_hook_settings(&input)?;
        atomic_write(&target.path, &output)?;
    }
    hooks_status()
}

pub fn hooks_status() -> Result<HooksStatusReport, String> {
    let home = home_dir()?;
    Ok(hooks_status_for_home(&home))
}

pub fn remember_hook_install_declined() -> Result<(), String> {
    let home = home_dir()?;
    let path = home.join(".sanctel").join(HOOK_DECLINED_FILE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create {} failed: {e}", parent.display()))?;
    }
    std::fs::write(&path, b"declined\n")
        .map_err(|e| format!("write {} failed: {e}", path.display()))
}

pub fn refresh_sanctel_symlink() -> Result<(), String> {
    let home = home_dir()?;
    let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    refresh_sanctel_symlink_for_home(&home, &current_exe)
}

fn hooks_status_for_home(home: &Path) -> HooksStatusReport {
    let agents: Vec<HookFileStatus> = hook_targets(home)
        .into_iter()
        .map(hook_file_status)
        .collect();
    let any_installed = agents.iter().any(|status| status.installed);
    let all_installed = agents.iter().all(|status| status.installed);
    HooksStatusReport {
        agents,
        any_installed,
        all_installed,
        prompt_declined: home.join(".sanctel").join(HOOK_DECLINED_FILE).exists(),
        prompt_skipped: std::env::var("SANCTEL_SKIP_HOOK_INSTALL_PROMPT")
            .is_ok_and(|value| value == "1"),
    }
}

fn hook_file_status(target: HookTarget) -> HookFileStatus {
    let path = target.path;
    let agent = target.agent.as_str().to_string();
    let path_display = path.display().to_string();

    match hook_installed_at(&path) {
        Ok(installed) => HookFileStatus {
            agent,
            path: path_display,
            installed,
            error: None,
        },
        Err(error) => HookFileStatus {
            agent,
            path: path_display,
            installed: false,
            error: Some(error),
        },
    }
}

fn hook_installed_at(path: &Path) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    std::fs::read_to_string(path)
        .map_err(|e| format!("read {} failed: {e}", path.display()))
        .and_then(|body| has_sanctel_hook(&body))
}

fn read_settings_or_empty(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(body) => Ok(body),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("{}".to_string()),
        Err(e) => Err(format!("read {} failed: {e}", path.display())),
    }
}

fn atomic_write(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create {} failed: {e}", parent.display()))?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("invalid settings path: {}", path.display()))?;
    let tmp_path = path.with_file_name(format!("{file_name}.tmp"));
    std::fs::write(&tmp_path, body)
        .map_err(|e| format!("write {} failed: {e}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path).map_err(|e| format!("rename {} failed: {e}", path.display()))
}

fn hook_targets(home: &Path) -> Vec<HookTarget> {
    vec![
        HookTarget {
            agent: AgentCli::Claude,
            path: home.join(".claude").join("settings.json"),
        },
        HookTarget {
            agent: AgentCli::Codex,
            path: home.join(".codex").join("hooks.json"),
        },
        HookTarget {
            agent: AgentCli::Gemini,
            path: home.join(".gemini").join("settings.json"),
        },
    ]
}

fn hook_command(home: &Path, agent: AgentCli) -> String {
    format!(
        "{} hook-handler {}",
        home.join(".sanctel").join("bin").join("sanctel").display(),
        agent.as_str(),
    )
}

fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME not set".to_string())
}

fn refresh_sanctel_symlink_for_home(home: &Path, current_exe: &Path) -> Result<(), String> {
    let bin_dir = home.join(".sanctel").join("bin");
    std::fs::create_dir_all(&bin_dir)
        .map_err(|e| format!("create {} failed: {e}", bin_dir.display()))?;
    let link_path = bin_dir.join("sanctel");

    // Resolve current_exe to its canonical (non-symlink) path. On macOS,
    // _NSGetExecutablePath returns the argv[0]-shaped path the binary was
    // invoked with, so when sanctel is launched THROUGH our own symlink
    // (e.g., `~/.sanctel/bin/sanctel install-hooks`), `current_exe()` is
    // the symlink path itself. Pointing the symlink at the symlink would
    // self-loop (`too many levels of symbolic links` from the OS).
    let canonical_target = std::fs::canonicalize(current_exe).map_err(|e| {
        format!(
            "canonicalize current exe ({}) failed: {e}",
            current_exe.display()
        )
    })?;
    if canonical_target == link_path {
        return Err(format!(
            "refusing to symlink {} to itself (canonical target collides with link path)",
            link_path.display()
        ));
    }

    if let Ok(meta) = std::fs::symlink_metadata(&link_path) {
        if meta.file_type().is_dir() {
            return Err(format!("{} is a directory", link_path.display()));
        }
        std::fs::remove_file(&link_path)
            .map_err(|e| format!("remove {} failed: {e}", link_path.display()))?;
    }
    create_symlink(&canonical_target, &link_path)
        .map_err(|e| format!("symlink {} failed: {e}", link_path.display()))
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const COMMAND: &str = "/home/alice/.sanctel/bin/sanctel hook-handler claude";

    fn parse(body: &str) -> serde_json::Value {
        serde_json::from_str(body).unwrap()
    }

    #[test]
    fn install_into_empty_settings_adds_session_start_hook() {
        let out = parse(&install_hook_settings("{}", COMMAND).unwrap());

        assert_eq!(
            out["hooks"]["SessionStart"],
            json!([{ "hooks": [{ "type": "command", "command": COMMAND }] }])
        );
    }

    #[test]
    fn install_is_idempotent() {
        let once = install_hook_settings("{}", COMMAND).unwrap();
        let twice = install_hook_settings(&once, COMMAND).unwrap();

        assert_eq!(
            parse(&twice)["hooks"]["SessionStart"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn install_preserves_third_party_entries() {
        let input = json!({
            "hooks": {
                "SessionStart": [
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/notify.sh" }] }
                ]
            }
        })
        .to_string();

        let out = parse(&install_hook_settings(&input, COMMAND).unwrap());
        let entries = out["hooks"]["SessionStart"].as_array().unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries
            .iter()
            .any(|e| e["hooks"][0]["command"] == "/usr/local/bin/notify.sh"));
        assert!(entries.iter().any(|e| e["hooks"][0]["command"] == COMMAND));
    }

    #[test]
    fn install_migrates_legacy_bare_entry_to_wrapped_shape() {
        // An earlier sanctel build wrote bare entries. Reinstalling must
        // replace them with the correct wrapped shape, not duplicate them.
        let input = json!({
            "hooks": {
                "SessionStart": [
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/notify.sh" }] },
                    { "type": "command", "command": COMMAND }
                ]
            }
        })
        .to_string();

        let out = parse(&install_hook_settings(&input, COMMAND).unwrap());
        let entries = out["hooks"]["SessionStart"].as_array().unwrap();

        assert_eq!(entries.len(), 2);
        // The third-party entry is preserved as-is.
        assert!(entries
            .iter()
            .any(|e| e["hooks"][0]["command"] == "/usr/local/bin/notify.sh"));
        // The sanctel entry exists in the WRAPPED shape; no bare entry remains.
        assert!(entries
            .iter()
            .any(|e| e["hooks"][0]["command"] == COMMAND));
        assert!(!entries
            .iter()
            .any(|e| e.get("command").and_then(Value::as_str) == Some(COMMAND)));
    }

    #[test]
    fn uninstall_removes_only_sanctel_entries() {
        let input = json!({
            "hooks": {
                "SessionStart": [
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/notify.sh" }] },
                    { "hooks": [{ "type": "command", "command": COMMAND }] }
                ]
            }
        })
        .to_string();

        let out = parse(&uninstall_hook_settings(&input).unwrap());

        assert_eq!(
            out["hooks"]["SessionStart"],
            json!([{ "hooks": [{ "type": "command", "command": "/usr/local/bin/notify.sh" }] }])
        );
    }

    #[test]
    fn uninstall_removes_legacy_bare_sanctel_entries() {
        // Mid-migration: an earlier sanctel build left a bare-shape entry.
        // Uninstall must still recognize and remove it.
        let input = json!({
            "hooks": {
                "SessionStart": [
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/notify.sh" }] },
                    { "type": "command", "command": COMMAND }
                ]
            }
        })
        .to_string();

        let out = parse(&uninstall_hook_settings(&input).unwrap());

        assert_eq!(
            out["hooks"]["SessionStart"],
            json!([{ "hooks": [{ "type": "command", "command": "/usr/local/bin/notify.sh" }] }])
        );
    }

    #[test]
    fn uninstall_when_absent_is_a_noop() {
        let input = json!({
            "hooks": {
                "SessionStart": [
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/notify.sh" }] }
                ]
            }
        })
        .to_string();

        assert_eq!(
            parse(&uninstall_hook_settings(&input).unwrap()),
            parse(&input)
        );
    }

    #[test]
    fn malformed_settings_json_returns_error() {
        assert!(install_hook_settings("{", COMMAND).is_err());
        assert!(uninstall_hook_settings("{").is_err());
        assert!(has_sanctel_hook("{").is_err());
    }

    #[test]
    fn refresh_symlink_resolves_current_exe_when_invoked_via_existing_symlink() {
        // Simulate sanctel being launched THROUGH the symlink it's about
        // to refresh — the path coming in from std::env::current_exe() is
        // the symlink path itself, not the real binary. We must follow
        // the symlink before installing the new one, or it self-loops.
        use std::process::Command;

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let scratch = std::env::temp_dir().join(format!(
            "sanctel-symlink-refresh-{}-{}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&scratch).unwrap();
        let home = scratch.join("home");
        let bin_dir = home.join(".sanctel").join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let link_path = bin_dir.join("sanctel");

        // A real binary somewhere else in the scratch dir is the canonical
        // target. Use any executable that exists; the symlink target just
        // has to be a real file on disk for canonicalize to succeed.
        let real_binary = scratch.join("sanctel-real");
        std::fs::write(&real_binary, b"#!/bin/sh\nexit 0\n").unwrap();
        Command::new("chmod")
            .args(["+x", real_binary.to_str().unwrap()])
            .status()
            .unwrap();

        // Pre-create the link pointing at the real binary (mirroring a
        // healthy prior install).
        std::os::unix::fs::symlink(&real_binary, &link_path).unwrap();

        // Now invoke refresh with the SYMLINK path as current_exe — the
        // case we're guarding against.
        refresh_sanctel_symlink_for_home(&home, &link_path)
            .expect("refresh must succeed when invoked through the symlink");

        // The symlink must point at the canonical binary, NOT at itself.
        let new_target = std::fs::read_link(&link_path).unwrap();
        assert_eq!(
            new_target,
            std::fs::canonicalize(&real_binary).unwrap(),
            "refresh produced a self-loop; expected canonical binary path",
        );

        std::fs::remove_dir_all(&scratch).ok();
    }
}
