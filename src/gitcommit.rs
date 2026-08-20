//! Real `git commit` from the Curation screen.
//!
//! Whole-file selections (`selected == total`) are staged with a plain
//! `git add`. Partial selections are staged by reconstructing a patch from
//! only the selected hunks' original text (each hunk carries its own
//! `@@ ... @@` header anchored to the base file, so dropping unselected
//! hunks entirely — rather than editing their contents — keeps every
//! remaining hunk's line numbers valid) and applying it with
//! `git apply --cached`. New/deleted files are only supported via the
//! whole-file path; a partial selection on one falls back to whole-file
//! staging too, since there's no `-- a/path`/`-- b/path` pair to patch
//! against.
//!
//! The index is reset to HEAD before any of that staging happens. Without
//! it, `commit()` only ever *adds* to whatever the index already held —
//! nothing here ever unstages anything — so any change already staged
//! before hoot ran (an external `git add`, a leftover partial stage from
//! earlier) would ride along into the final `git commit` regardless of
//! whether Curate's selection included it at all. The whole premise of
//! this screen is that the committed result matches the checked hunks
//! exactly, so the index can't be allowed to carry in state Curate never
//! had a say over.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::data::{CurationFile, Project};

pub fn commit(root: &Path, project: &Project, curation_files: &[CurationFile], message: &str) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("commit message is empty".to_string());
    }

    reset_index(root)?;

    let mut staged_any = false;
    for cf in curation_files {
        if cf.selected() == 0 {
            continue;
        }
        let Some(file) = project.files.iter().find(|f| f.path == cf.path) else { continue };

        if cf.selected() == cf.total() {
            stage_whole_file(root, &cf.path)?;
        } else {
            stage_partial_hunks(root, &cf.path, &file.hunks, &cf.hunk_selected)?;
        }
        staged_any = true;
    }

    if !staged_any {
        return Err("nothing selected to commit".to_string());
    }

    run_commit(root, message)
}

fn run_git(root: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git").args(args).current_dir(root).output().map_err(|e| e.to_string())
}

/// Unstages everything — back to matching HEAD, or an empty index in a repo
/// with no commits yet — without touching the working tree. Bare `git
/// reset` (no ref, no pathspec) does the right thing in both cases; passing
/// an explicit `HEAD` would fail outright when there's no commit to name.
fn reset_index(root: &Path) -> Result<(), String> {
    let out = run_git(root, &["reset", "-q"])?;
    if !out.status.success() {
        return Err(format!("git reset: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

fn run_git_with_stdin(root: &Path, args: &[&str], input: &[u8]) -> Result<std::process::Output, String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child.stdin.take().expect("piped stdin").write_all(input).map_err(|e| e.to_string())?;
    child.wait_with_output().map_err(|e| e.to_string())
}

fn stage_whole_file(root: &Path, path: &str) -> Result<(), String> {
    let out = run_git(root, &["add", "--", path])?;
    if !out.status.success() {
        return Err(format!("git add {path}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

fn stage_partial_hunks(root: &Path, path: &str, hunks: &[crate::data::Hunk], hunk_selected: &[bool]) -> Result<(), String> {
    let mut patch = format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n");
    let mut any = false;
    for (hunk, &sel) in hunks.iter().zip(hunk_selected) {
        if !sel {
            continue;
        }
        any = true;
        for line in &hunk.lines {
            patch.push_str(&line.text);
            patch.push('\n');
        }
    }
    if !any {
        return Ok(());
    }

    let out = run_git_with_stdin(root, &["apply", "--cached", "-"], patch.as_bytes())?;
    if !out.status.success() {
        return Err(format!("git apply --cached {path}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

fn run_commit(root: &Path, message: &str) -> Result<String, String> {
    let out = run_git_with_stdin(root, &["commit", "-F", "-"], message.as_bytes())?;
    if !out.status.success() {
        return Err(format!("git commit: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }

    let log = run_git(root, &["log", "-1", "--oneline"])?;
    Ok(String::from_utf8_lossy(&log.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Project;
    use std::fs;
    use std::path::PathBuf;

    fn scratch_repo(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hoot-gitcommit-test-{label}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        for args in [["init", "-q"].as_slice(), &["config", "user.email", "test@example.com"], &["config", "user.name", "test"]] {
            assert!(Command::new("git").args(args).current_dir(&dir).status().unwrap().success());
        }
        dir
    }

    fn project_and_curation(dir: &Path) -> (Project, Vec<CurationFile>) {
        let review = crate::gitreview::load(dir);
        (review.project, review.curation_files)
    }

    #[test]
    fn rejects_empty_message() {
        let dir = scratch_repo("empty-msg");
        let (project, cfs) = project_and_curation(&dir);
        let err = commit(&dir, &project, &cfs, "  ").unwrap_err();
        assert!(err.contains("empty"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_nothing_selected() {
        let dir = scratch_repo("nothing-selected");
        fs::write(dir.join("f.txt"), "line1\nline2\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::write(dir.join("f.txt"), "line1-changed\nline2\n").unwrap();

        let (project, mut cfs) = project_and_curation(&dir);
        for cf in &mut cfs {
            for s in &mut cf.hunk_selected {
                *s = false;
            }
        }
        let err = commit(&dir, &project, &cfs, "should not land").unwrap_err();
        assert!(err.contains("nothing selected"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn whole_file_commit_lands_and_cleans_the_tree() {
        let dir = scratch_repo("whole-file");
        fs::write(dir.join("f.txt"), "line1\nline2\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::write(dir.join("f.txt"), "line1-changed\nline2\n").unwrap();

        let (project, cfs) = project_and_curation(&dir);
        assert_eq!(cfs.len(), 1);
        assert_eq!(cfs[0].selected(), cfs[0].total()); // starts fully selected

        let summary = commit(&dir, &project, &cfs, "Update f.txt").unwrap();
        assert!(summary.contains("Update f.txt"), "{summary}");

        let status = Command::new("git").args(["status", "--short"]).current_dir(&dir).output().unwrap();
        assert!(status.stdout.is_empty(), "expected a clean tree, got {:?}", String::from_utf8_lossy(&status.stdout));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_hunk_commit_leaves_the_unselected_hunk_outstanding() {
        let dir = scratch_repo("partial-hunk");
        let original: String = (1..=20).map(|n| format!("line{n}\n")).collect();
        fs::write(dir.join("f.txt"), &original).unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();

        // Two far-apart single-line changes -> two separate hunks under -U3.
        let mut lines: Vec<String> = (1..=20).map(|n| format!("line{n}")).collect();
        lines[0] = "line1-CHANGED".to_string();
        lines[19] = "line20-CHANGED".to_string();
        fs::write(dir.join("f.txt"), lines.join("\n") + "\n").unwrap();

        let (project, mut cfs) = project_and_curation(&dir);
        assert_eq!(cfs[0].total(), 2, "expected two separate hunks");
        cfs[0].hunk_selected[0] = true;
        cfs[0].hunk_selected[1] = false;

        commit(&dir, &project, &cfs, "Fix line1 only").unwrap();

        let log = Command::new("git").args(["show", "HEAD:f.txt"]).current_dir(&dir).output().unwrap();
        let committed = String::from_utf8_lossy(&log.stdout);
        assert!(committed.starts_with("line1-CHANGED\n"), "{committed}");
        assert!(committed.contains("line20\n"), "line20 should not be committed yet: {committed}");

        // The second hunk should still show up as an outstanding real diff.
        let remaining = crate::gitreview::diff_files(&dir);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].hunks.len(), 1);
        assert!(remaining[0].hunks[0].lines.iter().any(|l| l.text.contains("line20-CHANGED")));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_deselected_hunk_is_excluded_even_when_the_whole_file_was_already_staged() {
        // Regression: the flagship promise here is "committed == exactly
        // what Curate has checked." That broke the moment the repo already
        // had staged content when hoot ran — `git add`/`git apply --cached`
        // only ever ADD to whatever's already in the index, they never
        // remove from it, so a hunk the user explicitly deselected could
        // still ride into the commit if it happened to already be staged
        // (an external `git add`, a leftover partial stage from earlier,
        // ...) before Curate ever got a say.
        let dir = scratch_repo("already-staged");
        let original: String = (1..=20).map(|n| format!("line{n}\n")).collect();
        fs::write(dir.join("f.txt"), &original).unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();

        let mut lines: Vec<String> = (1..=20).map(|n| format!("line{n}")).collect();
        lines[0] = "line1-CHANGED".to_string();
        lines[19] = "line20-CHANGED".to_string();
        fs::write(dir.join("f.txt"), lines.join("\n") + "\n").unwrap();

        // The whole file is staged *before* Curate even loads — outside
        // hoot entirely, exactly like a stray `git add .` or a half-done
        // manual stage.
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();

        let (project, mut cfs) = project_and_curation(&dir);
        assert_eq!(cfs[0].total(), 2, "expected two separate hunks");
        cfs[0].hunk_selected[0] = false; // explicitly deselect line1's hunk
        cfs[0].hunk_selected[1] = true;

        commit(&dir, &project, &cfs, "Fix line20 only").unwrap();

        let log = Command::new("git").args(["show", "HEAD:f.txt"]).current_dir(&dir).output().unwrap();
        let committed = String::from_utf8_lossy(&log.stdout);
        assert!(committed.starts_with("line1\n"), "the deselected hunk must not be committed: {committed}");
        assert!(committed.contains("line20-CHANGED\n"), "{committed}");

        // And the deselected hunk should still be sitting there as a real,
        // outstanding change — not silently dropped, just not committed.
        let remaining = crate::gitreview::diff_files(&dir);
        assert_eq!(remaining.len(), 1);
        assert!(remaining[0].hunks.iter().any(|h| h.lines.iter().any(|l| l.text.contains("line1-CHANGED"))));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fully_deselected_file_is_not_committed_even_when_it_was_already_staged() {
        // The more direct version of the regression above: a file with
        // *zero* selected hunks never reaches stage_whole_file or
        // stage_partial_hunks at all — commit()'s loop just skips it. If it
        // was already sitting in the index before hoot ran (an external
        // `git add`, a leftover stage from earlier), nothing here ever
        // touches it, and the final plain `git commit` would sweep it in
        // right alongside whatever Curate actually selected.
        let dir = scratch_repo("already-staged-deselected");
        fs::write(dir.join("a.txt"), "a1\na2\n").unwrap();
        fs::write(dir.join("b.txt"), "b1\nb2\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();

        fs::write(dir.join("a.txt"), "a1-changed\na2\n").unwrap();
        fs::write(dir.join("b.txt"), "b1-changed\nb2\n").unwrap();
        // Both staged externally, before Curate ever loads.
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();

        let (project, mut cfs) = project_and_curation(&dir);
        for cf in &mut cfs {
            if cf.path == "b.txt" {
                for s in &mut cf.hunk_selected {
                    *s = false; // the user does not want b.txt in this commit
                }
            }
        }

        commit(&dir, &project, &cfs, "Update a.txt only").unwrap();

        let log = Command::new("git").args(["show", "--stat", "--oneline", "HEAD"]).current_dir(&dir).output().unwrap();
        let stat = String::from_utf8_lossy(&log.stdout);
        assert!(stat.contains("a.txt"), "{stat}");
        assert!(!stat.contains("b.txt"), "b.txt was deselected and must not be in the commit: {stat}");

        // b.txt's change should still be outstanding (staged or not — just
        // not committed) rather than silently discarded.
        let status = Command::new("git").args(["status", "--porcelain", "--", "b.txt"]).current_dir(&dir).output().unwrap();
        assert!(!status.stdout.is_empty(), "b.txt's change should still be pending somewhere");

        let _ = fs::remove_dir_all(&dir);
    }
}
