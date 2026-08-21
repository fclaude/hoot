//! Builds the throwaway repository `--demo` runs against.
//!
//! A real git repository in a temp directory, with a baseline commit and a
//! working tree edited on top of it — not fabricated in-memory review data.
//! That difference is the whole point: an earlier version handed the UI a
//! hand-written `Project` full of invented hunks while Review's content
//! pane went on reading whatever was actually on disk, so the two panes
//! disagreed. Curate listed hunks that appeared nowhere in Review, and
//! Review showed every file as one solid block of added lines, which is the
//! one thing a diff viewer should never be demonstrating. Running the
//! ordinary `git diff` path against a real (if disposable) repo makes the
//! two agree by construction, and means every part of the loop — hunk
//! selection, staging, an actual commit — genuinely works while you're
//! looking around.
//!
//! Nothing here escapes the temp directory: `TempDir` deletes it on drop,
//! the repo's identity is set locally rather than touching `~/.gitconfig`,
//! and the commit runs with the ambient environment's hooks, templates and
//! signing configuration disabled so somebody's global setup can't fail the
//! demo (or, worse, run their pre-commit hook against it).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// A file in the demo repo: the committed baseline, and what the working
/// tree holds now. The difference between the two is the review.
struct DemoFile {
    path: &'static str,
    baseline: &'static str,
    working: &'static str,
}

/// A fictional `search-index` crate, mid-change: a postings list growing
/// position tracking and a faster intersect, a query parser learning to
/// fail, and callers following along. Shaped to give the UI something worth
/// showing — several files, more than one hunk in most of them, an
/// untracked file that plain `git diff` would never surface, and changes
/// far enough apart that Review's `Focused` view has something to collapse.
const FILES: &[DemoFile] = &[
    DemoFile {
        path: "src/main.rs",
        baseline: "mod index;\nmod query;\n\nuse index::Index;\nuse query::QueryParser;\n\nfn main() {\n    let index = Index::load(\"fixtures/wiki\").expect(\"index\");\n    let query = QueryParser::new(\"terminal user interface\").parse();\n    let results = index.search(&query);\n    println!(\"{} hits\", results.len());\n}\n",
        working: "mod index;\nmod query;\n\nuse index::Index;\nuse query::QueryParser;\n\nfn main() {\n    let index = Index::load(\"fixtures/wiki\").expect(\"index\");\n    let query = QueryParser::new(\"terminal user interface\").parse().expect(\"query\");\n    let results = index.search(&query);\n    for doc in &results {\n        println!(\"doc {doc}\");\n    }\n    println!(\"{} hits\", results.len());\n}\n",
    },
    DemoFile {
        path: "src/lib.rs",
        baseline: "pub mod index;\npub mod query;\n\npub use index::Index;\npub use query::QueryParser;\n",
        working: "pub mod index;\npub mod query;\n\npub use index::Index;\npub use query::parser::ParseError;\npub use query::QueryParser;\n",
    },
    DemoFile {
        path: "src/index/mod.rs",
        baseline: "pub mod postings;\n\nuse crate::query::Query;\n\npub struct Index {\n    pub postings: postings::Postings,\n}\n\nimpl Index {\n    pub fn load(path: &str) -> Option<Index> {\n        let _ = path;\n        None\n    }\n\n    pub fn search(&self, query: &Query) -> Vec<u32> {\n        self.postings.iter().filter(|doc| self.matches(*doc, query)).collect()\n    }\n\n    fn matches(&self, doc: u32, query: &Query) -> bool {\n        let _ = (doc, query);\n        true\n    }\n}\n",
        working: "pub mod postings;\n\nuse crate::query::Query;\n\npub struct Index {\n    pub postings: postings::Postings,\n}\n\nimpl Index {\n    pub fn load(path: &str) -> Option<Index> {\n        let _ = path;\n        None\n    }\n\n    pub fn search(&self, query: &Query) -> Vec<u32> {\n        if self.postings.is_empty() {\n            return Vec::new();\n        }\n        self.postings.iter().filter(|doc| self.matches(*doc, query)).collect()\n    }\n\n    fn matches(&self, doc: u32, query: &Query) -> bool {\n        let _ = (doc, query);\n        true\n    }\n}\n",
    },
    DemoFile {
        path: "src/index/postings.rs",
        baseline: "/// A postings list: the document ids a term appears in, plus the\n/// per-document frequencies used for ranking.\npub struct Postings {\n    docs: Vec<u32>,\n    freq: Vec<u32>,\n}\n\nimpl Postings {\n    pub fn new(docs: Vec<u32>, freq: Vec<u32>) -> Postings {\n        Postings { docs, freq }\n    }\n\n    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {\n        self.docs.iter().copied()\n    }\n\n    pub fn len(&self) -> usize {\n        self.docs.len()\n    }\n\n    pub fn doc_frequency(&self, doc: u32) -> u32 {\n        match self.docs.iter().position(|d| *d == doc) {\n            Some(i) => self.freq[i],\n            None => 0,\n        }\n    }\n\n    pub fn intersect(&self, other: &Postings) -> Vec<u32> {\n        let mut out = Vec::new();\n        for doc in self.iter() {\n            if other.docs.contains(&doc) {\n                out.push(doc);\n            }\n        }\n        out\n    }\n}\n",
        working: "/// A postings list: the document ids a term appears in, plus the\n/// per-document frequencies used for ranking.\npub struct Postings {\n    docs: Vec<u32>,\n    freq: Vec<u32>,\n    positions: Vec<Vec<u32>>,\n}\n\nimpl Postings {\n    pub fn new(docs: Vec<u32>, freq: Vec<u32>) -> Postings {\n        let positions = vec![Vec::new(); docs.len()];\n        Postings { docs, freq, positions }\n    }\n\n    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {\n        self.docs.iter().copied()\n    }\n\n    pub fn len(&self) -> usize {\n        self.docs.len()\n    }\n\n    pub fn is_empty(&self) -> bool {\n        self.docs.is_empty()\n    }\n\n    pub fn doc_frequency(&self, doc: u32) -> u32 {\n        self.docs.iter().position(|d| *d == doc).map(|i| self.freq[i]).unwrap_or(0)\n    }\n\n    pub fn positions_for(&self, doc: u32) -> &[u32] {\n        match self.docs.iter().position(|d| *d == doc) {\n            Some(i) => &self.positions[i],\n            None => &[],\n        }\n    }\n\n    /// Both sides are sorted, so this walks them in lockstep instead of\n    /// scanning `other` once per document \u{2014} O(n + m) rather than O(n * m).\n    pub fn intersect(&self, other: &Postings) -> Vec<u32> {\n        let mut out = Vec::new();\n        let (mut i, mut j) = (0, 0);\n        while i < self.docs.len() && j < other.docs.len() {\n            match self.docs[i].cmp(&other.docs[j]) {\n                std::cmp::Ordering::Equal => {\n                    out.push(self.docs[i]);\n                    i += 1;\n                    j += 1;\n                }\n                std::cmp::Ordering::Less => i += 1,\n                std::cmp::Ordering::Greater => j += 1,\n            }\n        }\n        out\n    }\n}\n",
    },
    DemoFile {
        path: "src/query/mod.rs",
        baseline: "pub mod parser;\n\npub use parser::QueryParser;\n\npub struct Query {\n    pub terms: Vec<String>,\n}\n",
        working: "pub mod parser;\n\npub use parser::QueryParser;\n\npub struct Query {\n    pub terms: Vec<String>,\n}\n",
    },
    DemoFile {
        path: "src/query/parser.rs",
        baseline: "use super::Query;\n\npub struct QueryParser<'a> {\n    input: &'a str,\n}\n\nimpl<'a> QueryParser<'a> {\n    pub fn new(input: &'a str) -> QueryParser<'a> {\n        QueryParser { input }\n    }\n\n    pub fn parse(&mut self) -> Query {\n        let terms = self.input.split_whitespace().map(|t| t.to_string()).collect();\n        Query { terms }\n    }\n}\n",
        working: "use super::Query;\n\n#[derive(Debug)]\npub enum ParseError {\n    EmptyQuery,\n    UnbalancedQuote,\n}\n\npub struct QueryParser<'a> {\n    input: &'a str,\n}\n\nimpl<'a> QueryParser<'a> {\n    pub fn new(input: &'a str) -> QueryParser<'a> {\n        QueryParser { input }\n    }\n\n    pub fn parse(&mut self) -> Result<Query, ParseError> {\n        if self.input.trim().is_empty() {\n            return Err(ParseError::EmptyQuery);\n        }\n        if self.input.matches('\"').count() % 2 != 0 {\n            return Err(ParseError::UnbalancedQuote);\n        }\n        let terms = self.input.split_whitespace().map(|t| t.to_string()).collect();\n        Ok(Query { terms })\n    }\n}\n",
    },
    DemoFile {
        path: "Cargo.toml",
        baseline: "[package]\nname = \"search-index\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        working: "[package]\nname = \"search-index\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    },
];

/// Written only into the working tree, never committed — so it shows up as
/// an untracked file. Worth having in the demo because plain `git diff`
/// never reports one, and picking it up anyway is a thing hoot does on
/// purpose (see `gitreview::untracked_files_diff`).
const UNTRACKED: (&str, &str) = (
    "tests/integration.rs",
    "use search_index::QueryParser;\n\n#[test]\nfn rejects_an_empty_query() {\n    assert!(QueryParser::new(\"   \").parse().is_err());\n}\n\n#[test]\nfn rejects_an_unbalanced_quote() {\n    assert!(QueryParser::new(\"\\\"exact phrase\").parse().is_err());\n}\n",
);

/// The repository's name on disk, and so the project name hoot shows in
/// its status line. A named subdirectory inside the temp directory rather
/// than the temp directory itself: `TempDir`'s own name carries a random
/// suffix, which would otherwise turn up on screen as a project called
/// something like `hoot-demo-VZhR5A`.
const REPO_NAME: &str = "search-index";

/// Creates the demo repository, returning its `TempDir` guard and the path
/// to the repo inside it. The caller has to keep the guard alive for as
/// long as the repo is in use — dropping it deletes the whole thing, which
/// is exactly how the demo cleans up after itself.
///
/// `TempDir`, not a predictable `$TMPDIR/hoot-demo-<pid>` path built by
/// hand: a guessable name in a world-writable temp directory is plantable,
/// and the writes below would follow a symlink somebody left in its place.
/// `TempDir` picks an unpredictable name and creates it atomically, so
/// there is nothing to plant in advance.
pub fn create_repo() -> io::Result<(TempDir, PathBuf)> {
    let dir = tempfile::Builder::new().prefix("hoot-demo-").tempdir()?;
    let repo = dir.path().join(REPO_NAME);
    std::fs::create_dir(&repo)?;
    let root = repo.as_path();

    for file in FILES {
        write(root, file.path, file.baseline)?;
    }

    git(root, &["init", "-q"])?;
    // Set locally, never globally: the demo must not touch (or depend on)
    // whatever identity the person running it has configured, and `git
    // commit` refuses to run without one.
    git(root, &["config", "user.name", "hoot demo"])?;
    git(root, &["config", "user.email", "demo@example.invalid"])?;
    git(root, &["add", "-A"])?;
    git(root, &["commit", "-q", "-m", "Initial search-index skeleton"])?;

    // The edits under review. Written after the commit so `git diff HEAD`
    // reports them as the working-tree changes they are.
    for file in FILES {
        if file.working != file.baseline {
            write(root, file.path, file.working)?;
        }
    }
    write(root, UNTRACKED.0, UNTRACKED.1)?;

    Ok((dir, repo))
}

fn write(root: &Path, rel: &str, content: &str) -> io::Result<()> {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// Runs one git command in the demo repo with the ambient environment's
/// hooks, commit templates and signing turned off.
///
/// Not paranoia about the commands themselves — it's that this repo is
/// created without being asked for. Somebody with `commit.gpgsign=true`
/// globally would otherwise get a signing prompt (or a hard failure) from
/// pressing `--demo`, and somebody with a global `core.hooksPath` would
/// have their own pre-commit hook run against a directory they have never
/// seen. `init.templateDir=` likewise stops a global template dropping
/// hooks into the new repo.
fn git(root: &Path, args: &[&str]) -> io::Result<()> {
    let out = Command::new("git")
        .args(["-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", "-c", "init.templateDir="])
        .args(args)
        .current_dir(root)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_a_real_repo_whose_diff_the_normal_path_can_read() {
        // The whole point of the rewrite: `--demo` goes through exactly the
        // same `gitreview::load` a real repo does, so Review and Curate
        // can't disagree about what changed.
        let (_dir, repo) = create_repo().expect("demo repo");
        assert!(crate::gitreview::is_git_repo(repo.as_path()));
        assert_eq!(repo.file_name().unwrap(), REPO_NAME, "the project name on screen comes from this");

        let review = crate::gitreview::load(repo.as_path());
        let paths: Vec<&str> = review.project.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"src/query/parser.rs"), "{paths:?}");
        assert!(paths.contains(&"src/index/postings.rs"), "{paths:?}");
        assert!(paths.contains(&"tests/integration.rs"), "the untracked file should show up too: {paths:?}");
        assert!(!paths.contains(&"Cargo.toml"), "an unchanged file has nothing to review: {paths:?}");
    }

    #[test]
    fn every_changed_file_has_real_hunks_to_select() {
        // The bug this replaces: Curate offered hunks that existed nowhere
        // in Review. Now both read the same parsed diff, so every file the
        // review lists has real, selectable content behind it.
        let (_dir, repo) = create_repo().expect("demo repo");
        let review = crate::gitreview::load(repo.as_path());
        assert!(!review.project.files.is_empty());
        for f in &review.project.files {
            assert!(!f.hunks.is_empty(), "{} has no hunks", f.path);
            assert_eq!(f.unsupported, None, "{} should be curatable", f.path);
        }
        for cf in &review.curation_files {
            assert!(cf.total() > 0, "{} offers nothing to select", cf.path);
        }
    }

    #[test]
    fn a_changed_file_shows_removed_lines_not_just_additions() {
        // Under the old demo, Review rendered every file as one solid block
        // of added lines, because it was diffing on-disk content against
        // nothing. A real baseline commit means real removals to look at.
        let (_dir, repo) = create_repo().expect("demo repo");
        let review = crate::gitreview::load(repo.as_path());
        let parser = review.project.files.iter().find(|f| f.path == "src/query/parser.rs").expect("parser.rs");
        let removed = parser.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == crate::data::DiffLineKind::Removed).count();
        assert!(removed > 0, "expected real removed lines in the parser change");
    }

    #[test]
    fn the_baseline_commit_is_the_only_history() {
        let (_dir, repo) = create_repo().expect("demo repo");
        let out = Command::new("git").args(["log", "--oneline"]).current_dir(repo.as_path()).output().expect("git log");
        assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 1);
    }

    #[test]
    fn the_index_starts_clean_so_curate_can_commit() {
        // `gitcommit::commit` refuses outright if anything is already
        // staged. The demo would be unable to demonstrate its own commit
        // flow if `git add` above left the untracked file behind in the
        // index.
        let (_dir, repo) = create_repo().expect("demo repo");
        let out =
            Command::new("git").args(["diff", "--cached", "--quiet"]).current_dir(repo.as_path()).output().expect("git diff --cached");
        assert!(out.status.success(), "the demo repo's index should start clean");
    }
}
