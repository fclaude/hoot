//! Backend-agnostic types shared by `pi_client.rs` and `opencode_client.rs`.
//!
//! Both backends stream NDJSON events from a subprocess and get parsed down
//! to the same [`AgentEvent`] enum on a background thread, so the rest of
//! the app (transcript rendering, turn dispatch) never needs to know which
//! one is actually running.

use std::process::Child;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

pub enum AgentEvent {
    Model(String),
    Thinking(String),
    Text(String),
    ToolCall {
        name: String,
        args: String,
    },
    ToolResult {
        name: String,
        summary: String,
    },
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
    /// Shared with the backend's stderr-reader thread, which is what
    /// actually calls `.wait()` on it (see `pi_client`/`opencode_client`) —
    /// this side only ever calls `.kill()`. Both backends hold the lock
    /// only briefly (a non-blocking `kill()`, or a `wait()` that runs after
    /// the subprocess's stdout/stderr have already closed), so the two
    /// never contend for long enough to matter.
    child: Arc<Mutex<Child>>,
}

impl AgentSession {
    pub fn new(rx: Receiver<AgentEvent>, child: Arc<Mutex<Child>>) -> Self {
        AgentSession { rx, child }
    }

    /// Best-effort: if the process already exited on its own, `Child::kill`
    /// just reports that, which is fine to ignore — either way, the
    /// stdout/stderr reader threads notice the pipes closing right after
    /// and finish the turn through the normal channel-disconnect path, same
    /// as any other turn ending.
    pub fn cancel(&self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
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

    #[test]
    fn cancel_kills_the_real_process_without_deadlocking() {
        // Exercises the exact Arc<Mutex<Child>> mechanism pi_client/
        // opencode_client use — a long-lived process standing in for a real
        // agent CLI (neither of which cargo test may spawn) — since the
        // real risk here is a deadlock: cancel() and the backend's own
        // wait()-on-exit both lock the same mutex, and getting the ordering
        // wrong would hang this test instead of just failing it.
        let (tx, rx) = std::sync::mpsc::channel::<AgentEvent>();
        let child = std::process::Command::new("sleep").arg("30").spawn().expect("spawn sleep 30");
        let pid = child.id();
        let child = Arc::new(Mutex::new(child));
        let session = AgentSession::new(rx, child.clone());

        // Mirrors the backend's own stderr-reader thread: waits (blocking)
        // on the same shared child, exactly where a lock ordering mistake
        // would show up as a hang.
        let waiter = std::thread::spawn(move || {
            let mut child = child.lock().unwrap();
            child.wait()
        });

        session.cancel();

        let status = waiter.join().expect("waiter thread panicked").expect("wait() failed");
        assert!(!status.success(), "a killed process should not report success");
        drop(tx);

        // The process should actually be gone, not just reported dead.
        let still_running =
            std::process::Command::new("kill").args(["-0", &pid.to_string()]).status().map(|s| s.success()).unwrap_or(false);
        assert!(!still_running, "pid {pid} should no longer exist after cancel()");
    }
}
