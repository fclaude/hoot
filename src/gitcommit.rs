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
//! Before any of that staging happens, three guards run in order:
//!
//! 1. Nothing selected at all → bail out before touching git. Checked
//!    first so a no-op commit attempt is actually a no-op, not a mutation
//!    that happens to also report an error.
//! 2. The index already has staged content → refuse outright, without
//!    touching it. An earlier version of this function instead reset the
//!    index to HEAD before restaging exactly the selection — correct for
//!    *this screen's* view of the world, but Curate only knows about
//!    changes `git diff` can see; it has no idea whether pre-existing
//!    staged content was something the user carefully built by hand
//!    outside hoot for an unrelated reason. Silently discarding that would
//!    trade a content-correctness bug for a data-loss one. Refusing and
//!    telling the user to resolve it themselves (`git status`) is the
//!    honest option: hoot only ever mutates an index it knows started
//!    clean.
//! 3. Each selected file's hunks are re-fetched from disk and compared
//!    against what `project` (Curate's last synced view) has, immediately
//!    before *that file* is staged — not trusted from whenever the caller
//!    last polled, and not checked once upfront for the whole batch
//!    either. `project` can be a poll tick or more stale: a background
//!    agent turn or an external edit can change a selected file between
//!    when the user looked at it in Curate and when they press `c` — or,
//!    for a multi-file commit, while an *earlier* file in the batch is
//!    still being staged. Staging from stale hunk data would mean
//!    `git add`ing whatever the file *currently* contains (not what was
//!    reviewed) for a whole-file selection, or handing a patch built from
//!    old line content to `git apply --cached` for a partial one — best
//!    case that errors, worst case it silently commits content nobody
//!    actually looked at. Refuse and ask for a re-review rather than
//!    gamble on which case it is.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::data::{CurationFile, Project};

pub fn commit(root: &Path, project: &Project, curation_files: &[CurationFile], message: &str) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("commit message is empty".to_string());
    }
    if !curation_files.iter().any(|cf| cf.selected() > 0) {
        return Err("nothing selected to commit".to_string());
    }
    if index_has_staged_changes(root)? {
        return Err(
            "there are changes already staged outside hoot (see `git status`) — resolve or unstage those first, then try again".to_string()
        );
    }

    let mut staged_any = false;
    for cf in curation_files {
        if cf.selected() == 0 {
            continue;
        }
        let Some(file) = project.files.iter().find(|f| f.path == cf.path) else { continue };

        // Re-fetched immediately before staging *this* file, not once
        // upfront for the whole batch: a multi-file commit was otherwise
        // still racy for every file after the first — the one upfront
        // check only ever reflected disk state from before *any* staging
        // happened, so a file two or three deep in the loop could still
        // get staged against hunks that had gone stale while the files
        // ahead of it were being staged. This costs a full re-diff per
        // selected file, which is real overhead on a repo with many
        // changed files — a deliberate tradeoff: correctness on the
        // operation this whole screen exists for over commit-time
        // latency, on an action a user triggers occasionally, not in a
        // hot loop.
        let current = crate::gitreview::diff_files(root);
        let now = current.iter().find(|f| f.path == cf.path).map(|f| &f.hunks);
        if now != Some(&file.hunks) {
            return Err(format!("{} changed since it was last reviewed here \u{2014} re-open Curate and check it again", cf.path));
        }

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

/// True if the index already differs from HEAD (or, pre-first-commit, from
/// an empty tree) — i.e. something is staged that hoot didn't just put
/// there. `git diff --cached --quiet` exits 1 when there's a difference, 0
/// when there's none; any other exit code is a real git failure, not a
/// yes/no answer.
fn index_has_staged_changes(root: &Path) -> Result<bool, String> {
    let out = run_git(root, &["diff", "--cached", "--quiet"])?;
    match out.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(format!("git diff --cached: {}", String::from_utf8_lossy(&out.stderr).trim())),
    }
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
    fn refuses_to_commit_when_the_index_already_has_staged_changes() {
        // Regression, second pass: an earlier fix here reset the index to
        // HEAD before restaging exactly Curate's selection, which *did*
        // make the commit content correct — but it did that by silently
        // discarding whatever was already staged, with no way to know
        // whether that was a stray `git add .` or work the user had
        // carefully built by hand outside hoot for something unrelated.
        // Correct-but-destructive is still not safe: hoot must refuse
        // outright rather than guess, and leave the index exactly as it
        // found it.
        let dir = scratch_repo("already-staged-refuse");
        let original: String = (1..=20).map(|n| format!("line{n}\n")).collect();
        fs::write(dir.join("f.txt"), &original).unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();

        let mut lines: Vec<String> = (1..=20).map(|n| format!("line{n}")).collect();
        lines[0] = "line1-CHANGED".to_string();
        lines[19] = "line20-CHANGED".to_string();
        fs::write(dir.join("f.txt"), lines.join("\n") + "\n").unwrap();

        // Staged *before* Curate even loads — outside hoot entirely.
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        let staged_before = Command::new("git").args(["diff", "--cached"]).current_dir(&dir).output().unwrap().stdout;

        let (project, mut cfs) = project_and_curation(&dir);
        assert_eq!(cfs[0].total(), 2, "expected two separate hunks");
        cfs[0].hunk_selected[0] = false;
        cfs[0].hunk_selected[1] = true;

        let err = commit(&dir, &project, &cfs, "Fix line20 only").unwrap_err();
        assert!(err.contains("already staged"), "{err}");

        // Nothing should have moved: HEAD unchanged, index exactly as the
        // user left it.
        let log = Command::new("git").args(["log", "--oneline"]).current_dir(&dir).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).lines().count(), 1, "no new commit should have landed");
        let staged_after = Command::new("git").args(["diff", "--cached"]).current_dir(&dir).output().unwrap().stdout;
        assert_eq!(staged_before, staged_after, "the pre-existing staged content must be untouched");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_selected_never_touches_git_even_with_unrelated_staged_content() {
        // The "nothing selected" guard must run *before* any git mutation
        // — otherwise a no-op commit attempt (everything deselected, or a
        // stray keypress) would still reset/inspect the index, which is
        // itself an unwanted side effect on a repo hoot has no business
        // touching when it isn't actually about to commit anything.
        let dir = scratch_repo("nothing-selected-with-staged");
        fs::write(dir.join("a.txt"), "a1\na2\n").unwrap();
        fs::write(dir.join("staged.txt"), "s1\ns2\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();

        fs::write(dir.join("a.txt"), "a1-changed\na2\n").unwrap();
        fs::write(dir.join("staged.txt"), "s1-changed\ns2\n").unwrap();
        Command::new("git").args(["add", "staged.txt"]).current_dir(&dir).status().unwrap();
        let staged_before = Command::new("git").args(["diff", "--cached"]).current_dir(&dir).output().unwrap().stdout;

        let (project, mut cfs) = project_and_curation(&dir);
        for cf in &mut cfs {
            for s in &mut cf.hunk_selected {
                *s = false;
            }
        }

        let err = commit(&dir, &project, &cfs, "should not run").unwrap_err();
        assert!(err.contains("nothing selected"), "{err}");

        let staged_after = Command::new("git").args(["diff", "--cached"]).current_dir(&dir).output().unwrap().stdout;
        assert_eq!(staged_before, staged_after, "an already-staged file must survive a no-op commit attempt untouched");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_to_commit_a_file_that_changed_since_it_was_last_reviewed() {
        // Regression: `project`/`curation_files` are whatever Curate last
        // synced — a poll tick or more stale by the time the user actually
        // presses `c`. A background agent turn or an external edit landing
        // on a selected file in that window used to be invisible to
        // commit(): it staged whatever the file *currently* contained (or
        // handed git apply a patch built from now-outdated line content),
        // not what was actually reviewed.
        let dir = scratch_repo("stale-review");
        fs::write(dir.join("f.txt"), "line1\nline2\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::write(dir.join("f.txt"), "line1-changed\nline2\n").unwrap();

        let (project, cfs) = project_and_curation(&dir);
        assert_eq!(cfs[0].selected(), cfs[0].total());

        // The file changes again *after* Curate synced — simulating a
        // background agent turn or an external edit landing in the gap
        // before the user actually presses `c`.
        fs::write(dir.join("f.txt"), "line1-changed\nline2-changed-too\n").unwrap();

        let err = commit(&dir, &project, &cfs, "should be refused").unwrap_err();
        assert!(err.contains("f.txt"), "{err}");
        assert!(err.contains("changed since"), "{err}");

        // Nothing should have been staged or committed.
        let log = Command::new("git").args(["log", "--oneline"]).current_dir(&dir).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).lines().count(), 1, "no new commit should have landed");
        let staged = Command::new("git").args(["diff", "--cached"]).current_dir(&dir).output().unwrap().stdout;
        assert!(staged.is_empty(), "nothing should have been staged: {}", String::from_utf8_lossy(&staged));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_multi_file_commit_still_catches_staleness_on_a_later_file() {
        // The freshness check now runs fresh, immediately before each
        // file's own staging call, instead of once upfront for the whole
        // batch — tightening the real window (a file changing while an
        // *earlier* one in the batch is being staged) that a one-time
        // check couldn't narrow no matter how it was structured. That
        // specific timing improvement isn't really something a
        // synchronous, single-threaded test can reproduce (there's no
        // true concurrent modification to inject mid-loop) — both the old
        // and new logic already catch a file that's stale by the time
        // commit() is even called, regardless of its position. What this
        // *does* usefully guard: that checking happens per-file, not just
        // once for whichever file iterates first — a plausible way a
        // refactor of this could regress without the timing angle
        // actually failing here.
        let dir = scratch_repo("stale-review-multi");
        fs::write(dir.join("a.txt"), "a1\na2\n").unwrap();
        fs::write(dir.join("b.txt"), "b1\nb2\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::write(dir.join("a.txt"), "a1-changed\na2\n").unwrap();
        fs::write(dir.join("b.txt"), "b1-changed\nb2\n").unwrap();

        let (project, cfs) = project_and_curation(&dir);
        assert_eq!(cfs.len(), 2);
        assert_eq!(cfs[1].path, "b.txt", "b.txt should be second in iteration order");

        // Only b.txt (the later file) goes stale — a.txt stays exactly as
        // reviewed.
        fs::write(dir.join("b.txt"), "b1-changed\nb2-changed-too\n").unwrap();

        let err = commit(&dir, &project, &cfs, "should be refused").unwrap_err();
        assert!(err.contains("b.txt"), "{err}");

        let log = Command::new("git").args(["log", "--oneline"]).current_dir(&dir).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).lines().count(), 1, "no new commit should have landed");

        let _ = fs::remove_dir_all(&dir);
    }
}
