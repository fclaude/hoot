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
///
/// A line-scoped note carries an `anchor` as well as a line number,
/// because the number on its own stops being true the moment the agent
/// edits the file: "Line 47" written against one function is not a
/// correction of whatever happens to sit at line 47 after the next turn.
/// The anchor is what lets `App::reanchor_notes` move the note to where
/// its line actually went — or, when it can't find it, mark the note
/// `stale` rather than keep quoting a number that now points somewhere
/// else. Notes are never dropped either way: the user wrote them.
pub struct Note {
    pub path: String,
    /// Where the note currently points, 1-based. Rewritten in place when
    /// the note is successfully re-anchored; left at its last known value
    /// once `stale` is set, so the prompt can still say where it *was*.
    pub line: Option<usize>,
    pub text: String,
    /// The file text around `line` as it stood when the note was written.
    /// `None` for a whole-file note, which points at no line and so can
    /// never go stale.
    pub anchor: Option<NoteAnchor>,
    /// Set when the file changed and `anchor` could no longer be found in
    /// it. The note stays in the queue and still goes to the agent — it
    /// just goes with an explicit "the line this was on is gone" instead
    /// of a line number that would be read as current.
    pub stale: bool,
}

impl Note {
    /// A note against a whole file. Nothing to anchor to, so nothing that
    /// can later go stale.
    pub fn on_file(path: String, text: String) -> Self {
        Self { path, line: None, text, anchor: None, stale: false }
    }

    /// A note against one 1-based line of `path`, anchored to the text
    /// around it. `anchor` is `None` only when the surrounding text
    /// couldn't be captured at all, which leaves the note pinned to a bare
    /// line number exactly as it was before anchors existed.
    pub fn on_line(path: String, line: usize, anchor: Option<NoteAnchor>, text: String) -> Self {
        Self { path, line: Some(line), text, anchor, stale: false }
    }
}

/// A line-scoped note's foothold in the file: a short window of file text
/// with the noted line inside it.
///
/// The window, not just the noted line, is what makes re-placing a note
/// worth attempting at all. Source files are full of lines that are not
/// remotely unique — `}`, `    }`, a blank line, `#[test]` — and matching
/// on one of those alone would confidently move a note onto some unrelated
/// closing brace. Its neighbours are what make it identifiable.
#[derive(Clone, PartialEq, Debug)]
pub struct NoteAnchor {
    /// Consecutive lines of the file, in order, centred on the noted line.
    pub window: Vec<String>,
    /// Index within `window` of the noted line itself.
    pub offset: usize,
}

impl NoteAnchor {
    /// How many lines of context either side of the noted line get
    /// captured. Enough to disambiguate an ordinary closing brace from the
    /// dozens of others in the file, short enough that an edit a few lines
    /// away doesn't automatically invalidate it — and the single-line
    /// fallback in `reanchor` covers that case anyway.
    const CONTEXT: usize = 3;

    /// The anchor for 1-based `line` of `lines`, or `None` if that line
    /// isn't in the file at all.
    pub fn capture(lines: &[String], line: usize) -> Option<Self> {
        let idx = line.checked_sub(1).filter(|i| *i < lines.len())?;
        let start = idx.saturating_sub(Self::CONTEXT);
        let end = (idx + Self::CONTEXT + 1).min(lines.len());
        Some(Self { window: lines[start..end].to_vec(), offset: idx - start })
    }

    /// The noted line's own text.
    pub fn line_text(&self) -> &str {
        self.window.get(self.offset).map(String::as_str).unwrap_or_default()
    }

    /// Where this anchor now sits in `lines`, as a 1-based line number —
    /// `None` if it can't be found, which is what makes a note stale.
    ///
    /// Two passes, in decreasing order of confidence. First the whole
    /// window: a contiguous match means the noted line *and* its
    /// neighbourhood survived intact, and the nearest such match to
    /// `previous` wins if the same block appears more than once. Failing
    /// that, the noted line alone — but only if the file contains it
    /// exactly once, so there is genuinely only one place it can have
    /// gone. A line that occurs three times with all of its context
    /// rewritten is not something to guess at; that's what `stale` is for.
    pub fn reanchor(&self, lines: &[String], previous: usize) -> Option<usize> {
        if self.window.is_empty() {
            return None;
        }
        let nearest = |starts: Vec<usize>, offset: usize| -> Option<usize> {
            starts.into_iter().map(|i| i + offset + 1).min_by_key(|line| (line.abs_diff(previous), *line))
        };
        let window_starts: Vec<usize> =
            lines.windows(self.window.len()).enumerate().filter(|(_, w)| *w == self.window.as_slice()).map(|(i, _)| i).collect();
        if !window_starts.is_empty() {
            return nearest(window_starts, self.offset);
        }
        let text = self.line_text();
        let mut hits = lines.iter().enumerate().filter(|(_, l)| l.as_str() == text).map(|(i, _)| i);
        let only = hits.next()?;
        if hits.next().is_some() {
            return None;
        }
        Some(only + 1)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    const FILE: &str = "\
fn one() {
    let a = 1;
}

fn two() {
    let b = 2;
}
";

    #[test]
    fn an_anchor_captures_the_noted_line_and_its_neighbours() {
        let a = NoteAnchor::capture(&lines(FILE), 2).expect("line 2 exists");
        assert_eq!(a.line_text(), "    let a = 1;");
        assert_eq!(a.window, lines("fn one() {\n    let a = 1;\n}\n\nfn two() {"));
        assert_eq!(a.offset, 1, "the noted line is the second of the captured window");
    }

    #[test]
    fn capturing_past_the_end_of_the_file_finds_nothing() {
        assert!(NoteAnchor::capture(&lines(FILE), 99).is_none());
        assert!(NoteAnchor::capture(&lines(FILE), 0).is_none(), "line numbers are 1-based");
    }

    #[test]
    fn a_note_follows_its_line_when_the_file_grows_above_it() {
        let a = NoteAnchor::capture(&lines(FILE), 6).expect("line 6 exists");
        assert_eq!(a.line_text(), "    let b = 2;");
        // Four lines inserted at the top: the noted line is now line 10.
        let moved = lines(&format!("// a\n// b\n// c\n// d\n{FILE}"));
        assert_eq!(a.reanchor(&moved, 6), Some(10));
    }

    #[test]
    fn a_line_that_survives_without_its_neighbours_still_re_anchors() {
        // The window is gone — everything around it was rewritten — but
        // the noted line itself is still in the file exactly once, so
        // there is only one place it can have gone.
        let a = NoteAnchor::capture(&lines(FILE), 2).expect("line 2 exists");
        let rewritten = lines("fn renamed(x: u8) -> u8 {\n    let a = 1;\n    x\n}\n");
        assert_eq!(a.reanchor(&rewritten, 2), Some(2));
    }

    #[test]
    fn a_line_that_is_gone_goes_stale_rather_than_landing_somewhere_plausible() {
        let a = NoteAnchor::capture(&lines(FILE), 2).expect("line 2 exists");
        let rewritten = lines("fn one() {\n    let a = 99;\n}\n");
        assert_eq!(a.reanchor(&rewritten, 2), None, "the line it was about no longer exists anywhere");
    }

    #[test]
    fn an_ambiguous_line_with_no_surviving_context_goes_stale_too() {
        // Exactly the case a single-line match would get confidently
        // wrong: `}` appears twice, both bare, and nothing about the
        // note's original neighbourhood is left to choose between them.
        let a = NoteAnchor::capture(&lines("fn one() {\n}\nfn two() {\n}\n"), 2).expect("line 2 exists");
        assert_eq!(a.line_text(), "}");
        let rewritten = lines("fn alpha() {\n}\nfn beta() {\n}\n");
        assert_eq!(a.reanchor(&rewritten, 2), None);
    }

    #[test]
    fn a_repeated_block_re_anchors_to_the_copy_nearest_where_the_note_was() {
        // Two byte-identical seven-line blocks, so the captured window
        // genuinely matches in both places and proximity to the note's
        // last known position is what breaks the tie.
        let block: Vec<String> = (1..=7).map(|n| format!("row{n}")).collect();
        let doubled: Vec<String> = block.iter().chain(block.iter()).cloned().collect();
        let a = NoteAnchor::capture(&doubled, 4).expect("line 4 exists");
        assert_eq!(a.line_text(), "row4");
        assert_eq!(a.reanchor(&doubled, 4), Some(4));
        assert_eq!(a.reanchor(&doubled, 11), Some(11), "asked from lower down, it picks the nearer copy");
    }

    #[test]
    fn a_whole_file_note_has_no_anchor_and_never_goes_stale() {
        let n = Note::on_file("a.rs".to_string(), "rename this module".to_string());
        assert_eq!(n.line, None);
        assert!(n.anchor.is_none());
        assert!(!n.stale);
    }
}
