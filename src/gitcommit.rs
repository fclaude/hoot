//! Real `git commit` from the Curation screen.
//!
//! Everything is staged the same way, whether one hunk of a file is
//! selected or all of them: hoot replays the *reviewed patch* — git's own
//! header lines plus the selected hunks' own text, verbatim — through
//! `git apply --cached`.
//!
//! That uniformity is the point. An earlier version took a shortcut for
//! whole-file selections and ran `git add <path>` instead, which stages
//! whatever the file contains *at that moment*. Between hoot re-reading
//! the diff and git reading the file, an editor or a running agent turn
//! could change it, and the commit would quietly contain content nobody
//! had looked at. Applying the reviewed patch removes that window
//! entirely: the bytes committed are the bytes displayed, and git itself
//! verifies the patch's preimage still matches the index before it applies
//! anything.
//!
//! Metadata travels with the content. A rename is staged from its own
//! `rename from`/`rename to` header, so the old path's deletion lands in
//! the same commit rather than being left behind for `git status` to
//! report afterwards; a mode change is staged from `old mode`/`new mode`.
//! Both are shown in Curate before the commit — nothing rides along
//! invisibly. A change that lives *entirely* in its header (a pure rename,
//! a mode-only flip, a new empty file) is a single selectable unit with no
//! hunks, staged from the header alone.
//!
//! Before any of that staging happens, four guards run in order. The
//! first two are pure checks — an empty commit message, or nothing
//! selected at all, bails out before a single git command runs, so a
//! no-op commit attempt is actually a no-op rather than a mutation that
//! happens to also report an error. Then:
//!
//! 3. The index already has staged content → refuse outright, without
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
//! 4. Each selected file's change is re-fetched from disk and compared
//!    against what `project` (Curate's last synced view) has, immediately
//!    before *that file* is staged — not trusted from whenever the caller
//!    last polled, and not checked once upfront for the whole batch
//!    either. `project` can be a poll tick or more stale: a background
//!    agent turn or an external edit can change a selected file between
//!    when the user looked at it in Curate and when they press `c` — or,
//!    for a multi-file commit, while an *earlier* file in the batch is
//!    still being staged. The comparison covers metadata as well as hunks,
//!    so a `chmod +x` or a `git mv` landing in that window is caught too;
//!    a hunks-only check used to wave those straight through.
//!
//! Then, after everything is staged and before `git commit` runs, the
//! index is read back and compared against what was selected. Nothing
//! *should* be able to fail that check — git verified each patch as it
//! applied it — which is exactly why it's worth checking: it is the last
//! chance to notice that the commit about to be made isn't the one that
//! was reviewed, and it costs one `git diff --cached`.
//!
//! Any failure along the way — a rejected patch, a stale file, a failed
//! verification, a `git commit` that a pre-commit hook or a missing
//! signing key rejects — unstages everything this call staged before
//! returning. Otherwise a retry would hit guard 3 above with a message
//! ("staged outside hoot") that's simply false for content hoot itself
//! just staged, and the user would be stuck manually unstaging before they
//! could even try again.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::data::{ChangeKind, CurationFile, DiffLine, DiffLineKind, FileEntry, Project};

/// A file this call has staged: which index paths to roll back if
/// something later goes wrong, and what the index is expected to hold for
/// it if everything went right.
struct Staged {
    paths: Vec<OsString>,
    expected: ChangeSummary,
}

/// A file's change reduced to what has to be true of the index for the
/// commit to be the reviewed one: the paths and metadata involved, plus
/// every added/removed line in order. Context lines and hunk boundaries
/// are deliberately left out — git is free to group the same change into
/// different hunks than the working-tree diff did, and that difference is
/// not a discrepancy worth refusing over.
#[derive(PartialEq, Debug)]
struct ChangeSummary {
    path: String,
    old_path: Option<String>,
    change: ChangeKind,
    old_mode: Option<String>,
    new_mode: Option<String>,
    lines: Vec<(DiffLineKind, String, bool)>,
}

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

    // What this call has staged so far, so a failure partway through a
    // multi-file commit can unstage exactly what *it* just added rather
    // than leaving the index in a state where a retry hits the "already
    // staged outside hoot" guard above — which would be actively wrong: the
    // reason it's staged is this function, not something outside hoot.
    let mut staged: Vec<Staged> = Vec::new();
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
        // get staged against a change that had gone stale while the files
        // ahead of it were being staged. This costs a full re-diff per
        // selected file, which is real overhead on a repo with many
        // changed files — a deliberate tradeoff: correctness on the
        // operation this whole screen exists for over commit-time
        // latency, on an action a user triggers occasionally, not in a
        // hot loop.
        // A git failure here is its own error, not a stale-file verdict:
        // rolling back and saying "it changed since you reviewed it" for a
        // repo git simply couldn't read sends the user to re-check a file
        // that is very likely fine.
        let current = match crate::gitreview::diff_files(root) {
            Ok(files) => files,
            Err(e) => {
                rollback(root, &staged);
                return Err(format!("{}: couldn't re-check it before staging \u{2014} {e}", cf.path));
            }
        };
        let fresh = current.iter().find(|f| f.path == cf.path).is_some_and(|now| now.same_change_as(file));
        if !fresh {
            rollback(root, &staged);
            return Err(format!("{} changed since it was last reviewed here \u{2014} re-open Curate and check it again", cf.path));
        }

        if let Err(e) = stage_file(root, file, &cf.hunk_selected) {
            rollback(root, &staged);
            return Err(e);
        }
        staged.push(Staged { paths: index_paths(file), expected: summarize(file, &cf.hunk_selected) });
    }

    if staged.is_empty() {
        return Err("nothing selected to commit".to_string());
    }

    if let Err(e) = verify_index(root, &staged) {
        rollback(root, &staged);
        return Err(e);
    }

    match run_commit(root, message) {
        Ok(summary) => Ok(summary),
        Err(e) => {
            // A commit can fail *after* everything is staged — a rejecting
            // pre-commit hook, a missing signing key, no configured
            // user.email. Leaving the index full in that case turned the
            // real error into a second, misleading one on the next attempt
            // ("already staged outside hoot"), so the index goes back to
            // how this call found it and the user sees only the error that
            // actually happened.
            rollback(root, &staged);
            Err(e)
        }
    }
}

/// Replays `file`'s reviewed patch into the index: git's own header lines
/// followed by the selected hunks, exactly as they were parsed.
fn stage_file(root: &Path, file: &FileEntry, selected: &[bool]) -> Result<(), String> {
    if let Some(reason) = file.unsupported {
        return Err(format!("{}: this is {reason} \u{2014} hoot can't stage it; use `git add` outside hoot", file.path));
    }
    let patch = build_patch(file, selected)?;
    // `--whitespace=nowarn` is not cosmetic: a repo (or user) with
    // `apply.whitespace=fix` configured would otherwise have git silently
    // rewrite trailing whitespace as it applied the patch, staging content
    // that differs from what was reviewed. The whole point here is that it
    // doesn't.
    let out = run_git_with_stdin(root, &["apply", "--cached", "--whitespace=nowarn", "-"], patch.as_bytes())?;
    if !out.status.success() {
        return Err(format!("git apply --cached {}: {}", file.path, String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// The patch text for `file` limited to its selected hunks. Each hunk
/// carries its own `@@ ... @@` header anchored to the base file, so
/// dropping unselected hunks entirely — rather than editing their contents
/// — keeps every remaining hunk's line numbers valid.
fn build_patch(file: &FileEntry, selected: &[bool]) -> Result<String, String> {
    if file.meta.header.is_empty() {
        return Err(format!("{}: no diff header to stage from \u{2014} re-open Curate and try again", file.path));
    }
    let mut patch = String::new();
    for line in &file.meta.header {
        patch.push_str(line);
        patch.push('\n');
    }
    if file.is_metadata_only() {
        return Ok(patch);
    }

    let partial = selected.iter().any(|s| !*s);
    if partial {
        // A creation or a deletion isn't a set of independent edits — its
        // patch says "this file did not exist before" or "it does not
        // exist after", which is only true of the file as a whole. git
        // produces exactly one hunk for either, so this is unreachable in
        // practice; it exists so that if that ever stops being true, the
        // answer is a clear refusal rather than a patch git rejects with
        // something cryptic.
        let kind = match file.meta.change {
            ChangeKind::Added => Some("a new file"),
            ChangeKind::Deleted => Some("a deletion"),
            _ => None,
        };
        if let Some(kind) = kind {
            return Err(format!("{}: {kind} has to be committed whole \u{2014} select all of it, or none of it", file.path));
        }
    }

    for (hunk, &sel) in file.hunks.iter().zip(selected) {
        if !sel {
            continue;
        }
        for line in &hunk.lines {
            patch.push_str(&line.text);
            patch.push('\n');
            // Re-emit the marker `parse_unified_diff` pulled off this line
            // as metadata — `git apply` rejects a patch that's missing it
            // whenever the line it belongs to falls inside the selection.
            if line.no_newline {
                patch.push_str("\\ No newline at end of file\n");
            }
        }
    }
    Ok(patch)
}

/// Every index path staging `file` touches — two of them for a rename,
/// since the old path's removal is as much a part of the change as the new
/// path's content.
fn index_paths(file: &FileEntry) -> Vec<OsString> {
    let mut paths = vec![file.meta.os_path()];
    if matches!(file.meta.change, ChangeKind::Renamed) {
        paths.extend(file.meta.os_old_path());
    }
    paths
}

fn summarize(file: &FileEntry, selected: &[bool]) -> ChangeSummary {
    let lines = file
        .hunks
        .iter()
        .zip(selected)
        .filter(|(_, sel)| **sel)
        .flat_map(|(hunk, _)| hunk.lines.iter())
        .filter(|l| matches!(l.kind, DiffLineKind::Added | DiffLineKind::Removed))
        .map(|l: &DiffLine| (l.kind, l.text.clone(), l.no_newline))
        .collect();
    ChangeSummary {
        path: file.path.clone(),
        old_path: file.meta.old_path.clone(),
        change: file.meta.change,
        old_mode: file.meta.old_mode.clone(),
        new_mode: file.meta.new_mode.clone(),
        lines,
    }
}

/// Reads the index back and confirms it holds exactly what was selected —
/// no more files, no fewer, and the same change for each.
fn verify_index(root: &Path, staged: &[Staged]) -> Result<(), String> {
    // Unreadable index means unverified, which means not committed. This
    // is the check the whole screen's guarantee rests on; degrading it to
    // "found nothing staged" on a git failure would turn it into a check
    // that passes hardest exactly when git is least trustworthy.
    let actual = crate::gitreview::staged_files(root)
        .map_err(|e| format!("couldn't read the index back to verify it \u{2014} {e}; nothing has been committed"))?;
    let actual: Vec<ChangeSummary> = actual.iter().map(|f| summarize(f, &vec![true; f.hunks.len()])).collect();

    for entry in staged {
        let expected = &entry.expected;
        let Some(got) = actual.iter().find(|a| a.path == expected.path) else {
            return Err(format!("{}: nothing was staged for it \u{2014} nothing has been committed", expected.path));
        };
        if got != expected {
            return Err(format!(
                "{}: what got staged doesn't match what you selected \u{2014} nothing has been committed; re-open Curate and check it again",
                expected.path
            ));
        }
    }
    if let Some(extra) = actual.iter().find(|a| !staged.iter().any(|s| s.expected.path == a.path)) {
        return Err(format!("{} was staged but wasn't part of the selection \u{2014} nothing has been committed", extra.path));
    }
    Ok(())
}

fn rollback(root: &Path, staged: &[Staged]) {
    let paths: Vec<OsString> = staged.iter().flat_map(|s| s.paths.iter().cloned()).collect();
    unstage(root, &paths);
}

/// Best-effort: unstages exactly `paths` (never a blanket reset) so a
/// failure inside `commit` leaves the index as clean as it found it. The
/// index-already-staged guard above already refused to run at all if the
/// index held anything before this call started, so at the point this is
/// called the index can only contain what this call itself staged — a
/// blanket reset would be reaching past that. Errors are swallowed — this
/// only ever runs while already unwinding a real error, and there's nothing
/// more useful to do with a second one than leave the affected paths staged
/// for the user to sort out by hand.
fn unstage(root: &Path, paths: &[OsString]) {
    if paths.is_empty() {
        return;
    }
    // `git reset -- <paths>` restores each index entry from HEAD. In a
    // repository with no commits yet there is no HEAD to restore *from*,
    // and `git reset` fails outright — so the entries have to be dropped
    // from the index instead, which is the same end state for a path that
    // only ever existed there because this call put it there.
    let mut args: Vec<&OsStr> = if has_head(root) {
        vec![OsStr::new("reset"), OsStr::new("-q"), OsStr::new("--")]
    } else {
        vec![
            OsStr::new("rm"),
            OsStr::new("-q"),
            OsStr::new("--cached"),
            OsStr::new("--force"),
            OsStr::new("--ignore-unmatch"),
            OsStr::new("--"),
        ]
    };
    args.extend(paths.iter().map(|p| p.as_os_str()));
    let _ = Command::new("git").args(&args).current_dir(root).output();
}

fn run_git(root: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git").args(args).current_dir(root).output().map_err(|e| e.to_string())
}

fn has_head(root: &Path) -> bool {
    run_git(root, &["rev-parse", "--verify", "-q", "HEAD"]).map(|o| o.status.success()).unwrap_or(false)
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
        let remaining = crate::gitreview::diff_files(&dir).unwrap();
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

    #[test]
    fn staleness_failure_partway_through_a_multi_file_commit_unstages_what_it_added() {
        // Regression for a real bug: a.txt is staged first in the loop,
        // then b.txt fails its per-file freshness check (same setup as
        // `a_multi_file_commit_still_catches_staleness_on_a_later_file`).
        // Before the fix, a.txt stayed staged after the error return — a
        // retry would then hit guard 2 ("already staged outside hoot"),
        // which is simply false: hoot staged it, moments ago, in this same
        // call. Confirms both halves: the index is clean immediately after
        // the failed call, and a retry actually succeeds instead of
        // tripping that guard.
        let dir = scratch_repo("staleness-rollback");
        fs::write(dir.join("a.txt"), "a1\na2\n").unwrap();
        fs::write(dir.join("b.txt"), "b1\nb2\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::write(dir.join("a.txt"), "a1-changed\na2\n").unwrap();
        fs::write(dir.join("b.txt"), "b1-changed\nb2\n").unwrap();

        let (project, cfs) = project_and_curation(&dir);
        assert_eq!(cfs.len(), 2);
        assert_eq!(cfs[1].path, "b.txt", "b.txt should be second in iteration order, after a.txt is staged");

        fs::write(dir.join("b.txt"), "b1-changed\nb2-changed-too\n").unwrap();

        let err = commit(&dir, &project, &cfs, "should be refused").unwrap_err();
        assert!(err.contains("b.txt"), "{err}");

        assert!(!index_has_staged_changes(&dir).unwrap(), "a.txt should have been unstaged along with the failed attempt");
        let status = Command::new("git").args(["status", "--short", "a.txt"]).current_dir(&dir).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&status.stdout), " M a.txt\n", "a.txt should show as modified-but-unstaged");

        // Retrying with just a.txt must not hit the "already staged
        // outside hoot" guard — that would mean the rollback above didn't
        // actually happen.
        let (project2, cfs2) = project_and_curation(&dir);
        let a_only: Vec<CurationFile> = cfs2.into_iter().filter(|cf| cf.path == "a.txt").collect();
        let summary = commit(&dir, &project2, &a_only, "Fix a.txt").unwrap();
        assert!(summary.contains("Fix a.txt"), "{summary}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unstage_reverses_exactly_the_given_paths() {
        // Direct test of the rollback primitive itself: stages two real
        // files, unstages only one by path, and confirms the other is left
        // exactly as staged — `unstage` must never touch more than what
        // it's told to.
        let dir = scratch_repo("unstage-helper");
        fs::write(dir.join("a.txt"), "a1\n").unwrap();
        fs::write(dir.join("b.txt"), "b1\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::write(dir.join("a.txt"), "a1-changed\n").unwrap();
        fs::write(dir.join("b.txt"), "b1-changed\n").unwrap();

        let (project, cfs) = project_and_curation(&dir);
        for cf in &cfs {
            let file = project.files.iter().find(|f| f.path == cf.path).unwrap();
            stage_file(&dir, file, &cf.hunk_selected).unwrap();
        }
        assert!(index_has_staged_changes(&dir).unwrap());

        unstage(&dir, &[OsString::from("a.txt")]);

        let status = Command::new("git").args(["status", "--short"]).current_dir(&dir).output().unwrap();
        let status = String::from_utf8_lossy(&status.stdout);
        assert!(status.contains(" M a.txt"), "a.txt should be unstaged: {status}");
        assert!(status.contains("M  b.txt"), "b.txt should remain staged: {status}");

        let _ = fs::remove_dir_all(&dir);
    }

    fn head_mode(dir: &Path, path: &str) -> String {
        let out = Command::new("git").args(["ls-tree", "HEAD", "--", path]).current_dir(dir).output().unwrap();
        String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap_or_default().to_string()
    }

    fn head_bytes(dir: &Path, path: &str) -> Vec<u8> {
        Command::new("git").args(["show", &format!("HEAD:{path}")]).current_dir(dir).output().unwrap().stdout
    }

    #[test]
    fn a_mode_change_is_staged_from_its_header_alongside_the_content() {
        // Regression, and the reason `FileMeta` exists at all: the parser
        // used to keep only the destination path and the text hunks, so an
        // executable bit flipping alongside an edit was invisible in Curate
        // *and* rode into the commit anyway (whole-file selections went
        // through `git add`, which picks the mode up from disk). Now the
        // mode is part of the reviewed patch — visible in `describe()`, and
        // staged because it was reviewed, not as a side effect.
        let dir = scratch_repo("mode-and-edit");
        fs::write(dir.join("f.sh"), "echo one\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::write(dir.join("f.sh"), "echo one\necho two\n").unwrap();
        fs::set_permissions(dir.join("f.sh"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

        let (project, cfs) = project_and_curation(&dir);
        let file = &project.files[0];
        assert!(file.meta.mode_changed(), "the mode change must be captured, not dropped");
        assert!(
            file.meta.describe().iter().any(|d| d.contains("100755") && d.contains("executable")),
            "and it must be sayable out loud: {:?}",
            file.meta.describe()
        );

        commit(&dir, &project, &cfs, "Make f.sh executable and extend it").unwrap();
        assert_eq!(head_mode(&dir, "f.sh"), "100755");
        assert_eq!(head_bytes(&dir, "f.sh"), b"echo one\necho two\n");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_mode_only_change_is_one_selectable_unit_and_commits_from_its_header() {
        // `chmod +x` with no edit: no hunks at all, but a real change with
        // something real to commit. It used to be written off as "not
        // curatable here" and could only be committed outside hoot.
        let dir = scratch_repo("mode-only");
        fs::write(dir.join("f.sh"), "echo hi\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::set_permissions(dir.join("f.sh"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

        let (project, cfs) = project_and_curation(&dir);
        assert!(project.files[0].hunks.is_empty());
        assert!(project.files[0].is_metadata_only());
        assert_eq!(cfs[0].total(), 1, "the mode change itself is the selectable unit");
        assert_eq!(cfs[0].selected(), 1);

        commit(&dir, &project, &cfs, "Make f.sh executable").unwrap();
        assert_eq!(head_mode(&dir, "f.sh"), "100755");

        let status = Command::new("git").args(["status", "--short"]).current_dir(&dir).output().unwrap();
        assert!(status.stdout.is_empty(), "tree should be clean: {:?}", String::from_utf8_lossy(&status.stdout));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rename_is_staged_with_the_old_paths_deletion_not_just_the_new_path() {
        // The P1 this whole metadata layer was built for. Staging a
        // rename+edit by path (`git add new-path`) leaves the *deletion* of
        // the old path unstaged, so the commit contains both copies of the
        // file. Replaying git's own `rename from`/`rename to` header stages
        // the pair as the single change it is.
        //
        // Driven through `stage_file` rather than `commit`: git only ever
        // reports a rename once it's in the index (rename detection needs
        // both sides *in* the diff, and an untracked file isn't), and
        // `commit`'s second guard refuses outright when the index already
        // holds anything. So the index is reset back to HEAD here to
        // reproduce what `stage_file` is actually handed — a clean index
        // and a reviewed rename.
        let dir = scratch_repo("rename-staging");
        let original: String = (1..=30).map(|n| format!("line{n}\n")).collect();
        fs::write(dir.join("old.txt"), &original).unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();

        Command::new("git").args(["mv", "old.txt", "new.txt"]).current_dir(&dir).status().unwrap();
        fs::write(dir.join("new.txt"), original.replacen("line1\n", "line1-CHANGED\n", 1)).unwrap();

        let (project, cfs) = project_and_curation(&dir);
        let file = &project.files[0];
        assert_eq!(file.path, "new.txt");
        assert_eq!(file.meta.change, ChangeKind::Renamed);
        assert_eq!(file.meta.old_path.as_deref(), Some("old.txt"));
        assert!(!file.hunks.is_empty(), "a rename+edit still has real hunks");

        Command::new("git").args(["reset", "-q"]).current_dir(&dir).status().unwrap();
        stage_file(&dir, file, &cfs[0].hunk_selected).unwrap();

        let status = Command::new("git").args(["status", "--short"]).current_dir(&dir).output().unwrap();
        let status = String::from_utf8_lossy(&status.stdout);
        assert!(status.contains("R  old.txt -> new.txt"), "both halves of the rename must be staged: {status}");
        assert!(!status.contains("D  old.txt") || status.contains("->"), "the old path must not be left behind: {status}");

        // And the index reads back as exactly the reviewed change, which is
        // what `verify_index` checks before any commit is made.
        verify_index(&dir, &[Staged { paths: index_paths(file), expected: summarize(file, &cfs[0].hunk_selected) }]).unwrap();

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_added_line_whose_content_starts_with_plus_signs_survives() {
        // Regression: the hunk parser excluded any line starting with `+++`
        // or `---`, to avoid mistaking the header's `+++ b/path` for
        // content. Inside a hunk that exclusion has no header to protect
        // against and only does harm: adding a line that itself begins with
        // `++` produces the diff line `+++...`, which was dropped outright.
        // The hunk's body then disagreed with the line counts in its own
        // `@@` header, and staging it failed as a corrupt patch.
        let dir = scratch_repo("plus-prefixed-content");
        fs::write(dir.join("notes.md"), "intro\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::write(dir.join("notes.md"), "intro\n++ a line starting with plus signs\n--- and one with dashes\n").unwrap();

        let (project, cfs) = project_and_curation(&dir);
        commit(&dir, &project, &cfs, "Add odd-looking lines").unwrap();
        assert_eq!(head_bytes(&dir, "notes.md"), b"intro\n++ a line starting with plus signs\n--- and one with dashes\n");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_deleted_file_is_committed_as_a_deletion() {
        // Deletions used to be staged by `git add <path>`, which stages a
        // removal as a side effect. They now go through the same patch path
        // as everything else — git's `deleted file mode` header plus the
        // hunk that removes every line.
        let dir = scratch_repo("deletion");
        fs::write(dir.join("gone.txt"), "one\ntwo\n").unwrap();
        fs::write(dir.join("stays.txt"), "kept\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::remove_file(dir.join("gone.txt")).unwrap();

        let (project, cfs) = project_and_curation(&dir);
        assert_eq!(project.files.len(), 1);
        assert_eq!(project.files[0].meta.change, ChangeKind::Deleted);

        commit(&dir, &project, &cfs, "Remove gone.txt").unwrap();

        let tree = Command::new("git").args(["ls-tree", "--name-only", "HEAD"]).current_dir(&dir).output().unwrap();
        let tree = String::from_utf8_lossy(&tree.stdout);
        assert!(!tree.contains("gone.txt"), "the deletion must land: {tree}");
        assert!(tree.contains("stays.txt"), "and nothing else: {tree}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_untracked_symlink_is_committed_as_a_symlink() {
        // Its blob content is the target path with no trailing newline —
        // which is why the synthetic diff for one carries a no-newline
        // marker. Without it, git stores a link target with a stray newline
        // in it, and the link resolves to nothing.
        let dir = scratch_repo("symlink-commit");
        Command::new("git").args(["commit", "-q", "--allow-empty", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::write(dir.join("real.txt"), "hello\n").unwrap();
        std::os::unix::fs::symlink("real.txt", dir.join("link.txt")).unwrap();

        let (project, cfs) = project_and_curation(&dir);
        commit(&dir, &project, &cfs, "Add real.txt and a link to it").unwrap();

        let entry = Command::new("git").args(["ls-tree", "HEAD", "--", "link.txt"]).current_dir(&dir).output().unwrap();
        let entry = String::from_utf8_lossy(&entry.stdout);
        assert!(entry.starts_with("120000"), "it must be committed as a symlink, not a text file: {entry}");
        assert_eq!(head_bytes(&dir, "link.txt"), b"real.txt", "and point exactly where it pointed");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_untracked_file_is_committed_byte_for_byte() {
        // Whole-file selections used to be staged with `git add`, which
        // reads the file itself — so the synthetic diff hoot built for an
        // untracked file never had to be exactly right. Now that same patch
        // *is* what gets committed, and two of its details matter: CRLF
        // endings (`str::lines()` silently ate the `\r`) and a missing final
        // newline (which was silently added).
        let dir = scratch_repo("untracked-bytes");
        Command::new("git").args(["commit", "-q", "--allow-empty", "-m", "init"]).current_dir(&dir).status().unwrap();
        let content = b"first\r\nsecond\r\nno trailing newline";
        fs::write(dir.join("crlf.txt"), content).unwrap();

        let (project, cfs) = project_and_curation(&dir);
        commit(&dir, &project, &cfs, "Add crlf.txt").unwrap();

        assert_eq!(head_bytes(&dir, "crlf.txt"), content, "committed content must match the file exactly");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_untracked_executable_file_keeps_its_mode() {
        let dir = scratch_repo("untracked-exec");
        Command::new("git").args(["commit", "-q", "--allow-empty", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::write(dir.join("run.sh"), "#!/bin/sh\necho hi\n").unwrap();
        fs::set_permissions(dir.join("run.sh"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

        let (project, cfs) = project_and_curation(&dir);
        commit(&dir, &project, &cfs, "Add run.sh").unwrap();
        assert_eq!(head_mode(&dir, "run.sh"), "100755");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_new_file_is_committable_even_though_it_has_no_hunks() {
        let dir = scratch_repo("empty-new-file");
        Command::new("git").args(["commit", "-q", "--allow-empty", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::write(dir.join("placeholder"), "").unwrap();

        let (project, cfs) = project_and_curation(&dir);
        assert!(project.files[0].is_metadata_only());
        assert_eq!(cfs[0].total(), 1);

        commit(&dir, &project, &cfs, "Add an empty placeholder").unwrap();
        assert!(head_bytes(&dir, "placeholder").is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_partial_commit_works_on_a_filename_that_needs_quoting() {
        // Regression: the reconstructed patch used to interpolate the
        // decoded path straight into unquoted `--- a/<path>` headers. A name
        // containing a quote (or a newline, or a backslash) produced a patch
        // git either rejected or misread — while the same name in git's own
        // output was C-quoted and escaped. Preserving git's header verbatim
        // means never having to re-derive that quoting.
        let dir = scratch_repo("quoted-name");
        let name = "she\"said\u{e9}.txt";
        let original: String = (1..=20).map(|n| format!("line{n}\n")).collect();
        fs::write(dir.join(name), &original).unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();

        let mut lines: Vec<String> = (1..=20).map(|n| format!("line{n}")).collect();
        lines[0] = "line1-CHANGED".to_string();
        lines[19] = "line20-CHANGED".to_string();
        fs::write(dir.join(name), lines.join("\n") + "\n").unwrap();

        let (project, mut cfs) = project_and_curation(&dir);
        assert_eq!(project.files[0].path, name, "the name must survive parsing intact");
        assert_eq!(cfs[0].total(), 2);
        cfs[0].hunk_selected[1] = false;

        commit(&dir, &project, &cfs, "Fix line1 only").unwrap();
        let committed = String::from_utf8_lossy(&head_bytes(&dir, name)).into_owned();
        assert!(committed.starts_with("line1-CHANGED\n"), "{committed}");
        assert!(committed.contains("line20\n"), "the deselected hunk must stay out: {committed}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_commit_rejected_by_a_hook_unstages_everything_it_staged() {
        // Regression: staging succeeds, then `git commit` fails — a
        // pre-commit hook, a missing signing key, no configured identity.
        // The index used to be left full, so the *next* attempt reported
        // "already staged outside hoot", which was both false and a dead
        // end for anyone who didn't know to run `git reset` themselves.
        let dir = scratch_repo("failing-hook");
        fs::write(dir.join("f.txt"), "line1\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::write(dir.join("f.txt"), "line1-changed\n").unwrap();

        let hook = dir.join(".git/hooks/pre-commit");
        fs::write(&hook, "#!/bin/sh\necho 'nope' >&2\nexit 1\n").unwrap();
        fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

        let (project, cfs) = project_and_curation(&dir);
        let err = commit(&dir, &project, &cfs, "should be rejected").unwrap_err();
        assert!(err.contains("git commit"), "{err}");

        assert!(!index_has_staged_changes(&dir).unwrap(), "the index must be left as the call found it");
        let status = Command::new("git").args(["status", "--short"]).current_dir(&dir).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&status.stdout), " M f.txt\n");

        // And the retry (once the hook is gone) is a normal commit, not a
        // "staged outside hoot" refusal.
        fs::remove_file(&hook).unwrap();
        let (project, cfs) = project_and_curation(&dir);
        commit(&dir, &project, &cfs, "Fix f.txt").unwrap();

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_commit_in_a_repo_with_no_commits_yet_rolls_back_cleanly() {
        // The rollback path can't use `git reset` here: with no HEAD there
        // is nothing to restore the index entry *from*, and `git reset`
        // fails outright — which used to mean a failed first commit left the
        // index populated with no way back through hoot.
        let dir = scratch_repo("no-head-rollback");
        fs::write(dir.join("f.txt"), "hello\n").unwrap();
        let hook = dir.join(".git/hooks/pre-commit");
        fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

        let (project, cfs) = project_and_curation(&dir);
        let err = commit(&dir, &project, &cfs, "first commit").unwrap_err();
        assert!(err.contains("git commit"), "{err}");
        assert!(!index_has_staged_changes(&dir).unwrap(), "the index must be empty again, not left holding f.txt");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_index_refuses_when_the_index_holds_something_other_than_the_selection() {
        // The last line of defense, tested directly: `verify_index` is
        // handed a selection that says one thing and an index that holds
        // another, and must say no. In production nothing should be able to
        // get here — git validates every patch as it applies it — which is
        // exactly why the check is cheap insurance rather than dead weight.
        let dir = scratch_repo("verify-mismatch");
        fs::write(dir.join("f.txt"), "line1\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::write(dir.join("f.txt"), "line1-changed\n").unwrap();

        let (project, cfs) = project_and_curation(&dir);
        let file = &project.files[0];
        stage_file(&dir, file, &cfs[0].hunk_selected).unwrap();

        // Truthful expectation: passes.
        let honest = Staged { paths: index_paths(file), expected: summarize(file, &cfs[0].hunk_selected) };
        verify_index(&dir, &[honest]).unwrap();

        // Expectation that claims a different line was selected: refused.
        let mut lying = summarize(file, &cfs[0].hunk_selected);
        lying.lines.push((DiffLineKind::Added, "+line2-that-was-never-staged".to_string(), false));
        let err = verify_index(&dir, &[Staged { paths: index_paths(file), expected: lying }]).unwrap_err();
        assert!(err.contains("doesn't match"), "{err}");

        // And an extra file in the index that nothing selected is caught too.
        fs::write(dir.join("extra.txt"), "surprise\n").unwrap();
        Command::new("git").args(["add", "extra.txt"]).current_dir(&dir).status().unwrap();
        let err = verify_index(&dir, &[Staged { paths: index_paths(file), expected: summarize(file, &cfs[0].hunk_selected) }]).unwrap_err();
        assert!(err.contains("extra.txt"), "{err}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_partial_selection_still_verifies_when_the_file_has_repeated_lines() {
        // `verify_index` compares the added/removed lines of the staged
        // result against those of the selected hunks, in order. A file full
        // of identical lines is where git has the most freedom to describe
        // the same result with a differently-arranged edit script, so it's
        // the case most likely to make that comparison cry wolf.
        let dir = scratch_repo("repeated-lines");
        let original: String = std::iter::repeat_n("same\n", 30).collect();
        fs::write(dir.join("f.txt"), &original).unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();

        let mut lines: Vec<String> = std::iter::repeat_n("same".to_string(), 30).collect();
        lines[0] = "first-changed".to_string();
        lines[29] = "last-changed".to_string();
        fs::write(dir.join("f.txt"), lines.join("\n") + "\n").unwrap();

        let (project, mut cfs) = project_and_curation(&dir);
        assert_eq!(cfs[0].total(), 2);
        cfs[0].hunk_selected[1] = false;

        commit(&dir, &project, &cfs, "Change the first line only").unwrap();
        let committed = String::from_utf8_lossy(&head_bytes(&dir, "f.txt")).into_owned();
        assert!(committed.starts_with("first-changed\n"), "{committed}");
        assert!(committed.ends_with("same\n"), "the second hunk must stay out: {committed}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_hunk_commit_on_a_file_with_no_trailing_newline_applies_cleanly() {
        // Regression: `parse_unified_diff` used to drop the
        // "\ No newline at end of file" marker entirely instead of
        // recording it on the line it belongs to. Reconstructing a patch
        // from only the selected hunks' `DiffLine`s then produced text
        // `git apply --cached` rejected outright for any file lacking a
        // trailing newline — exactly the kind of file this repo itself
        // has plenty of (README.md, source files edited by hand, ...).
        let dir = scratch_repo("no-trailing-newline");
        let original: String = (1..=20).map(|n| format!("line{n}")).collect::<Vec<_>>().join("\n");
        fs::write(dir.join("f.txt"), &original).unwrap(); // no trailing newline
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();

        // Two far-apart changes -> two hunks; the second one touches the
        // file's last line, which is where the no-newline marker attaches.
        let mut lines: Vec<String> = (1..=20).map(|n| format!("line{n}")).collect();
        lines[0] = "line1-CHANGED".to_string();
        lines[19] = "line20-CHANGED".to_string();
        fs::write(dir.join("f.txt"), lines.join("\n")).unwrap(); // still no trailing newline

        let (project, mut cfs) = project_and_curation(&dir);
        assert_eq!(cfs[0].total(), 2, "expected two separate hunks");
        cfs[0].hunk_selected[0] = false;
        cfs[0].hunk_selected[1] = true; // select only the hunk touching the no-newline line

        let summary = commit(&dir, &project, &cfs, "Fix line20 only");
        assert!(summary.is_ok(), "git apply should accept the reconstructed patch: {summary:?}");

        let log = Command::new("git").args(["show", "HEAD:f.txt"]).current_dir(&dir).output().unwrap();
        let committed = String::from_utf8_lossy(&log.stdout);
        assert!(committed.ends_with("line20-CHANGED"), "{committed}");
        assert!(!committed.ends_with('\n'), "committed content should still have no trailing newline: {committed:?}");

        let _ = fs::remove_dir_all(&dir);
    }
}
