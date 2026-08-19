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
        Err(_) => return None,
    };
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(text.as_bytes()) {
            return Some(Err(format!("{cmd}: {e}")));
        }
    }
    Some(match child.wait() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("{cmd} exited with {status}")),
        Err(e) => Err(format!("{cmd}: {e}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A throwaway `#!/bin/sh` script standing in for a real clipboard
    /// tool, so tests never touch the actual system clipboard. `body` is
    /// free to use `"$CAPTURE"` — it's set to a scratch file path this
    /// script can write its stdin to, so the test can inspect what a
    /// "copy" actually received.
    fn script(body: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let script_path = std::env::temp_dir().join(format!("steer-clipboard-test-{}-{suffix}", std::process::id()));
        let capture_path = std::env::temp_dir().join(format!("steer-clipboard-capture-{}-{suffix}", std::process::id()));
        let mut f = std::fs::File::create(&script_path).unwrap();
        writeln!(f, "#!/bin/sh\nCAPTURE={:?}\n{body}", capture_path.to_str().unwrap()).unwrap();
        drop(f);
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        (script_path, capture_path)
    }

    #[test]
    fn copies_to_the_first_available_candidate() {
        let (script_path, capture_path) = script("cat > \"$CAPTURE\"");
        let candidates: Vec<(&str, &[&str])> = vec![(script_path.to_str().unwrap(), &[])];

        copy_with(&candidates, "hello clipboard").unwrap();
        assert_eq!(std::fs::read_to_string(&capture_path).unwrap(), "hello clipboard");

        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&capture_path);
    }

    #[test]
    fn falls_through_to_the_next_candidate_when_the_first_is_missing() {
        let (script_path, capture_path) = script("cat > \"$CAPTURE\"");
        let candidates: Vec<(&str, &[&str])> =
            vec![("steer-definitely-not-a-real-binary-xyz", &[]), (script_path.to_str().unwrap(), &[])];

        copy_with(&candidates, "second candidate wins").unwrap();
        assert_eq!(std::fs::read_to_string(&capture_path).unwrap(), "second candidate wins");

        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&capture_path);
    }

    #[test]
    fn reports_a_real_failure_instead_of_trying_the_next_candidate() {
        let (script_path, _capture_path) = script("exit 1");
        let candidates: Vec<(&str, &[&str])> = vec![(script_path.to_str().unwrap(), &[]), ("cat", &[])];

        let err = copy_with(&candidates, "whatever").unwrap_err();
        assert!(err.contains("exited"), "{err}");

        let _ = std::fs::remove_file(&script_path);
    }

    #[test]
    fn errors_when_no_candidate_is_available() {
        let candidates: Vec<(&str, &[&str])> =
            vec![("steer-definitely-not-a-real-binary-xyz1", &[]), ("steer-definitely-not-a-real-binary-xyz2", &[])];
        let err = copy_with(&candidates, "whatever").unwrap_err();
        assert!(err.contains("no clipboard tool found"), "{err}");
    }
}
