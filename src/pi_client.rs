//! Drives a real `pi` coding-agent subprocess (<https://github.com/earendil-works/pi>)
//! and streams its `--mode json` NDJSON event log back as [`AgentEvent`]s.
//!
//! `--approve` is used unconditionally. What it actually does, verified
//! against `pi --help` rather than assumed: "Trust project-local files for
//! this run" — it's unrelated to `--tools`/tool-call permissions (pi has
//! no native "pause and wait for approval before writing" hook regardless
//! — confirmed by testing: `write` executes as soon as the model calls it,
//! `--approve` or not — so there's no filesystem-level gate to stage
//! writes through either way). What `--approve` actually controls is
//! whether pi trusts repo-local config/extension files enough to run them
//! with the user's privileges — i.e. this is real code execution from
//! whatever's in the target repo, on *every* turn, including nominally
//! read-only ones (`--tools read` narrows what the model can call; it
//! doesn't touch this). That's a materially bigger trust boundary than
//! "auto-approve tool calls," and worth knowing precisely before pointing
//! this at an unfamiliar repo. Hoot's diff view and plain `git` are the
//! review/undo mechanism for whatever the model itself does with its
//! tools — they have no bearing on this.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::Value;

use crate::agent_client::{AgentEvent, AgentSession, ToolProfile};

fn tools_arg(profile: ToolProfile) -> &'static str {
    match profile {
        ToolProfile::ReadOnly => "read",
        ToolProfile::ReadWrite => "read,write",
    }
}

/// Spawns `pi --mode json --print --session <file> --tools <profile>` in
/// `cwd`, writes `prompt` to its stdin, and streams parsed events back over
/// a channel. Non-blocking: stdout and stderr are each read on their own
/// thread.
///
/// The prompt goes over stdin rather than as a trailing CLI argument
/// (confirmed pi supports this — it reads the message from stdin when no
/// positional prompt is given) so it never shows up in `ps`/`/proc/*/cmdline`
/// for other users on the same machine, and never risks the OS's
/// argument-length limit on a large pasted diff.
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
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped())
        // Its own process group (pgid = its own pid), not hoot's — a tool
        // call `pi` runs (a shell command, a linter, a test suite, ...)
        // inherits that same group by default, so cancelling the turn can
        // signal the whole group at once instead of leaving grandchildren
        // behind as orphans still running after "Cancelled." shows.
        .process_group(0);

    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let mut stdin = child.stdin.take().expect("piped stdin");
    let child = Arc::new(Mutex::new(child));
    let prompt = prompt.to_string();
    thread::spawn(move || {
        // A closed read end (pi exits before reading, e.g. bad flags) would
        // otherwise surface as a SIGPIPE-driven write error here — ignored,
        // since the stdout/stderr threads already report anything that
        // actually went wrong.
        let _ = stdin.write_all(prompt.as_bytes());
    });

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

    let wait_child = child.clone();
    thread::spawn(move || {
        let mut said_something = false;
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if !line.trim().is_empty() {
                said_something = true;
                let _ = tx.send(AgentEvent::Error(line));
            }
        }
        if let Ok(mut child) = wait_child.lock() {
            // See `opencode_client` for why the status is checked rather
            // than discarded: a silent non-zero exit otherwise presents as
            // a completed turn.
            if let Ok(status) = child.wait() {
                if !status.success() && !said_something {
                    let _ = tx.send(AgentEvent::Error(format!("pi exited unsuccessfully ({status})")));
                }
            }
        }
    });

    Ok(AgentSession::new(rx, child))
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
                    let args = tc.get("arguments").map(|a| a.to_string()).unwrap_or_default();
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
        let text_len: usize = content.iter().filter_map(|c| c.get("text").and_then(|t| t.as_str())).map(|t| t.len()).sum();
        if text_len > 0 {
            return format!("[{text_len} bytes]");
        }
    }
    "done".to_string()
}
