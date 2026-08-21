//! Real, filesystem-backed code navigation.
//!
//! This is intentionally a lightweight heuristic scanner, not a language
//! server: `scan_symbols` recognizes common definition shapes (`fn`, `func`,
//! `def`, `class`, `struct`, `enum`, ...) across a handful of languages via
//! simple prefix matching, and `hover_for_line`/`reference_count` do
//! whole-word text search rather than semantic resolution. Good enough for
//! jump-to-definition-ish navigation over a real repo; not a substitute for
//! `rust-analyzer` et al.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::data::{SymbolResult, TreeEntry};

// Everything here is skipped by exact name, not by a blanket "starts with
// a dot" rule — that used to hide every dotfile from Review, including
// ones very much worth reviewing (.github/workflows/*, .gitignore,
// .env.example, ...). `.git` itself and a handful of known-noise
// directories/files are the only things that never belong in the tree.
const SKIP_DIRS: &[&str] =
    &[".git", "target", "node_modules", ".venv", "venv", "dist", "build", "__pycache__", ".idea", ".vscode", ".DS_Store"];
const SOURCE_EXTS: &[&str] = &["rs", "ts", "tsx", "js", "jsx", "go", "py", "java", "c", "h", "cpp", "hpp", "rb", "swift", "kt"];
const TREE_BUDGET: usize = 400;
const SCAN_FILE_BUDGET: usize = 500;
const SYMBOL_BUDGET: usize = 500;

pub fn build_tree(root: &Path) -> Vec<TreeEntry> {
    let mut out = Vec::new();
    let ignored = crate::gitreview::ignored_paths(root);
    walk(root, 0, &mut out, &ignored);
    out
}

fn walk(dir: &Path, depth: u8, out: &mut Vec<TreeEntry>, ignored: &HashSet<PathBuf>) {
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
        if SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        // Gitignored paths (build output, caches, local scratch/log dirs,
        // ...) are exactly what a Git review tool's file list should never
        // show — they're not part of what's being reviewed, and an ignored
        // directory full of logs was enough on its own to exhaust
        // TREE_BUDGET before any real source file was reached.
        if ignored.contains(&path) {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            let child_count = visible_child_count(&path, ignored);
            out.push(TreeEntry { label: format!("{name}/"), depth, is_dir: true, child_count: Some(child_count), path: path.clone() });
            walk(&path, depth + 1, out, ignored);
        } else {
            out.push(TreeEntry { label: name, depth, is_dir: false, child_count: None, path });
        }
    }
}

/// How many of `dir`'s direct children would actually show up in the tree —
/// the same `SKIP_DIRS`/gitignore filtering `walk` applies, so the count
/// next to a directory's name matches what expanding it actually reveals
/// instead of a raw `read_dir` tally that includes noise and ignored
/// entries the tree never displays.
fn visible_child_count(dir: &Path, ignored: &HashSet<PathBuf>) -> u32 {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    rd.filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            !SKIP_DIRS.contains(&name.as_str()) && !ignored.contains(&e.path())
        })
        .count() as u32
}

/// Files larger than this are shown as a placeholder instead of being read
/// in full — same reasoning and same limit as `gitreview.rs`'s
/// `MAX_SYNTHETIC_DIFF_SIZE`: this guards a poll-tick-driven read against a
/// pathologically large file, not just a one-off open.
const MAX_READABLE_SIZE: u64 = 10 * 1024 * 1024; // 10 MiB

/// Reads `path` for display in the tree/content pane.
///
/// Uses `symlink_metadata`, not `metadata`/`fs::read_to_string` directly —
/// both follow symlinks, so a symlink pointing outside the repo (planted by
/// whatever produced the tree being reviewed, same threat `gitreview.rs`'s
/// `synthetic_new_file_diff` defends against for untracked files) would
/// otherwise silently display an arbitrary external file's content. Shown
/// as its target path text instead, matching how git itself displays a
/// symlink. Anything that isn't a symlink or a regular file (a FIFO,
/// socket, device node, ...) is skipped outright rather than read — opening
/// one can block indefinitely, and this runs on every poll tick as well as
/// on open, so a hang here would freeze the whole single-threaded event
/// loop with no way to recover short of an external kill.
pub fn read_file(path: &Path) -> Vec<String> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) => return vec![format!("(couldn't read {}: {e})", path.display())],
    };
    if meta.is_symlink() {
        let target = fs::read_link(path).map(|p| p.display().to_string()).unwrap_or_default();
        return vec![format!("(symlink -> {target})")];
    }
    if !meta.is_file() {
        return vec![format!("({} is not a regular file)", path.display())];
    }
    if meta.len() > MAX_READABLE_SIZE {
        return vec!["(file too large to display)".to_string()];
    }
    match fs::read_to_string(path) {
        Ok(content) => content.lines().take(4000).map(|l| l.to_string()).collect(),
        Err(e) => vec![format!("(couldn't read {}: {e})", path.display())],
    }
}

fn is_source_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).map(|e| SOURCE_EXTS.contains(&e)).unwrap_or(false)
}

/// Reads `path` for `scan_symbols`/`reference_count`, applying the same
/// symlink and size guards as `read_file` — these two run over every
/// matched source file in the tree on essentially every symbol-related
/// action (typing in the symbol jump overlay, opening a file, ...), so a
/// symlink extension-matched as a "source file" (an `evil.rs` pointing at
/// `~/.ssh/id_rsa`, say) would otherwise have its target's content quietly
/// folded into symbol/reference results the same way `read_file` used to
/// leak one into the content pane. Unlike `read_file`, a rejected path is
/// just skipped rather than shown as a placeholder line — this feeds a
/// bulk background scan, not a single "here's what you opened" display.
fn read_source_file(path: &Path) -> Option<String> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_READABLE_SIZE {
        return None;
    }
    fs::read_to_string(path).ok()
}

fn collect_source_files(dir: &Path, out: &mut Vec<PathBuf>, ignored: &HashSet<PathBuf>) {
    if out.len() >= SCAN_FILE_BUDGET {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.filter_map(|e| e.ok()) {
        if out.len() >= SCAN_FILE_BUDGET {
            return;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        if ignored.contains(&path) {
            continue;
        }
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            collect_source_files(&path, out, ignored);
        } else if is_source_file(&path) {
            out.push(path);
        }
    }
}

pub fn scan_symbols(root: &Path) -> Vec<SymbolResult> {
    let mut files = Vec::new();
    collect_source_files(root, &mut files, &crate::gitreview::ignored_paths(root));

    let mut out = Vec::new();
    'files: for path in &files {
        let Some(content) = read_source_file(path) else { continue };
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
    let words: Vec<&str> = line_text.split(|c: char| !c.is_alphanumeric() && c != '_').filter(|w| !w.is_empty()).collect();
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
    collect_source_files(root, &mut files, &crate::gitreview::ignored_paths(root));
    let mut count = 0u32;
    for path in files {
        let Some(content) = read_source_file(&path) else { continue };
        for line in content.lines() {
            count += line.split(|c: char| !c.is_alphanumeric() && c != '_').filter(|w| *w == name).count() as u32;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hoot-fsnav-test-{label}-{}-{:?}",
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
        let symbols =
            vec![SymbolResult { name: "parse".to_string(), path: PathBuf::from("a.rs"), line: 1, preview: "fn parse()".to_string() }];
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
    fn scan_symbols_does_not_follow_a_symlink_out_of_the_tree() {
        // Regression: scan_symbols/reference_count used to read every
        // extension-matched path with fs::read_to_string directly, which
        // follows symlinks — an `evil.rs` symlinked to an arbitrary file
        // outside the tree (e.g. ~/.ssh/id_rsa) would have its target
        // content quietly scanned into symbol results. Same class of bug
        // read_file already had, fixed the same way: skip anything that
        // isn't a real regular file.
        let dir = scratch_dir("scan-symlink");
        // In a genuinely separate directory, not just another file inside
        // `dir` — otherwise it'd get scanned as its own legitimate source
        // file regardless of the symlink, and the test would pass without
        // actually exercising the symlink guard at all.
        let outside_dir = scratch_dir("scan-symlink-outside");
        let outside = outside_dir.join("outside.rs");
        fs::write(&outside, "fn top_secret_function() {}\n").unwrap();
        let link = dir.join("evil.rs");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let symbols = scan_symbols(&dir);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(!names.contains(&"top_secret_function"), "symlink target leaked into symbol scan: {names:?}");

        let count = reference_count(&dir, "top_secret_function");
        assert_eq!(count, 0, "symlink target leaked into reference count");

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside_dir);
    }

    #[test]
    fn scan_symbols_skips_an_oversized_source_file() {
        let dir = scratch_dir("scan-oversized");
        let mut padded = "fn should_not_appear() {}\n".to_string();
        padded.push_str(&"x".repeat((MAX_READABLE_SIZE + 1) as usize - padded.len()));
        fs::write(dir.join("big.rs"), &padded).unwrap();
        // A well-formed small file alongside it proves the scan still runs
        // and only the oversized one gets skipped, not that scanning broke
        // entirely.
        fs::write(dir.join("small.rs"), "fn kept() {}\n").unwrap();

        let symbols = scan_symbols(&dir);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"kept"), "names = {names:?}");
        assert!(!names.contains(&"should_not_appear"), "oversized file should have been skipped entirely: {names:?}");

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
        assert!(!labels.contains(&".git/"), "labels = {labels:?}");
        assert!(!labels.contains(&"target/"), "labels = {labels:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_tree_does_not_hide_every_dotfile() {
        // Regression: dotfiles used to be excluded wholesale, hiding real,
        // important content like .github/workflows/*.yml — only `.git`
        // itself and the explicit noise list should ever be skipped.
        let dir = scratch_dir("tree-dotfiles");
        fs::create_dir_all(dir.join(".github/workflows")).unwrap();
        fs::write(dir.join(".gitignore"), "/target\n").unwrap();
        fs::write(dir.join(".github/workflows/release.yml"), "name: release\n").unwrap();

        let tree = build_tree(&dir);
        let labels: Vec<&str> = tree.iter().map(|e| e.label.as_str()).collect();
        assert!(labels.contains(&".gitignore"), "labels = {labels:?}");
        assert!(labels.contains(&".github/"), "labels = {labels:?}");
        assert!(labels.contains(&"release.yml"), "labels = {labels:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_tree_excludes_gitignored_directories() {
        // Regression: build_tree used to walk the raw filesystem with no
        // idea what Git ignores, so a gitignored directory full of logs or
        // scratch files (a local tool's working state, a build cache not
        // yet excluded from SKIP_DIRS by name, ...) showed up in the
        // Review sidebar right alongside real tracked/untracked source —
        // and could exhaust TREE_BUDGET before any real file was reached.
        let dir = scratch_dir("tree-gitignore");
        let status = std::process::Command::new("git").args(["init", "-q"]).current_dir(&dir).status().unwrap();
        assert!(status.success());
        fs::write(dir.join(".gitignore"), "/ignored-dir/\n").unwrap();
        fs::create_dir_all(dir.join("ignored-dir")).unwrap();
        fs::write(dir.join("ignored-dir/noise.log"), "noise\n").unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();

        let tree = build_tree(&dir);
        let labels: Vec<&str> = tree.iter().map(|e| e.label.as_str()).collect();
        assert!(!labels.contains(&"ignored-dir/"), "labels = {labels:?}");
        assert!(labels.contains(&"src/"), "labels = {labels:?}");
        assert!(labels.contains(&"main.rs"), "labels = {labels:?}");
        assert!(labels.contains(&".gitignore"), "labels = {labels:?}");

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

    #[test]
    fn read_file_returns_a_regular_files_content() {
        let dir = scratch_dir("read-regular");
        let path = dir.join("f.txt");
        fs::write(&path, "line1\nline2\n").unwrap();
        assert_eq!(read_file(&path), vec!["line1".to_string(), "line2".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_file_shows_a_symlinks_target_path_not_the_targets_content() {
        // Regression: read_file used to call fs::read_to_string directly,
        // which follows symlinks — a symlink pointing outside the reviewed
        // directory (planted deliberately, or just an absolute symlink left
        // by some tool) would silently display an arbitrary external
        // file's content in the tree/content pane. Mirrors the same defense
        // gitreview.rs's synthetic_new_file_diff already has for untracked
        // symlinks in a real diff.
        let dir = scratch_dir("read-symlink");
        let outside = dir.join("outside.txt");
        fs::write(&outside, "TOP_SECRET_SHOULD_NEVER_APPEAR").unwrap();
        let link = dir.join("link.txt");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let lines = read_file(&link);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains(&outside.display().to_string()), "{lines:?}");
        assert!(!lines.iter().any(|l| l.contains("TOP_SECRET")), "target content leaked: {lines:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_file_skips_a_fifo_instead_of_blocking() {
        // Regression: with no file-type guard at all, opening a FIFO
        // blocked the read forever — and since fsnav::read_file runs on
        // every poll tick as well as on open, that froze the entire
        // single-threaded event loop with no way to recover short of an
        // external kill. A FIFO with no reader ever attached (as here)
        // would hang this test indefinitely on the old code instead of
        // just failing it.
        let dir = scratch_dir("read-fifo");
        let fifo = dir.join("pipe");
        let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo failed");

        let lines = read_file(&fifo);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("not a regular file"), "{lines:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_file_caps_an_oversized_file_instead_of_reading_it_in_full() {
        let dir = scratch_dir("read-oversized");
        let path = dir.join("big.txt");
        {
            let f = fs::File::create(&path).unwrap();
            f.set_len(MAX_READABLE_SIZE + 1).unwrap();
        }
        let lines = read_file(&path);
        assert_eq!(lines, vec!["(file too large to display)".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }
}
