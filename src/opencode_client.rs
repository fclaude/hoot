//! Drives a real `opencode` coding-agent subprocess (<https://opencode.ai>)
//! and streams its `run --format json` NDJSON event log back as
//! [`AgentEvent`]s — the same event type `pi_client` produces, so the rest
//! of the app doesn't need to know which backend is actually running.
//!
//! Confirmed against a real `opencode` install (not just its docs) before
//! writing this: `opencode run --format json` streams one JSON object per
//! line, each shaped `{"type": ..., "sessionID": ..., "part": {...}}` (or,
//! for errors, `{"type": "error", "error": {...}}` with no `part`).
//!
//! opencode has no `--tools read|read,write` equivalent — permissions are
//! scoped to named *agents* instead. Its built-in `plan` agent is genuinely
//! read-only (verified: asked it to write a file under `plan`, it explained
//! a plan instead and never touched disk); `build` is the standard
//! read+write agent. `--auto` is passed unconditionally for the same reason
//! pi always gets `--approve`: without it, a permission set to "ask" in
//! someone's config would hang forever waiting for a TTY that isn't there.
//!
//! Session continuity also works differently than pi's: pi gets an
//! explicit file path upfront and resumes it automatically; opencode hands
//! back a server-generated `sessionID` only *after* a turn runs, which then
//! has to be threaded back in via `--session <id>` on every later call —
//! see `Session` in `AgentEvent`.
//!
//! Unlike `pi_client`, `opencode run` genuinely has no stdin-prompt mode —
//! confirmed by testing, not assumed: with no positional message it just
//! hangs waiting on stdin rather than reading a prompt from it. But `--file`
//! *does* work as a workaround once paired with a message, even though a
//! bare `--file` with no message errors ("You must provide a message or a
//! command") — confirmed by testing that too: a short, fixed, non-sensitive
//! instruction as the positional message plus the real prompt content in a
//! securely-created temp file passed via `--file` gets the model to follow
//! the file's content correctly. So the real prompt — which can include a
//! full selected diff — never appears in `ps`/`/proc/*/cmdline` for this
//! backend either, and doesn't risk the OS's argument-length limit.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::Value;

use crate::agent_client::{AgentEvent, AgentSession, ToolProfile};

fn agent_arg(profile: ToolProfile) -> &'static str {
    match profile {
        ToolProfile::ReadOnly => "plan",
        ToolProfile::ReadWrite => "build",
    }
}

/// Fixed, non-sensitive stand-in for the real prompt on the command line —
/// see the module doc comment for why this (plus `--file`) is necessary.
const FILE_PROMPT_INSTRUCTION: &str =
    "Use the attached file as the full user request. Follow its instructions exactly; don't mention this message.";

/// Spawns `opencode run "<fixed instruction>" --file <tempfile> --format
/// json --auto --agent <plan|build> --dir <cwd> [--session <id>]` and
/// streams parsed events back over a channel. Non-blocking: stdout and
/// stderr are each read on their own thread, same shape as
/// `pi_client::spawn`.
///
/// `session_id` is `None` for the first turn of a run (opencode assigns
/// one, surfaced back to the caller as `AgentEvent::Session`) and
/// `Some(id)` for every turn after, to resume it.
pub fn spawn(prompt: &str, cwd: &Path, session_id: Option<&str>, tools: ToolProfile) -> std::io::Result<AgentSession> {
    let mut prompt_file = tempfile::Builder::new().prefix("hoot-prompt-").suffix(".txt").tempfile()?;
    prompt_file.write_all(prompt.as_bytes())?;
    prompt_file.flush()?;

    let mut cmd = Command::new("opencode");
    cmd.arg("run")
        .arg(FILE_PROMPT_INSTRUCTION)
        .arg("--file")
        .arg(prompt_file.path())
        .arg("--format")
        .arg("json")
        .arg("--auto")
        .arg("--agent")
        .arg(agent_arg(tools))
        .arg("--dir")
        .arg(cwd);
    if let Some(id) = session_id {
        cmd.arg("--session").arg(id);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
    // Its own process group (pgid = its own pid), not hoot's — a tool call
    // opencode runs (a shell command, a linter, a test suite, ...)
    // inherits that same group by default, so cancelling the turn can
    // signal the whole group at once instead of leaving grandchildren
    // behind as orphans still running after "Cancelled." shows.
    cmd.process_group(0);

    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let child = Arc::new(Mutex::new(child));

    let (tx, rx) = mpsc::channel();

    let tx_out = tx.clone();
    thread::spawn(move || {
        // The session id is the same on every line of a given run, so only
        // the first one actually needs to be surfaced back to the caller.
        let mut sent_session = false;
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let Some(v) = serde_json::from_str::<Value>(&line).ok() else {
                continue;
            };
            if !sent_session {
                if let Some(id) = v.get("sessionID").and_then(|s| s.as_str()) {
                    sent_session = true;
                    if tx_out.send(AgentEvent::Session(id.to_string())).is_err() {
                        break;
                    }
                }
            }
            if let Some(ev) = parse_event(&v) {
                if tx_out.send(ev).is_err() {
                    break;
                }
            }
        }
    });

    let wait_child = child.clone();
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if !line.trim().is_empty() {
                let _ = tx.send(AgentEvent::Error(line));
            }
        }
        if let Ok(mut child) = wait_child.lock() {
            let _ = child.wait();
        }
        // `prompt_file` stays alive (and thus on disk) until here — the
        // child has now fully exited, so it's definitely done reading it.
        // Dropping it here deletes it.
        drop(prompt_file);
    });

    Ok(AgentSession::new(rx, child))
}

fn parse_event(v: &Value) -> Option<AgentEvent> {
    let ty = v.get("type")?.as_str()?;
    match ty {
        "error" => {
            let msg = v
                .get("error")
                .and_then(|e| {
                    e.get("data").and_then(|d| d.get("message")).and_then(|m| m.as_str()).or_else(|| e.get("name").and_then(|n| n.as_str()))
                })
                .unwrap_or("unknown error");
            Some(AgentEvent::Error(msg.to_string()))
        }
        "reasoning" => Some(AgentEvent::Thinking(v.get("part")?.get("text")?.as_str()?.to_string())),
        "text" => Some(AgentEvent::Text(v.get("part")?.get("text")?.as_str()?.to_string())),
        "tool_use" => {
            let part = v.get("part")?;
            let name = part.get("tool")?.as_str()?.to_string();
            let state = part.get("state")?;
            let status = state.get("status")?.as_str()?;
            let args = state.get("input").map(|i| i.to_string()).unwrap_or_default();
            match status {
                // Local, fast tools routinely resolve within the single
                // line hoot sees — there's no separate "call" line to
                // catch first — so a completed call gets a ToolResult
                // synthesized straight from the one event, without a
                // preceding ToolCall line, whereas a slow tool (still
                // "running" when its line arrives) shows the ToolCall and
                // its result only when a later line reports completion.
                "completed" => {
                    let summary = summarize_output(state.get("output"));
                    Some(AgentEvent::ToolResult { name, summary })
                }
                "error" => {
                    let msg = state.get("output").and_then(|o| o.as_str()).unwrap_or("tool call failed");
                    Some(AgentEvent::Error(format!("{name}: {msg}")))
                }
                _ => Some(AgentEvent::ToolCall { name, args }),
            }
        }
        "step_finish" => {
            let reason = v.get("part")?.get("reason")?.as_str()?;
            (reason == "stop").then_some(AgentEvent::TurnEnd)
        }
        _ => None,
    }
}

fn summarize_output(output: Option<&Value>) -> String {
    match output.and_then(|o| o.as_str()) {
        Some(s) if !s.is_empty() => format!("[{} bytes]", s.len()),
        _ => "done".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(json: &str) -> AgentEvent {
        parse_event(&serde_json::from_str(json).unwrap()).expect("expected an event")
    }

    #[test]
    fn parses_reasoning_as_thinking() {
        match line(r#"{"type":"reasoning","part":{"text":"hmm, let me think"}}"#) {
            AgentEvent::Thinking(t) => assert_eq!(t, "hmm, let me think"),
            _ => panic!("expected Thinking"),
        }
    }

    #[test]
    fn parses_text() {
        match line(r#"{"type":"text","part":{"text":"the answer"}}"#) {
            AgentEvent::Text(t) => assert_eq!(t, "the answer"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn completed_tool_use_becomes_a_tool_result() {
        let json =
            r#"{"type":"tool_use","part":{"tool":"read","state":{"status":"completed","input":{"filePath":"a.rs"},"output":"fn a() {}"}}}"#;
        match line(json) {
            AgentEvent::ToolResult { name, summary } => {
                assert_eq!(name, "read");
                assert_eq!(summary, "[9 bytes]");
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn pending_tool_use_becomes_a_tool_call() {
        let json = r#"{"type":"tool_use","part":{"tool":"bash","state":{"status":"running","input":{"command":"ls"}}}}"#;
        match line(json) {
            AgentEvent::ToolCall { name, .. } => assert_eq!(name, "bash"),
            _ => panic!("expected ToolCall"),
        }
    }

    #[test]
    fn top_level_error_event_carries_the_message() {
        let json = r#"{"type":"error","error":{"name":"UnknownError","data":{"message":"boom"}}}"#;
        match line(json) {
            AgentEvent::Error(e) => assert_eq!(e, "boom"),
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn step_finish_with_stop_reason_ends_the_turn() {
        let json = r#"{"type":"step_finish","part":{"reason":"stop"}}"#;
        assert!(matches!(line(json), AgentEvent::TurnEnd));
    }

    #[test]
    fn step_finish_for_a_tool_call_is_not_a_turn_end() {
        let json = r#"{"type":"step_finish","part":{"reason":"tool-calls"}}"#;
        assert!(parse_event(&serde_json::from_str(json).unwrap()).is_none());
    }

    #[test]
    fn unrecognized_event_types_are_ignored() {
        let v: Value = serde_json::from_str(r#"{"type":"step_start","part":{}}"#).unwrap();
        assert!(parse_event(&v).is_none());
    }
}
