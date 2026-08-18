//! Real change review, backed by `git diff` in the target directory.
//!
//! Falls back to the mockup's static demo data only when the target isn't a
//! git repository at all; inside a real repo with a clean tree this reports
//! an honest empty changeset rather than silently substituting fake content.

use std::path::Path;
use std::process::{Command, Output};

use crate::data::{self, CurationFile, DiffLine, DiffLineKind, FileEntry, Hunk, Project};

pub struct ReviewData {
    pub project: Project,
    pub curation_files: Vec<CurationFile>,
    pub is_real: bool,
}

pub fn load(root: &Path) -> ReviewData {
    if !is_git_repo(root) {
        return ReviewData {
            project: data::mock_project(),
            curation_files: data::mock_curation_files(),
            is_real: false,
        };
    }

    let files = diff_files(root);

    let curation_files = files
        .iter()
        .map(|f| CurationFile { path: f.path.clone(), hunk_selected: vec![true; f.hunks.len()], status: None })
        .collect();

    let name = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| root.display().to_string());
    let project = Project { name: name.clone(), root: name, files };

    ReviewData { project, curation_files, is_real: true }
}

/// The parsed working-tree diff for `root`: empty if it's not a git repo or
/// has no changes. Also used by `sandbox.rs` to diff a disposable worktree.
pub fn diff_files(root: &Path) -> Vec<FileEntry> {
    let diff_text = run_git_diff(root);
    parse_unified_diff(&diff_text)
}

fn git(root: &Path, args: &[&str]) -> Option<Output> {
    Command::new("git").args(args).current_dir(root).output().ok()
}

fn is_git_repo(root: &Path) -> bool {
    git(root, &["rev-parse", "--is-inside-work-tree"]).map(|o| o.status.success()).unwrap_or(false)
}

/// Diffs against HEAD (staged + unstaged) when a commit exists; otherwise
/// falls back to a plain working-tree diff (e.g. a repo with zero commits).
fn run_git_diff(root: &Path) -> String {
    let has_head = git(root, &["rev-parse", "--verify", "-q", "HEAD"]).map(|o| o.status.success()).unwrap_or(false);
    let args: &[&str] =
        if has_head { &["diff", "HEAD", "--no-color", "-U3"] } else { &["diff", "--no-color", "-U3"] };
    git(root, args).map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default()
}

fn parse_unified_diff(diff: &str) -> Vec<FileEntry> {
    let mut files: Vec<FileEntry> = Vec::new();
    let mut current: Option<FileEntry> = None;
    let mut current_hunk: Option<Hunk> = None;

    let flush_hunk = |file: &mut FileEntry, hunk: &mut Option<Hunk>| {
        if let Some(h) = hunk.take() {
            file.hunks.push(h);
        }
    };
    let flush_file = |files: &mut Vec<FileEntry>, file: &mut Option<FileEntry>, hunk: &mut Option<Hunk>| {
        if let Some(mut f) = file.take() {
            flush_hunk(&mut f, hunk);
            f.hunk_count = f.hunks.len() as u32;
            files.push(f);
        }
    };

    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("diff --git ").map(parse_diff_git_path) {
            flush_file(&mut files, &mut current, &mut current_hunk);
            current = Some(FileEntry { path, hunk_count: 0, notes: 0, selected: true, flagged: false, hunks: Vec::new() });
        } else if line.starts_with("@@ ") {
            if let Some(f) = current.as_mut() {
                flush_hunk(f, &mut current_hunk);
            }
            current_hunk =
                Some(Hunk { lines: vec![DiffLine { kind: DiffLineKind::HunkHeader, text: line.to_string() }], note: None });
        } else if let Some(h) = current_hunk.as_mut() {
            let kind = if line.starts_with('+') && !line.starts_with("+++") {
                DiffLineKind::Added
            } else if line.starts_with('-') && !line.starts_with("---") {
                DiffLineKind::Removed
            } else if line.starts_with(' ') {
                DiffLineKind::Context
            } else {
                continue; // e.g. "\ No newline at end of file"
            };
            h.lines.push(DiffLine { kind, text: line.to_string() });
        }
    }
    flush_file(&mut files, &mut current, &mut current_hunk);

    files
}

/// "a/path/to/file b/path/to/file" -> "path/to/file"
fn parse_diff_git_path(rest: &str) -> String {
    match rest.rfind(" b/") {
        Some(idx) => rest[idx + 3..].to_string(),
        None => rest.to_string(),
    }
}
