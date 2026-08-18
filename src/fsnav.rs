//! Real, filesystem-backed code navigation.
//!
//! This is intentionally a lightweight heuristic scanner, not a language
//! server: `scan_symbols` recognizes common definition shapes (`fn`, `func`,
//! `def`, `class`, `struct`, `enum`, ...) across a handful of languages via
//! simple prefix matching, and `hover_for_line`/`reference_count` do
//! whole-word text search rather than semantic resolution. Good enough for
//! jump-to-definition-ish navigation over a real repo; not a substitute for
//! `rust-analyzer` et al.

use std::fs;
use std::path::{Path, PathBuf};

use crate::data::{SymbolResult, TreeEntry};

const SKIP_DIRS: &[&str] = &[
    ".git", "target", "node_modules", ".venv", "venv", "dist", "build", "__pycache__", ".idea", ".vscode",
];
const SOURCE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "go", "py", "java", "c", "h", "cpp", "hpp", "rb", "swift", "kt",
];
const TREE_BUDGET: usize = 400;
const SCAN_FILE_BUDGET: usize = 500;
const SYMBOL_BUDGET: usize = 500;

pub fn build_tree(root: &Path) -> Vec<TreeEntry> {
    let mut out = Vec::new();
    walk(root, 0, &mut out);
    out
}

fn walk(dir: &Path, depth: u8, out: &mut Vec<TreeEntry>) {
    if out.len() >= TREE_BUDGET {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| (e.file_type().map(|t| t.is_file()).unwrap_or(false), e.file_name()));

    for entry in entries {
        if out.len() >= TREE_BUDGET {
            return;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            let child_count = fs::read_dir(&path).map(|d| d.filter_map(|e| e.ok()).count() as u32).unwrap_or(0);
            out.push(TreeEntry {
                label: format!("{name}/"),
                depth,
                is_dir: true,
                child_count: Some(child_count),
                path: path.clone(),
            });
            walk(&path, depth + 1, out);
        } else {
            out.push(TreeEntry { label: name, depth, is_dir: false, child_count: None, path });
        }
    }
}

pub fn read_file(path: &Path) -> Vec<String> {
    match fs::read_to_string(path) {
        Ok(content) => content.lines().take(4000).map(|l| l.to_string()).collect(),
        Err(e) => vec![format!("(couldn't read {}: {e})", path.display())],
    }
}

fn is_source_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).map(|e| SOURCE_EXTS.contains(&e)).unwrap_or(false)
}

fn collect_source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if out.len() >= SCAN_FILE_BUDGET {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.filter_map(|e| e.ok()) {
        if out.len() >= SCAN_FILE_BUDGET {
            return;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            collect_source_files(&path, out);
        } else if is_source_file(&path) {
            out.push(path);
        }
    }
}

pub fn scan_symbols(root: &Path) -> Vec<SymbolResult> {
    let mut files = Vec::new();
    collect_source_files(root, &mut files);

    let mut out = Vec::new();
    'files: for path in &files {
        let Ok(content) = fs::read_to_string(path) else { continue };
        for (i, line) in content.lines().enumerate() {
            if let Some(name) = extract_symbol_name(line) {
                out.push(SymbolResult { name, path: path.clone(), line: i + 1, preview: line.trim().to_string() });
            }
            if out.len() >= SYMBOL_BUDGET {
                break 'files;
            }
        }
    }
    out
}

fn extract_symbol_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();

    // Go methods: `func (recv Type) Name(...)` — the receiver breaks plain
    // prefix matching, so pull the name out after the closing paren.
    if let Some(rest) = trimmed.strip_prefix("func (") {
        if let Some(close) = rest.find(')') {
            let after = rest[close + 1..].trim_start();
            let name: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }

    // Go type declarations: `type Name struct { ... }` / `interface { ... }`
    // / plain aliases — Go puts the name before the kind, unlike Rust/TS.
    if let Some(rest) = trimmed.strip_prefix("type ") {
        let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if !name.is_empty() {
            return Some(name);
        }
    }

    const MARKERS: &[&str] = &[
        "pub async fn ",
        "pub fn ",
        "async fn ",
        "fn ",
        "func ",
        "async def ",
        "def ",
        "pub struct ",
        "struct ",
        "pub enum ",
        "enum ",
        "pub trait ",
        "trait ",
        "class ",
        "interface ",
        "export default function ",
        "export function ",
        "export class ",
        "function ",
    ];
    for marker in MARKERS {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

/// Finds a symbol whose name appears as a whole word on `line_text`.
pub fn hover_for_line<'a>(symbols: &'a [SymbolResult], line_text: &str) -> Option<&'a SymbolResult> {
    let words: Vec<&str> = line_text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .collect();
    symbols.iter().find(|s| words.contains(&s.name.as_str()))
}

/// Simple case-insensitive subsequence fuzzy match: every character of
/// `needle` must appear in `haystack` in the same order, not necessarily
/// adjacent — so "mnrs" matches "main.rs". No scoring/ranking, just
/// yes-or-no; callers sort however they like (here, filtered lists just
/// keep the underlying collection's order).
pub fn fuzzy_match(needle: &str, haystack: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mut hay = haystack.to_lowercase().chars().collect::<Vec<_>>().into_iter();
    'needle: for nc in needle.to_lowercase().chars() {
        for hc in hay.by_ref() {
            if hc == nc {
                continue 'needle;
            }
        }
        return false;
    }
    true
}

/// Whole-word occurrence count of `name` across scanned source files.
pub fn reference_count(root: &Path, name: &str) -> u32 {
    let mut files = Vec::new();
    collect_source_files(root, &mut files);
    let mut count = 0u32;
    for path in files {
        let Ok(content) = fs::read_to_string(&path) else { continue };
        for line in content.lines() {
            count += line
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .filter(|w| *w == name)
                .count() as u32;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "steer-fsnav-test-{label}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn extracts_rust_definitions() {
        assert_eq!(extract_symbol_name("pub fn draw() {"), Some("draw".to_string()));
        assert_eq!(extract_symbol_name("  async fn run() {"), Some("run".to_string()));
        assert_eq!(extract_symbol_name("pub struct FileEntry {"), Some("FileEntry".to_string()));
        assert_eq!(extract_symbol_name("pub enum DiffLineKind {"), Some("DiffLineKind".to_string()));
        assert_eq!(extract_symbol_name("trait Widget {"), Some("Widget".to_string()));
    }

    #[test]
    fn extracts_go_plain_function() {
        assert_eq!(extract_symbol_name("func NewServer(addr string) *Server {"), Some("NewServer".to_string()));
    }

    #[test]
    fn extracts_go_method_with_receiver() {
        // The receiver `(s *Server)` used to break plain prefix matching and
        // silently drop the method entirely — see fsnav.rs's dedicated case.
        assert_eq!(extract_symbol_name("func (s *Server) Handle(w string) {"), Some("Handle".to_string()));
    }

    #[test]
    fn extracts_go_type_declarations() {
        assert_eq!(extract_symbol_name("type Server struct {"), Some("Server".to_string()));
        assert_eq!(extract_symbol_name("type Reader interface {"), Some("Reader".to_string()));
        assert_eq!(extract_symbol_name("type Handler func(w string)"), Some("Handler".to_string()));
    }

    #[test]
    fn extracts_python_definitions_including_async() {
        assert_eq!(extract_symbol_name("class Widget:"), Some("Widget".to_string()));
        assert_eq!(extract_symbol_name("    def __init__(self, name):"), Some("__init__".to_string()));
        assert_eq!(extract_symbol_name("    async def render(self):"), Some("render".to_string()));
    }

    #[test]
    fn non_definition_lines_extract_nothing() {
        assert_eq!(extract_symbol_name("    self.name = name"), None);
        assert_eq!(extract_symbol_name("// just a comment"), None);
        assert_eq!(extract_symbol_name(""), None);
    }

    #[test]
    fn hover_for_line_finds_whole_word_matches_only() {
        let symbols = vec![
            SymbolResult { name: "parse".to_string(), path: PathBuf::from("a.rs"), line: 1, preview: "fn parse()".to_string() },
        ];
        assert!(hover_for_line(&symbols, "let x = parse(input);").is_some());
        // "reparse" contains "parse" as a substring but not as a whole word.
        assert!(hover_for_line(&symbols, "let x = reparse(input);").is_none());
        assert!(hover_for_line(&symbols, "totally unrelated line").is_none());
    }

    #[test]
    fn scan_symbols_finds_definitions_across_go_and_python_files() {
        let dir = scratch_dir("scan");
        fs::write(dir.join("server.go"), "package main\n\nfunc (s *Server) Handle() {}\n\ntype Server struct {}\n").unwrap();
        fs::write(dir.join("app.py"), "class Widget:\n    async def render(self):\n        pass\n").unwrap();
        fs::write(dir.join("skip.bin"), "not source").unwrap();

        let symbols = scan_symbols(&dir);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Handle"), "names = {names:?}");
        assert!(names.contains(&"Server"), "names = {names:?}");
        assert!(names.contains(&"Widget"), "names = {names:?}");
        assert!(names.contains(&"render"), "names = {names:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_tree_skips_noise_directories() {
        let dir = scratch_dir("tree");
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(dir.join("target/junk"), "").unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();

        let tree = build_tree(&dir);
        let labels: Vec<&str> = tree.iter().map(|e| e.label.as_str()).collect();
        assert!(labels.contains(&"src/"), "labels = {labels:?}");
        assert!(labels.contains(&"main.rs"), "labels = {labels:?}");
        assert!(!labels.iter().any(|l| l.starts_with('.')), "labels = {labels:?}");
        assert!(!labels.contains(&"target/"), "labels = {labels:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fuzzy_match_finds_non_contiguous_subsequences() {
        assert!(fuzzy_match("mnrs", "main.rs"));
        assert!(fuzzy_match("main.rs", "main.rs"));
        assert!(fuzzy_match("", "anything"));
        assert!(fuzzy_match("MNRS", "main.rs"), "should be case-insensitive");
    }

    #[test]
    fn fuzzy_match_rejects_out_of_order_or_missing_characters() {
        assert!(!fuzzy_match("srn", "main.rs")); // right letters, wrong order
        assert!(!fuzzy_match("xyz", "main.rs"));
        assert!(!fuzzy_match("main.rs.extra", "main.rs")); // needle longer than haystack
    }
}
