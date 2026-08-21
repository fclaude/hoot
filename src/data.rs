//! The types every screen shares: a parsed diff (files, hunks, lines), the
//! metadata git prints around it, and the small per-screen structs built
//! from those.
//!
//! Deliberately holds no data of its own any more. It used to also carry a
//! hand-written `Project` full of invented hunks for `--demo` to display,
//! which is exactly what let the demo drift out of step with what Review
//! read off disk — `demo.rs` builds a real repository instead, and every
//! screen reads it through the same `gitreview` path as any other repo.

use crate::theme::FileStatus;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
    HunkHeader,
}

#[derive(Clone, PartialEq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
    /// True if the diff this was parsed from marked this line with a
    /// trailing `\ No newline at end of file` — i.e. this line is the last
    /// one in its file (old or new side) and that file doesn't end in a
    /// newline. Metadata on the line rather than a line of its own so
    /// staging/rendering code doesn't need a fifth `DiffLineKind` it must
    /// otherwise special-case everywhere `kind` is matched exhaustively.
    pub no_newline: bool,
}

#[derive(Clone, PartialEq)]
pub struct Hunk {
    pub lines: Vec<DiffLine>,
    /// A note-to-agent attached under this hunk, if any.
    pub note: Option<String>,
}

impl Hunk {
    /// The line number this hunk starts at in the *new* (current) file —
    /// parsed from its own header line, e.g. the `18` in
    /// `@@ -12,7 +18,11 @@ impl Foo {`. `None` if `lines` is somehow empty
    /// or doesn't start with a header (shouldn't happen for a real hunk —
    /// every parser here always pushes the header first — but this is
    /// reached from Curate's real hunk data, not just tests, so it stays a
    /// clean `Option` rather than an assumption baked in with `.unwrap()`).
    pub fn new_file_start_line(&self) -> Option<usize> {
        self.lines.first().filter(|l| l.kind == DiffLineKind::HunkHeader).and_then(|l| parse_hunk_new_start(&l.text))
    }
}

/// What git said happened to a file, beyond its line content.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ChangeKind {
    #[default]
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
}

/// Everything about a file's change that *isn't* hunk text: which path(s)
/// it involves, what kind of change it is, its file modes, and the exact
/// header lines git itself printed for it.
///
/// This exists because a diff is not just its hunks. A rename that also
/// edits content, an executable-bit flip, a brand-new empty file: all of
/// those carry their entire meaning in the header, and an earlier version
/// of this struct kept only the destination path and the text hunks. That
/// dropped metadata was not merely invisible — it made commits wrong. A
/// rename staged by path left the *old* path's deletion unstaged, so the
/// commit contained both copies of the file; a mode flip rode along into a
/// commit without ever being shown.
///
/// `header` is kept verbatim, byte-for-byte as git printed it, rather than
/// re-rendered from the structured fields. It is what gets replayed to
/// `git apply --cached`, so re-rendering it would mean re-deriving git's
/// own path quoting — the exact thing that broke on paths containing
/// quotes, backslashes, newlines, or non-UTF-8 bytes. The structured
/// fields below are for *display and comparison*; the header is for git.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct FileMeta {
    pub change: ChangeKind,
    /// The pre-rename/copy path, for display. `None` for everything else.
    pub old_path: Option<String>,
    /// The new path's real bytes, exactly as they are on disk — which is
    /// not always what `path` holds, since that one is a lossy, printable
    /// rendering. Used whenever a path has to be handed back to git as an
    /// argument (`git reset -- <path>`).
    pub raw_path: Vec<u8>,
    pub raw_old_path: Option<Vec<u8>>,
    /// Set only when git actually printed them, which it does for a mode
    /// change (`old mode`/`new mode`), a new file (`new` only) or a
    /// deletion (`old` only) — not for an ordinary edit.
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    /// The extended header, verbatim: `diff --git` through `+++`,
    /// everything before the first `@@`. Replayed as-is when staging.
    pub header: Vec<String>,
    /// A gitlink (mode 160000) on either side — a submodule pointer, whose
    /// "content" is a commit id in another repository. Hoot has no way to
    /// stage one faithfully, so it refuses rather than guessing.
    pub submodule: bool,
}

impl FileMeta {
    /// An ordinary in-place content change to `path`, with no real git
    /// header behind it. Test-only: every `FileMeta` in the running app is
    /// parsed from actual `git diff` output, header included (that header
    /// is what gets replayed when staging, so a synthesized one would be a
    /// commit nobody reviewed). This exists so a unit test can hand-build a
    /// `FileEntry` without a repo on disk.
    #[cfg(test)]
    pub fn modified(path: &str) -> Self {
        Self { raw_path: path.as_bytes().to_vec(), ..Default::default() }
    }

    /// True when git reported different modes on the two sides — an
    /// executable bit flipped, or a regular file turned into a symlink.
    pub fn mode_changed(&self) -> bool {
        matches!((&self.old_mode, &self.new_mode), (Some(a), Some(b)) if a != b)
    }

    /// Short human phrases for everything in here that isn't line content,
    /// so the UI can *show* the parts of a change that have no hunks of
    /// their own. Empty for a plain edit, which needs no explaining.
    pub fn describe(&self) -> Vec<String> {
        let mut out = Vec::new();
        match self.change {
            ChangeKind::Renamed => out.push(format!("renamed from {}", self.old_path.as_deref().unwrap_or("?"))),
            ChangeKind::Copied => out.push(format!("copied from {}", self.old_path.as_deref().unwrap_or("?"))),
            ChangeKind::Added => out.push("new file".to_string()),
            ChangeKind::Deleted => out.push("deleted".to_string()),
            ChangeKind::Modified => {}
        }
        if self.mode_changed() {
            let (old, new) = (self.old_mode.as_deref().unwrap_or("?"), self.new_mode.as_deref().unwrap_or("?"));
            out.push(format!("mode {old} \u{2192} {new}{}", mode_meaning(old, new)));
        }
        out
    }

    /// The new path as real filesystem/argv bytes, for handing back to git.
    pub fn os_path(&self) -> std::ffi::OsString {
        os_string_from_bytes(&self.raw_path)
    }

    pub fn os_old_path(&self) -> Option<std::ffi::OsString> {
        self.raw_old_path.as_deref().map(os_string_from_bytes)
    }
}

/// The human-readable half of a mode change, when it's one of the two that
/// actually mean something to a person — a plain `100644 → 100755` reads
/// as noise otherwise.
fn mode_meaning(old: &str, new: &str) -> &'static str {
    match (old, new) {
        ("100644", "100755") => " (made executable)",
        ("100755", "100644") => " (executable bit removed)",
        (_, "120000") => " (now a symlink)",
        ("120000", _) => " (no longer a symlink)",
        _ => "",
    }
}

#[cfg(unix)]
fn os_string_from_bytes(bytes: &[u8]) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(bytes).to_os_string()
}

#[cfg(not(unix))]
fn os_string_from_bytes(bytes: &[u8]) -> std::ffi::OsString {
    std::ffi::OsString::from(String::from_utf8_lossy(bytes).into_owned())
}

/// A file as it appears in the Review tree.
#[derive(Clone, PartialEq)]
pub struct FileEntry {
    /// A printable rendering of the new path — lossy for a name that isn't
    /// valid UTF-8 (see `gitreview::display_path`). Identity within the
    /// UI, but never what gets handed to git: `meta.raw_path` is.
    pub path: String,
    pub hunk_count: u32,
    pub notes: u32,
    pub flagged: bool,
    pub hunks: Vec<Hunk>,
    /// Set when the diff parser found a real change here that hoot cannot
    /// curate *or* stage — binary content, or a submodule pointer update.
    /// This is the reason, so the UI can say so plainly instead of
    /// silently showing "0/0 hunks" with no explanation.
    ///
    /// Note what is deliberately *not* here any more: pure renames and
    /// mode-only changes. Those have no hunks either, but they are
    /// perfectly stageable from their header alone — see `is_metadata_only`.
    pub unsupported: Option<&'static str>,
    pub meta: FileMeta,
}

impl FileEntry {
    /// True when git reported a real change with no hunk content at all,
    /// yet the change is still something that can be staged faithfully
    /// from its header alone: a pure rename, a mode-only change, a
    /// brand-new empty file. Curation gives these a single selectable
    /// unit — there are no hunks to pick from, but there *is* something to
    /// commit, and refusing to commit a `git mv` would be its own kind of
    /// wrong.
    pub fn is_metadata_only(&self) -> bool {
        self.hunks.is_empty() && self.unsupported.is_none() && !self.meta.header.is_empty()
    }

    /// Whether two views of the same path describe the same change —
    /// content *and* metadata. The freshness check at commit time compares
    /// this rather than hunks alone: a concurrent `chmod +x` or `git mv`
    /// leaves every hunk identical while changing what a commit would
    /// contain, and used to sail straight through.
    pub fn same_change_as(&self, other: &FileEntry) -> bool {
        self.hunks == other.hunks && self.meta == other.meta
    }
}

/// `hunks` flattened into one line sequence and paired with the
/// corresponding line number in the *current* file content — `None` for a
/// `Removed` line, since those no longer exist in the current file.
/// Hunk-header lines are dropped entirely (they're structural, not file
/// content). Called with a whole-file-context diff
/// (`gitreview::file_diff_in_context`), this reconstructs the entire
/// current file in order, changes overlaid — not just isolated snippets
/// around each change. Powers both the whole-file diff view and
/// line-scoped review comments.
pub fn diff_lines_with_file_line_numbers(hunks: &[Hunk]) -> Vec<(Option<usize>, DiffLine)> {
    let mut out = Vec::new();
    for hunk in hunks {
        let mut line_no = hunk.new_file_start_line().unwrap_or(1);
        for dl in &hunk.lines {
            match dl.kind {
                DiffLineKind::HunkHeader => continue,
                DiffLineKind::Removed => out.push((None, dl.clone())),
                DiffLineKind::Context | DiffLineKind::Added => {
                    out.push((Some(line_no), dl.clone()));
                    line_no += 1;
                }
            }
        }
    }
    out
}

/// Extracts the new-file starting line number from a hunk header like
/// `@@ -12,7 +18,11 @@ impl Foo {` — the `18` after the `+`.
fn parse_hunk_new_start(header: &str) -> Option<usize> {
    let plus = header.split('+').nth(1)?;
    let num = plus.split([',', ' ']).next()?;
    num.parse().ok()
}

/// Collapses long unchanged stretches of `lines` down to `context` lines
/// immediately around each `Added`/`Removed` line, replacing anything
/// longer with a single placeholder line (paired with `None`, since it
/// doesn't correspond to any one line) — the compact "just the changes"
/// style, derived from the same full-context, numbered lines the
/// whole-file view uses rather than a second, separately-loaded diff.
pub fn focus_diff_lines(lines: &[(Option<usize>, DiffLine)], context: usize) -> Vec<(Option<usize>, DiffLine)> {
    let changed: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, (_, l))| matches!(l.kind, DiffLineKind::Added | DiffLineKind::Removed))
        .map(|(i, _)| i)
        .collect();
    if changed.is_empty() {
        return Vec::new();
    }
    let mut keep = vec![false; lines.len()];
    for &i in &changed {
        let lo = i.saturating_sub(context);
        let hi = (i + context).min(lines.len().saturating_sub(1));
        for k in keep.iter_mut().take(hi + 1).skip(lo) {
            *k = true;
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if keep[i] {
            out.push(lines[i].clone());
            i += 1;
        } else {
            let start = i;
            while i < lines.len() && !keep[i] {
                i += 1;
            }
            let n = i - start;
            let text = format!("\u{22ef} {n} unchanged line{} \u{22ef}", if n == 1 { "" } else { "s" });
            out.push((None, DiffLine { kind: DiffLineKind::HunkHeader, text, no_newline: false }));
        }
    }
    out
}

pub struct Project {
    pub name: String,
    pub root: String,
    pub files: Vec<FileEntry>,
}

// ---------------------------------------------------------------------
// FILE TREE
// ---------------------------------------------------------------------

#[derive(PartialEq)]
pub struct TreeEntry {
    pub label: String,
    pub depth: u8,
    pub is_dir: bool,
    pub child_count: Option<u32>,
    pub path: std::path::PathBuf,
}

/// A real, free-text review note left in Review — against a whole file
/// (`line: None`) or against one specific line. Feeds into the prompt
/// Review's "iterate" sends to the agent (and the identical one `y` copies
/// to the clipboard).
pub struct Note {
    pub path: String,
    pub line: Option<usize>,
    pub text: String,
}

pub struct HoverInfo {
    pub signature: String,
    pub location: String,
    pub references: u32,
}

// ---------------------------------------------------------------------
// SYMBOL JUMP
// ---------------------------------------------------------------------

pub struct SymbolResult {
    pub name: String,
    pub path: std::path::PathBuf,
    pub line: usize,
    pub preview: String,
}

impl SymbolResult {
    pub fn location(&self, root: &std::path::Path) -> String {
        let rel = self.path.strip_prefix(root).unwrap_or(&self.path);
        format!("{}:{}", rel.display(), self.line)
    }
}

// ---------------------------------------------------------------------
// AGENT PANE
// ---------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AgentLineKind {
    Done,
    /// The agent's final answer — rendered at full brightness.
    Text,
    /// The agent's private reasoning, shown dimmed so it reads as
    /// clearly secondary to the actual answer instead of blending into it.
    Thinking,
    ToolCall,
    Proposal,
    /// A line the user typed and sent, prefixed with `>` and shown bold so
    /// it stands out as the human side of the conversation.
    UserPrompt,
    Blank,
}

pub struct AgentLine {
    pub kind: AgentLineKind,
    pub text: String,
}

// ---------------------------------------------------------------------
// CURATION
// ---------------------------------------------------------------------

pub struct CurationFile {
    pub path: String,
    /// One entry per hunk in the corresponding `FileEntry.hunks`, in order.
    pub hunk_selected: Vec<bool>,
    pub status: Option<FileStatus>,
}

impl CurationFile {
    pub fn selected(&self) -> u32 {
        self.hunk_selected.iter().filter(|s| **s).count() as u32
    }

    pub fn total(&self) -> u32 {
        self.hunk_selected.len() as u32
    }
}
