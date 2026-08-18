//! Sandboxed write/approval loop.
//!
//! Testing confirmed `pi` has no native "pause and wait for external
//! approval before writing" hook — `write` executes as soon as the model
//! calls it, `--approve` or not. So approval happens at the filesystem
//! level instead: an edit turn runs against a disposable `git worktree`,
//! never the real target directory. Only after the user reviews the
//! worktree's real diff and explicitly approves does anything touch the
//! real project — via `git apply`, which is inherently all-or-nothing per
//! invocation (this doesn't attempt partial-hunk approval the way Curation
//! does).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::data::FileEntry;

/// Creates a detached worktree checked out from HEAD at a scratch path.
/// Requires the target to already be a git repo with at least one commit.
pub fn create(target_dir: &Path) -> Result<PathBuf, String> {
    let has_head = Command::new("git")
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .current_dir(target_dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_head {
        return Err("Edit mode needs a git repository with at least one commit".to_string());
    }

    let suffix = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("steer-sandbox-{}-{suffix}", std::process::id()));

    let out = Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(&dir)
        .arg("HEAD")
        .current_dir(target_dir)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("git worktree add: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(dir)
}

/// The real diff of everything changed inside the sandbox so far.
pub fn diff(worktree: &Path) -> Vec<FileEntry> {
    crate::gitreview::diff_files(worktree)
}

/// Applies the sandbox's current diff to the real target directory.
pub fn apply(worktree: &Path, target_dir: &Path) -> Result<(), String> {
    let diff_out = Command::new("git")
        .args(["diff", "--no-color", "-U3"])
        .current_dir(worktree)
        .output()
        .map_err(|e| e.to_string())?;
    if !diff_out.status.success() {
        return Err(format!("git diff (sandbox): {}", String::from_utf8_lossy(&diff_out.stderr).trim()));
    }
    if diff_out.stdout.is_empty() {
        return Err("nothing to apply — no changes in the sandbox".to_string());
    }

    let mut child = Command::new("git")
        .arg("apply")
        .current_dir(target_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child.stdin.take().expect("piped stdin").write_all(&diff_out.stdout).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("git apply: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// Discards the sandbox worktree, whether or not it was applied. Must run
/// with its cwd inside the *main* repo (`target_dir`), not the worktree
/// itself and not wherever the steer process happened to start — `git
/// worktree remove` needs to resolve which repo it's operating on.
pub fn discard(target_dir: &Path, worktree: &Path) {
    let _ = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(worktree)
        .current_dir(target_dir)
        .output();
}
