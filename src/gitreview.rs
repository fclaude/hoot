//! Real change review, backed by `git diff` in the target directory.
//!
//! Falls back to the mockup's static demo data only when the target isn't a
//! git repository at all; inside a real repo with a clean tree this reports
//! an honest empty changeset rather than silently substituting fake content.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::data::{self, CurationFile, DiffLine, DiffLineKind, FileEntry, Hunk, Project};

pub struct ReviewData {
    pub project: Project,
    pub curation_files: Vec<CurationFile>,
    pub is_real: bool,
}

pub fn load(root: &Path) -> ReviewData {
    if !is_git_repo(root) {
        return ReviewData { project: data::mock_project(), curation_files: data::mock_curation_files(), is_real: false };
    }

    let files = diff_files(root);

    let curation_files =
        files.iter().map(|f| CurationFile { path: f.path.clone(), hunk_selected: vec![true; f.hunks.len()], status: None }).collect();

    let name = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| root.display().to_string());
    let project = Project { name: name.clone(), root: name, files };

    ReviewData { project, curation_files, is_real: true }
}

/// The parsed working-tree diff for `root`: empty if it's not a git repo or
/// has no changes.
pub fn diff_files(root: &Path) -> Vec<FileEntry> {
    let diff_text = run_git_diff(root);
    parse_unified_diff(&diff_text)
}

fn git(root: &Path, args: &[&str]) -> Option<Output> {
    Command::new("git").args(args).current_dir(root).output().ok()
}

pub fn is_git_repo(root: &Path) -> bool {
    git(root, &["rev-parse", "--is-inside-work-tree"]).map(|o| o.status.success()).unwrap_or(false)
}

/// Runs a `git ls-files`-family command and returns the raw relative paths
/// it names, unquoted. By default git quotes any path with a non-ASCII or
/// otherwise "unusual" byte — `café.txt` comes back as the literal text
/// `"caf\303\251.txt"`, quote marks and octal escapes included — which is
/// exactly what breaks joining the result onto `root` to get a real
/// filesystem path back. `-z` (added here, not by callers) makes git emit
/// the actual filesystem bytes NUL-terminated instead, so there's nothing
/// to unescape.
fn ls_files_paths(root: &Path, args: &[&str]) -> Vec<String> {
    let mut args = args.to_vec();
    args.push("-z");
    let Some(listing) = git(root, &args) else { return Vec::new() };
    listing.stdout.split(|&b| b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect()
}

/// Absolute paths under `root` that Git ignores — files and whole
/// directories, matched the same way `git status` would (`.gitignore`,
/// nested `.gitignore`s, global excludes, ...). `--directory` reports an
/// ignored directory as one entry (`build/`) instead of walking in and
/// listing every file inside it — exactly what a caller skipping whole
/// subtrees during a filesystem walk wants, and far cheaper for something
/// like a large `target/` or `node_modules/`. Empty (not an error) outside
/// a git repo, so callers can use this unconditionally.
pub fn ignored_paths(root: &Path) -> HashSet<PathBuf> {
    ls_files_paths(root, &["ls-files", "--others", "--ignored", "--exclude-standard", "--directory"])
        .into_iter()
        .map(|rel| root.join(rel.trim_end_matches('/')))
        .collect()
}

/// Diffs against HEAD (staged + unstaged) when a commit exists; otherwise
/// falls back to a plain working-tree diff (e.g. a repo with zero commits).
/// Appends untracked files too — plain `git diff` never shows those (they
/// aren't in the index at all), which would otherwise make a file the
/// agent just created invisible to Hoot until it's staged. Uses git's
/// default (small) context window — this is the canonical hunk breakdown
/// Curation's per-hunk selection and partial commits rely on, so it needs
/// real, separate hunk boundaries, not one hunk spanning the whole file.
/// See `file_diff_in_context` for the Review screen's whole-file view.
fn run_git_diff(root: &Path) -> String {
    let has_head = git(root, &["rev-parse", "--verify", "-q", "HEAD"]).map(|o| o.status.success()).unwrap_or(false);
    // `--find-renames`: off by default for `git diff` (unlike `git status`)
    // unless `diff.renames` is configured. Without it, a plain `git mv`
    // shows as an unrelated delete-of-the-old-name + add-of-the-new-name
    // pair — each with the *entire* file's content duplicated as
    // removed/added lines — instead of the single, contentless "rename
    // from/to" entry `parse_unified_diff`'s unsupported-change handling
    // expects and Curation has nothing useful to do with either way.
    let mut diff = if has_head {
        git(root, &["diff", "HEAD", "--no-color", "--find-renames"])
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default()
    } else {
        // Plain `git diff` alone only shows index-vs-worktree (unstaged)
        // changes — with no HEAD to compare against, anything already
        // staged (`git add`ed) would otherwise never show up at all, since
        // for a staged-but-unmodified-since file the index and worktree
        // agree. `git diff --cached` fills that gap: git special-cases a
        // HEAD-less repo there and diffs the index against the empty tree,
        // so staged content shows as a normal "new file" diff (confirmed
        // by testing) — same union of staged+unstaged that `git diff HEAD`
        // gives once a commit exists.
        let staged = git(root, &["diff", "--cached", "--no-color", "--find-renames"])
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        let unstaged = git(root, &["diff", "--no-color", "--find-renames"])
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        staged + &unstaged
    };
    diff.push_str(&untracked_files_diff(root));
    diff
}

/// Builds a "new file" unified diff for each untracked, non-ignored file
/// directly (no git subprocess involved) and concatenates the results —
/// text in exactly the same format `parse_unified_diff` already handles,
/// so no separate "new file" code path is needed on the parsing side.
///
/// Used to shell out to `git diff --no-index` once per file instead —
/// correct, but this runs on every `sync_from_disk` poll tick (every
/// second), so a tree with hundreds of untracked files (a generated build
/// output dir not yet gitignored, say) meant hundreds of subprocess spawns
/// a second and a visibly stalling UI. One `ls-files` plus reading each
/// file directly is the same information without the fork-per-file cost.
fn untracked_files_diff(root: &Path) -> String {
    let mut diff = String::new();
    for path in ls_files_paths(root, &["ls-files", "--others", "--exclude-standard"]) {
        diff.push_str(&synthetic_new_file_diff(root, &path));
    }
    diff
}

/// Files larger than this are treated like binaries (a marker line, no
/// content dump) instead of being read in full — protects against a
/// pathologically large untracked file consuming unbounded memory on every
/// poll tick. Generous for real source files.
const MAX_SYNTHETIC_DIFF_SIZE: u64 = 10 * 1024 * 1024; // 10 MiB

/// A `diff --git a/<path> b/<path>` "new file" entry for `rel_path`,
/// exactly the shape `git diff --no-index /dev/null <path>` would produce
/// for a plain-text file. Binary content gets git's own "Binary files ...
/// differ" marker line instead of being diffed line-by-line (matches
/// `parse_unified_diff`'s existing binary-file handling) rather than
/// risking garbage output from treating arbitrary bytes as UTF-8 text.
///
/// Uses `symlink_metadata`, not `metadata`/`fs::read` directly — those
/// follow symlinks, so an untracked symlink pointing outside the repo (a
/// classic "untracked symlink to ~/.ssh/id_rsa" style trick, or just an
/// absolute symlink left by some tool) would otherwise read and expose an
/// arbitrary external file's content in Review — and that content could
/// end up in a prompt sent to a real agent. A symlink here is shown as git
/// itself shows one: its target *path text* as one added line, not
/// whatever the target contains. Anything that isn't a symlink or a
/// regular file (a FIFO, socket, device node, ...) is skipped outright —
/// reading one of those could block indefinitely or return nonsense.
fn synthetic_new_file_diff(root: &Path, rel_path: &str) -> String {
    let full = root.join(rel_path);
    let Ok(meta) = std::fs::symlink_metadata(&full) else {
        return String::new();
    };
    let header = format!("diff --git a/{rel_path} b/{rel_path}\n");

    if meta.is_symlink() {
        let target = std::fs::read_link(&full).map(|p| p.display().to_string()).unwrap_or_default();
        return format!("{header}new file mode 120000\n--- /dev/null\n+++ b/{rel_path}\n@@ -0,0 +1 @@\n+{target}\n");
    }
    if !meta.is_file() {
        return String::new();
    }

    let header = format!("{header}new file mode 100644\n");
    if meta.len() > MAX_SYNTHETIC_DIFF_SIZE {
        return format!("{header}Binary files /dev/null and b/{rel_path} differ\n");
    }

    let Ok(bytes) = std::fs::read(&full) else {
        return String::new();
    };
    if bytes.contains(&0) {
        return format!("{header}Binary files /dev/null and b/{rel_path} differ\n");
    }
    let text = String::from_utf8_lossy(&bytes);
    let line_count = text.lines().count();
    let mut out = format!("{header}--- /dev/null\n+++ b/{rel_path}\n@@ -0,0 +1,{line_count} @@\n");
    for line in text.lines() {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// A context window comfortably larger than any real source file, so a
/// hunk's context lines cover the *entire* file rather than just a few
/// lines around each change — used only for `file_diff_in_context`, never
/// for the canonical per-file hunk breakdown `run_git_diff` returns.
const FULL_CONTEXT: &str = "-U100000";

/// The single file `rel_path`'s diff, loaded with a huge context window so
/// (in practice) one hunk reconstructs the entire current file in order,
/// changes overlaid in place — what the Review screen's content pane
/// shows, as opposed to the small, separately-loaded hunks `run_git_diff`
/// returns for Curation. `rel_path` is relative to `root`. Returns an
/// empty list for a file with no changes (or that doesn't exist).
pub fn file_diff_in_context(root: &Path, rel_path: &str) -> Vec<Hunk> {
    let has_head = git(root, &["rev-parse", "--verify", "-q", "HEAD"]).map(|o| o.status.success()).unwrap_or(false);
    let tracked_diff = if has_head {
        git(root, &["diff", "HEAD", "--no-color", FULL_CONTEXT, "--", rel_path])
    } else {
        git(root, &["diff", "--no-color", FULL_CONTEXT, "--", rel_path])
    };
    let mut diff = tracked_diff.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    if diff.trim().is_empty() {
        // Not a tracked change — might be untracked entirely.
        if let Some(out) = git(root, &["diff", "--no-color", FULL_CONTEXT, "--no-index", "/dev/null", rel_path]) {
            diff = String::from_utf8_lossy(&out.stdout).to_string();
        }
    }
    parse_unified_diff(&diff).into_iter().next().map(|f| f.hunks).unwrap_or_default()
}

fn parse_unified_diff(diff: &str) -> Vec<FileEntry> {
    let mut files: Vec<FileEntry> = Vec::new();
    let mut current: Option<FileEntry> = None;
    let mut current_hunk: Option<Hunk> = None;
    // Set when a header line names a change class with no hunk lines at
    // all (binary, pure rename, mode-only, submodule) — see `flush_file`.
    let mut unsupported_reason: Option<&'static str> = None;

    let flush_hunk = |file: &mut FileEntry, hunk: &mut Option<Hunk>| {
        if let Some(h) = hunk.take() {
            file.hunks.push(h);
        }
    };
    let flush_file =
        |files: &mut Vec<FileEntry>, file: &mut Option<FileEntry>, hunk: &mut Option<Hunk>, reason: &mut Option<&'static str>| {
            if let Some(mut f) = file.take() {
                flush_hunk(&mut f, hunk);
                f.hunk_count = f.hunks.len() as u32;
                // Only worth flagging if it really did leave nothing to
                // curate — a rename *with* content changes still gets its
                // real hunks and has plenty to select, `rename from`/`rename
                // to` notwithstanding.
                if f.hunks.is_empty() {
                    f.unsupported = reason.take();
                }
                files.push(f);
            }
            *reason = None;
        };

    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("diff --git ").map(parse_diff_git_path) {
            flush_file(&mut files, &mut current, &mut current_hunk, &mut unsupported_reason);
            current = Some(FileEntry { path, hunk_count: 0, notes: 0, flagged: false, hunks: Vec::new(), unsupported: None });
        } else if line.starts_with("Binary files ") && line.ends_with(" differ") {
            unsupported_reason = Some("binary file");
        } else if line.starts_with("rename from ") {
            unsupported_reason = Some("renamed, no content change");
        } else if line.starts_with("old mode ") {
            unsupported_reason = Some("file mode changed only");
        } else if line.starts_with("Subproject commit ") {
            unsupported_reason = Some("submodule pointer changed");
        } else if line.starts_with("@@ ") {
            if let Some(f) = current.as_mut() {
                flush_hunk(f, &mut current_hunk);
            }
            current_hunk = Some(Hunk { lines: vec![DiffLine { kind: DiffLineKind::HunkHeader, text: line.to_string() }], note: None });
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
    flush_file(&mut files, &mut current, &mut current_hunk, &mut unsupported_reason);

    files
}

/// "a/path/to/file b/path/to/file" -> "path/to/file". Also handles git's
/// C-style quoted form — `"a/<escaped>" "b/<escaped>"` — which it switches
/// to for a path containing a quote, backslash, or (with the default
/// `core.quotePath=true`) any non-ASCII byte. Confirmed against real git
/// output, not assumed: a plain space in a name isn't quoted at all
/// (`a/weird file.txt b/weird file.txt`, handled by the plain rfind
/// below), but `"` or non-ASCII bytes are, e.g. `"a/\303\251motion.txt"
/// "b/\303\251motion.txt"` for `émotion.txt`. The naive rfind alone
/// would return that literal quoted-and-escaped garbage as the "path".
fn parse_diff_git_path(rest: &str) -> String {
    let rest = rest.trim_end();
    if let Some(after_first_quote) = rest.strip_prefix('"') {
        if let Some(first_end) = find_unescaped_quote(after_first_quote) {
            let remainder = after_first_quote[first_end + 1..].trim_start();
            if let Some(second) = remainder.strip_prefix('"') {
                if let Some(second_end) = find_unescaped_quote(second) {
                    let b_side = unquote_c_style(&second[..second_end]);
                    return b_side.strip_prefix("b/").unwrap_or(&b_side).to_string();
                }
            }
        }
        // Malformed/unexpected quoting shape — fall through rather than
        // silently mangling it further; better a visibly-off path (the
        // leading '"' will look wrong) than a panic.
    }
    match rest.rfind(" b/") {
        Some(idx) => rest[idx + 3..].to_string(),
        None => rest.to_string(),
    }
}

/// Byte index of the first `"` in `s` that isn't itself escaped (not
/// preceded by a backslash that's part of the quoted content).
fn find_unescaped_quote(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some(i),
            b'\\' => i += 2, // skip the escaped byte, whatever it is
            _ => i += 1,
        }
    }
    None
}

/// Un-escapes git's C-style quoting: `\\`, `\"`, `\t`, `\n`, and `\NNN`
/// octal byte escapes (used for non-ASCII bytes — each byte of a multi-byte
/// UTF-8 character gets its own `\NNN`, so these are collected as raw bytes
/// and decoded together, not one at a time).
fn unquote_c_style(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                }
                b'"' => {
                    out.push(b'"');
                    i += 2;
                }
                b't' => {
                    out.push(b'\t');
                    i += 2;
                }
                b'n' => {
                    out.push(b'\n');
                    i += 2;
                }
                b'0'..=b'7' if bytes.len() >= i + 4 && bytes[i + 1..i + 4].iter().all(|b| matches!(b, b'0'..=b'7')) => {
                    let octal = std::str::from_utf8(&bytes[i + 1..i + 4]).unwrap();
                    out.push(u8::from_str_radix(octal, 8).unwrap_or(b'?'));
                    i += 4;
                }
                other => {
                    out.push(b'\\');
                    out.push(other);
                    i += 2;
                }
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

    #[test]
    fn parse_diff_git_path_handles_an_unquoted_space_in_the_name() {
        // Confirmed via real git output: a plain space alone doesn't
        // trigger quoting.
        assert_eq!(parse_diff_git_path("a/weird file.txt b/weird file.txt"), "weird file.txt");
    }

    #[test]
    fn parse_diff_git_path_handles_a_quoted_path_with_embedded_quotes() {
        // Exact real git output for a file named `file"with"quotes.txt`.
        let rest = r#""a/file\"with\"quotes.txt" "b/file\"with\"quotes.txt""#;
        assert_eq!(parse_diff_git_path(rest), "file\"with\"quotes.txt");
    }

    #[test]
    fn parse_diff_git_path_handles_octal_escaped_non_ascii_bytes() {
        // Exact real git output (core.quotePath=true, the default) for a
        // file named `émotion.txt` — \303\251 is é's two UTF-8 bytes,
        // escaped individually and must be recombined, not decoded byte by
        // byte.
        let rest = r#""a/\303\251motion.txt" "b/\303\251motion.txt""#;
        assert_eq!(parse_diff_git_path(rest), "\u{e9}motion.txt");
    }

    // --- integration: exercises is_git_repo/run_git_diff/load against a real repo ---

    fn scratch_repo(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hoot-gitreview-test-{label}-{}-{:?}",
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
        let dir = std::env::temp_dir().join(format!("hoot-gitreview-not-a-repo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let review = load(&dir);
        assert!(!review.is_real);
        assert_eq!(review.project.name, "search-index"); // mock_project's fixed name

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_sees_a_staged_file_in_a_repo_with_no_commits_yet() {
        // Regression: plain `git diff` (no HEAD to compare against) only
        // shows index-vs-worktree changes, so a `git add`ed file in a
        // brand-new repo was invisible — the index and worktree already
        // agree for it, and there's no earlier commit to diff against.
        let dir = scratch_repo("no-head-staged");
        fs::write(dir.join("new.txt"), "brand new content\n").unwrap();
        run(&dir, &["add", "new.txt"]);

        let review = load(&dir);
        assert!(review.is_real);
        assert!(
            review.project.files.iter().any(|f| f.path == "new.txt"),
            "{:?}",
            review.project.files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );

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

    #[test]
    fn load_includes_untracked_files_not_just_tracked_changes() {
        // Plain `git diff` never shows untracked files (they're not in the
        // index at all) — without special-casing them, a file the agent
        // just created via `write` would be invisible here until staged.
        let dir = scratch_repo("untracked");
        run(&dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
        fs::write(dir.join("new.txt"), "hello\nworld\n").unwrap();

        let review = load(&dir);
        assert_eq!(review.project.files.len(), 1, "should pick up the untracked file");
        assert_eq!(review.project.files[0].path, "new.txt");
        let (added, removed) = review.project.files[0].hunks.iter().flat_map(|h| &h.lines).fold((0, 0), |(a, r), l| match l.kind {
            DiffLineKind::Added => (a + 1, r),
            DiffLineKind::Removed => (r, r + 1),
            _ => (a, r),
        });
        assert_eq!(added, 2, "both lines of a brand-new file should show as added");
        assert_eq!(removed, 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_handles_an_untracked_file_whose_name_starts_with_a_dash() {
        // Regression: `git diff --no-index /dev/null <path>` without a `--`
        // separator would parse a path like `-weird.txt` as a flag instead
        // of a positional argument.
        let dir = scratch_repo("dash-filename");
        run(&dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
        fs::write(dir.join("-weird.txt"), "hello\n").unwrap();

        let review = load(&dir);
        assert_eq!(review.project.files.len(), 1, "should still pick up the dash-prefixed file");
        assert_eq!(review.project.files[0].path, "-weird.txt");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_flags_a_binary_file_as_unsupported_instead_of_silently_zero_hunks() {
        let dir = scratch_repo("binary-change");
        fs::write(dir.join("data.bin"), [0u8, 159, 146, 150, 1, 2, 3]).unwrap();
        run(&dir, &["add", "-A"]);

        let review = load(&dir);
        assert_eq!(review.project.files.len(), 1);
        assert!(review.project.files[0].hunks.is_empty());
        assert_eq!(review.project.files[0].unsupported, Some("binary file"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn untracked_binary_file_gets_a_synthetic_binary_marker_not_garbled_text_hunks() {
        // Untracked files are diffed by reading them directly rather than
        // shelling out to `git diff --no-index` per file (see
        // `synthetic_new_file_diff`) — confirms that path also recognizes
        // binary content instead of feeding raw bytes through as if they
        // were UTF-8 text lines.
        let dir = scratch_repo("untracked-binary");
        run(&dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
        fs::write(dir.join("data.bin"), [0u8, 159, 146, 150, 1, 2, 3]).unwrap();

        let review = load(&dir);
        assert_eq!(review.project.files.len(), 1);
        assert!(review.project.files[0].hunks.is_empty());
        assert_eq!(review.project.files[0].unsupported, Some("binary file"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn untracked_symlink_shows_its_target_path_not_the_target_files_content() {
        // Regression: synthetic_new_file_diff used to `std::fs::read` the
        // path directly, which follows symlinks — an untracked symlink
        // pointing outside the repo (or at a sensitive file elsewhere on
        // disk) would silently read and expose that *other* file's content
        // in Review, and that content could end up in a prompt sent to a
        // real agent. Confirms the fix: a symlink shows its target path
        // text as one added line (exactly what real `git diff` does for a
        // symlink), and the actual external content never appears at all.
        let dir = scratch_repo("untracked-symlink");
        run(&dir, &["commit", "-q", "--allow-empty", "-m", "init"]);

        let outside_dir = std::env::temp_dir().join(format!("hoot-gitreview-outside-{}", std::process::id()));
        fs::create_dir_all(&outside_dir).unwrap();
        let secret_path = outside_dir.join("secret.txt");
        fs::write(&secret_path, "TOP_SECRET_SHOULD_NEVER_APPEAR_IN_REVIEW").unwrap();

        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret_path, dir.join("link")).unwrap();

        let review = load(&dir);
        assert_eq!(review.project.files.len(), 1);
        assert_eq!(review.project.files[0].path, "link");
        let text: String = review.project.files[0].hunks.iter().flat_map(|h| &h.lines).map(|l| l.text.as_str()).collect();
        assert!(!text.contains("TOP_SECRET"), "the linked-to file's content must never appear: {text:?}");
        assert!(text.contains(&secret_path.display().to_string()), "should show the link's target path instead: {text:?}");

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside_dir);
    }

    #[test]
    fn load_flags_a_pure_rename_as_unsupported_but_not_a_rename_with_content_changes() {
        // A longer file, so a one-line edit still leaves similarity well
        // above git's default 50% rename-detection threshold (confirmed
        // empirically — a short 3-line file with one line changed fell
        // *below* that threshold and stopped being recognized as a rename
        // at all, which would've made this test flaky on the exact
        // content rather than testing what it means to).
        let content: String = (1..=30).map(|n| format!("line{n}\n")).collect();
        let dir = scratch_repo("rename-only");
        fs::write(dir.join("old.txt"), &content).unwrap();
        run(&dir, &["add", "-A"]);
        run(&dir, &["commit", "-q", "-m", "init"]);
        run(&dir, &["mv", "old.txt", "new.txt"]);

        let review = load(&dir);
        assert_eq!(review.project.files.len(), 1);
        assert!(review.project.files[0].hunks.is_empty(), "a pure rename has no content diff");
        assert_eq!(review.project.files[0].unsupported, Some("renamed, no content change"));

        // A rename that *also* changes content gets real hunks, and must
        // NOT be flagged unsupported — there's plenty here to curate.
        fs::write(dir.join("new.txt"), content.replacen("line1\n", "line1-CHANGED\n", 1)).unwrap();
        let review = load(&dir);
        assert_eq!(review.project.files.len(), 1);
        assert!(!review.project.files[0].hunks.is_empty(), "rename+edit should still produce real hunks");
        assert_eq!(review.project.files[0].unsupported, None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_mixes_tracked_changes_and_untracked_files_together() {
        let dir = scratch_repo("mixed");
        fs::write(dir.join("tracked.txt"), "a\n").unwrap();
        run(&dir, &["add", "-A"]);
        run(&dir, &["commit", "-q", "-m", "init"]);
        fs::write(dir.join("tracked.txt"), "a-changed\n").unwrap();
        fs::write(dir.join("untracked.txt"), "b\n").unwrap();

        let review = load(&dir);
        let mut paths: Vec<&str> = review.project.files.iter().map(|f| f.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, vec!["tracked.txt", "untracked.txt"]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignored_paths_covers_ignored_files_and_whole_directories_but_not_tracked_ones() {
        let dir = scratch_repo("ignored-paths");
        fs::write(dir.join(".gitignore"), "*.log\n/scratch/\n").unwrap();
        fs::write(dir.join("keep.txt"), "a\n").unwrap();
        fs::write(dir.join("debug.log"), "noise\n").unwrap();
        fs::create_dir_all(dir.join("scratch")).unwrap();
        fs::write(dir.join("scratch/temp.md"), "noise\n").unwrap();
        run(&dir, &["add", "keep.txt", ".gitignore"]);
        run(&dir, &["commit", "-q", "-m", "init"]);

        let ignored = ignored_paths(&dir);
        assert!(ignored.contains(&dir.join("debug.log")), "{ignored:?}");
        // Reported as the directory itself, not each file inside it — a
        // caller skipping whole subtrees during a walk needs exactly that.
        assert!(ignored.contains(&dir.join("scratch")), "{ignored:?}");
        assert!(!ignored.contains(&dir.join("scratch/temp.md")), "{ignored:?}");
        assert!(!ignored.contains(&dir.join("keep.txt")), "{ignored:?}");
        assert!(!ignored.contains(&dir.join(".gitignore")), "{ignored:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignored_paths_matches_a_real_path_for_non_ascii_names() {
        // Regression: by default `git ls-files` quotes any path with a
        // non-ASCII byte as literal text like `"caf\303\251/"` — joining
        // that string onto `root` produces a PathBuf that will never equal
        // a real `café/` entry from `fs::read_dir`, so the ignored
        // directory would silently stay visible. `-z` output has no
        // quoting to undo.
        let dir = scratch_repo("ignored-paths-unicode");
        fs::write(dir.join(".gitignore"), "/café/\n").unwrap();
        fs::create_dir_all(dir.join("café")).unwrap();
        fs::write(dir.join("café/f.txt"), "noise\n").unwrap();
        run(&dir, &["add", ".gitignore"]);
        run(&dir, &["commit", "-q", "-m", "init"]);

        let ignored = ignored_paths(&dir);
        assert!(ignored.contains(&dir.join("café")), "{ignored:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn untracked_files_diff_reads_a_non_ascii_named_file() {
        // Same quoting issue, the other call site: an untracked file with a
        // non-ASCII name used to come back from `ls-files` as a quoted,
        // escaped string that didn't match anything on disk, so
        // `symlink_metadata` silently failed and the file never appeared in
        // the diff at all.
        let dir = scratch_repo("untracked-diff-unicode");
        fs::write(dir.join("café.txt"), "bonjour\n").unwrap();

        let diff = untracked_files_diff(&dir);
        assert!(diff.contains("café.txt"), "{diff}");
        assert!(diff.contains("bonjour"), "{diff}");

        let _ = fs::remove_dir_all(&dir);
    }
}
