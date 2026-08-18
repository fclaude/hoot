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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    const TWO_FILE_DIFF: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,4 @@
 fn main() {
-    old();
+    new();
+    extra();
 }
diff --git a/README.md b/README.md
index 3333333..4444444 100644
--- a/README.md
+++ b/README.md
@@ -1,2 +1,2 @@
-# Old Title
+# New Title
 body text
";

    #[test]
    fn parses_multiple_files_and_hunks() {
        let files = parse_unified_diff(TWO_FILE_DIFF);
        assert_eq!(files.len(), 2);

        let main_rs = &files[0];
        assert_eq!(main_rs.path, "src/main.rs");
        assert_eq!(main_rs.hunks.len(), 1);
        assert_eq!(main_rs.hunk_count, 1);
        assert!(main_rs.selected);
        assert!(!main_rs.flagged);

        let readme = &files[1];
        assert_eq!(readme.path, "README.md");
        assert_eq!(readme.hunks.len(), 1);
    }

    #[test]
    fn classifies_diff_line_kinds_correctly() {
        let files = parse_unified_diff(TWO_FILE_DIFF);
        let hunk = &files[0].hunks[0];

        assert_eq!(hunk.lines[0].kind, DiffLineKind::HunkHeader);
        assert_eq!(hunk.lines[0].text, "@@ -1,3 +1,4 @@");

        let removed: Vec<&str> = hunk.lines.iter().filter(|l| l.kind == DiffLineKind::Removed).map(|l| l.text.as_str()).collect();
        assert_eq!(removed, vec!["-    old();"]);

        let added: Vec<&str> = hunk.lines.iter().filter(|l| l.kind == DiffLineKind::Added).map(|l| l.text.as_str()).collect();
        assert_eq!(added, vec!["+    new();", "+    extra();"]);

        let context: Vec<&str> = hunk.lines.iter().filter(|l| l.kind == DiffLineKind::Context).map(|l| l.text.as_str()).collect();
        assert_eq!(context, vec![" fn main() {", " }"]);
    }

    #[test]
    fn multiple_hunks_in_one_file_stay_separate() {
        let diff = "\
diff --git a/f.txt b/f.txt
index 111..222 100644
--- a/f.txt
+++ b/f.txt
@@ -1,2 +1,2 @@
-line1
+line1-CHANGED
 line2
@@ -9,2 +9,2 @@ line8
 line9
-line10
+line10-CHANGED
";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!(files[0].hunks[0].lines[0].text, "@@ -1,2 +1,2 @@");
        assert_eq!(files[0].hunks[1].lines[0].text, "@@ -9,2 +9,2 @@ line8");
    }

    #[test]
    fn empty_diff_produces_no_files() {
        assert!(parse_unified_diff("").is_empty());
    }

    #[test]
    fn parse_diff_git_path_strips_a_and_b_prefixes() {
        assert_eq!(parse_diff_git_path("a/src/main.rs b/src/main.rs"), "src/main.rs");
        assert_eq!(parse_diff_git_path("a/nested/dir/file.py b/nested/dir/file.py"), "nested/dir/file.py");
    }

    // --- integration: exercises is_git_repo/run_git_diff/load against a real repo ---

    fn scratch_repo(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "steer-gitreview-test-{label}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q"]);
        run(&dir, &["config", "user.email", "test@example.com"]);
        run(&dir, &["config", "user.name", "test"]);
        dir
    }

    fn run(dir: &Path, args: &[&str]) {
        let status = Command::new("git").args(args).current_dir(dir).status().unwrap();
        assert!(status.success(), "git {args:?} failed in {dir:?}");
    }

    #[test]
    fn load_falls_back_to_mock_outside_a_git_repo() {
        let dir = std::env::temp_dir().join(format!("steer-gitreview-not-a-repo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let review = load(&dir);
        assert!(!review.is_real);
        assert_eq!(review.project.name, "search-index"); // mock_project's fixed name

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_reports_honest_empty_state_on_a_clean_repo() {
        let dir = scratch_repo("clean");
        fs::write(dir.join("f.txt"), "hello\n").unwrap();
        run(&dir, &["add", "-A"]);
        run(&dir, &["commit", "-q", "-m", "init"]);

        let review = load(&dir);
        assert!(review.is_real);
        assert!(review.project.files.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_reflects_a_real_uncommitted_change() {
        let dir = scratch_repo("dirty");
        fs::write(dir.join("f.txt"), "line1\nline2\n").unwrap();
        run(&dir, &["add", "-A"]);
        run(&dir, &["commit", "-q", "-m", "init"]);
        fs::write(dir.join("f.txt"), "line1-changed\nline2\n").unwrap();

        let review = load(&dir);
        assert!(review.is_real);
        assert_eq!(review.project.files.len(), 1);
        assert_eq!(review.project.files[0].path, "f.txt");
        assert_eq!(review.curation_files.len(), 1);
        assert_eq!(review.curation_files[0].total(), 1);
        assert_eq!(review.curation_files[0].selected(), 1); // starts fully selected

        let _ = fs::remove_dir_all(&dir);
    }
}
