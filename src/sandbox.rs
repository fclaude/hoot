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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_repo(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "steer-sandbox-test-{label}-{}-{:?}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        for args in [["init", "-q"].as_slice(), &["config", "user.email", "test@example.com"], &["config", "user.name", "test"]] {
            assert!(Command::new("git").args(args).current_dir(&dir).status().unwrap().success());
        }
        dir
    }

    #[test]
    fn create_fails_without_a_commit() {
        let dir = scratch_repo("no-head");
        let err = create(&dir).unwrap_err();
        assert!(err.contains("at least one commit"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_round_trip_create_edit_diff_apply_discard() {
        let dir = scratch_repo("round-trip");
        fs::write(dir.join("f.txt"), "hello\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();

        let worktree = create(&dir).unwrap();
        assert!(worktree.exists());
        // Editing the sandbox must never touch the real file.
        fs::write(worktree.join("f.txt"), "hello from sandbox\n").unwrap();
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "hello\n");

        let changes = diff(&worktree);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "f.txt");

        apply(&worktree, &dir).unwrap();
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "hello from sandbox\n");

        discard(&dir, &worktree);
        assert!(!worktree.exists(), "worktree should be removed after discard");
        let worktrees = Command::new("git").args(["worktree", "list"]).current_dir(&dir).output().unwrap();
        let listing = String::from_utf8_lossy(&worktrees.stdout);
        assert_eq!(listing.lines().count(), 1, "expected only the main worktree left: {listing}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_with_no_sandbox_changes_errors() {
        let dir = scratch_repo("no-changes");
        fs::write(dir.join("f.txt"), "hello\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();

        let worktree = create(&dir).unwrap();
        let err = apply(&worktree, &dir).unwrap_err();
        assert!(err.contains("nothing to apply"), "{err}");

        discard(&dir, &worktree);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discard_without_apply_leaves_real_dir_untouched() {
        let dir = scratch_repo("reject-path");
        fs::write(dir.join("f.txt"), "hello\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();

        let worktree = create(&dir).unwrap();
        fs::write(worktree.join("f.txt"), "sandbox edit that gets rejected\n").unwrap();
        discard(&dir, &worktree);

        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "hello\n");
        assert!(!worktree.exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
