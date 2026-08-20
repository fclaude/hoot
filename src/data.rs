//! Mock data for the `hoot` demo. Content mirrors the worked example used
//! throughout the source design mockups (a fictional `search-index` crate).

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
}

fn ctx(text: &str) -> DiffLine {
    DiffLine { kind: DiffLineKind::Context, text: text.to_string() }
}
fn add(text: &str) -> DiffLine {
    DiffLine { kind: DiffLineKind::Added, text: text.to_string() }
}
fn rem(text: &str) -> DiffLine {
    DiffLine { kind: DiffLineKind::Removed, text: text.to_string() }
}
fn hdr(text: &str) -> DiffLine {
    DiffLine { kind: DiffLineKind::HunkHeader, text: text.to_string() }
}

#[derive(Clone, PartialEq)]
pub struct Hunk {
    pub lines: Vec<DiffLine>,
    /// A note-to-agent attached under this hunk, if any.
    pub note: Option<String>,
}

/// A file as it appears in the Review tree.
#[derive(Clone, PartialEq)]
pub struct FileEntry {
    pub path: String,
    pub hunk_count: u32,
    pub notes: u32,
    pub flagged: bool,
    pub hunks: Vec<Hunk>,
    /// Set when the diff parser found a real change here but nothing it
    /// can turn into selectable hunks — binary content, a pure rename, a
    /// mode-only change, or a submodule pointer update. `hunks` is empty
    /// in that case; this is why, so the UI can say so plainly instead of
    /// silently showing "0/0 hunks" with no explanation.
    pub unsupported: Option<&'static str>,
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
        let mut line_no =
            hunk.lines.first().filter(|l| l.kind == DiffLineKind::HunkHeader).and_then(|l| parse_hunk_new_start(&l.text)).unwrap_or(1);
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
            out.push((None, DiffLine { kind: DiffLineKind::HunkHeader, text }));
        }
    }
    out
}

pub struct Project {
    pub name: String,
    pub root: String,
    pub files: Vec<FileEntry>,
}

pub fn mock_project() -> Project {
    let postings_hunks = vec![
        Hunk {
            lines: vec![
                hdr("@@ -42,7 +42,11 @@ impl Postings {"),
                ctx("  pub fn iter(&self) -> PostingsIter {"),
                ctx("    PostingsIter {"),
                rem("-     docs: &self.docs,"),
                add("+     docs: &self.docs,"),
                add("+     positions: &self.positions,"),
                add("+     freq: &self.freq,"),
                add("+     offsets: &self.offsets,"),
                ctx("    }"),
                ctx("  }"),
            ],
            note: Some(
                "note to agent: extract this into a named IterState type instead of an anonymous struct literal — reusable from search() too.".to_string(),
            ),
        },
        Hunk {
            lines: vec![
                hdr("@@ -89,9 +97,15 @@ impl Postings {"),
                ctx("  let phrase = self.parse_phrase(phrase_query)?;"),
                rem("-   for &doc_id in phrase.docs() {"),
                add("+   for &doc_id in phrase.docs().iter() {"),
                ctx("      let postings = self.index.get(doc_id)?;"),
                ctx("      if postings.contains_all(&phrase.terms) {"),
                ctx("        results.push(doc_id);"),
                ctx("      }"),
            ],
            note: None,
        },
    ];

    let parser_hunks = vec![Hunk {
        lines: vec![
            hdr("@@ -12,6 +12,9 @@ impl QueryParser {"),
            ctx("  pub fn parse(&mut self) -> Result<Query, ParseError> {"),
            rem("-   self.token()?"),
            add("+   self.token().map_err(|e| e.with_context(self.pos))?"),
            ctx("  }"),
        ],
        note: Some("extract into a named IterState type — reusable from search() too.".to_string()),
    }];

    let files = vec![
        FileEntry { path: "main.rs".to_string(), hunk_count: 2, notes: 0, flagged: false, hunks: vec![], unsupported: None },
        FileEntry { path: "lib.rs".to_string(), hunk_count: 1, notes: 0, flagged: false, hunks: vec![], unsupported: None },
        FileEntry { path: "index/mod.rs".to_string(), hunk_count: 3, notes: 0, flagged: false, hunks: vec![], unsupported: None },
        FileEntry {
            path: "index/postings.rs".to_string(),
            hunk_count: 5,
            notes: 2,
            flagged: true,
            hunks: postings_hunks,
            unsupported: None,
        },
        FileEntry { path: "query/parser.rs".to_string(), hunk_count: 6, notes: 1, flagged: false, hunks: parser_hunks, unsupported: None },
        FileEntry {
            path: "tests/integration_test.rs".to_string(),
            hunk_count: 3,
            notes: 0,
            flagged: false,
            hunks: vec![],
            unsupported: None,
        },
    ];

    Project { name: "search-index".to_string(), root: "search-index/src".to_string(), files }
}

// ---------------------------------------------------------------------
// NAVIGATE
// ---------------------------------------------------------------------

#[derive(PartialEq)]
pub struct TreeEntry {
    pub label: String,
    pub depth: u8,
    pub is_dir: bool,
    pub child_count: Option<u32>,
    pub path: std::path::PathBuf,
}

/// A real, free-text review note — from Hoot (file-level, `line: None`) or
/// Navigate (a specific line while browsing). Feeds into the real prompt
/// Hoot's "iterate" sends to `pi`.
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

pub fn mock_curation_files() -> Vec<CurationFile> {
    let cf = |path: &str, selected: u32, total: u32, status| CurationFile {
        path: path.to_string(),
        hunk_selected: (0..total).map(|i| i < selected).collect(),
        status,
    };
    vec![
        cf("main.rs", 1, 2, Some(FileStatus::Clean)),
        cf("lib.rs", 1, 1, Some(FileStatus::Clean)),
        cf("index/mod.rs", 2, 3, Some(FileStatus::Clean)),
        cf("index/postings.rs", 0, 5, Some(FileStatus::Flagged)),
        cf("query/parser.rs", 3, 4, Some(FileStatus::HasNotes)),
        cf("tests/integration_test.rs", 0, 3, Some(FileStatus::HasNotes)),
    ]
}

pub fn mock_commit_message() -> String {
    "Optimize postings iteration and improve error messages\n\
     \n\
     - Extract postings iteration into a dedicated iterator type\n\
     \x20 for better code reuse across search and merge paths\n\
     - Improve QueryParser error handling with source context\n\
     - Add phrase_match optimization for large indices\n\
     \n\
     Affected: postings.rs, parser.rs, integration_test.rs"
        .to_string()
}
