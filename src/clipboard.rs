//! Copies text to the system clipboard by shelling out to whatever
//! platform clipboard tool is available, the same "trust the real external
//! tool" approach as `editor.rs` — rather than linking a clipboard crate
//! (which on Linux means talking to X11 or Wayland directly, and doesn't
//! degrade gracefully when neither is running, e.g. over plain SSH).

use std::io::Write;
use std::process::{Command, Stdio};

/// Tries each candidate command in order, piping `text` to its stdin, and
/// returns the first one that's actually installed. Fails only if none of
/// them are — a real copy failure (e.g. the clipboard tool crashes) still
/// reports its own error rather than silently trying the next candidate,
/// since that'd hide a real problem behind a misleading "not found".
pub fn copy(text: &str) -> Result<(), String> {
    copy_with(&candidates(), text)
}

#[cfg(target_os = "macos")]
fn candidates() -> Vec<(&'static str, &'static [&'static str])> {
    vec![("pbcopy", &[])]
}

#[cfg(not(target_os = "macos"))]
fn candidates() -> Vec<(&'static str, &'static [&'static str])> {
    // wl-copy for Wayland, xclip/xsel for X11 — tried in that order since
    // wl-copy is a no-op (or missing) under X11 but xclip/xsel would just
    // hang waiting for a display that isn't there under Wayland-only.
    vec![("wl-copy", &[]), ("xclip", &["-selection", "clipboard"]), ("xsel", &["--clipboard", "--input"])]
}

fn copy_with(candidates: &[(&str, &[&str])], text: &str) -> Result<(), String> {
    for (cmd, args) in candidates {
        if let Some(result) = try_one(cmd, args, text) {
            return result;
        }
    }
    let tried: Vec<&str> = candidates.iter().map(|(cmd, _)| *cmd).collect();
    Err(format!("no clipboard tool found (tried: {})", tried.join(", ")))
}

/// `None` means "`cmd` isn't installed, try the next candidate"; `Some`
/// means it ran (successfully or not) and that's the final answer.
fn try_one(cmd: &str, args: &[&str], text: &str) -> Option<Result<(), String>> {
    let mut child = match Command::new(cmd).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(c) => c,
        // Only "no such command" means move on to the next candidate. Any
        // other spawn failure — a permission problem, a missing
        // interpreter — used to be reported as "no clipboard tool found",
        // which sends someone looking for a tool that is installed and
        // sitting right there.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => return Some(Err(format!("{cmd}: {e}"))),
    };
    // Held rather than returned on. A tool that exits before reading its
    // stdin makes this write fail with EPIPE, and returning that lost the
    // exit status underneath — reporting "Broken pipe" for what is really
    // "xclip died with status 1". The broken pipe is the symptom; the
    // status is the cause, so the status wins when there is one. Taking
    // stdin also closes it here, which is what lets a tool that *does*
    // read to EOF finish.
    let write_err = child.stdin.take().and_then(|mut stdin| stdin.write_all(text.as_bytes()).err());
    Some(match child.wait() {
        Ok(status) if !status.success() => Err(format!("{cmd} exited with {status}")),
        Ok(_) => match write_err {
            Some(e) => Err(format!("{cmd}: {e}")),
            None => Ok(()),
        },
        Err(e) => Err(format!("{cmd}: {e}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A scratch file path for a stand-in clipboard tool to write its
    /// stdin to, so tests never touch the actual system clipboard.
    ///
    /// The stand-in is `sh -c <body>` rather than a generated executable.
    /// Writing a script and immediately exec'ing it races with every other
    /// test thread: a concurrent `fork` inherits the still-open write
    /// descriptor, and the `exec` then fails with ETXTBSY ("text file
    /// busy") — intermittently, on whichever machine happens to interleave
    /// them that way. Nothing here needs a file to exist to test the
    /// candidate logic, and `sh` is already required by every other test
    /// in this file.
    fn capture_path(tag: &str) -> std::path::PathBuf {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("hoot-clipboard-capture-{}-{tag}-{suffix}", std::process::id()))
    }

    #[test]
    fn copies_to_the_first_available_candidate() {
        let capture = capture_path("first");
        let body = format!("cat > {}", capture.to_str().unwrap());
        let args = ["-c", body.as_str()];
        let candidates: Vec<(&str, &[&str])> = vec![("sh", &args)];

        copy_with(&candidates, "hello clipboard").unwrap();
        assert_eq!(std::fs::read_to_string(&capture).unwrap(), "hello clipboard");

        let _ = std::fs::remove_file(&capture);
    }

    #[test]
    fn falls_through_to_the_next_candidate_when_the_first_is_missing() {
        let capture = capture_path("fallthrough");
        let body = format!("cat > {}", capture.to_str().unwrap());
        let args = ["-c", body.as_str()];
        let candidates: Vec<(&str, &[&str])> = vec![("hoot-definitely-not-a-real-binary-xyz", &[]), ("sh", &args)];

        copy_with(&candidates, "second candidate wins").unwrap();
        assert_eq!(std::fs::read_to_string(&capture).unwrap(), "second candidate wins");

        let _ = std::fs::remove_file(&capture);
    }

    #[test]
    fn reports_a_real_failure_instead_of_trying_the_next_candidate() {
        // `exit 1` without reading stdin, deliberately: that is what makes
        // the parent's write fail with EPIPE, and the whole point is that
        // the exit status is reported rather than the broken pipe it
        // caused. Whether the write loses the race is timing-dependent —
        // this passed for a long time simply by usually winning it.
        let args = ["-c", "exit 1"];
        let candidates: Vec<(&str, &[&str])> = vec![("sh", &args), ("cat", &[])];

        let err = copy_with(&candidates, "whatever").unwrap_err();
        assert!(err.contains("exited"), "{err}");
    }

    #[test]
    fn errors_when_no_candidate_is_available() {
        let candidates: Vec<(&str, &[&str])> =
            vec![("hoot-definitely-not-a-real-binary-xyz1", &[]), ("hoot-definitely-not-a-real-binary-xyz2", &[])];
        let err = copy_with(&candidates, "whatever").unwrap_err();
        assert!(err.contains("no clipboard tool found"), "{err}");
    }
}
