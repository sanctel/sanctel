use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

use crate::agent_cli::AgentCli;
use crate::tmux_cli::{RealCommandRunner, TmuxCli, DEFAULT_CONF_PATH, DEFAULT_SOCKET};

pub trait SessionNameResolver {
    fn resolve_session_name(&self) -> Result<String, String>;
}

#[derive(Debug, Eq, PartialEq)]
pub enum HookHandlerOutcome {
    Wrote,
    IgnoredMalformedPayload,
}

#[derive(Serialize)]
struct HookSidecar<'a> {
    agent: &'a str,
    session_id: &'a str,
    ts: u64,
}

pub fn handle_hook_payload(
    agent: AgentCli,
    payload: &str,
    hooks_dir: &Path,
    resolver: &dyn SessionNameResolver,
    ts: u64,
) -> Result<HookHandlerOutcome, String> {
    let Some(session_id) = session_id_from_payload(payload) else {
        return Ok(HookHandlerOutcome::IgnoredMalformedPayload);
    };

    let session_name = resolver.resolve_session_name()?;
    std::fs::create_dir_all(hooks_dir).map_err(|e| format!("create hooks dir failed: {e}"))?;
    let final_path = hooks_dir.join(format!("{session_name}.json"));
    let tmp_path = hooks_dir.join(format!("{session_name}.json.tmp"));
    let sidecar = HookSidecar {
        agent: agent.as_str(),
        session_id: &session_id,
        ts,
    };
    let body = serde_json::to_vec_pretty(&sidecar)
        .map_err(|e| format!("serialize hook sidecar failed: {e}"))?;
    std::fs::write(&tmp_path, body).map_err(|e| format!("write hook sidecar tmp failed: {e}"))?;
    std::fs::rename(&tmp_path, &final_path)
        .map_err(|e| format!("rename hook sidecar failed: {e}"))?;

    Ok(HookHandlerOutcome::Wrote)
}

fn session_id_from_payload(payload: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(payload).ok()?;
    parsed
        .get("session_id")?
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

pub struct TmuxPaneSessionResolver;

impl SessionNameResolver for TmuxPaneSessionResolver {
    fn resolve_session_name(&self) -> Result<String, String> {
        let pane = std::env::var("TMUX_PANE").map_err(|_| "TMUX_PANE not set".to_string())?;
        let tmux = TmuxCli::new(DEFAULT_SOCKET, DEFAULT_CONF_PATH, RealCommandRunner);
        tmux.session_name_for_pane(&pane).map_err(|e| e.to_string())
    }
}

pub fn default_hooks_dir() -> Result<std::path::PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or_else(|| "HOME not set".to_string())?;
    Ok(std::path::PathBuf::from(home)
        .join(".sanctel")
        .join("hooks"))
}

pub fn unix_timestamp_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn run_hook_handler(agent: AgentCli) -> Result<HookHandlerOutcome, String> {
    let mut payload = String::new();
    std::io::stdin()
        .read_to_string(&mut payload)
        .map_err(|e| format!("read hook payload failed: {e}"))?;
    handle_hook_payload(
        agent,
        &payload,
        &default_hooks_dir()?,
        &TmuxPaneSessionResolver,
        unix_timestamp_now(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct StaticSessionName(&'static str);

    impl SessionNameResolver for StaticSessionName {
        fn resolve_session_name(&self) -> Result<String, String> {
            Ok(self.0.to_string())
        }
    }

    fn temp_hooks_dir(test_name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("sanctel-{test_name}-{nonce}"))
    }

    #[test]
    fn claude_session_start_payload_writes_sidecar_for_tmux_session() {
        let hooks_dir = temp_hooks_dir("claude-sidecar");
        let outcome = handle_hook_payload(
            AgentCli::Claude,
            r#"{"hook_event_name":"SessionStart","session_id":"claude-session-1"}"#,
            &hooks_dir,
            &StaticSessionName("sanctel_wt_sanctel-main__term-3"),
            1_779_311_720,
        )
        .unwrap();

        assert_eq!(outcome, HookHandlerOutcome::Wrote);
        let sidecar = hooks_dir.join("sanctel_wt_sanctel-main__term-3.json");
        let body = fs::read_to_string(sidecar).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["agent"], "claude");
        assert_eq!(parsed["session_id"], "claude-session-1");
        assert_eq!(parsed["ts"], 1_779_311_720);
        assert!(!hooks_dir
            .join("sanctel_wt_sanctel-main__term-3.json.tmp")
            .exists());

        let _ = fs::remove_dir_all(hooks_dir);
    }

    #[test]
    fn codex_session_start_payload_writes_codex_sidecar() {
        let hooks_dir = temp_hooks_dir("codex-sidecar");
        let outcome = handle_hook_payload(
            AgentCli::Codex,
            r#"{"session_id":"codex-session-1","event":"SessionStart"}"#,
            &hooks_dir,
            &StaticSessionName("sanctel_wt_sanctel-main__term-4"),
            1_779_311_721,
        )
        .unwrap();

        assert_eq!(outcome, HookHandlerOutcome::Wrote);
        let body =
            fs::read_to_string(hooks_dir.join("sanctel_wt_sanctel-main__term-4.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["agent"], "codex");
        assert_eq!(parsed["session_id"], "codex-session-1");
        assert_eq!(parsed["ts"], 1_779_311_721);

        let _ = fs::remove_dir_all(hooks_dir);
    }

    #[test]
    fn gemini_session_start_payload_writes_gemini_sidecar() {
        let hooks_dir = temp_hooks_dir("gemini-sidecar");
        let outcome = handle_hook_payload(
            AgentCli::Gemini,
            r#"{"session_id":"gemini-session-1","hook_event_name":"SessionStart"}"#,
            &hooks_dir,
            &StaticSessionName("sanctel_wt_sanctel-main__term-5"),
            1_779_311_722,
        )
        .unwrap();

        assert_eq!(outcome, HookHandlerOutcome::Wrote);
        let body =
            fs::read_to_string(hooks_dir.join("sanctel_wt_sanctel-main__term-5.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["agent"], "gemini");
        assert_eq!(parsed["session_id"], "gemini-session-1");
        assert_eq!(parsed["ts"], 1_779_311_722);

        let _ = fs::remove_dir_all(hooks_dir);
    }

    #[test]
    fn malformed_payload_exits_cleanly_without_sidecar() {
        let hooks_dir = temp_hooks_dir("malformed-sidecar");
        let outcome = handle_hook_payload(
            AgentCli::Claude,
            "",
            &hooks_dir,
            &StaticSessionName("sanctel_wt_sanctel-main__term-6"),
            1_779_311_723,
        )
        .unwrap();

        assert_eq!(outcome, HookHandlerOutcome::IgnoredMalformedPayload);
        assert!(!hooks_dir.exists());
    }
}
