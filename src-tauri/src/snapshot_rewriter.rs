use std::collections::HashMap;

use crate::agent_cli::AgentCli;

const RECORD_TYPE_COLUMN: usize = 0;
const SESSION_NAME_COLUMN: usize = 1;
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
    let Some(command) = resume_command(resume) else {
        return line.to_string();
    };

    columns[COMMAND_COLUMN] = &command;
    format!("{}{}", columns.join("\t"), newline)
}

fn resume_command(resume: &AgentResume) -> Option<String> {
    match AgentCli::parse(&resume.agent)? {
        AgentCli::Claude => Some(format!(":claude --resume {}", resume.session_id)),
        AgentCli::Codex => Some(format!(":codex resume {}", resume.session_id)),
        AgentCli::Gemini => Some(format!(":gemini --session-id {}", resume.session_id)),
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
            "pane\tsanctel_wt_sanctel-main__term-3\t0\t1\t:*\t0\tgemini\t:/repo\t1\tgemini\t:gemini --session-id gemini-session-1\n",
        );
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
