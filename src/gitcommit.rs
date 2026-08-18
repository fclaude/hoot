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

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::data::{CurationFile, Project};

pub fn commit(root: &Path, project: &Project, curation_files: &[CurationFile], message: &str) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("commit message is empty".to_string());
    }

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
            "steer-gitcommit-test-{label}-{}-{:?}",
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
}
