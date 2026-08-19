//! Drives a real `pi` coding-agent subprocess (https://github.com/earendil-works/pi)
//! and streams its `--mode json` NDJSON event log back as [`AgentEvent`]s.
//!
//! `--approve` is used unconditionally: pi has no native "pause and wait
//! for external approval before writing" hook (confirmed by testing —
//! `write` executes as soon as the model calls it, `--approve` or not), so
//! there's no filesystem-level gate to stage changes through either way.
//! Steer's diff view and plain `git` are the review/undo mechanism instead,
//! same as any other change made to the repo.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use serde_json::Value;

use crate::agent_client::{AgentEvent, AgentSession, ToolProfile};

fn tools_arg(profile: ToolProfile) -> &'static str {
    match profile {
        ToolProfile::ReadOnly => "read",
        ToolProfile::ReadWrite => "read,write",
    }
}

/// Spawns `pi --mode json --print --session <file> --tools <profile>
/// <prompt>` in `cwd` and streams parsed events back over a channel.
/// Non-blocking: stdout and stderr are each read on their own thread.
///
/// `session_file` (not `--session-id`) is what makes cross-turn memory
/// actually work: `pi` scopes `--session-id <id>` lookups by *both* the id
/// and the current working directory (it's a "project session"), so a turn
/// run from a different `cwd` would silently get a brand-new, empty
/// session even with the same id. Passing an explicit file via `--session`
/// bypasses that cwd scoping entirely: `pi` creates the file on first use
/// and resumes it exactly on every call after, regardless of which
/// directory the call runs from.
pub fn spawn(prompt: &str, cwd: &Path, session_file: &Path, tools: ToolProfile) -> std::io::Result<AgentSession> {
    let mut cmd = Command::new("pi");
    cmd.arg("--mode")
        .arg("json")
        .arg("--print")
        .arg("--approve")
        .arg("--session")
        .arg(session_file)
        .arg("--tools")
        .arg(tools_arg(tools))
        .arg(prompt)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    let (tx, rx) = mpsc::channel();

    let tx_out = tx.clone();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            if let Some(ev) = parse_line(&line) {
                if tx_out.send(ev).is_err() {
                    break;
                }
            }
        }
    });

    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if !line.trim().is_empty() {
                let _ = tx.send(AgentEvent::Error(line));
            }
        }
        let _ = child.wait();
    });

    Ok(AgentSession { rx })
}

fn parse_line(line: &str) -> Option<AgentEvent> {
    let v: Value = serde_json::from_str(line).ok()?;
    let ty = v.get("type")?.as_str()?;
    match ty {
        "message_start" => {
            let msg = v.get("message")?;
            if msg.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                if let Some(model) = msg.get("model").and_then(|m| m.as_str()) {
                    return Some(AgentEvent::Model(model.to_string()));
                }
            }
            None
        }
        "message_update" => {
            let ev = v.get("assistantMessageEvent")?;
            match ev.get("type")?.as_str()? {
                "thinking_end" => Some(AgentEvent::Thinking(ev.get("content")?.as_str()?.to_string())),
                "text_end" => Some(AgentEvent::Text(ev.get("content")?.as_str()?.to_string())),
                "toolcall_end" => {
                    let tc = ev.get("toolCall")?;
                    let name = tc.get("name")?.as_str()?.to_string();
                    let args = tc
                        .get("arguments")
                        .map(|a| a.to_string())
                        .unwrap_or_default();
                    Some(AgentEvent::ToolCall { name, args })
                }
                _ => None,
            }
        }
        "tool_execution_end" => {
            let name = v.get("toolName")?.as_str()?.to_string();
            let summary = summarize_result(v.get("result"));
            Some(AgentEvent::ToolResult { name, summary })
        }
        "turn_end" => Some(AgentEvent::TurnEnd),
        "agent_end" => Some(AgentEvent::AgentEnd),
        _ => None,
    }
}

fn summarize_result(result: Option<&Value>) -> String {
    let Some(result) = result else { return "done".to_string() };
    if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
        let text_len: usize = content
            .iter()
            .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
            .map(|t| t.len())
            .sum();
        if text_len > 0 {
            return format!("[{text_len} bytes]");
        }
    }
    "done".to_string()
}
