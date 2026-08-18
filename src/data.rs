//! Mock data for the `steer` demo. Content mirrors the worked example used
//! throughout the source design mockups (a fictional `search-index` crate).

use crate::theme::FileStatus;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
    HunkHeader,
}

#[derive(Clone)]
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

#[derive(Clone)]
pub struct Hunk {
    pub lines: Vec<DiffLine>,
    /// A note-to-agent attached under this hunk, if any.
    pub note: Option<String>,
}

/// A file as it appears in the STEER sidebar.
#[derive(Clone)]
pub struct FileEntry {
    pub path: String,
    pub hunk_count: u32,
    pub notes: u32,
    pub selected: bool,
    pub flagged: bool,
    pub hunks: Vec<Hunk>,
}

impl FileEntry {
    /// (added lines, removed lines) across every hunk.
    pub fn diff_stat(&self) -> (u32, u32) {
        let mut plus = 0;
        let mut minus = 0;
        for line in self.hunks.iter().flat_map(|h| &h.lines) {
            match line.kind {
                DiffLineKind::Added => plus += 1,
                DiffLineKind::Removed => minus += 1,
                _ => {}
            }
        }
        (plus, minus)
    }
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
        FileEntry {
            path: "main.rs".to_string(),
            hunk_count: 2,
            notes: 0,
            selected: true,
            flagged: false,
            hunks: vec![],
        },
        FileEntry {
            path: "lib.rs".to_string(),
            hunk_count: 1,
            notes: 0,
            selected: true,
            flagged: false,
            hunks: vec![],
        },
        FileEntry {
            path: "index/mod.rs".to_string(),
            hunk_count: 3,
            notes: 0,
            selected: false,
            flagged: false,
            hunks: vec![],
        },
        FileEntry {
            path: "index/postings.rs".to_string(),
            hunk_count: 5,
            notes: 2,
            selected: true,
            flagged: true,
            hunks: postings_hunks,
        },
        FileEntry {
            path: "query/parser.rs".to_string(),
            hunk_count: 6,
            notes: 1,
            selected: false,
            flagged: false,
            hunks: parser_hunks,
        },
        FileEntry {
            path: "tests/integration_test.rs".to_string(),
            hunk_count: 3,
            notes: 0,
            selected: false,
            flagged: false,
            hunks: vec![],
        },
    ];

    Project { name: "search-index".to_string(), root: "search-index/src".to_string(), files }
}

// ---------------------------------------------------------------------
// NAVIGATE
// ---------------------------------------------------------------------

pub struct TreeEntry {
    pub label: String,
    pub depth: u8,
    pub is_dir: bool,
    pub child_count: Option<u32>,
    pub path: std::path::PathBuf,
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
    InProgress,
    Text,
    ToolCall,
    Proposal,
    Blank,
}

pub struct AgentLine {
    pub kind: AgentLineKind,
    pub text: String,
}

fn al(kind: AgentLineKind, text: &str) -> AgentLine {
    AgentLine { kind, text: text.to_string() }
}

pub fn mock_transcript() -> Vec<AgentLine> {
    use AgentLineKind::*;
    vec![
        al(Text, "This is demo transcript. Type a prompt below and press Enter to talk to the real `pi` agent."),
        al(Blank, ""),
        al(Done, "Initial context loaded (187 tokens)"),
        al(InProgress, "Analyzing codebase..."),
        al(Text, "  Looking at query parser error handling patterns."),
        al(Text, "  Implementation re-throws all errors; needs better messages."),
        al(Blank, ""),
        al(Text, "  Observation: ParseError enum has 5 variants, only 2 documented."),
        al(Text, "  Decision: improve error messages and optimize phrase_match."),
        al(Blank, ""),
        al(ToolCall, "Calling: read_file(\"src/query/parser.rs\")"),
        al(Done, "read_file(\"src/query/parser.rs\")  [1247 bytes]"),
        al(ToolCall, "Calling: analyze_code(pattern=\"error handling\")"),
        al(Done, "analyze_code returned 3 issues"),
        al(Blank, ""),
        al(Proposal, "Proposing changes to 3 files:"),
    ]
}

// ---------------------------------------------------------------------
// PERMISSION PROMPT
// ---------------------------------------------------------------------

pub struct FileWrite {
    pub path: String,
    pub kind: &'static str,
    pub plus: u32,
    pub minus: u32,
}

pub fn mock_permission_writes() -> Vec<FileWrite> {
    vec![
        FileWrite { path: "src/index/postings.rs".to_string(), kind: "modify", plus: 47, minus: 8 },
        FileWrite { path: "src/query/parser.rs".to_string(), kind: "modify", plus: 15, minus: 3 },
        FileWrite { path: "tests/integration_test.rs".to_string(), kind: "modify", plus: 23, minus: 0 },
    ]
}

pub fn mock_permission_commands() -> Vec<&'static str> {
    vec!["cargo test --lib"]
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
