//! Backend-agnostic types shared by `pi_client.rs` and `opencode_client.rs`.
//!
//! Both backends stream NDJSON events from a subprocess and get parsed down
//! to the same [`AgentEvent`] enum on a background thread, so the rest of
//! the app (transcript rendering, turn dispatch) never needs to know which
//! one is actually running.

use std::sync::mpsc::Receiver;

pub enum AgentEvent {
    Model(String),
    Thinking(String),
    Text(String),
    ToolCall { name: String, args: String },
    ToolResult { name: String, summary: String },
    /// A backend-assigned session identifier, for backends (opencode) that
    /// hand one back rather than taking an explicit session file upfront
    /// like pi does. Never fired by pi_client.
    Session(String),
    TurnEnd,
    AgentEnd,
    Error(String),
}

pub struct AgentSession {
    pub rx: Receiver<AgentEvent>,
}

/// Which tools a spawned turn is allowed to use.
///
/// `ReadOnly` is for normal chat/Q&A in the Agent pane — the backend can
/// inspect the real target directory but never write to it. `ReadWrite`
/// writes straight to the real target directory: Review's diff view and
/// plain `git` are the review/undo mechanism, same as any other change made
/// to the repo — see `pi_client`'s doc comment for why there's no
/// filesystem-level staging gate either way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolProfile {
    ReadOnly,
    ReadWrite,
}

/// Which coding-agent CLI drives a turn. Selected once at startup via
/// `--agent pi|opencode` (see `main.rs`); nothing about picking a backend
/// happens mid-session today.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentBackend {
    Pi,
    OpenCode,
}

impl AgentBackend {
    /// Parses the `--agent` CLI flag's value. `None` for anything else, so
    /// `main.rs` can report a clear error instead of silently guessing.
    pub fn parse(s: &str) -> Option<AgentBackend> {
        match s {
            "pi" => Some(AgentBackend::Pi),
            "opencode" => Some(AgentBackend::OpenCode),
            _ => None,
        }
    }

    /// The binary name, and what the Agent pane header shows.
    pub fn label(self) -> &'static str {
        match self {
            AgentBackend::Pi => "pi",
            AgentBackend::OpenCode => "opencode",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_backend_names() {
        assert_eq!(AgentBackend::parse("pi"), Some(AgentBackend::Pi));
        assert_eq!(AgentBackend::parse("opencode"), Some(AgentBackend::OpenCode));
    }

    #[test]
    fn rejects_unknown_backend_names() {
        assert_eq!(AgentBackend::parse("claude"), None);
        assert_eq!(AgentBackend::parse(""), None);
    }
}
