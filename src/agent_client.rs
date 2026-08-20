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

    /// Best-effort: if the process already exited on its own, the kill
    /// just reports that, which is fine to ignore — either way, the
    /// stdout/stderr reader threads notice the pipes closing right after
    /// and finish the turn through the normal channel-disconnect path, same
    /// as any other turn ending.
    ///
    /// Kills the backend's whole process *tree*, not just its own pid —
    /// confirmed against real opencode, not assumed: its bash tool moves
    /// whatever it runs into a *new* session (its own process group,
    /// unrelated to opencode's), specifically so `kill(-pid, ...)` —
    /// `process_group(0)` at spawn time covers everything that stays in
    /// the group it started in, which was the original plan here, but
    /// doesn't reach a child that deliberately left it — misses it
    /// entirely. Walking `ppid` relationships via `ps` still finds it:
    /// changing your own process group doesn't change who your parent is.
    // `mut child` below is only needed on the `Child::kill` fallback,
    // which `cfg(unix)` compiles out entirely — the unix path only ever
    // calls `child.id()`, an immutable method.
    #[cfg_attr(unix, allow(unused_mut))]
    pub fn cancel(&self) {
        if let Ok(mut child) = self.child.lock() {
            // If the backend's own reader thread already reaped this child
            // (the turn finished naturally at almost the same moment this
            // was called), its pid may have already been reused by the OS
            // for an unrelated process by the time the signals below fire.
            // try_wait() only tells us what this Child already knows, so it
            // can't close the race entirely — the pid could still be
            // reused in the instant between this check and the kill calls
            // — but it collapses the window from "arbitrarily long" (until
            // something notices the turn ended) to a handful of
            // instructions, for free.
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            #[cfg(unix)]
            {
                let pid = child.id() as i32;
                kill_process_tree(pid);
                // Belt and suspenders: also signal the process group
                // `process_group(0)` put the backend itself in, in case
                // something in the tree walk raced a just-forked process
                // that `ps` hadn't listed yet.
                //
                // SAFETY: `pid` came from `Child::id()`, valid for as long
                // as we hold the lock; negating it targets the process
                // group that pid leads, per POSIX kill(2).
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
            #[cfg(not(unix))]
            {
                let _ = child.kill();
            }
        }
    }
}

/// Kills `root_pid` and every process transitively descended from it.
/// Best-effort like `cancel()` itself: a `ps` failure just means nothing
/// beyond `root_pid` gets targeted here (the process-group signal in
/// `cancel()` still runs regardless).
#[cfg(unix)]
fn kill_process_tree(root_pid: i32) {
    for pid in descendants_of(root_pid, &current_process_parents()) {
        // SAFETY: `pid` is a process id read from `ps` moments ago. It may
        // have already exited (normal exit, or an earlier iteration of
        // this same loop killing its parent first) — signaling a pid that
        // no longer exists just returns ESRCH, which is fine to ignore.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

/// Every `(pid, ppid)` pair currently on the system, via `ps -eo
/// pid=,ppid=` (the trailing `=` on each key suppresses that column's
/// header, so every line is just data — no header row to skip, no
/// header-text to accidentally parse as a pid). Empty (not an error) if
/// `ps` itself fails to run; the tree walk below degrades to "just the
/// root" in that case rather than panicking over a best-effort cleanup.
#[cfg(unix)]
fn current_process_parents() -> Vec<(i32, i32)> {
    let Ok(out) = std::process::Command::new("ps").args(["-eo", "pid=,ppid="]).output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid: i32 = parts.next()?.parse().ok()?;
            let ppid: i32 = parts.next()?.parse().ok()?;
            Some((pid, ppid))
        })
        .collect()
}

/// `root` plus every pid transitively descended from it, per `pairs`
/// (`(pid, ppid)` for the whole system). Expands breadth-first: any pid
/// whose `ppid` is already a known target becomes one too, repeating
/// until a full pass finds nothing new — so a grandchild (or deeper) is
/// found regardless of what order `pairs` happens to list processes in.
fn descendants_of(root: i32, pairs: &[(i32, i32)]) -> Vec<i32> {
    let mut targets = vec![root];
    let mut remaining: Vec<(i32, i32)> = pairs.to_vec();
    loop {
        let mut added = false;
        remaining.retain(|&(pid, ppid)| {
            if targets.contains(&ppid) {
                targets.push(pid);
                added = true;
                false
            } else {
                true
            }
        });
        if !added {
            break;
        }
    }
    targets
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
    fn descendants_of_finds_a_multi_generation_chain_in_any_order() {
        // 100 -> 200 -> 300 -> 400, plus an unrelated process (999/1) that
        // must not be swept in. Pairs deliberately listed out of
        // generation order, since real `ps` output has no such guarantee.
        let pairs = [(400, 300), (999, 1), (200, 100), (300, 200), (1, 0)];
        let mut found = descendants_of(100, &pairs);
        found.sort();
        assert_eq!(found, vec![100, 200, 300, 400]);
    }

    #[test]
    fn descendants_of_a_leaf_is_just_itself() {
        let pairs = [(200, 100), (300, 200)];
        assert_eq!(descendants_of(300, &pairs), vec![300]);
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
        use std::os::unix::process::CommandExt;
        let (tx, rx) = std::sync::mpsc::channel::<AgentEvent>();
        // `process_group(0)` matters here, not just in the real backends:
        // cancel() signals the *group* (`-pid`), and without this the
        // spawned process keeps whatever group it inherited from the test
        // harness — a group that doesn't actually contain it — so the
        // kill would silently miss and this test would hang for the full
        // 30s instead of failing fast.
        let child = std::process::Command::new("sleep").arg("30").process_group(0).spawn().expect("spawn sleep 30");
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

    #[test]
    fn cancel_kills_a_real_grandchild_process_too() {
        // Regression: `cancel()` used to signal only the backend's own
        // process group — confirmed against a real opencode run, not
        // assumed: its bash tool moves whatever it runs into a *new*
        // session, so a `sleep 60` it kicked off outlived opencode itself
        // getting killed, orphaned and still running. A real two-
        // generation process chain here (test binary -> sh -> sleep) is
        // what actually exercises `ps` parsing and the multi-target kill
        // end to end — `descendants_of`'s own logic is process-group-
        // agnostic (it only ever looks at ppid), so this doesn't need to
        // reproduce the session change itself to cover the fix; that part
        // is confirmed separately, live, against real opencode.
        let (tx, rx) = std::sync::mpsc::channel::<AgentEvent>();
        // `& wait`, not a bare `sleep 300`: a shell running a single,
        // final command commonly optimizes by exec-ing straight into it
        // instead of forking — same pid, no separate child at all, which
        // would make this test pass for the wrong reason (there'd be
        // nothing else for `descendants_of` to need to find). Explicitly
        // backgrounding and waiting forces a real fork, since `sh` has to
        // stay alive to wait for it.
        let root = std::process::Command::new("sh").arg("-c").arg("sleep 300 & wait").spawn().expect("spawn sh");
        let root_pid = root.id() as i32;

        // Give `sh` a moment to actually fork `sleep` before looking for it.
        let grandchild_pid = (0..20)
            .find_map(|_| {
                std::thread::sleep(std::time::Duration::from_millis(50));
                current_process_parents().into_iter().find(|&(_, ppid)| ppid == root_pid).map(|(pid, _)| pid)
            })
            .expect("sh should have spawned sleep as a real child by now");

        let alive =
            |pid: i32| std::process::Command::new("kill").args(["-0", &pid.to_string()]).status().map(|s| s.success()).unwrap_or(false);
        assert!(alive(grandchild_pid), "sanity check: the grandchild should be running before cancel()");

        let child = Arc::new(Mutex::new(root));
        let session = AgentSession::new(rx, child.clone());
        let waiter = std::thread::spawn(move || child.lock().unwrap().wait());
        session.cancel();
        waiter.join().expect("waiter thread panicked").ok();
        drop(tx);

        assert!(!alive(root_pid), "pid {root_pid} (sh) should no longer exist after cancel()");
        assert!(!alive(grandchild_pid), "pid {grandchild_pid} (sleep, sh's child) should no longer exist after cancel()");
    }

    #[test]
    fn cancel_on_an_already_reaped_child_does_not_signal_the_now_stale_pid() {
        // Regression (Low severity): cancel() used to always sig the pid
        // from child.id(), with no check for whether the process behind
        // that pid had already exited and been reaped — e.g. by the
        // backend's own reader thread finishing a turn naturally at almost
        // the same moment cancel() runs. A reaped pid can be reused by the
        // OS for an unrelated process; signaling it then hits that
        // process's group instead. try_wait() closes the overwhelming
        // majority of that window by skipping the signal entirely once
        // this Child already knows it's gone. Exercises the exact ordering
        // that used to be racy: wait() completes (simulating the reader
        // thread) strictly before cancel() runs, so this always takes the
        // early-return path, not just possibly.
        let (tx, rx) = std::sync::mpsc::channel::<AgentEvent>();
        let mut child = std::process::Command::new("true").spawn().expect("spawn true");
        child.wait().expect("wait for `true` to exit"); // reaped before cancel() ever runs
        let child = Arc::new(Mutex::new(child));
        let session = AgentSession::new(rx, child);

        session.cancel(); // must return promptly without panicking or blocking
        drop(tx);
    }
}
