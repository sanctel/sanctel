use std::collections::HashMap;

use crate::agent_cli::AgentCli;

const RECORD_TYPE_COLUMN: usize = 0;
const SESSION_NAME_COLUMN: usize = 1;
// Column 9 in resurrect's pane record is `pane_current_command` — the
// short program name (truncated by the kernel: macOS reports `codex` as
// `codex-aarch64-a`, gemini as `node` since it's a node script).
// Resurrect's restore matches this column against `@resurrect-processes`;
// if there's no match, the pane comes back as an empty shell regardless
// of what we wrote in column 10. So when we have a capture, we normalize
// column 9 to the canonical agent name (`claude` / `codex` / `gemini`)
// so the conf's allowlist hits and the rewritten command actually runs.
const PROGRAM_COLUMN: usize = 9;
const COMMAND_COLUMN: usize = 10;
const MIN_PANE_COLUMNS: usize = COMMAND_COLUMN + 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentResume {
    pub agent: String,
    pub session_id: String,
}

pub fn rewrite_snapshot(snapshot: &str, captures: &HashMap<String, AgentResume>) -> String {
    let mut rewritten = String::with_capacity(snapshot.len());
    for line in snapshot.split_inclusive('\n') {
        rewritten.push_str(&rewrite_line(line, captures));
    }
    rewritten
}

pub fn capture_map(
    captures: impl IntoIterator<Item = (String, AgentResume, u64)>,
) -> HashMap<String, AgentResume> {
    let mut freshest: HashMap<String, (AgentResume, u64)> = HashMap::new();
    for (session_name, resume, ts) in captures {
        match freshest.get(&session_name) {
            Some((_, existing_ts)) if *existing_ts >= ts => {}
            _ => {
                freshest.insert(session_name, (resume, ts));
            }
        }
    }
    freshest
        .into_iter()
        .map(|(session_name, (resume, _))| (session_name, resume))
        .collect()
}

fn rewrite_line(line: &str, captures: &HashMap<String, AgentResume>) -> String {
    let (body, newline) = line
        .strip_suffix('\n')
        .map(|body| (body, "\n"))
        .unwrap_or((line, ""));
    let mut columns = body.split('\t').collect::<Vec<_>>();
    if columns.len() < MIN_PANE_COLUMNS || columns[RECORD_TYPE_COLUMN] != "pane" {
        return line.to_string();
    }

    let Some(resume) = captures.get(columns[SESSION_NAME_COLUMN]) else {
        return line.to_string();
    };
    let Some(agent) = AgentCli::parse(&resume.agent) else {
        return line.to_string();
    };
    // The sidecar records "an agent session was active in this pane at
    // some point" — it persists across the agent exiting. If the pane
    // has moved on (user typed /exit and is at a shell prompt, or
    // crashed back to zsh), don't inject the resume command: respect
    // that the user has left. We detect this by comparing the
    // snapshot's column 9 (pane_current_command) against the set of
    // names each agent is known to surface as in tmux.
    if !pane_is_running_agent(columns[PROGRAM_COLUMN], agent) {
        return line.to_string();
    }
    let program = agent.as_str();
    let command = resume_command(agent, &resume.session_id);

    columns[PROGRAM_COLUMN] = program;
    columns[COMMAND_COLUMN] = &command;
    format!("{}{}", columns.join("\t"), newline)
}

fn pane_is_running_agent(snapshot_program: &str, agent: AgentCli) -> bool {
    match agent {
        AgentCli::Claude => snapshot_program == "claude",
        // macOS truncates `comm` to ~15 chars, so the arm64 codex
        // binary shows up as `codex-aarch64-a`. Match either form.
        AgentCli::Codex => {
            snapshot_program == "codex" || snapshot_program.starts_with("codex-")
        }
        // Gemini is shipped as a node script, so tmux's pane_current_command
        // is `node` (or `gemini` if the user has a wrapper). Both are
        // accepted. Edge case: a user running unrelated `node` in a pane
        // that ever had a gemini sidecar will get a stale resume on
        // restore. Acceptable for now; if it bites, the SessionEnd hook
        // (not yet registered) is the cleaner fix.
        AgentCli::Gemini => {
            snapshot_program == "gemini" || snapshot_program == "node"
        }
    }
}

fn resume_command(agent: AgentCli, session_id: &str) -> String {
    match agent {
        AgentCli::Claude => format!(":claude --resume {}", session_id),
        AgentCli::Codex => format!(":codex resume {}", session_id),
        // Gemini's `--session-id` errors if the id already exists ("Use
        // --resume to resume it"). Its `--help` claims `--resume` only
        // accepts an index or "latest", but the runtime error message
        // ("Use --resume {number}, --resume {uuid}, or --resume latest")
        // confirms UUIDs are accepted too. Verified empirically.
        AgentCli::Gemini => format!(":gemini --resume {}", session_id),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn capture(agent: &str, session_id: &str) -> AgentResume {
        AgentResume {
            agent: agent.to_string(),
            session_id: session_id.to_string(),
        }
    }

    #[test]
    fn claude_pane_command_is_rewritten() {
        let snapshot = "pane\tsanctel_wt_sanctel-main__term-1\t0\t1\t:*\t0\tclaude\t:/repo\t1\tclaude\t:claude\n";
        let mut captures = HashMap::new();
        captures.insert(
            "sanctel_wt_sanctel-main__term-1".to_string(),
            capture("claude", "claude-session-1"),
        );

        let rewritten = rewrite_snapshot(snapshot, &captures);

        assert_eq!(
            rewritten,
            "pane\tsanctel_wt_sanctel-main__term-1\t0\t1\t:*\t0\tclaude\t:/repo\t1\tclaude\t:claude --resume claude-session-1\n",
        );
    }

    #[test]
    fn codex_pane_command_is_rewritten() {
        let snapshot =
            "pane\tsanctel_wt_sanctel-main__term-2\t0\t1\t:*\t0\tcodex\t:/repo\t1\tcodex\t:codex\n";
        let mut captures = HashMap::new();
        captures.insert(
            "sanctel_wt_sanctel-main__term-2".to_string(),
            capture("codex", "codex-session-1"),
        );

        let rewritten = rewrite_snapshot(snapshot, &captures);

        assert_eq!(
            rewritten,
            "pane\tsanctel_wt_sanctel-main__term-2\t0\t1\t:*\t0\tcodex\t:/repo\t1\tcodex\t:codex resume codex-session-1\n",
        );
    }

    #[test]
    fn gemini_pane_command_is_rewritten() {
        let snapshot = "pane\tsanctel_wt_sanctel-main__term-3\t0\t1\t:*\t0\tgemini\t:/repo\t1\tgemini\t:gemini\n";
        let mut captures = HashMap::new();
        captures.insert(
            "sanctel_wt_sanctel-main__term-3".to_string(),
            capture("gemini", "gemini-session-1"),
        );

        let rewritten = rewrite_snapshot(snapshot, &captures);

        assert_eq!(
            rewritten,
            "pane\tsanctel_wt_sanctel-main__term-3\t0\t1\t:*\t0\tgemini\t:/repo\t1\tgemini\t:gemini --resume gemini-session-1\n",
        );
    }

    #[test]
    fn codex_pane_normalizes_truncated_program_name() {
        // macOS truncates `comm` to ~15 chars, so `codex` (a native arm64
        // binary) shows up as `codex-aarch64-a` in column 9. Resurrect's
        // @resurrect-processes match runs against column 9 — if we don't
        // normalize, the allowlist lookup misses and the pane comes back
        // empty even though column 10 has the resume command.
        let snapshot = "pane\tsanctel_wt_sanctel-main__term-2\t0\t1\t:*\t0\tsanctel\t:/repo\t1\tcodex-aarch64-a\t:codex\n";
        let mut captures = HashMap::new();
        captures.insert(
            "sanctel_wt_sanctel-main__term-2".to_string(),
            capture("codex", "codex-session-1"),
        );

        let rewritten = rewrite_snapshot(snapshot, &captures);

        assert_eq!(
            rewritten,
            "pane\tsanctel_wt_sanctel-main__term-2\t0\t1\t:*\t0\tsanctel\t:/repo\t1\tcodex\t:codex resume codex-session-1\n",
        );
    }

    #[test]
    fn gemini_pane_normalizes_node_wrapper_program_name() {
        // gemini is shipped as a node script, so column 9 is `node`, not
        // `gemini`. Same normalization requirement as codex.
        let snapshot = "pane\tsanctel_wt_sanctel-scratch__term-1\t0\t1\t:*\t0\tReady\t:/home\t1\tnode\t:/opt/homebrew/opt/node/bin/node /opt/homebrew/bin/gemini\n";
        let mut captures = HashMap::new();
        captures.insert(
            "sanctel_wt_sanctel-scratch__term-1".to_string(),
            capture("gemini", "gemini-session-1"),
        );

        let rewritten = rewrite_snapshot(snapshot, &captures);

        assert_eq!(
            rewritten,
            "pane\tsanctel_wt_sanctel-scratch__term-1\t0\t1\t:*\t0\tReady\t:/home\t1\tgemini\t:gemini --resume gemini-session-1\n",
        );
    }

    #[test]
    fn claude_pane_back_at_shell_is_not_rewritten() {
        // User ran claude, exited it, sat at the shell prompt, then
        // quit sanctel. The sidecar still records claude's session_id,
        // but the snapshot's column 9 is `zsh`. Respect the user's
        // exit — don't resurrect the conversation.
        let snapshot = "pane\tsanctel_wt_sanctel-main__term-1\t0\t1\t:*\t0\tshell\t:/repo\t1\tzsh\t:zsh\n";
        let mut captures = HashMap::new();
        captures.insert(
            "sanctel_wt_sanctel-main__term-1".to_string(),
            capture("claude", "stale-session-id"),
        );

        let rewritten = rewrite_snapshot(snapshot, &captures);

        assert_eq!(rewritten, snapshot);
    }

    #[test]
    fn codex_pane_back_at_shell_is_not_rewritten() {
        let snapshot = "pane\tsanctel_wt_sanctel-main__term-2\t0\t1\t:*\t0\tshell\t:/repo\t1\tbash\t:bash\n";
        let mut captures = HashMap::new();
        captures.insert(
            "sanctel_wt_sanctel-main__term-2".to_string(),
            capture("codex", "stale-session-id"),
        );

        let rewritten = rewrite_snapshot(snapshot, &captures);

        assert_eq!(rewritten, snapshot);
    }

    #[test]
    fn gemini_pane_back_at_shell_is_not_rewritten() {
        let snapshot = "pane\tsanctel_wt_sanctel-scratch__term-1\t0\t1\t:*\t0\tshell\t:/home\t1\tfish\t:fish\n";
        let mut captures = HashMap::new();
        captures.insert(
            "sanctel_wt_sanctel-scratch__term-1".to_string(),
            capture("gemini", "stale-session-id"),
        );

        let rewritten = rewrite_snapshot(snapshot, &captures);

        assert_eq!(rewritten, snapshot);
    }

    #[test]
    fn pane_without_capture_passes_through() {
        let snapshot =
            "pane\tsanctel_wt_sanctel-main__term-4\t0\t1\t:*\t0\tbash\t:/repo\t1\tbash\t:\n";
        let captures = HashMap::new();

        let rewritten = rewrite_snapshot(snapshot, &captures);

        assert_eq!(rewritten, snapshot);
    }

    #[test]
    fn mixed_snapshot_rewrites_only_captured_panes() {
        let snapshot = concat!(
            "pane\tsanctel_wt_sanctel-main__term-1\t0\t1\t:*\t0\tclaude\t:/repo\t1\tclaude\t:claude\n",
            "pane\tsanctel_wt_sanctel-main__term-2\t0\t1\t:*\t0\tbash\t:/repo\t1\tbash\t:\n",
            "window\tsanctel_wt_sanctel-main__term-1\t0\t:term-1\t1\t:*\tlayout\t\n",
        );
        let mut captures = HashMap::new();
        captures.insert(
            "sanctel_wt_sanctel-main__term-1".to_string(),
            capture("claude", "claude-session-1"),
        );
        captures.insert(
            "sanctel_wt_sanctel-main__missing".to_string(),
            capture("codex", "ignored-session"),
        );

        let rewritten = rewrite_snapshot(snapshot, &captures);

        assert_eq!(
            rewritten,
            concat!(
                "pane\tsanctel_wt_sanctel-main__term-1\t0\t1\t:*\t0\tclaude\t:/repo\t1\tclaude\t:claude --resume claude-session-1\n",
                "pane\tsanctel_wt_sanctel-main__term-2\t0\t1\t:*\t0\tbash\t:/repo\t1\tbash\t:\n",
                "window\tsanctel_wt_sanctel-main__term-1\t0\t:term-1\t1\t:*\tlayout\t\n",
            ),
        );
    }
}
