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
