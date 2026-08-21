//! Real change review, backed by `git diff` in the target directory.
//!
//! There is no fabricated-data path here any more, and deliberately so.
//! `load` used to substitute hand-written demo content whenever the target
//! wasn't a git repository, which meant a typo'd path could open a UI full
//! of invented changes, and `--demo` itself showed hunks that disagreed
//! with what Review read off disk. `main.rs` now rejects a non-repo
//! outright and `--demo` builds a real throwaway repo (see `demo.rs`), so
//! every screen in the app is looking at the same real `git diff`. A repo
//! with a clean tree reports an honest empty changeset.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::data::{ChangeKind, CurationFile, DiffLine, DiffLineKind, FileEntry, FileMeta, Hunk, Project};

pub struct ReviewData {
    pub project: Project,
    pub curation_files: Vec<CurationFile>,
}

/// The working-tree review for `root`. A directory that isn't a git
/// repository has nothing to review and reports exactly that — an empty
/// changeset — rather than standing in fake content for it.
pub fn load(root: &Path) -> ReviewData {
    let files = if is_git_repo(root) { diff_files(root) } else { Vec::new() };

    let curation_files =
        files.iter().map(|f| CurationFile { path: f.path.clone(), hunk_selected: vec![true; selectable_units(f)], status: None }).collect();

    let name = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| root.display().to_string());
    let project = Project { name: name.clone(), root: name, files };

    ReviewData { project, curation_files }
}

/// How many independently selectable units a file offers in Curation: one
/// per hunk for an ordinary change, exactly one for a change that lives
/// entirely in its header (a pure rename, a mode flip, a new empty file),
/// and none at all for one hoot can't stage faithfully — a binary file or
/// a submodule pointer — so those can never be pulled into a commit by
/// accident.
pub fn selectable_units(f: &FileEntry) -> usize {
    if f.unsupported.is_some() {
        0
    } else if f.is_metadata_only() {
        1
    } else {
        f.hunks.len()
    }
}

/// The parsed working-tree diff for `root`: empty if it's not a git repo or
/// has no changes.
pub fn diff_files(root: &Path) -> Vec<FileEntry> {
    let diff_text = run_git_diff(root);
    parse_unified_diff(&diff_text)
}

/// Every git invocation in this module goes through here, and every one of
/// them is pinned to the same config regardless of what the repository or
/// the user has set:
///
/// * `core.quotePath=true` (git's default, forced anyway) keeps every path
///   in diff output pure ASCII — non-UTF-8 filenames arrive as `\303\251`
///   escapes rather than raw bytes, so the diff text is always valid UTF-8
///   no matter what the filenames are.
/// * `diff.noprefix` / `diff.mnemonicPrefix` off keeps the `a/` and `b/`
///   prefixes that both this parser and `git apply` expect. A user with
///   `diff.noprefix=true` in their global config would otherwise get diffs
///   this code can't read and git can't re-apply.
fn git(root: &Path, args: &[&str]) -> Option<Output> {
    Command::new("git")
        .args(["-c", "core.quotePath=true", "-c", "diff.noprefix=false", "-c", "diff.mnemonicPrefix=false"])
        .args(args)
        .current_dir(root)
        .output()
        .ok()
}

/// The flags every `git diff` here shares. `--no-ext-diff`/`--no-textconv`
/// matter more than they look: a repo that configures an external diff
/// driver or a textconv filter (common for binaries, PDFs, notebooks)
/// would otherwise hand back human-readable output that is not a patch at
/// all — pleasant to read, impossible to re-apply, and this module stages
/// commits by re-applying exactly what it parsed.
const DIFF_FLAGS: [&str; 4] = ["--no-color", "--no-ext-diff", "--no-textconv", "--find-renames"];

pub fn is_git_repo(root: &Path) -> bool {
    git(root, &["rev-parse", "--is-inside-work-tree"]).map(|o| o.status.success()).unwrap_or(false)
}

/// Runs a `git ls-files`-family command and returns the relative paths it
/// names as raw bytes. By default git quotes any path with a non-ASCII or
/// otherwise "unusual" byte — `café.txt` comes back as the literal text
/// `"caf\303\251.txt"`, quote marks and octal escapes included — which is
/// exactly what breaks joining the result onto `root` to get a real
/// filesystem path back. `-z` (added here, not by callers) makes git emit
/// the actual filesystem bytes NUL-terminated instead, so there's nothing
/// to unescape.
///
/// Bytes, not `String`: a filename on Unix is an arbitrary byte string,
/// and lossily decoding one here (the old behavior) produced a path with
/// U+FFFD in it that matches nothing on disk and can never be handed back
/// to git.
fn ls_files_paths(root: &Path, args: &[&str]) -> Vec<Vec<u8>> {
    let mut args = args.to_vec();
    args.push("-z");
    let Some(listing) = git(root, &args) else { return Vec::new() };
    listing.stdout.split(|&b| b == 0).filter(|s| !s.is_empty()).map(|s| s.to_vec()).collect()
}

/// Raw path bytes back to a real `PathBuf`, without a lossy round-trip
/// through `String`.
#[cfg(unix)]
fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
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
        .map(|rel| {
            let trimmed = rel.strip_suffix(b"/").unwrap_or(&rel);
            root.join(bytes_to_path(trimmed))
        })
        .collect()
}

/// Diffs against HEAD (staged + unstaged) when a commit exists; otherwise
/// falls back to a plain working-tree diff (e.g. a repo with zero commits).
/// Appends untracked files too — plain `git diff` never shows those (they
/// aren't in the index at all), which would otherwise make a file the
/// agent just created invisible in Review until it's staged. Uses git's
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
        diff_text(root, &["HEAD"])
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
        let staged = diff_text(root, &["--cached"]);
        let unstaged = diff_text(root, &[]);
        staged + &unstaged
    };
    diff.push_str(&untracked_files_diff(root));
    diff
}

/// `git diff <args>` with this module's fixed flags, as text.
fn diff_text(root: &Path, args: &[&str]) -> String {
    let mut all: Vec<&str> = vec!["diff"];
    all.extend(DIFF_FLAGS);
    all.extend(args);
    git(root, &all).map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default()
}

/// What is currently staged in the index, parsed exactly like the
/// working-tree diff. Used to verify, after staging and before committing,
/// that the index really does hold what was selected — see
/// `gitcommit::commit`.
pub fn staged_files(root: &Path) -> Vec<FileEntry> {
    parse_unified_diff(&diff_text(root, &["--cached"]))
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

/// Raw bytes of a path, without a lossy round-trip through `String`.
#[cfg(unix)]
fn path_to_bytes(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_to_bytes(p: &Path) -> Vec<u8> {
    p.to_string_lossy().into_owned().into_bytes()
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
fn synthetic_new_file_diff(root: &Path, rel_path: &[u8]) -> String {
    let full = root.join(bytes_to_path(rel_path));
    let Ok(meta) = std::fs::symlink_metadata(&full) else {
        return String::new();
    };
    // Quoted exactly the way git would quote it, and only when git would:
    // this text is fed straight back to `git apply` when the file gets
    // staged, so a name containing a quote, a backslash, a newline or a
    // non-UTF-8 byte has to survive the round trip intact.
    let a = diff_header_path("a/", rel_path);
    let b = diff_header_path("b/", rel_path);
    let header = format!("diff --git {a} {b}\n");
    let binary = || format!("{header}new file mode 100644\nBinary files /dev/null and {b} differ\n");

    if meta.is_symlink() {
        let Ok(target) = std::fs::read_link(&full) else {
            return String::new();
        };
        // A symlink's blob content is the target path, with no trailing
        // newline — hence the marker, which is not decoration: staging
        // replays this patch verbatim, and without it git would write a
        // link target with a stray newline in it.
        let Ok(target) = String::from_utf8(path_to_bytes(&target)) else { return binary() };
        return format!("{header}new file mode 120000\n--- /dev/null\n+++ {b}\n@@ -0,0 +1 @@\n+{target}\n\\ No newline at end of file\n");
    }
    if !meta.is_file() {
        return String::new();
    }

    if meta.len() > MAX_SYNTHETIC_DIFF_SIZE {
        return binary();
    }
    let Ok(bytes) = std::fs::read(&full) else {
        return String::new();
    };
    // Not just NUL-scanning any more: any content that isn't valid UTF-8
    // is treated as binary here, because everything downstream of this
    // point is a Rust `String`. Lossily decoding a Latin-1 text file (the
    // old behavior) produced hunks full of U+FFFD that *looked* like a
    // reviewable diff, and staging replays exactly those hunks — which
    // would have committed the mangled version. Refusing to curate it is
    // the honest outcome; `git add` outside hoot still works fine.
    if bytes.contains(&0) {
        return binary();
    }
    let Ok(text) = std::str::from_utf8(&bytes) else { return binary() };

    let mode = if is_executable(&meta) { "100755" } else { "100644" };
    // `split_inclusive`, not `lines()`: `lines()` strips a trailing `\r`,
    // so every CRLF file would have been staged with its line endings
    // silently rewritten to LF, and it can't distinguish a file that ends
    // in a newline from one that doesn't.
    let mut body = String::new();
    let mut count = 0usize;
    for chunk in text.split_inclusive('\n') {
        count += 1;
        body.push('+');
        match chunk.strip_suffix('\n') {
            Some(line) => {
                body.push_str(line);
                body.push('\n');
            }
            None => {
                body.push_str(chunk);
                body.push_str("\n\\ No newline at end of file\n");
            }
        }
    }
    if count == 0 {
        // An empty new file: header and nothing else, which is exactly
        // what git itself emits. It has no hunks to select, but it is
        // still perfectly stageable from this header alone — see
        // `FileEntry::is_metadata_only`.
        return format!("{header}new file mode {mode}\n");
    }
    format!("{header}new file mode {mode}\n--- /dev/null\n+++ {b}\n@@ -0,0 +1,{count} @@\n{body}")
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
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
    let mut diff =
        if has_head { diff_text(root, &["HEAD", FULL_CONTEXT, "--", rel_path]) } else { diff_text(root, &[FULL_CONTEXT, "--", rel_path]) };
    if diff.trim().is_empty() {
        // Not a tracked change — might be untracked entirely.
        diff = diff_text(root, &[FULL_CONTEXT, "--no-index", "--", "/dev/null", rel_path]);
    }
    parse_unified_diff(&diff).into_iter().next().map(|f| f.hunks).unwrap_or_default()
}

fn parse_unified_diff(diff: &str) -> Vec<FileEntry> {
    let mut files: Vec<FileEntry> = Vec::new();
    let mut current: Option<FileEntry> = None;
    let mut current_hunk: Option<Hunk> = None;
    // Set when a header line names a change hoot can't stage at all —
    // binary content, today. Deliberately *not* set for a rename or a mode
    // change: those have no hunks either, but they are staged straight from
    // the header. See `flush_file`.
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
                if f.meta.submodule {
                    // Unlike everything else here, this one holds even
                    // when there *are* hunks: a submodule's "content" is a
                    // commit id in another repository, and the `-Subproject
                    // commit ...` lines git prints for it are not something
                    // `git apply --cached` can stage. Better to say so than
                    // to offer a selection that can't be honored.
                    f.unsupported = Some("a submodule pointer change");
                } else if f.hunks.is_empty() {
                    // Only worth flagging if it really did leave nothing to
                    // curate — a rename *with* content changes still gets
                    // its real hunks and has plenty to select. And a reason
                    // is only recorded for changes hoot genuinely can't
                    // handle: a pure rename or a mode-only change has no
                    // hunks either, but is stageable straight from its
                    // header, so it deliberately stays un-flagged.
                    f.unsupported = reason.take();
                }
                files.push(f);
            }
            *reason = None;
        };

    // `split('\n')`, not `lines()`: `lines()` strips a trailing `\r`, which
    // would quietly rewrite every CRLF file's content on its way through
    // here — and since staging replays these exact line strings back to
    // `git apply`, that meant a patch whose context no longer matched the
    // file it came from. The trailing empty piece a `\n`-terminated diff
    // leaves behind matches none of the prefixes below and falls through
    // harmlessly.
    for line in diff.split('\n') {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            flush_file(&mut files, &mut current, &mut current_hunk, &mut unsupported_reason);
            let raw_path = parse_diff_git_new_path(rest);
            current = Some(FileEntry {
                path: display_path(&raw_path),
                hunk_count: 0,
                notes: 0,
                flagged: false,
                hunks: Vec::new(),
                unsupported: None,
                meta: FileMeta { raw_path, header: vec![line.to_string()], ..Default::default() },
            });
            continue;
        }
        let Some(f) = current.as_mut() else { continue };

        if line.starts_with("@@ ") {
            flush_hunk(f, &mut current_hunk);
            current_hunk = Some(Hunk {
                lines: vec![DiffLine { kind: DiffLineKind::HunkHeader, text: line.to_string(), no_newline: false }],
                note: None,
            });
            continue;
        }

        // Anything before the first `@@` is the file's extended header;
        // everything after it, until the next `diff --git`, is hunk content.
        let Some(h) = current_hunk.as_mut() else {
            read_header_line(f, line, &mut unsupported_reason);
            continue;
        };

        if line.starts_with("\\ No newline at end of file") {
            // Applies to the line immediately above it, not a content
            // line of its own — recorded as metadata on that line so
            // `gitcommit::build_patch` can re-emit it verbatim when only
            // some hunks of a file are staged. Dropping it outright (the
            // old behavior) silently produced a patch `git apply`
            // rejects whenever the affected line falls inside a
            // selected hunk.
            if let Some(last) = h.lines.last_mut() {
                last.no_newline = true;
            }
            continue;
        }
        // Inside a hunk, the first byte *is* the classification — there is
        // no `---`/`+++` to exclude here, because those only ever appear in
        // the header region handled above. Excluding them anyway (the old
        // behavior) silently dropped any added line whose own content
        // started with `++`, leaving a hunk whose body no longer matched
        // the line counts in its own `@@` header.
        let kind = match line.as_bytes().first() {
            Some(b'+') => DiffLineKind::Added,
            Some(b'-') => DiffLineKind::Removed,
            Some(b' ') => DiffLineKind::Context,
            // Not diff content at all: a stray line, or the empty piece a
            // `\n`-terminated diff leaves at the end.
            _ => continue,
        };
        h.lines.push(DiffLine { kind, text: line.to_string(), no_newline: false });
    }
    flush_file(&mut files, &mut current, &mut current_hunk, &mut unsupported_reason);

    files
}

/// One line of a file's extended header — everything git prints between
/// `diff --git` and the first `@@`. Two things happen to it: the parts
/// that describe the change get pulled out into structured fields for the
/// UI and for comparison, and the line itself is kept verbatim in
/// `meta.header` if it's part of the patch proper, so staging can replay
/// git's own framing rather than trying to reconstruct it.
fn read_header_line(f: &mut FileEntry, line: &str, reason: &mut Option<&'static str>) {
    if let Some(mode) = line.strip_prefix("old mode ") {
        f.meta.old_mode = Some(mode.trim().to_string());
    } else if let Some(mode) = line.strip_prefix("new mode ") {
        f.meta.new_mode = Some(mode.trim().to_string());
    } else if let Some(mode) = line.strip_prefix("new file mode ") {
        f.meta.change = ChangeKind::Added;
        f.meta.new_mode = Some(mode.trim().to_string());
    } else if let Some(mode) = line.strip_prefix("deleted file mode ") {
        f.meta.change = ChangeKind::Deleted;
        f.meta.old_mode = Some(mode.trim().to_string());
    } else if let Some(path) = line.strip_prefix("rename from ") {
        f.meta.change = ChangeKind::Renamed;
        set_old_path(f, path);
    } else if let Some(path) = line.strip_prefix("copy from ") {
        f.meta.change = ChangeKind::Copied;
        set_old_path(f, path);
    } else if let Some(rest) = line.strip_prefix("index ") {
        // `index <old>..<new> <mode>` — the trailing mode is only present
        // when both sides share it, which is where a gitlink announces
        // itself for a submodule whose pointer moved.
        if rest.split_whitespace().nth(1) == Some(GITLINK_MODE) {
            f.meta.submodule = true;
        }
    } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
        *reason = Some("binary content");
        // Deliberately not kept as framing: it isn't something `git apply`
        // can replay without the full binary payload.
        return;
    }
    if f.meta.old_mode.as_deref() == Some(GITLINK_MODE) || f.meta.new_mode.as_deref() == Some(GITLINK_MODE) {
        f.meta.submodule = true;
    }
    if is_patch_framing(line) {
        f.meta.header.push(line.to_string());
    }
}

/// git's mode for a submodule pointer (a "gitlink").
const GITLINK_MODE: &str = "160000";

/// Header lines that are part of the patch itself — the ones `git apply`
/// reads. Anything else git may print in that region (`Binary files ...
/// differ`, `GIT binary patch`, a `warning:`) is not replayed.
fn is_patch_framing(line: &str) -> bool {
    const PREFIXES: [&str; 13] = [
        "old mode ",
        "new mode ",
        "new file mode ",
        "deleted file mode ",
        "copy from ",
        "copy to ",
        "rename from ",
        "rename to ",
        "similarity index ",
        "dissimilarity index ",
        "index ",
        "--- ",
        "+++ ",
    ];
    PREFIXES.iter().any(|p| line.starts_with(p))
}

fn set_old_path(f: &mut FileEntry, quoted: &str) {
    let raw = unquote_diff_path(quoted);
    f.meta.old_path = Some(display_path(&raw));
    f.meta.raw_old_path = Some(raw);
}

/// The b-side (destination) path from a `diff --git ...` line's tail, as
/// raw bytes. Handles git's C-style quoted form — `"a/<escaped>"
/// "b/<escaped>"` — which it switches to for a path containing a quote, a
/// backslash, a control character, or (with the default
/// `core.quotePath=true`) any non-ASCII byte. Confirmed against real git
/// output, not assumed: a plain space in a name isn't quoted at all
/// (`a/weird file.txt b/weird file.txt`, handled by the plain rfind
/// below), but `"` or non-ASCII bytes are, e.g. `"a/\303\251motion.txt"
/// "b/\303\251motion.txt"` for `émotion.txt`. Each side is quoted
/// independently, so a rename can mix the two forms.
fn parse_diff_git_new_path(rest: &str) -> Vec<u8> {
    let rest = rest.trim_end();
    // A quoted b-side always begins at the last ` "` in the line; a quoted
    // a-side, if any, is everything before it.
    if let Some(idx) = rest.rfind(" \"") {
        let candidate = &rest[idx + 1..];
        if let Some(inner) = candidate.strip_prefix('"').and_then(|c| find_unescaped_quote(c).map(|e| &c[..e])) {
            let decoded = unquote_c_style(inner);
            return strip_side_prefix(&decoded, b"b/");
        }
    }
    if let Some(after_quote) = rest.strip_prefix('"') {
        // Quoted a-side, unquoted b-side: everything after the closing
        // quote and the space that follows it.
        if let Some(end) = find_unescaped_quote(after_quote) {
            let remainder = after_quote[end + 1..].trim_start();
            return strip_side_prefix(remainder.as_bytes(), b"b/");
        }
    }
    match rest.rfind(" b/") {
        Some(idx) => rest.as_bytes()[idx + 3..].to_vec(),
        None => rest.as_bytes().to_vec(),
    }
}

fn strip_side_prefix(path: &[u8], prefix: &[u8]) -> Vec<u8> {
    path.strip_prefix(prefix).unwrap_or(path).to_vec()
}

/// A path as it appears in a `rename from`/`rename to`/`copy from` header:
/// bare, or C-quoted in its entirety when it needs escaping.
fn unquote_diff_path(text: &str) -> Vec<u8> {
    let text = text.trim_end();
    match text.strip_prefix('"').and_then(|t| find_unescaped_quote(t).map(|e| &t[..e])) {
        Some(inner) => unquote_c_style(inner),
        None => text.as_bytes().to_vec(),
    }
}

/// A printable rendering of a path's real bytes. Valid UTF-8 passes
/// through untouched; anything else falls back to git's own quoted-and-
/// escaped form rather than `to_string_lossy`. U+FFFD would look tidier,
/// but it discards which bytes were actually there and collapses two
/// genuinely different filenames into one identical-looking string — and
/// this string is what the UI uses to tell files apart.
fn display_path(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => quote_c_style(bytes),
    }
}

/// `<prefix><path>` rendered for a diff header, quoted exactly when git
/// would quote it. Used for the synthetic untracked-file diffs this module
/// builds by hand, which are fed back to `git apply` when staging.
fn diff_header_path(prefix: &str, path: &[u8]) -> String {
    let mut full = prefix.as_bytes().to_vec();
    full.extend_from_slice(path);
    let needs_quoting = full.iter().any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\') || std::str::from_utf8(&full).is_err();
    if needs_quoting {
        quote_c_style(&full)
    } else {
        String::from_utf8_lossy(&full).into_owned()
    }
}

/// The inverse of `unquote_c_style`: git's own quoting, including the
/// surrounding double quotes.
fn quote_c_style(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 2);
    out.push('"');
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\{b:03o}")),
        }
    }
    out.push('"');
    out
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

/// Un-escapes git's C-style quoting: `\\`, `\"`, `\t`, `\n`, `\r` and
/// `\NNN` octal byte escapes (used for non-ASCII bytes — each byte of a
/// multi-byte UTF-8 character gets its own `\NNN`). Returns raw bytes: a
/// filename is a byte string, and the escapes can spell one that isn't
/// valid UTF-8 at all.
fn unquote_c_style(s: &str) -> Vec<u8> {
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
                b'r' => {
                    out.push(b'\r');
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
    out
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

    /// The parsed b-side path, rendered for comparison in tests.
    fn parsed_path(rest: &str) -> String {
        display_path(&parse_diff_git_new_path(rest))
    }

    #[test]
    fn parse_diff_git_path_strips_a_and_b_prefixes() {
        assert_eq!(parsed_path("a/src/main.rs b/src/main.rs"), "src/main.rs");
        assert_eq!(parsed_path("a/nested/dir/file.py b/nested/dir/file.py"), "nested/dir/file.py");
    }

    #[test]
    fn parse_diff_git_path_handles_an_unquoted_space_in_the_name() {
        // Confirmed via real git output: a plain space alone doesn't
        // trigger quoting.
        assert_eq!(parsed_path("a/weird file.txt b/weird file.txt"), "weird file.txt");
    }

    #[test]
    fn parse_diff_git_path_handles_a_quoted_path_with_embedded_quotes() {
        // Exact real git output for a file named `file"with"quotes.txt`.
        let rest = r#""a/file\"with\"quotes.txt" "b/file\"with\"quotes.txt""#;
        assert_eq!(parsed_path(rest), "file\"with\"quotes.txt");
    }

    #[test]
    fn parse_diff_git_path_handles_octal_escaped_non_ascii_bytes() {
        // Exact real git output (core.quotePath=true, the default) for a
        // file named `émotion.txt` — \303\251 is é's two UTF-8 bytes,
        // escaped individually and must be recombined, not decoded byte by
        // byte.
        let rest = r#""a/\303\251motion.txt" "b/\303\251motion.txt""#;
        assert_eq!(parsed_path(rest), "\u{e9}motion.txt");
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
    fn load_reports_nothing_to_review_outside_a_git_repo() {
        // Regression: this used to return hand-written demo content, so a
        // typo'd path opened a full UI of invented changes with nothing to
        // say they weren't real. An empty changeset is the honest answer;
        // `main.rs` refuses to get this far in the first place.
        let dir = std::env::temp_dir().join(format!("hoot-gitreview-not-a-repo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let review = load(&dir);
        assert!(review.project.files.is_empty());
        assert!(review.curation_files.is_empty());

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
        assert_eq!(review.project.files[0].unsupported, Some("binary content"));

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
        assert_eq!(review.project.files[0].unsupported, Some("binary content"));

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
    fn load_treats_a_pure_rename_as_one_stageable_change_with_no_hunks() {
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
        let renamed = &review.project.files[0];
        assert!(renamed.hunks.is_empty(), "a pure rename has no content diff");
        // Not "unsupported": there are no hunks to pick from, but the
        // change is fully described by its own header and stages from it
        // exactly — so it gets one selectable unit rather than being
        // written off as something hoot can't handle.
        assert_eq!(renamed.unsupported, None);
        assert!(renamed.is_metadata_only());
        assert_eq!(renamed.meta.change, ChangeKind::Renamed);
        assert_eq!(renamed.meta.old_path.as_deref(), Some("old.txt"));
        assert_eq!(renamed.path, "new.txt");
        assert_eq!(review.curation_files[0].total(), 1, "one unit to select: the rename itself");

        // A rename that *also* changes content gets real hunks — and still
        // remembers it's a rename, which is what makes it stageable
        // without leaving the old path's deletion behind.
        fs::write(dir.join("new.txt"), content.replacen("line1\n", "line1-CHANGED\n", 1)).unwrap();
        let review = load(&dir);
        assert_eq!(review.project.files.len(), 1);
        assert!(!review.project.files[0].hunks.is_empty(), "rename+edit should still produce real hunks");
        assert_eq!(review.project.files[0].unsupported, None);
        assert_eq!(review.project.files[0].meta.change, ChangeKind::Renamed);
        assert_eq!(review.project.files[0].meta.old_path.as_deref(), Some("old.txt"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_submodule_pointer_change_is_uncommittable_even_though_it_has_hunks() {
        // A gitlink's "content" is a commit id in another repository. git
        // prints it as an ordinary one-line hunk, which used to make it look
        // like something Curate could stage — it isn't, and `git apply
        // --cached` can't do it either. It offers no selectable unit at all,
        // so it can never be pulled into a commit by accident.
        let diff = "\
diff --git a/vendor/lib b/vendor/lib
index 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 160000
--- a/vendor/lib
+++ b/vendor/lib
@@ -1 +1 @@
-Subproject commit 1111111111111111111111111111111111111111
+Subproject commit 2222222222222222222222222222222222222222
";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        assert!(files[0].meta.submodule);
        assert_eq!(files[0].unsupported, Some("a submodule pointer change"));
        assert!(!files[0].hunks.is_empty(), "git really does give it a hunk");
        assert_eq!(selectable_units(&files[0]), 0, "but nothing here is selectable");
    }

    #[test]
    fn a_mode_change_alongside_an_edit_keeps_both_the_modes_and_the_hunks() {
        // The exact shape that used to lose its metadata: `old mode`/`new
        // mode` *and* content hunks. The hunks were kept, the modes thrown
        // away — so an executable bit could ride into a commit having never
        // been shown.
        let diff = "\
diff --git a/run.sh b/run.sh
old mode 100644
new mode 100755
index 1111111..2222222
--- a/run.sh
+++ b/run.sh
@@ -1,2 +1,2 @@
 #!/bin/sh
-echo old
+echo new
";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.meta.old_mode.as_deref(), Some("100644"));
        assert_eq!(f.meta.new_mode.as_deref(), Some("100755"));
        assert!(f.meta.mode_changed());
        assert_eq!(f.hunks.len(), 1);
        assert_eq!(f.unsupported, None, "there's plenty to curate here");
        // The header is kept verbatim, in order, because it is what gets
        // replayed to `git apply` — including the mode lines.
        assert_eq!(
            f.meta.header,
            vec![
                "diff --git a/run.sh b/run.sh",
                "old mode 100644",
                "new mode 100755",
                "index 1111111..2222222",
                "--- a/run.sh",
                "+++ b/run.sh",
            ]
        );
    }

    #[test]
    fn a_binary_marker_is_never_kept_as_replayable_framing() {
        let diff = "\
diff --git a/logo.png b/logo.png
index 1111111..2222222 100644
Binary files a/logo.png and b/logo.png differ
";
        let files = parse_unified_diff(diff);
        assert_eq!(files[0].unsupported, Some("binary content"));
        assert!(!files[0].meta.header.iter().any(|h| h.starts_with("Binary files")), "{:?}", files[0].meta.header);
        assert_eq!(selectable_units(&files[0]), 0);
    }

    #[test]
    fn parsing_preserves_carriage_returns_in_crlf_content() {
        // Regression: the parser split the diff with `lines()`, which strips
        // a trailing `\r`. Every CRLF file's content was silently rewritten
        // to LF on the way in — and since staging replays these same strings
        // back to `git apply`, the patch no longer described the file it came
        // from.
        let diff = "diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,1 +1,1 @@\n-old\r\n+new\r\n";
        let files = parse_unified_diff(diff);
        let texts: Vec<&str> = files[0].hunks[0].lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["@@ -1,1 +1,1 @@", "-old\r", "+new\r"]);
    }

    #[test]
    fn an_untracked_file_diff_records_a_missing_final_newline() {
        // Whole-file staging used to go through `git add`, so this synthetic
        // diff only had to be right enough to *look* at. It's now the thing
        // that gets committed: without the marker git would append a newline
        // the file never had.
        let dir = scratch_repo("untracked-no-newline");
        fs::write(dir.join("f.txt"), "one\ntwo").unwrap();

        let diff = untracked_files_diff(&dir);
        assert!(diff.ends_with("+two\n\\ No newline at end of file\n"), "{diff:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_untracked_file_that_isnt_valid_utf8_is_treated_as_binary() {
        // It has no NUL bytes, so the old check called it text and ran it
        // through `from_utf8_lossy` — producing hunks full of U+FFFD that
        // looked reviewable. Staging replays exactly those hunks, so
        // committing one would have written the mangled version.
        let dir = scratch_repo("untracked-latin1");
        fs::write(dir.join("latin1.txt"), [0x63, 0x61, 0x66, 0xe9, 0x0a]).unwrap(); // "café\n" in Latin-1

        let review = load(&dir);
        assert_eq!(review.project.files.len(), 1);
        assert_eq!(review.project.files[0].unsupported, Some("binary content"));
        assert!(review.project.files[0].hunks.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_that_isnt_valid_utf8_stays_distinguishable_instead_of_becoming_replacement_characters() {
        // Two different filenames, one lossy conversion, one indistinguishable
        // string: `to_string_lossy` maps both of these to "f\u{fffd}.txt".
        // The UI uses this string to tell files apart, so it can't be allowed
        // to collapse them.
        let a = display_path(b"f\xff.txt");
        let b = display_path(b"f\xfe.txt");
        assert_ne!(a, b);
        assert_eq!(a, r#""f\377.txt""#);
        assert_eq!(display_path("plain.txt".as_bytes()), "plain.txt");
    }

    #[test]
    fn diff_header_paths_are_quoted_exactly_when_git_would_quote_them() {
        assert_eq!(diff_header_path("a/", b"plain.txt"), "a/plain.txt");
        // A space alone doesn't trigger quoting — confirmed against real
        // git output.
        assert_eq!(diff_header_path("b/", b"weird file.txt"), "b/weird file.txt");
        // Valid UTF-8 doesn't need it either: `git apply` reads the raw
        // bytes back just fine, and quoting is only about what git *emits*.
        assert_eq!(diff_header_path("a/", "caf\u{e9}.txt".as_bytes()), "a/caf\u{e9}.txt");
        // A quote, a backslash, a newline, or a non-UTF-8 byte does.
        assert_eq!(diff_header_path("a/", b"she\"said.txt"), r#""a/she\"said.txt""#);
        assert_eq!(diff_header_path("a/", b"back\\slash"), r#""a/back\\slash""#);
        assert_eq!(diff_header_path("b/", b"line\nbreak"), r#""b/line\nbreak""#);
        assert_eq!(diff_header_path("b/", b"f\xff.txt"), r#""b/f\377.txt""#);
    }

    #[test]
    fn a_quoted_path_survives_the_round_trip_through_quoting_and_back() {
        for name in [&b"plain.txt"[..], b"she\"said.txt", b"back\\slash", b"line\nbreak", b"caf\xc3\xa9.txt", b"f\xff.txt"] {
            let quoted = diff_header_path("b/", name);
            let parsed = parse_diff_git_new_path(&format!("a/whatever {quoted}"));
            assert_eq!(parsed, name, "round trip failed for {quoted}");
        }
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
