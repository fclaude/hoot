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
