use std::path::PathBuf;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::agent_client::{AgentBackend, AgentEvent, AgentSession, ToolProfile};
use crate::data::{self, AgentLine, AgentLineKind, CurationFile, HoverInfo, Hunk, Note, Project, SymbolResult, TreeEntry};
use crate::fsnav;
use crate::keymap::{Action, Keymap};
use crate::{opencode_client, pi_client};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Merged file tree + change review: browse the repo and see each
    /// file's diff (if it has uncommitted changes) or plain source
    /// (otherwise) in the same screen — see `ContentView`.
    Review,
    Agent,
    Curation,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overlay {
    None,
    SymbolJump,
    FileFinder,
    NoteInput,
    QuitConfirm,
    AgentTrustConfirm,
    DiscardConfirm,
}

/// Which pane arrow keys currently move.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NavFocus {
    Tree,
    Content,
}

/// How the right-hand content pane shows the currently open file. Both
/// variants show the *whole* file, not just isolated snippets — real
/// diffs load with a huge context window (`gitreview::FULL_CONTEXT`), so
/// there's no separate "plain source" state to fall back to: a file with
/// no changes just renders as all-context, which looks exactly like plain
/// source anyway. `Context` is the default; `Focused` collapses long
/// unchanged stretches down to a few lines around each change.
/// `ReviewToggleView` switches between them by hand, but only has any
/// effect while the open file actually has a diff.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ContentView {
    Context,
    Focused,
}

/// Which slice of the working tree Review is currently looking at.
///
/// `All` is the whole uncommitted changeset — everything `git diff HEAD`
/// plus untracked reports, which is what Review has always shown. `Turn`
/// narrows that to the files the current (or most recent) agent turn
/// actually changed, measured against the changeset snapshotted the
/// instant that turn was spawned.
///
/// The distinction only exists for *presentation*. Both scopes read the
/// same HEAD-anchored diff, and Curate keeps seeing the whole changeset
/// regardless — a scope that also changed which patch got staged would
/// mean two different diff vocabularies in one app, and the one Curate
/// replays through `git apply --cached` has to stay anchored to HEAD.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReviewScope {
    All,
    Turn,
}

/// What an in-flight agent turn is for. `Chat` (the normal Agent-pane
/// conversation, in either read-only or edit mode) streams into the
/// visible transcript as usual. `CommitMessage` is a silent background
/// turn — its result goes straight to `commit_message`, never the
/// transcript, and completing it opens `$EDITOR` for a last pass.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TurnPurpose {
    Chat,
    CommitMessage,
}

pub struct App {
    pub mode: Mode,
    pub overlay: Overlay,
    pub should_quit: bool,
    pub keymap: Keymap,
    /// Last time `sync_from_disk` actually re-read the filesystem — gates
    /// the poll to `FS_POLL_INTERVAL` so an idle app isn't re-running `git
    /// diff` and re-reading the tree ten times a second.
    last_fs_poll: Instant,

    // REVIEW
    pub project: Project,
    /// Whether this session is running against `--demo`'s throwaway repo
    /// rather than a repository the user chose.
    ///
    /// An explicit flag from `main.rs`, not something inferred from the
    /// review data. It used to be `review_is_real`, meaning "gitreview
    /// handed back real data rather than mock data" — but the demo repo is
    /// a real git repository now (see `demo.rs`), so there is nothing left
    /// in the diff itself to tell the two apart, and nothing should be:
    /// the demo's whole value is that it exercises the same code path.
    pub demo: bool,
    pub split_diff: bool,
    pub tree: Vec<TreeEntry>,
    pub tree_index: usize,
    pub nav_focus: NavFocus,
    pub content_view: ContentView,
    pub nav_file: PathBuf,
    pub source: Vec<String>,
    /// `nav_file`'s diff, loaded with a huge context window
    /// (`gitreview::file_diff_in_context`) — empty if the file has no
    /// changes. Kept alongside `source` rather than recomputed from
    /// `self.project.files` because it deliberately uses a *different*,
    /// wider context than the small hunks Curation relies on for partial
    /// commits; the two shouldn't be conflated into one field.
    pub diff_context: Vec<Hunk>,
    pub nav_line: usize,
    pub nav_scroll_x: u16,
    pub hover: Option<HoverInfo>,
    pub show_hover: bool,

    // SYMBOL JUMP (overlay)
    pub symbols: Vec<SymbolResult>,
    pub symbol_filter: String,
    pub symbol_index: usize,

    // FILE FINDER (overlay)
    pub file_finder_filter: String,
    pub file_finder_index: usize,

    // NOTES (real free-text review notes, left while reviewing)
    pub notes: Vec<Note>,
    pub(crate) note_target: Option<NoteTarget>,
    pub note_input: String,
    pub note_cursor: usize,
    /// The assembled iterate prompt, shown in `$EDITOR` for a last-pass
    /// edit (via `EditorTarget::IteratePrompt`) before it's ever sent.
    pub iterate_draft: String,
    /// Result of the last Review action with something to say for itself —
    /// `y` (copy prompt), or a scope toggle that couldn't do what was
    /// asked. Shown under the tree footer until the next one overwrites
    /// it.
    pub review_status: Option<Result<String, String>>,
    /// Set when the last diff read failed outright, so Review's footer can
    /// say the changeset is unknown instead of letting an unreadable repo
    /// render identically to a clean one. See `gitreview::ReviewData`.
    pub review_error: Option<String>,
    /// Whether `tree`/`symbols` are complete. Both scans stop at a budget
    /// (see `fsnav`), and a capped list looks exactly like a small repo
    /// unless something says otherwise — which is what these drive.
    pub tree_truncated: bool,
    pub symbols_truncated: bool,
    /// Whether Review is showing the whole uncommitted changeset or just
    /// what the last agent turn wrote — see `ReviewScope`.
    pub review_scope: ReviewScope,
    /// The changeset exactly as it stood the instant the current (or most
    /// recent) chat turn was spawned, re-read from git at that moment
    /// rather than taken from the last poll — a poll tick of staleness
    /// here would attribute someone else's edit to the turn.
    ///
    /// This is what makes `ReviewScope::Turn` mean anything: "changed this
    /// turn" is "this file's change identity differs from its identity in
    /// here". `None` until the first turn runs, which is why `Turn` scope
    /// is unreachable before then.
    ///
    /// A snapshot of the *changeset*, not of the tree: no blobs are
    /// written, no index is built, and nothing is copied to disk. It costs
    /// what a `git diff` costs, once per turn, and it answers the only
    /// question the scope actually asks — which paths moved.
    turn_baseline: Option<Vec<data::FileEntry>>,
    /// Paths in `project.files` that differ from `turn_baseline`. Cached
    /// rather than recomputed per tree row: `draw_tree` asks about every
    /// visible row, every frame.
    turn_scope: Vec<String>,
    /// One flag per `tree` row: whether Review currently shows it. Always
    /// all-true under `ReviewScope::All`; under `Turn` it's the files this
    /// turn changed plus the directories leading down to them. Kept
    /// parallel to `tree` rather than filtering `tree` itself so
    /// `tree_index` keeps meaning what it always did — an index into the
    /// full scan — and every existing "find this file's row" path stays
    /// correct whichever scope is active.
    tree_visible: Vec<bool>,

    // AGENT
    pub target_dir: PathBuf,
    pub agent_backend: AgentBackend,
    /// `pi`'s session file for this run — not just a bare session id. `pi`
    /// scopes `--session-id` lookups by (cwd, id), so a bare id would
    /// silently lose memory the moment a turn ran from a different cwd
    /// than the one that created it. Every turn here runs in `target_dir`,
    /// so that's moot today, but passing this exact file via `--session`
    /// instead sidesteps the cwd-scoping question entirely — pi resumes it
    /// on every call, regardless of cwd. See `pi_client::spawn`.
    ///
    /// A `NamedTempFile`, not a bare `PathBuf`: created with a random
    /// suffix and `O_EXCL` (via the `tempfile` crate) rather than a
    /// PID-based name, so it can't be pre-planted as a symlink by another
    /// local user before hoot creates it, and it's 0600 (owner-only) from
    /// the moment it exists rather than whatever the umask would've given
    /// a plain `fs::write`. Kept alive in `App` so it's deleted on drop —
    /// i.e. cleaned up when hoot exits — instead of accumulating in
    /// `/tmp` (or `$TMPDIR`) forever. Only used when `agent_backend` is
    /// `Pi`, and created lazily on the first pi turn (see
    /// `ensure_session_file`) rather than unconditionally in `App::new` —
    /// a full or read-only `$TMPDIR` would otherwise fail every launch,
    /// including for opencode users who never touch this field at all.
    pub session_file: Option<tempfile::NamedTempFile>,
    /// opencode's equivalent of `session_file`, except it can't be decided
    /// upfront — opencode assigns this itself and only hands it back after
    /// the first turn runs (`AgentEvent::Session`), so it starts `None` and
    /// gets threaded into every call after. Only used when `agent_backend`
    /// is `OpenCode`.
    pub opencode_session_id: Option<String>,
    pub transcript: Vec<AgentLine>,
    pub agent_input: String,
    /// Char index into `agent_input` (not a byte offset — see `char_boundary`).
    pub agent_cursor: usize,
    /// Lines scrolled up from the bottom of the transcript; 0 = pinned to
    /// the latest content (and stays pinned as new lines arrive).
    pub agent_scroll: usize,
    pub agent_model_live: Option<String>,
    pub agent_running: bool,
    agent_session: Option<AgentSession>,
    agent_purpose: TurnPurpose,
    /// Set by `cancel_agent_turn` and read (and cleared) by `finish_turn` —
    /// there was previously no way to distinguish a turn that ran to
    /// completion from one the user killed mid-flight, so `finish_turn`
    /// always reported the same "N files changed" summary either way.
    agent_cancelled: bool,
    /// Whether the one-time "Agent runs with broad, unsandboxed trust"
    /// prompt has been shown and confirmed — see `trust.rs`. Defaults to
    /// already-acknowledged here in `App::new`, *not* to whatever
    /// `trust::is_acknowledged()` reports for the real `$HOME`: that would
    /// make every test's behavior depend on whether this machine has ever
    /// run hoot for real before, right down to whether a test spawns a
    /// genuine `pi`/`opencode` subprocess. `main.rs` is the only real
    /// production entry point (never exercised by `cargo test`), and it
    /// sets this explicitly from the real on-disk state right after
    /// construction — see `run()`.
    pub agent_trust_acknowledged: bool,
    /// A turn `spawn_turn` held back pending that confirmation, to run (or
    /// discard) once `on_key_agent_trust_confirm` answers it.
    pending_turn: Option<(String, PathBuf, ToolProfile, TurnPurpose)>,

    // CURATION
    pub curation_files: Vec<CurationFile>,
    pub curation_index: usize,
    /// Which of the current file's hunks is shown in the preview pane and
    /// targeted by `CurateToggleHunk` — independent of `hunk_selected`
    /// (which hunks are *selected* for commit), so you can look at and
    /// toggle any hunk, not just whichever one happens to be selected.
    pub curation_hunk_index: usize,
    pub commit_message: String,
    pub commit_message_status: Option<String>,
    /// Set to ask main.rs's event loop to suspend the TUI and open $EDITOR
    /// on the buffer named by the target — App itself doesn't own the
    /// Terminal, so it can only request the suspend/resume, not do it.
    pub open_editor_requested: Option<EditorTarget>,
    pub last_commit: Option<Result<String, String>>,
    /// The hunk `Overlay::DiscardConfirm` is asking about: which file, and
    /// which of its selectable units. Held here rather than re-derived
    /// when the answer comes back so the thing that gets thrown away is
    /// the thing the prompt named, even if the cursor moved.
    pub(crate) pending_discard: Option<PendingDiscard>,
    /// Outcome of the last discard, shown in Curate the same way
    /// `last_commit` is.
    pub last_discard: Option<Result<String, String>>,
}

/// A discard waiting on its confirmation.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct PendingDiscard {
    pub path: String,
    /// Index into the file's selectable units — a hunk, or unit 0 for a
    /// change that lives entirely in its header.
    pub unit: usize,
    /// What the confirmation prompt says is about to go, phrased once here
    /// so the prompt and the result line can't describe it differently.
    pub what: String,
}

/// What the open note overlay is about to attach a note to. Carries the
/// anchor as well as the line number, captured when the overlay opened so
/// the note records the file as the user was actually looking at it.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct NoteTarget {
    pub path: String,
    pub line: Option<usize>,
    pub anchor: Option<crate::data::NoteAnchor>,
}

/// Which buffer a requested `$EDITOR` session is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditorTarget {
    CommitMessage,
    IteratePrompt,
}

/// How often `sync_from_disk` re-reads the repo to pick up changes made
/// outside hoot (an agent run in another terminal, an editor, `git` on the
/// command line). A plain poll rather than an OS file-watcher: this app
/// already re-derives all of its state from disk on demand (git diff, fs
/// reads), so a cheap periodic re-check reuses that instead of adding a new
/// notification-based dependency and its own failure modes.
const FS_POLL_INTERVAL: Duration = Duration::from_millis(1000);

/// Lines per Page Up/Page Down in Review. app.rs doesn't know the actual
/// rendered pane height, so this is a fixed, editor-typical step rather
/// than a true screen-relative page.
const PAGE_SIZE: usize = 20;

fn clamped_move(current: usize, delta: i64, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let max = len - 1;
    if delta < 0 {
        current.saturating_sub(delta.unsigned_abs() as usize)
    } else {
        (current + delta as usize).min(max)
    }
}

/// Byte offset of the `n`th character in `s` (or `s.len()` if `n` is past
/// the end) — lets a char-indexed cursor drive byte-indexed String methods
/// like `insert`/`remove` without splitting a multi-byte character.
fn char_boundary(s: &str, n: usize) -> usize {
    s.char_indices().nth(n).map(|(i, _)| i).unwrap_or(s.len())
}

/// `file`'s whole-file-context diff, relative to `target_dir` — empty if
/// `file` isn't inside `target_dir` at all (e.g. it still points at
/// `target_dir` itself, the placeholder used when a repo has no files).
fn diff_context_for(target_dir: &std::path::Path, file: &std::path::Path) -> Vec<Hunk> {
    let Ok(rel) = file.strip_prefix(target_dir) else { return Vec::new() };
    if rel.as_os_str().is_empty() {
        return Vec::new();
    }
    crate::gitreview::file_diff_in_context(target_dir, rel)
}

/// Beyond this, a file is not re-read to re-place notes on it. Nothing
/// hoot displays gets near it (`fsnav`'s own display reader gives up an
/// order of magnitude sooner), so a file this size is one that grew into
/// something else entirely since the note was written.
const MAX_ANCHOR_FILE_SIZE: u64 = 32 * 1024 * 1024;

/// What re-anchoring has to work with for one file.
enum AnchorSource {
    /// The file's current lines.
    Lines(Vec<String>),
    /// The file is verifiably not there any more, so neither is any line
    /// in it — a definite answer, not a failure to get one.
    Gone,
    /// It couldn't be read. That says nothing about the notes on it, so
    /// they are left exactly as they are.
    Unreadable,
}

/// `path`'s lines for note re-anchoring. Split with `lines()` to match how
/// the anchor was captured, so a `\r\n` file and an `\n` file compare the
/// same either way.
fn read_lines_for_anchoring(path: &std::path::Path) -> AnchorSource {
    // `symlink_metadata`, not `metadata`: the same reason `fsnav` and
    // `gitreview` use it — following a symlink here would read a file from
    // outside the repo entirely, and then quote a line of it back to the
    // agent through whichever note re-anchored onto it.
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return AnchorSource::Gone,
        Err(_) => return AnchorSource::Unreadable,
    };
    if !meta.is_file() || meta.len() > MAX_ANCHOR_FILE_SIZE {
        return AnchorSource::Unreadable;
    }
    match std::fs::read_to_string(path) {
        Ok(text) => AnchorSource::Lines(text.lines().map(str::to_string).collect()),
        Err(_) => AnchorSource::Unreadable,
    }
}

/// How the confirmation names what is about to be thrown away. Kept
/// deliberately concrete — "hunk 2/5 of src/app.rs" rather than "this
/// change" — since it is the last thing shown before content that exists
/// nowhere else goes away.
///
/// A brand-new file is called out separately because it is the one case
/// where the phrase "discard" understates what happens: there is no
/// previous version to fall back to, so discarding it is deleting it.
fn describe_discard(file: &data::FileEntry, unit: usize, units: usize) -> String {
    if matches!(file.meta.change, data::ChangeKind::Added) {
        return format!("delete {} (a new file \u{2014} nothing to fall back to)", file.path);
    }
    if file.is_metadata_only() {
        let what = file.meta.describe().join(", ");
        let what = if what.is_empty() { "change".to_string() } else { what };
        return format!("undo {}'s {what}", file.path);
    }
    format!("throw away hunk {}/{units} of {}", unit + 1, file.path)
}

impl App {
    pub fn new(target_dir: PathBuf, keymap: Keymap, agent_backend: AgentBackend, demo: bool) -> Self {
        let review = crate::gitreview::load(&target_dir);
        let tree_scan = fsnav::build_tree(&target_dir);
        let symbol_scan = fsnav::scan_symbols(&target_dir);
        let (tree, tree_truncated) = (tree_scan.entries, tree_scan.truncated);
        let (symbols, symbols_truncated) = (symbol_scan.results, symbol_scan.truncated);
        // Opens on the first *changed* file when there is one, rather than
        // wherever alphabetical tree order happens to land — a file with
        // an actual diff to look at is a far more useful place to start
        // than an arbitrary unchanged file. Falls back to the first tree
        // entry (matching the old behavior) only when nothing's changed.
        let first_changed_file =
            review.project.files.first().map(|f| target_dir.join(&f.path)).filter(|p| tree.iter().any(|e| &e.path == p));
        let nav_file =
            first_changed_file.or_else(|| tree.iter().find(|e| !e.is_dir).map(|e| e.path.clone())).unwrap_or_else(|| target_dir.clone());
        let tree_index = tree.iter().position(|e| e.path == nav_file).unwrap_or(0);
        let source = fsnav::read_file(&nav_file);
        let diff_context = diff_context_for(&target_dir, &nav_file);

        let mut app = App {
            mode: Mode::Review,
            overlay: Overlay::None,
            should_quit: false,
            keymap,
            last_fs_poll: Instant::now(),

            project: review.project,
            review_error: review.error,
            tree_truncated,
            symbols_truncated,
            review_scope: ReviewScope::All,
            turn_baseline: None,
            turn_scope: Vec::new(),
            tree_visible: Vec::new(),
            demo,
            split_diff: false,

            tree,
            tree_index,
            nav_focus: NavFocus::Tree,
            content_view: ContentView::Context,
            nav_file,
            source,
            diff_context,
            nav_line: 0,
            nav_scroll_x: 0,
            hover: None,
            show_hover: true,

            symbols,
            symbol_filter: String::new(),
            symbol_index: 0,

            file_finder_filter: String::new(),
            file_finder_index: 0,

            notes: Vec::new(),
            note_target: None,
            note_input: String::new(),
            note_cursor: 0,
            iterate_draft: String::new(),
            review_status: None,

            target_dir,
            agent_backend,
            session_file: None,
            opencode_session_id: None,
            transcript: Vec::new(),
            agent_input: String::new(),
            agent_cursor: 0,
            agent_scroll: 0,
            agent_model_live: None,
            agent_running: false,
            agent_session: None,
            agent_purpose: TurnPurpose::Chat,
            agent_cancelled: false,
            agent_trust_acknowledged: true,
            pending_turn: None,

            curation_files: review.curation_files,
            curation_index: 0,
            curation_hunk_index: 0,
            // Always empty, demo or not: the message is something you write
            // (or ask the agent for), never something pre-filled on your
            // behalf and then committed because it looked ready.
            commit_message: String::new(),
            commit_message_status: None,
            open_editor_requested: None,
            last_commit: None,
            pending_discard: None,
            last_discard: None,
        };
        app.content_view = app.default_content_view();
        app.recompute_turn_scope();
        app.refresh_hover();
        app
    }

    /// Reloads the git-diff-backed review data (file list, hunks, curation
    /// selections) — used after a real commit changes what's outstanding.
    fn refresh_review(&mut self) {
        let review = crate::gitreview::load(&self.target_dir);
        self.project = review.project;
        self.curation_files = review.curation_files;
        self.curation_index = 0;
        self.curation_hunk_index = 0;
        self.commit_message.clear();
        self.commit_message_status = None;
        self.content_view = self.default_content_view();
        // A commit moves HEAD, and `turn_baseline` is a changeset measured
        // against the old one — every entry in it is now describing a
        // comparison that no longer exists. "What this turn wrote" stops
        // being computable at that point, so it's dropped rather than
        // reinterpreted, and Review falls back to showing everything.
        self.turn_baseline = None;
        self.review_scope = ReviewScope::All;
        self.recompute_turn_scope();
    }

    // -------------------------------------------------------------
    // REVIEW SCOPE (what this turn wrote, vs everything uncommitted)
    // -------------------------------------------------------------

    /// Re-derives `turn_scope` and `tree_visible` from the current
    /// changeset, baseline and scope. Cheap, and called from every place
    /// that can change any of those three — a stale scope would show the
    /// previous turn's files under this turn's heading, which is worse
    /// than not offering the filter at all.
    fn recompute_turn_scope(&mut self) {
        self.turn_scope = match &self.turn_baseline {
            None => Vec::new(),
            Some(baseline) => self
                .project
                .files
                .iter()
                .filter(|f| !baseline.iter().any(|b| b.path == f.path && b.same_change_as(f)))
                .map(|f| f.path.clone())
                .collect(),
        };
        self.recompute_tree_visibility();
    }

    /// Which tree rows the active scope shows. Under `Turn`, a file row is
    /// visible when this turn changed it, and a directory row when it
    /// leads to one that is — a bare list of basenames would leave two
    /// `mod.rs` entries indistinguishable.
    fn recompute_tree_visibility(&mut self) {
        if self.review_scope == ReviewScope::All {
            self.tree_visible = vec![true; self.tree.len()];
            return;
        }
        let mut visible = vec![false; self.tree.len()];
        let mut shown_files: Vec<PathBuf> = Vec::new();
        for (i, entry) in self.tree.iter().enumerate() {
            if entry.is_dir {
                continue;
            }
            let rel = crate::gitreview::display_path_of(entry.path.strip_prefix(&self.target_dir).unwrap_or(&entry.path));
            if self.turn_scope.contains(&rel) {
                visible[i] = true;
                shown_files.push(entry.path.clone());
            }
        }
        for (i, entry) in self.tree.iter().enumerate() {
            if entry.is_dir && shown_files.iter().any(|f| f.starts_with(&entry.path)) {
                visible[i] = true;
            }
        }
        self.tree_visible = visible;
    }

    /// Whether tree row `i` is shown under the active scope. Always true
    /// under `All`. `pub(crate)` so `ui::review` renders exactly the rows
    /// navigation moves through.
    pub(crate) fn tree_row_visible(&self, i: usize) -> bool {
        self.tree_visible.get(i).copied().unwrap_or(true)
    }

    /// Stands in for a real agent turn: snapshots the changeset the way
    /// `spawn_turn_now` does, runs `turn` (whatever it writes to the repo
    /// is that turn's output), then folds the result back in the way
    /// `finish_turn` does — without spawning a subprocess or waiting out a
    /// poll interval.
    ///
    /// Test-only, and deliberately the *same* two calls production makes
    /// rather than a hand-set baseline: a seam that skipped either end
    /// would let the scope pass a test while being wrong in the app.
    #[cfg(test)]
    pub(crate) fn run_fake_turn(&mut self, turn: impl FnOnce()) {
        self.turn_baseline = crate::gitreview::diff_files(&self.target_dir).ok();
        self.recompute_turn_scope();
        turn();
        self.sync_review_from_disk();
        self.sync_tree_from_disk();
    }

    /// How many files the last turn changed. `pub` for Review's footer,
    /// which counts a different thing under each scope.
    pub fn turn_scope_len(&self) -> usize {
        self.turn_scope.len()
    }

    /// Whether there is a turn to narrow to at all. Drives whether Review
    /// offers the scope toggle: a key that only ever reports "there is no
    /// this-turn yet" is worse than no key on screen.
    pub fn has_turn_baseline(&self) -> bool {
        self.turn_baseline.is_some()
    }

    /// Every tree row the active scope shows, in order.
    pub(crate) fn visible_tree_rows(&self) -> Vec<usize> {
        (0..self.tree.len()).filter(|i| self.tree_row_visible(*i)).collect()
    }

    /// Switches Review between the whole changeset and just this turn's
    /// files. A `Turn` scope with no baseline behind it would silently
    /// show an empty tree for a reason the user has no way to see, so it
    /// says why instead and stays where it is.
    fn toggle_review_scope(&mut self) {
        match self.review_scope {
            ReviewScope::Turn => self.review_scope = ReviewScope::All,
            ReviewScope::All => {
                if self.turn_baseline.is_none() {
                    self.review_status = Some(Err("no agent turn has run yet \u{2014} there is no \"this turn\" to narrow to".to_string()));
                    return;
                }
                self.review_scope = ReviewScope::Turn;
            }
        }
        self.review_status = None;
        self.recompute_tree_visibility();
        self.settle_tree_selection();
    }

    /// Pulls the tree cursor onto a row the active scope actually shows,
    /// and opens whatever it lands on. A selection left parked on a hidden
    /// row renders as no selection at all, with the arrow keys appearing
    /// to do nothing until they walk far enough to reach a visible row.
    fn settle_tree_selection(&mut self) {
        if self.tree.is_empty() || self.tree_row_visible(self.tree_index) {
            return;
        }
        let Some(target) = self.visible_tree_rows().into_iter().min_by_key(|i| i.abs_diff(self.tree_index)) else {
            return;
        };
        self.tree_index = target;
        self.preview_tree_selection();
    }

    /// `self.project.files`' index for `path` (relative to `target_dir`),
    /// if that file has any uncommitted changes — the source of truth for
    /// whether the content pane *can* show a diff at all. `pub(crate)` so
    /// `ui::review` can annotate tree rows for files other than the one
    /// currently open.
    pub(crate) fn diff_index_for(&self, path: &std::path::Path) -> Option<usize> {
        let rel = crate::gitreview::display_path_of(path.strip_prefix(&self.target_dir).unwrap_or(path));
        self.project.files.iter().position(|f| f.path == rel)
    }

    /// `diff_index_for` on the currently open file — which file (if any)
    /// in `self.project.files` the content pane's diff view refers to.
    /// `pub(crate)` so `ui::review` can decide what to render.
    pub(crate) fn current_diff_index(&self) -> Option<usize> {
        self.diff_index_for(&self.nav_file)
    }

    /// The `content_view` every newly-opened file starts in. Always
    /// `Context`, since it renders correctly whether or not the file has a
    /// diff — a changed file shows the whole file with its changes overlaid
    /// in place, an unchanged one shows plain source. `Focused` is only ever
    /// reached by pressing `v` on a file that actually has a diff. Kept as a
    /// named function rather than inlining the constant so every "open a
    /// file" path is guaranteed to agree on the answer.
    fn default_content_view(&self) -> ContentView {
        ContentView::Context
    }

    /// Commits whatever's currently selected in Curation, for real.
    fn commit_selected(&mut self) {
        // Hoot's own agent is the one thing here that is *known* to be
        // writing to the repo right now. Committing on top of a turn in
        // flight means racing a writer whose edits, by definition, nobody
        // has reviewed yet — gitcommit's freshness check would catch most
        // of it and refuse, but refusing after the fact is a worse answer
        // than not starting. Every other writer (an editor, a second
        // terminal) is genuinely unknowable from in here and stays the
        // freshness check's job.
        if self.agent_running {
            self.last_commit = Some(Err(
                "an agent turn is running \u{2014} wait for it to finish, or press Esc to cancel it, before committing".to_string(),
            ));
            return;
        }
        let result = crate::gitcommit::commit(&self.target_dir, &self.project, &self.curation_files, &self.commit_message);
        let ok = result.is_ok();
        self.last_discard = None;
        self.last_commit = Some(result);
        if ok {
            self.refresh_review();
        }
    }

    // -------------------------------------------------------------
    // DISCARD (the other half of curation)
    // -------------------------------------------------------------

    /// `D` in Curate: asks about throwing away the hunk currently on
    /// screen. Nothing is applied here — this only assembles the question,
    /// because unlike every other key in Curate the answer isn't
    /// recoverable.
    fn request_discard(&mut self) {
        self.last_discard = None;
        // One result row, showing whichever of the two actually happened
        // last — a success line left over from the previous commit sitting
        // under a discard's error would read as if the discard had worked.
        self.last_commit = None;
        // Same reasoning as `commit_selected`: the agent is a known writer
        // to this exact tree, and reversing a patch out from under a turn
        // that is still writing is racing a writer whose output nobody has
        // reviewed. Refusing to start beats failing partway.
        if self.agent_running {
            self.last_discard = Some(Err(
                "an agent turn is running \u{2014} wait for it to finish, or press Esc to cancel it, before discarding".to_string(),
            ));
            return;
        }
        let Some(cf) = self.curation_files.get(self.curation_index) else { return };
        let path = cf.path.clone();
        let Some(file) = self.project.files.iter().find(|f| f.path == path) else { return };
        let units = crate::gitreview::selectable_units(file);
        if units == 0 {
            self.last_discard = Some(Err(format!(
                "{path}: {} \u{2014} hoot can't discard it; use `git checkout`/`rm` outside hoot",
                file.unsupported.unwrap_or("nothing here can be curated")
            )));
            return;
        }
        let unit = self.curation_hunk_index.min(units - 1);
        let what = describe_discard(file, unit, units);
        self.pending_discard = Some(PendingDiscard { path, unit, what });
        self.overlay = Overlay::DiscardConfirm;
    }

    fn on_key_discard_confirm(&mut self, key: KeyEvent) {
        // Deliberately not the same "Enter or the action's own key"
        // shorthand the quit confirmation uses: the key that opened this
        // is `D`, and accepting an irreversible discard by pressing the
        // same key twice is exactly how a held keypress destroys something.
        if key.code == KeyCode::Enter {
            self.overlay = Overlay::None;
            self.perform_discard();
            return;
        }
        if self.keymap.is(&key, Action::NoteCancel) || key.code == KeyCode::Char('n') {
            self.overlay = Overlay::None;
            self.pending_discard = None;
        }
    }

    /// Reverse-applies the confirmed hunk and folds the result back in.
    fn perform_discard(&mut self) {
        let Some(pending) = self.pending_discard.take() else { return };
        // Re-checked rather than assumed: the confirmation is a real pause,
        // and a turn can have been started from another mode during it.
        if self.agent_running {
            self.last_discard = Some(Err("an agent turn started while that was open \u{2014} nothing was discarded".to_string()));
            return;
        }
        let result = crate::gitcommit::discard(&self.target_dir, &self.project, &pending.path, pending.unit);
        let ok = result.is_ok();
        self.last_discard = Some(result);
        if ok {
            // Straight through the normal sync rather than a wholesale
            // reload: the discarded file's hunks have genuinely shifted
            // shape, so it lands in Curate marked stale with nothing
            // selected — which is the correct reading of "part of this
            // file just went away, look at the rest again" — while every
            // other file keeps the selection the user built up.
            self.sync_review_from_disk();
            self.sync_tree_from_disk();
        }
    }

    /// Called by main.rs once the suspended `$EDITOR` session for
    /// `EditorTarget::CommitMessage` returns.
    pub fn finish_editing_commit_message(&mut self, result: Result<String, String>) {
        match result {
            Ok(text) => self.commit_message = text.trim_end().to_string(),
            Err(e) => self.commit_message_status = Some(e),
        }
    }

    /// Called by main.rs once the suspended `$EDITOR` session for
    /// `EditorTarget::IteratePrompt` returns. An empty (or all-deleted)
    /// buffer cancels the send — that's how you back out of iterating
    /// after seeing the assembled prompt.
    pub fn finish_editing_iterate_prompt(&mut self, result: Result<String, String>) {
        match result {
            Ok(text) => {
                let text = text.trim().to_string();
                if !text.is_empty() {
                    self.mode = Mode::Agent;
                    self.start_agent_turn(text);
                }
            }
            Err(e) => {
                self.transcript.push(AgentLine { kind: AgentLineKind::Text, text: format!("Error opening editor: {e}") });
            }
        }
    }

    /// Called every event-loop tick; re-reads the repo from disk at most
    /// once per `FS_POLL_INTERVAL` and folds in anything that changed
    /// outside hoot — an agent run in another terminal, an editor, `git` on
    /// the command line. Keeps Review usable as a pure review/browsing
    /// layer even when whatever's making the changes isn't hoot's own
    /// Agent pane.
    pub fn sync_from_disk(&mut self) {
        if self.last_fs_poll.elapsed() < FS_POLL_INTERVAL {
            return;
        }
        self.last_fs_poll = Instant::now();

        self.sync_review_from_disk();
        self.sync_tree_from_disk();
    }

    /// Re-reads the git diff and merges it into `self.project` /
    /// `self.curation_files`. A no-op (nothing replaced, nothing reset) if
    /// the diff is byte-for-byte the same as last time — so idle polling
    /// never disturbs in-progress Curation hunk selections. When the diff
    /// really did change, per-file `selected`/`flagged`/`notes` are carried
    /// over by path; a file whose hunks shift shape gets marked
    /// `FileStatus::Stale` with every hunk deselected rather than
    /// re-selected, since hunks can't be reliably matched index-for-index
    /// across a real content change and silently defaulting back to
    /// "commit everything" risks staging content the user never reviewed.
    fn sync_review_from_disk(&mut self) {
        let mut review = crate::gitreview::load(&self.target_dir);
        // Before the early return below, not after: a failed read produces
        // an empty file list, which compares equal to a genuinely clean
        // tree — so bailing out first would leave a stale (or absent)
        // error on screen for exactly the repo that most needs one.
        self.review_error = review.error.take();
        if review.project.files == self.project.files {
            return;
        }
        for f in &mut review.project.files {
            if let Some(old) = self.project.files.iter().find(|o| o.path == f.path) {
                f.flagged = old.flagged;
                f.notes = old.notes;
            }
        }
        // Preserve each file's curation selection when that file's own
        // hunks are unchanged. Regression: this used to replace
        // curation_files wholesale on *any* diff change anywhere in the
        // repo — so a background edit to one file (another pi/opencode run,
        // an editor, plain `git`) silently reselected every hunk in every
        // *other* file too, including ones the user had deliberately
        // deselected. Only a file whose hunks actually shifted shape needs
        // resetting — that's the one case index-for-index selection
        // genuinely can't be trusted.
        //
        // And when it *is* that case, resetting means "nothing selected,
        // marked stale" rather than gitreview::load's normal "everything
        // selected" default: the file existed under previous review, the
        // user may well have deliberately deselected part of it, and there
        // is no way here to tell "content changed but the old exclusion is
        // still what they want" from "content changed and needs a fresh
        // look" — so a brand-new file gets the trusting default, but one
        // that changed shape out from under an existing review does not.
        for cf in &mut review.curation_files {
            let old_new =
                self.project.files.iter().find(|f| f.path == cf.path).zip(review.project.files.iter().find(|f| f.path == cf.path));
            match old_new {
                Some((old, new)) if old.same_change_as(new) => {
                    if let Some(old_cf) = self.curation_files.iter().find(|c| c.path == cf.path) {
                        if old_cf.hunk_selected.len() == cf.hunk_selected.len() {
                            cf.hunk_selected = old_cf.hunk_selected.clone();
                        }
                        // The `Stale` badge has to come across with the
                        // selection it explains. `gitreview::load` builds
                        // every file with `status: None`, so a file marked
                        // stale on one tick lost its badge on the next —
                        // but kept the empty selection the badge was there
                        // to account for, which reads as a file that
                        // simply has nothing selected for no reason. (Only
                        // reachable once a note or a flag exists anywhere:
                        // until then the byte-identical early return above
                        // means this loop never runs a second time.)
                        cf.status = old_cf.status;
                    }
                }
                Some(_) => {
                    cf.hunk_selected = vec![false; cf.hunk_selected.len()];
                    cf.status = Some(crate::theme::FileStatus::Stale);
                }
                None => {}
            }
        }
        // Every path whose change identity moved in this sync — the same
        // comparison the stale marking above is built on. Collected before
        // `self.project` is replaced, since that's the only moment both
        // views exist, and used below to re-place the notes hanging off
        // those files.
        let mut moved: Vec<String> = Vec::new();
        let every_path = review.project.files.iter().map(|f| f.path.clone()).chain(self.project.files.iter().map(|f| f.path.clone()));
        for path in every_path {
            if moved.contains(&path) {
                continue;
            }
            let before = self.project.files.iter().find(|f| f.path == path);
            let after = review.project.files.iter().find(|f| f.path == path);
            let changed = match (before, after) {
                (Some(b), Some(a)) => !b.same_change_as(a),
                (None, None) => false,
                // Entering or leaving the changeset is a content change
                // like any other: a file the agent just created, or one
                // whose edit was discarded back to what HEAD holds.
                _ => true,
            };
            if changed {
                moved.push(path);
            }
        }

        self.project = review.project;
        self.curation_files = review.curation_files;
        self.curation_index = self.curation_index.min(self.curation_files.len().saturating_sub(1));
        let hunk_total = self.curation_files.get(self.curation_index).map(|f| f.total()).unwrap_or(0);
        self.curation_hunk_index = self.curation_hunk_index.min(hunk_total.saturating_sub(1) as usize);
        // `Focused` has nothing to show once there's no diff left to focus
        // on — fall back to `Context`, which renders fine either way.
        if self.current_diff_index().is_none() {
            self.content_view = ContentView::Context;
        }
        self.reanchor_notes(&moved);
        // Deliberately no `settle_tree_selection` here. A poll that drops
        // a file out of the turn scope — you discarded its hunk, or
        // reverted it by hand — must not also drag the cursor off whatever
        // you were reading; being moved once a second by a background
        // refresh is worse than a cursor parked on a row that has stopped
        // being drawn, which `tree_step` already navigates out of.
        self.recompute_turn_scope();
    }

    /// Re-places every line-scoped note on a file in `moved`, or marks it
    /// stale when its anchor is gone.
    ///
    /// This is what stops `i`/`y` from being a one-shot. A note says "Line
    /// 47", the agent then edits the file, and line 47 is now a different
    /// function — so the next iterate prompt would send a correction
    /// pointing at code the note was never about. Re-anchoring moves the
    /// note to wherever its line actually went; failing that, the note is
    /// kept and marked, and `build_iterate_prompt` says the line is gone
    /// rather than naming one.
    ///
    /// Deliberately never deletes a note. The user wrote it, and `d`/`D`
    /// remain the only things that throw one away.
    fn reanchor_notes(&mut self, moved: &[String]) {
        if moved.is_empty() || self.notes.is_empty() {
            return;
        }
        for path in moved {
            if !self.notes.iter().any(|n| n.path == *path && n.line.is_some() && n.anchor.is_some()) {
                continue;
            }
            // The changeset's own path bytes when the file is still in it —
            // `Note.path` is a printable rendering, which for a name that
            // isn't valid UTF-8 names nothing on disk. Falling back to
            // joining it is only reached for a file that has left the
            // changeset entirely, where there's nothing better to use.
            let full = match self.project.files.iter().find(|f| f.path == *path) {
                Some(f) => self.target_dir.join(f.meta.os_path()),
                None => self.target_dir.join(path),
            };
            // Read straight from disk rather than through
            // `fsnav::read_file`, which is a *display* reader: it answers a
            // symlink, an oversized file or a permission error with a
            // one-line placeholder describing the problem. That
            // placeholder contains no source line, so every note on the
            // file would match nothing and be marked stale — a verdict
            // about the user's notes reached from a failure to read the
            // file at all.
            let lines = match read_lines_for_anchoring(&full) {
                AnchorSource::Lines(lines) => lines,
                // The file is gone, so every line in it is. That is the
                // same statement `stale` makes, arrived at without needing
                // to search for anything.
                AnchorSource::Gone => Vec::new(),
                AnchorSource::Unreadable => continue,
            };
            for note in self.notes.iter_mut().filter(|n| n.path == *path) {
                let (Some(anchor), Some(line)) = (&note.anchor, note.line) else { continue };
                match anchor.reanchor(&lines, line) {
                    Some(now) => {
                        note.line = Some(now);
                        note.stale = false;
                    }
                    None => note.stale = true,
                }
            }
        }
    }

    /// How many queued notes no longer point at a line that exists.
    pub fn notes_stale(&self) -> u32 {
        self.notes.iter().filter(|n| n.stale).count() as u32
    }

    /// Re-scans the file tree and re-reads the currently open source file,
    /// each a no-op unless it actually changed. Symbols are only
    /// re-scanned alongside a tree change (a file was added/removed/moved)
    /// rather than on every poll, since a full symbol scan is the more
    /// expensive of the two and a same-file content edit doesn't need it.
    fn sync_tree_from_disk(&mut self) {
        let scan = fsnav::build_tree(&self.target_dir);
        self.tree_truncated = scan.truncated;
        if scan.entries != self.tree {
            self.tree = scan.entries;
            self.tree_index = self.tree_index.min(self.tree.len().saturating_sub(1));
            let symbol_scan = fsnav::scan_symbols(&self.target_dir);
            self.symbols = symbol_scan.results;
            self.symbols_truncated = symbol_scan.truncated;
            self.symbol_index = self.symbol_index.min(self.symbols.len().saturating_sub(1));
            // `tree_visible` is indexed by row, so it is only meaningful
            // against the scan it was built from.
            self.recompute_tree_visibility();
            self.settle_tree_selection();
        }

        let new_source = fsnav::read_file(&self.nav_file);
        if new_source != self.source {
            self.source = new_source;
            self.diff_context = diff_context_for(&self.target_dir, &self.nav_file);
            self.nav_line = self.nav_line.min(self.content_line_count().saturating_sub(1));
            self.refresh_hover();
        }
    }

    fn open_file(&mut self, path: PathBuf) {
        self.source = fsnav::read_file(&path);
        self.diff_context = diff_context_for(&self.target_dir, &path);
        self.nav_file = path;
        self.nav_line = 0;
        self.nav_scroll_x = 0;
        self.content_view = self.default_content_view();
        self.refresh_hover();
    }

    /// Switches to Review focused on `path` — the tree/explorer pane and
    /// the content pane both, not just the content pane `open_file` alone
    /// moves. Every "open this file in Review" entry point (symbol jump,
    /// file finder, jumping here from a Curate hunk) goes through this, so
    /// they all agree on what "opening a file" means: previously only the
    /// content pane actually moved, and the tree pane's selection/scroll
    /// silently stayed wherever it had been.
    fn open_in_review(&mut self, path: PathBuf, line: Option<usize>) {
        self.open_file(path.clone());
        self.content_view = ContentView::Context;
        if let Some(line) = line {
            self.nav_line = self.nav_line_for_file_line(line);
        }
        self.nav_focus = NavFocus::Content;
        // `path` itself won't be in the tree for a file Curate can still
        // show a diff for but the tree can't (a deletion — gone from disk
        // entirely — or a file past `fsnav::TREE_BUDGET`'s scan cap). Falling
        // back to `self.tree_index` unchanged in that case left whatever
        // unrelated row the cursor happened to be parked on looking
        // selected/highlighted, with nothing about it connected to the file
        // the content pane just jumped to. The containing directory is
        // still something real to land on instead — it's what a file
        // explorer would do — so try that before giving up and leaving the
        // stale position.
        self.tree_index = self
            .tree
            .iter()
            .position(|e| e.path == path)
            .or_else(|| path.parent().and_then(|parent| self.tree.iter().position(|e| e.is_dir && e.path == parent)))
            // A file directly in the repo root has no parent *row* to fall
            // back to — the tree starts at the root's children, and the
            // root itself isn't one of them — so the directory lookup above
            // finds nothing and the cursor would be left parked on whatever
            // unrelated row it happened to be on, looking selected. Row 0
            // is the top of the very directory that contains the file, which
            // is the same answer the directory case gives one level down.
            .or_else(|| (path.parent() == Some(self.target_dir.as_path()) && !self.tree.is_empty()).then_some(0))
            .unwrap_or(self.tree_index);
        // Asking for a file the active scope hides is a clear enough
        // statement that the scope is in the way — widening beats landing
        // the cursor on a row nothing draws and leaving the tree looking
        // like it ignored the request. Nothing is hidden about it either:
        // Review's footer names the scope on every frame.
        if !self.tree_row_visible(self.tree_index) {
            self.review_scope = ReviewScope::All;
            self.recompute_tree_visibility();
        }
        self.refresh_hover();
        self.mode = Mode::Review;
    }

    fn refresh_hover(&mut self) {
        // `nav_line` indexes into whatever's currently on screen — when
        // that's a diff view, it isn't a position in `self.source` at all
        // (a diff has a different length: removed lines are shown too,
        // and `Focused` collapses stretches), so hover just doesn't apply.
        if self.current_diff_index().is_some() {
            self.hover = None;
            return;
        }
        self.hover = self.source.get(self.nav_line).and_then(|line| {
            let sym = fsnav::hover_for_line(&self.symbols, line)?;
            let references = fsnav::reference_count(&self.target_dir, &sym.name);
            Some(HoverInfo { signature: sym.preview.clone(), location: sym.location(&self.target_dir), references })
        });
    }

    /// The real count of queued notes. Deliberately `self.notes.len()`, not
    /// a sum over `FileEntry.notes` — that counter only exists on files
    /// that currently have a diff, but a line-scoped comment can be left
    /// on *any* file being browsed in source view, changed or not. Summing
    /// the per-file counters used to undercount (silently missing notes on
    /// clean files), which let `quit_risk` decide there was nothing to
    /// confirm about even with real queued notes still unsent.
    pub fn notes_queued(&self) -> u32 {
        self.notes.len() as u32
    }

    pub fn files_flagged(&self) -> usize {
        self.project.files.iter().filter(|f| f.flagged).count()
    }

    /// Sends `prompt` to the active agent backend against `self.target_dir`
    /// with read+write — it's already a real git repo, so Review's diff
    /// view and plain `git` are the review/undo mechanism for whatever it
    /// writes, same as any other change made to the repo.
    pub fn start_agent_turn(&mut self, prompt: String) {
        let target_dir = self.target_dir.clone();
        self.spawn_turn(prompt, target_dir, ToolProfile::ReadWrite, TurnPurpose::Chat);
    }

    /// Spawns a turn in `cwd` with the given tool profile, on whichever
    /// backend `self.agent_backend` selects. Reuses the backend's session
    /// (`session_file` for pi, `opencode_session_id` for opencode) across
    /// every call in this run, so it has real cross-turn memory.
    /// `CommitMessage` turns never touch the visible transcript at all —
    /// see `apply_agent_event`.
    fn spawn_turn(&mut self, prompt: String, cwd: PathBuf, tools: ToolProfile, purpose: TurnPurpose) {
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() || self.agent_running {
            return;
        }
        // First real turn ever on this machine: hold it and ask before a
        // subprocess with this much trust (auto-approved tool calls, real
        // code execution — see trust.rs) runs for the first time.
        if !self.agent_trust_acknowledged {
            self.pending_turn = Some((prompt, cwd, tools, purpose));
            self.overlay = Overlay::AgentTrustConfirm;
            return;
        }
        self.spawn_turn_now(prompt, cwd, tools, purpose);
    }

    /// Creates `session_file` on first use rather than in `App::new` — see
    /// the field's doc comment — and returns its path either way.
    fn ensure_session_file(&mut self) -> std::io::Result<PathBuf> {
        if self.session_file.is_none() {
            self.session_file = Some(tempfile::Builder::new().prefix("hoot-session-").suffix(".jsonl").tempfile()?);
        }
        Ok(self.session_file.as_ref().expect("just set").path().to_path_buf())
    }

    fn spawn_turn_now(&mut self, prompt: String, cwd: PathBuf, tools: ToolProfile, purpose: TurnPurpose) {
        self.agent_purpose = purpose;
        self.agent_model_live = None;
        if purpose == TurnPurpose::Chat {
            // The baseline for "what this turn wrote", taken here rather
            // than read off the last poll: up to a second of drift would
            // hand this turn credit for an edit someone else had already
            // made. Read fresh from git, and only for a Chat turn — a
            // commit-message turn is read-only and has no business
            // resetting what Review is scoped to.
            //
            // A git failure leaves the previous baseline in place rather
            // than installing an empty one: an empty baseline would make
            // every uncommitted file look like this turn's work, which is
            // exactly the confusion the scope exists to remove.
            if let Ok(files) = crate::gitreview::diff_files(&cwd) {
                self.turn_baseline = Some(files);
                self.recompute_turn_scope();
            }
            self.transcript.push(AgentLine { kind: AgentLineKind::UserPrompt, text: prompt.clone() });
            self.transcript.push(AgentLine { kind: AgentLineKind::Blank, text: String::new() });
            self.agent_scroll = 0; // jump to the bottom to watch it stream in
        } else {
            self.commit_message_status = Some("Generating\u{2026}".to_string());
        }

        let result = match self.agent_backend {
            AgentBackend::Pi => match self.ensure_session_file() {
                Ok(path) => pi_client::spawn(&prompt, &cwd, &path, tools),
                Err(e) => Err(e),
            },
            AgentBackend::OpenCode => opencode_client::spawn(&prompt, &cwd, self.opencode_session_id.as_deref(), tools),
        };
        match result {
            Ok(session) => {
                self.agent_session = Some(session);
                self.agent_running = true;
            }
            Err(e) => {
                let label = self.agent_backend.label();
                let msg = format!("Error: couldn't start `{label}` ({e}). Is it installed and on PATH?");
                if purpose == TurnPurpose::Chat {
                    self.transcript.push(AgentLine { kind: AgentLineKind::Text, text: msg });
                } else {
                    self.commit_message_status = Some(msg);
                }
            }
        }
    }

    /// `g` in Curate: asks the active agent backend to draft a commit
    /// message from the real diff of everything currently selected — a
    /// silent, read-only, no-tools-needed turn that never touches the
    /// visible Agent transcript. Completing it opens `$EDITOR` for a last
    /// pass (see `apply_agent_event`'s `AgentEnd` handling).
    pub fn generate_commit_message(&mut self) {
        if self.agent_running {
            return;
        }
        // The demo's diff is real now (it's a real repo), so this is no
        // longer about fabricated content — it's about cost. `g` is one
        // keypress away while someone is poking around `--demo`, and it
        // spawns a real agent subprocess against a real model. Nobody
        // exploring the UI asked to spend an API call on a throwaway repo's
        // invented change.
        if self.demo {
            self.commit_message_status =
                Some("Demo mode \u{2014} not spending a real agent turn on a throwaway repo. Press e to write one.".to_string());
            return;
        }
        let diff_text = self.selected_diff_text();
        if diff_text.trim().is_empty() {
            self.commit_message_status = Some("Nothing selected to summarize.".to_string());
            return;
        }
        let prompt = format!(
            "Write a concise commit message (a short summary line, plus a body only if it adds \
             real value) for the following diff. Output ONLY the commit message text — no \
             commentary, no markdown code fences.\n\n{diff_text}"
        );
        let target_dir = self.target_dir.clone();
        self.spawn_turn(prompt, target_dir, ToolProfile::ReadOnly, TurnPurpose::CommitMessage);
    }

    /// Real unified-diff text for every currently-selected hunk, grouped by
    /// file — used as the source material for commit-message generation.
    fn selected_diff_text(&self) -> String {
        let mut out = String::new();
        for cf in &self.curation_files {
            if cf.selected() == 0 {
                continue;
            }
            let Some(file) = self.project.files.iter().find(|f| f.path == cf.path) else { continue };
            // git's own header, not a reconstructed one. The old synthetic
            // `diff --git a/p b/p` + `---`/`+++` triple dropped every piece
            // of extended metadata git puts there — `rename from`/`to`,
            // `new file mode`, `deleted file mode`, `old mode`/`new mode`
            // — so a pure rename, a chmod, or a deletion reached the model
            // as a contentless stub and came back with a commit message
            // describing nothing. This is the same header `build_patch`
            // replays when staging, so the message is drafted from the
            // same framing the commit is actually built from.
            for line in &file.meta.header {
                out.push_str(line);
                out.push('\n');
            }
            for (hunk, &sel) in file.hunks.iter().zip(&cf.hunk_selected) {
                if !sel {
                    continue;
                }
                for line in &hunk.lines {
                    out.push_str(&line.text);
                    out.push('\n');
                }
            }
        }
        out
    }

    /// Drains any events the background reader thread has queued up. Called
    /// once per event-loop tick; never blocks.
    pub fn poll_agent(&mut self) {
        while let Some(session) = &self.agent_session {
            match session.rx.try_recv() {
                Ok(ev) => self.apply_agent_event(ev),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.finish_turn();
                    break;
                }
            }
        }
    }

    fn apply_agent_event(&mut self, event: AgentEvent) {
        use AgentEvent::*;

        if self.agent_purpose == TurnPurpose::CommitMessage {
            match event {
                Model(m) => self.agent_model_live = Some(m),
                Session(id) => self.opencode_session_id = Some(id),
                Text(t) => self.commit_message = t.trim().to_string(),
                Error(e) => self.commit_message_status = Some(format!("Error generating message: {e}")),
                // The turn's actual end is handled uniformly in
                // `finish_turn`, triggered when the backend process exits
                // (the channel disconnects) — see its doc comment for why.
                Thinking(_) | ToolCall { .. } | ToolResult { .. } | TurnEnd | AgentEnd => {}
            }
            return;
        }

        match event {
            Model(m) => self.agent_model_live = Some(m),
            Session(id) => self.opencode_session_id = Some(id),
            Thinking(t) => {
                // Rendered dimmed (AgentLineKind::Thinking) and followed by
                // a blank line, so reasoning prose reads as clearly
                // secondary to — and doesn't visually run into — whatever
                // comes next (a tool call or the final answer).
                self.transcript.push(AgentLine { kind: AgentLineKind::Thinking, text: t });
                self.transcript.push(AgentLine { kind: AgentLineKind::Blank, text: String::new() });
            }
            Text(t) => self.transcript.push(AgentLine { kind: AgentLineKind::Text, text: t }),
            ToolCall { name, args } => {
                self.transcript.push(AgentLine { kind: AgentLineKind::ToolCall, text: format!("Calling: {name}({args})") })
            }
            ToolResult { name, summary } => {
                self.transcript.push(AgentLine { kind: AgentLineKind::Done, text: format!("{name}  {summary}") })
            }
            TurnEnd => self.transcript.push(AgentLine { kind: AgentLineKind::Blank, text: String::new() }),
            // pi sends an explicit AgentEnd right before exiting; opencode
            // has no equivalent event at all. `finish_turn` (fired on the
            // channel disconnecting, i.e. the process actually exiting)
            // covers both uniformly, so this is a no-op either way.
            AgentEnd => {}
            Error(e) => self.transcript.push(AgentLine { kind: AgentLineKind::Text, text: format!("stderr: {e}") }),
        }
    }

    /// Runs once a spawned turn's backend process has actually exited
    /// (the event channel disconnecting is the signal, not any particular
    /// JSON event — see `AgentEnd`'s doc comment above). Whichever backend
    /// was used, this is the one place "the turn is fully over" logic
    /// lives, so behavior doesn't fork per backend here.
    fn finish_turn(&mut self) {
        self.agent_running = false;
        self.agent_session = None;
        let cancelled = std::mem::take(&mut self.agent_cancelled);
        if self.agent_purpose == TurnPurpose::CommitMessage {
            if cancelled {
                self.commit_message_status = Some("Cancelled.".to_string());
                return;
            }
            // If the turn errored, `apply_agent_event` already put that
            // message in `commit_message_status` — leave it there instead
            // of clobbering it, and don't open $EDITOR on what would be an
            // empty (or stale) buffer with no indication anything went
            // wrong. The user can still write one by hand with 'e'.
            let had_error = self.commit_message_status.as_deref().is_some_and(|s| s.starts_with("Error"));
            if !had_error {
                self.commit_message_status = None;
                self.open_editor_requested = Some(EditorTarget::CommitMessage);
            }
            return;
        }
        if cancelled {
            self.transcript.push(AgentLine { kind: AgentLineKind::Text, text: "Cancelled.".to_string() });
            // A cancelled turn can still have written real files before it
            // was killed — pull those in too, same as a completed turn,
            // and scope Review to them the same way. Half a turn's output
            // is exactly the kind of thing you want isolated from
            // everything else in the tree before deciding what to keep.
            self.sync_review_from_disk();
            self.sync_tree_from_disk();
            self.report_turn_changes("changed before the turn was cancelled");
            return;
        }
        self.sync_review_from_disk();
        self.sync_tree_from_disk();
        self.report_turn_changes("changed");
    }

    /// Summarizes what the turn that just ended actually wrote, and points
    /// Review at exactly those files.
    ///
    /// The count is `turn_scope`'s, not the whole changeset's. "N files
    /// changed" used to mean every dirty file in the repo — work that was
    /// already there before the turn started, and files the agent never
    /// touched — which made the one line the user is most likely to act on
    /// without opening Review the least accurate thing on screen.
    ///
    /// Switching the scope here is what makes the line's own advice work:
    /// F1 lands on this turn's files, and `t` steps back out to the whole
    /// tree. Nothing is hidden silently — Review's footer names the active
    /// scope either way.
    fn report_turn_changes(&mut self, verb: &str) {
        // Not "(no file changes)": the turn may well have written plenty,
        // and saying otherwise here is the one summary the user is most
        // likely to act on without opening Review.
        if let Some(e) = &self.review_error {
            let text = format!("  (couldn't read the diff after this turn: {e})");
            self.transcript.push(AgentLine { kind: AgentLineKind::Proposal, text });
            return;
        }
        let scoped = self.turn_baseline.is_some();
        let n = if scoped { self.turn_scope.len() } else { self.project.files.len() };
        let plural = if n == 1 { "" } else { "s" };
        let text = if n == 0 {
            "  (no file changes)".to_string()
        } else if scoped {
            self.review_scope = ReviewScope::Turn;
            self.recompute_tree_visibility();
            self.settle_tree_selection();
            format!("{n} file{plural} {verb} \u{2014} Review (F1) is scoped to them; t shows the whole tree")
        } else {
            // No baseline to measure against — only reachable if the diff
            // read at spawn time failed. Falls back to the whole
            // uncommitted changeset, and says that's what it is rather
            // than letting the number pass for this turn's work.
            format!("{n} file{plural} uncommitted (no pre-turn snapshot, so this isn't only what {verb}) \u{2014} see Review (F1)")
        };
        self.transcript.push(AgentLine { kind: AgentLineKind::Proposal, text });
    }

    /// Kills the running agent subprocess, if any — the only way to stop a
    /// turn short of quitting hoot entirely before this existed. `pi`/
    /// `opencode` can run for a long time on a wide-scoped prompt, and nothing
    /// here caps how long that runs on its own.
    pub fn cancel_agent_turn(&mut self) {
        if !self.agent_running {
            return;
        }
        if let Some(session) = &self.agent_session {
            session.cancel();
        }
        self.agent_cancelled = true;
        if self.agent_purpose == TurnPurpose::Chat {
            self.transcript.push(AgentLine { kind: AgentLineKind::Text, text: "Cancelling\u{2026}".to_string() });
        } else {
            self.commit_message_status = Some("Cancelling\u{2026}".to_string());
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        // Fixed, not remappable, and checked first so it's never swallowed
        // by whatever overlay or text field happens to be focused — but no
        // longer an unconditional instant quit: it goes through the same
        // risk check as 'q' (see quit_risk), so it can't silently drop
        // unsent notes/flags/a drafted commit message/a running turn any
        // more than 'q' can. It stays a real escape hatch either way: a
        // second Ctrl+C while the confirmation is already showing confirms
        // immediately, same as pressing 'q' or Enter would.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.overlay == Overlay::QuitConfirm || self.quit_risk().is_none() {
                self.should_quit = true;
            } else {
                self.overlay = Overlay::QuitConfirm;
            }
            return;
        }

        match self.overlay {
            Overlay::SymbolJump => return self.on_key_symbol_jump(key),
            Overlay::FileFinder => return self.on_key_file_finder(key),
            Overlay::NoteInput => return self.on_key_note_input(key),
            Overlay::QuitConfirm => return self.on_key_quit_confirm(key),
            Overlay::AgentTrustConfirm => return self.on_key_agent_trust_confirm(key),
            Overlay::DiscardConfirm => return self.on_key_discard_confirm(key),
            Overlay::None => {}
        }

        // Cancelling a running turn works from any mode, not just Agent
        // or Curate — a turn started from Agent keeps running in the
        // background if you switch to Review to look something up while
        // it works, and there was previously no way to stop it again
        // without switching back first. Gated on agent_running so Esc
        // still falls through to whatever else it might mean per-mode
        // when nothing's actually running to cancel.
        if self.agent_running && self.keymap.is(&key, Action::AgentCancel) {
            self.cancel_agent_turn();
            return;
        }

        // Mode switches always work, even mid-text-entry (their default
        // chords are F1-F3, which no text field would otherwise consume).
        if self.keymap.is(&key, Action::SwitchReview) {
            self.mode = Mode::Review;
            return;
        }
        if self.keymap.is(&key, Action::SwitchAgent) {
            self.mode = Mode::Agent;
            return;
        }
        if self.keymap.is(&key, Action::SwitchCurate) {
            self.mode = Mode::Curation;
            return;
        }

        // Text-entry modes swallow most keys before global shortcuts apply.
        if self.mode == Mode::Agent && self.on_key_agent_input(key) {
            return;
        }

        if self.keymap.is(&key, Action::OpenSymbolJump) {
            self.overlay = Overlay::SymbolJump;
            return;
        }
        if self.keymap.is(&key, Action::OpenFileFinder) {
            self.overlay = Overlay::FileFinder;
            self.file_finder_filter.clear();
            self.file_finder_index = 0;
            return;
        }
        if self.keymap.is(&key, Action::Quit) {
            if self.quit_risk().is_some() {
                self.overlay = Overlay::QuitConfirm;
            } else {
                self.should_quit = true;
            }
            return;
        }

        match self.mode {
            Mode::Review => self.on_key_review(key),
            Mode::Agent => self.on_key_agent(key),
            Mode::Curation => self.on_key_curation(key),
        }
    }

    // -------------------------------------------------------------
    // REVIEW
    // -------------------------------------------------------------

    /// Moves the focused pane's index by `delta` (negative = up/back),
    /// clamped to its bounds. `tree_len` is passed in since the tree and
    /// source have different lengths and only one is relevant per call.
    fn move_nav_focus(&mut self, delta: i64, tree_len: usize) {
        if self.nav_focus == NavFocus::Tree {
            self.tree_index = self.tree_step(delta, tree_len);
            self.preview_tree_selection();
        } else {
            self.nav_line = clamped_move(self.nav_line, delta, self.content_line_count());
            if self.show_hover {
                self.refresh_hover();
            }
        }
    }

    /// Where `delta` rows of tree movement land, counting only the rows
    /// the active scope actually shows. Identical to `clamped_move` under
    /// `ReviewScope::All`, where every row is visible; under `Turn` the
    /// hidden rows are stepped over rather than through, so a filtered
    /// tree moves one visible row per keypress instead of appearing to
    /// stall for however many hidden rows sit in between.
    fn tree_step(&self, delta: i64, tree_len: usize) -> usize {
        if self.review_scope == ReviewScope::All {
            return clamped_move(self.tree_index, delta, tree_len);
        }
        let rows = self.visible_tree_rows();
        if rows.is_empty() {
            return self.tree_index;
        }
        // The cursor can sit on a hidden row (an out-of-scope file opened
        // from the finder, say), so "where am I in the visible list" is a
        // nearest-match question, not a lookup.
        let here = rows
            .iter()
            .position(|i| *i == self.tree_index)
            .unwrap_or_else(|| rows.iter().enumerate().min_by_key(|(_, i)| i.abs_diff(self.tree_index)).map(|(n, _)| n).unwrap_or(0));
        rows[clamped_move(here, delta, rows.len())]
    }

    /// Jumps the focused pane's index directly to `target` (clamped to its
    /// bounds) — `usize::MAX` means "the last entry".
    fn jump_nav_focus(&mut self, target: usize, tree_len: usize) {
        if self.nav_focus == NavFocus::Tree {
            let clamped = target.min(tree_len.saturating_sub(1));
            // Home/End have to mean the first/last row that is actually on
            // screen, not the first/last row of the underlying scan — a
            // filtered tree would otherwise jump the cursor onto a row
            // nothing is drawing.
            self.tree_index = if self.review_scope == ReviewScope::All {
                clamped
            } else {
                let rows = self.visible_tree_rows();
                match rows.iter().rev().find(|i| **i <= clamped).or_else(|| rows.first()) {
                    Some(i) => *i,
                    None => self.tree_index,
                }
            };
            self.preview_tree_selection();
        } else {
            self.nav_line = target.min(self.content_line_count().saturating_sub(1));
            if self.show_hover {
                self.refresh_hover();
            }
        }
    }

    /// How many rows the content pane currently has — the plain file's
    /// line count when there's no diff to show, otherwise however many
    /// rows the active `ContentView` renders (a diff has a different
    /// length than the file itself: removed lines are shown too, and
    /// `Focused` collapses unchanged stretches). Clamps `nav_line` to
    /// whatever's actually on screen.
    fn content_line_count(&self) -> usize {
        match self.current_diff_content_lines() {
            Some(lines) => lines.len(),
            None => self.source.len(),
        }
    }

    /// The open file's diff, numbered with current-file line numbers and
    /// collapsed to match whichever `ContentView` is active — `None` if
    /// the file has no diff at all (plain source browsing instead).
    /// Shared by `content_line_count` and `current_content_line_number` so
    /// they can't disagree about what's actually on screen.
    fn current_diff_content_lines(&self) -> Option<Vec<(Option<usize>, data::DiffLine)>> {
        self.current_diff_index()?;
        let numbered = data::diff_lines_with_file_line_numbers(&self.diff_context);
        Some(match self.content_view {
            ContentView::Context => numbered,
            ContentView::Focused => data::focus_diff_lines(&numbered, 3),
        })
    }

    /// The file line number the cursor (`nav_line`) currently corresponds
    /// to, for line-scoped review comments — `None` if there's nothing
    /// sensible to attach a note to at that exact position (an empty
    /// file, a `Removed` line that no longer exists in the current file,
    /// or a collapsed "unchanged" placeholder in `Focused` view).
    fn current_content_line_number(&self) -> Option<usize> {
        match self.current_diff_content_lines() {
            Some(lines) => lines.get(self.nav_line).and_then(|(n, _)| *n),
            None => {
                if self.source.is_empty() {
                    None
                } else {
                    Some(self.nav_line + 1)
                }
            }
        }
    }

    /// The `nav_line` row that shows 1-based `file_line` of the file
    /// that's open *right now* — used to land on the right row after a
    /// symbol jump or file-finder open, regardless of whether the content
    /// pane ends up showing plain source or a diff (which can have a
    /// different row for the same file line, since removed lines are
    /// shown too). Always resolves against `Context`, never `Focused`,
    /// since a `Focused` view can collapse the exact target line away.
    fn nav_line_for_file_line(&self, file_line: usize) -> usize {
        if self.current_diff_index().is_none() {
            return file_line.saturating_sub(1).min(self.source.len().saturating_sub(1));
        }
        let numbered = data::diff_lines_with_file_line_numbers(&self.diff_context);
        numbered.iter().position(|(n, _)| *n == Some(file_line)).unwrap_or(0).min(numbered.len().saturating_sub(1))
    }

    /// Live-previews whatever file the tree cursor currently points at in
    /// the content pane, without changing pane focus — the same idea as
    /// the File Finder's live preview, so browsing the tree always shows
    /// what's currently highlighted rather than needing an explicit "open".
    fn preview_tree_selection(&mut self) {
        if let Some(entry) = self.tree.get(self.tree_index) {
            if !entry.is_dir {
                let path = entry.path.clone();
                self.open_file(path);
            }
        }
    }

    fn on_key_review(&mut self, key: KeyEvent) {
        let n = self.tree.len();
        let k = &self.keymap;
        // j/k always move too, regardless of ReviewUp/ReviewDown's
        // configured chord — vim muscle memory shouldn't require a remap.
        // Which pane they move depends on nav_focus, same as the arrows:
        // only one pane moves at a time, never both.
        if k.is(&key, Action::ReviewUp) || key.code == KeyCode::Char('k') {
            self.move_nav_focus(-1, n);
        } else if k.is(&key, Action::ReviewDown) || key.code == KeyCode::Char('j') {
            self.move_nav_focus(1, n);
        } else if k.is(&key, Action::ReviewPageUp) {
            self.move_nav_focus(-(PAGE_SIZE as i64), n);
        } else if k.is(&key, Action::ReviewPageDown) {
            self.move_nav_focus(PAGE_SIZE as i64, n);
        } else if k.is(&key, Action::ReviewHome) {
            self.jump_nav_focus(0, n);
        } else if k.is(&key, Action::ReviewEnd) {
            self.jump_nav_focus(usize::MAX, n);
        } else if k.is(&key, Action::ReviewOpen) {
            self.preview_tree_selection();
            self.nav_focus = NavFocus::Content;
        } else if k.is(&key, Action::ReviewToggleFocus) {
            self.nav_focus = match self.nav_focus {
                NavFocus::Tree => NavFocus::Content,
                NavFocus::Content => NavFocus::Tree,
            };
        } else if k.is(&key, Action::ReviewScrollLeft) {
            // Applies in both plain source and the diff-in-context view —
            // long lines get truncated either way (draw_source and
            // draw_diff_scrollable both honor nav_scroll_x). The
            // before/after split view isn't included: its two columns are
            // already narrower and scroll independently, which doesn't fit
            // a single shared offset.
            if self.nav_focus == NavFocus::Content && !self.split_diff {
                self.nav_scroll_x = self.nav_scroll_x.saturating_sub(4);
            }
        } else if k.is(&key, Action::ReviewScrollRight) {
            if self.nav_focus == NavFocus::Content && !self.split_diff {
                self.nav_scroll_x = self.nav_scroll_x.saturating_add(4);
            }
        } else if k.is(&key, Action::ReviewToggleHover) {
            self.show_hover = !self.show_hover;
            if self.show_hover {
                self.refresh_hover();
            }
        } else if k.is(&key, Action::ReviewOpenSymbolJump) {
            self.overlay = Overlay::SymbolJump;
        } else if k.is(&key, Action::ReviewToggleView) {
            if self.current_diff_index().is_some() {
                self.content_view = match self.content_view {
                    ContentView::Context => ContentView::Focused,
                    ContentView::Focused => ContentView::Context,
                };
                self.nav_line = 0; // the two views have different lengths
            }
        } else if k.is(&key, Action::ReviewMarkGood) {
            if let Some(i) = self.current_diff_index() {
                let path = self.project.files[i].path.clone();
                let f = &mut self.project.files[i];
                f.flagged = false;
                f.notes = 0;
                self.notes.retain(|n| n.path != path);
            }
        } else if k.is(&key, Action::ReviewFlagRework) {
            if let Some(i) = self.current_diff_index() {
                self.project.files[i].flagged = true;
            }
        } else if k.is(&key, Action::ReviewClearFileNotes) {
            // Just the notes, unlike Mark Good ('g') which also clears the
            // flag — for wiping stale comments on a file you're not ready
            // to call done yet.
            if let Some(i) = self.current_diff_index() {
                let path = self.project.files[i].path.clone();
                self.project.files[i].notes = 0;
                self.notes.retain(|n| n.path != path);
            }
        } else if k.is(&key, Action::ReviewClearAllNotes) {
            for f in &mut self.project.files {
                f.notes = 0;
            }
            self.notes.clear();
        } else if k.is(&key, Action::ReviewToggleScope) {
            self.toggle_review_scope();
        } else if k.is(&key, Action::ReviewSplitView) {
            self.split_diff = true;
        } else if k.is(&key, Action::ReviewUnifiedView) {
            self.split_diff = false;
        } else if k.is(&key, Action::ReviewIterate) {
            // Open every queued note and flag for a last-pass edit before
            // anything is sent — same reasoning as the commit-message
            // flow: never fire an assembled prompt at the agent sight
            // unseen.
            self.iterate_draft = self.build_iterate_prompt();
            self.open_editor_requested = Some(EditorTarget::IteratePrompt);
        } else if k.is(&key, Action::ReviewCopyPrompt) {
            // For running the actual agent in a separate terminal/session
            // instead of hoot's embedded one: builds the exact same
            // prompt as Iterate, but puts it on the system clipboard
            // instead of sending it anywhere.
            let prompt = self.build_iterate_prompt();
            self.review_status = Some(match crate::clipboard::copy(&prompt) {
                Ok(()) => {
                    let n = self.notes_queued();
                    Ok(format!("Copied prompt ({n} note{})", if n == 1 { "" } else { "s" }))
                }
                Err(e) => Err(e),
            });
        } else if k.is(&key, Action::ReviewComment) {
            // With the content pane focused, a comment is scoped to
            // whatever line the cursor is actually on — works the same
            // whether that's plain source or a diff. Otherwise (Tree
            // focused, split-diff view (its before/after columns don't
            // track nav_line at all, so there's no visible indication of
            // which line a comment would even attach to — rather than
            // silently pick a stale or wrong one, fall back), or the
            // cursor's on a line with no clean file-line mapping — a
            // removed line, or a `Focused`-view placeholder — it falls
            // back to a whole-file comment.
            let line_target =
                if self.nav_focus == NavFocus::Content && !self.split_diff { self.current_content_line_number() } else { None };
            if let Some(line) = line_target {
                let rel = crate::gitreview::display_path_of(self.nav_file.strip_prefix(&self.target_dir).unwrap_or(&self.nav_file));
                self.open_note_input(rel, Some(line));
            } else if let Some(i) = self.current_diff_index() {
                let path = self.project.files[i].path.clone();
                self.open_note_input(path, None);
            }
        }
    }

    /// Turns every queued review note and flag into a real prompt for the
    /// agent — all of them, not just whatever file happens to be open right
    /// now. Grouped under their file, with an explicit line annotation, so
    /// the agent can't confuse which file (or line) a piece of feedback is
    /// actually about. This is only ever shown to the user in the iterate
    /// draft editor before sending, never fired off directly.
    fn build_iterate_prompt(&self) -> String {
        let mut prompt = String::from(
            "Here is a review of the current changes for you to work through. Each note below \
             is attached to a specific file, and a specific line when one is given:\n\n",
        );
        let mut any = false;

        let mut order: Vec<&str> = Vec::new();
        let mut grouped: std::collections::HashMap<&str, Vec<&Note>> = std::collections::HashMap::new();
        for note in &self.notes {
            grouped
                .entry(note.path.as_str())
                .or_insert_with(|| {
                    order.push(note.path.as_str());
                    Vec::new()
                })
                .push(note);
        }
        for path in &order {
            any = true;
            prompt.push_str(&format!("File: {path}\n"));
            for note in &grouped[path] {
                match (note.line, note.stale) {
                    // A note whose anchor survived the agent's own edits
                    // carries the line it moved to, not the one it was
                    // written against — see `App::reanchor_notes`.
                    (Some(line), false) => prompt.push_str(&format!("  - Line {line}: {}\n", note.text)),
                    // The line is gone and hoot could not work out where
                    // it went. Sending "Line 47" here would point at
                    // whatever now occupies line 47, which is the one
                    // thing the note is definitely not about — so the note
                    // still goes, with its position given as history
                    // rather than as fact.
                    (Some(line), true) => prompt.push_str(&format!(
                        "  - (was on line {line}, which no longer exists \u{2014} find where this applies now): {}\n",
                        note.text
                    )),
                    (None, _) => prompt.push_str(&format!("  - {}\n", note.text)),
                }
            }
        }
        for file in &self.project.files {
            if file.flagged {
                any = true;
                prompt.push_str(&format!("File: {}\n  - Flagged for rework: please redo this file's change.\n", file.path));
            }
        }
        if !any {
            prompt.push_str("No specific notes were left — please review the current diffs and suggest improvements.\n");
        }
        prompt
    }

    // -------------------------------------------------------------
    // SYMBOL JUMP
    // -------------------------------------------------------------
    fn filtered_symbols(&self) -> Vec<usize> {
        self.symbols.iter().enumerate().filter(|(_, s)| fsnav::fuzzy_match(&self.symbol_filter, &s.name)).map(|(i, _)| i).collect()
    }

    fn on_key_symbol_jump(&mut self, key: KeyEvent) {
        let k = &self.keymap;
        if k.is(&key, Action::SymbolClose) {
            self.overlay = Overlay::None;
        } else if k.is(&key, Action::SymbolJumpTo) {
            if let Some(&i) = self.filtered_symbols().get(self.symbol_index) {
                let sym = &self.symbols[i];
                // `open_in_review` always lands on Context (never
                // Focused): Focused can collapse the exact target line
                // away if it's not near a change, but Context always has
                // every line.
                self.open_in_review(sym.path.clone(), Some(sym.line));
            }
            self.overlay = Overlay::None;
            self.mode = Mode::Review;
        } else if k.is(&key, Action::SymbolUp) {
            self.symbol_index = self.symbol_index.saturating_sub(1);
        } else if k.is(&key, Action::SymbolDown) {
            let len = self.filtered_symbols().len();
            if len > 0 {
                self.symbol_index = (self.symbol_index + 1).min(len - 1);
            }
        } else if key.code == KeyCode::Backspace {
            self.symbol_filter.pop();
            self.symbol_index = 0;
        } else if let KeyCode::Char(c) = key.code {
            self.symbol_filter.push(c);
            self.symbol_index = 0;
        }
    }

    pub fn symbol_results(&self) -> Vec<&SymbolResult> {
        self.filtered_symbols().into_iter().map(|i| &self.symbols[i]).collect()
    }

    // -------------------------------------------------------------
    // FILE FINDER
    // -------------------------------------------------------------
    fn filtered_files(&self) -> Vec<usize> {
        self.tree
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.is_dir)
            .filter(|(_, e)| {
                let rel = crate::gitreview::display_path_of(e.path.strip_prefix(&self.target_dir).unwrap_or(&e.path));
                fsnav::fuzzy_match(&self.file_finder_filter, &rel)
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn file_finder_results(&self) -> Vec<&TreeEntry> {
        self.filtered_files().into_iter().map(|i| &self.tree[i]).collect()
    }

    fn on_key_file_finder(&mut self, key: KeyEvent) {
        let k = &self.keymap;
        if k.is(&key, Action::FinderClose) {
            self.overlay = Overlay::None;
        } else if k.is(&key, Action::FinderOpen) {
            if let Some(&i) = self.filtered_files().get(self.file_finder_index) {
                let path = self.tree[i].path.clone();
                self.open_in_review(path, None);
            }
            self.overlay = Overlay::None;
            self.mode = Mode::Review;
        } else if k.is(&key, Action::FinderUp) {
            self.file_finder_index = self.file_finder_index.saturating_sub(1);
        } else if k.is(&key, Action::FinderDown) {
            let len = self.filtered_files().len();
            if len > 0 {
                self.file_finder_index = (self.file_finder_index + 1).min(len - 1);
            }
        } else if key.code == KeyCode::Backspace {
            self.file_finder_filter.pop();
            self.file_finder_index = 0;
        } else if let KeyCode::Char(c) = key.code {
            self.file_finder_filter.push(c);
            self.file_finder_index = 0;
        }
    }

    // -------------------------------------------------------------
    // NOTES
    // -------------------------------------------------------------
    fn open_note_input(&mut self, path: String, line: Option<usize>) {
        // Captured here rather than when the note is confirmed: this is
        // the file exactly as it was on screen when the user decided to
        // comment on it, and the poll can re-read `source` underneath an
        // open overlay.
        let anchor = line.and_then(|l| crate::data::NoteAnchor::capture(&self.source, l));
        self.note_target = Some(NoteTarget { path, line, anchor });
        self.note_input.clear();
        self.note_cursor = 0;
        self.overlay = Overlay::NoteInput;
    }

    fn on_key_note_input(&mut self, key: KeyEvent) {
        if self.keymap.is(&key, Action::NoteCancel) {
            self.overlay = Overlay::None;
            self.note_target = None;
            return;
        }
        if self.keymap.is(&key, Action::NoteConfirm) {
            self.confirm_note();
            return;
        }
        let char_count = self.note_input.chars().count();
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let byte = char_boundary(&self.note_input, self.note_cursor);
                self.note_input.insert(byte, c);
                self.note_cursor += 1;
            }
            KeyCode::Backspace => {
                if self.note_cursor > 0 {
                    let start = char_boundary(&self.note_input, self.note_cursor - 1);
                    let end = char_boundary(&self.note_input, self.note_cursor);
                    self.note_input.replace_range(start..end, "");
                    self.note_cursor -= 1;
                }
            }
            KeyCode::Delete => {
                if self.note_cursor < char_count {
                    let start = char_boundary(&self.note_input, self.note_cursor);
                    let end = char_boundary(&self.note_input, self.note_cursor + 1);
                    self.note_input.replace_range(start..end, "");
                }
            }
            KeyCode::Left => self.note_cursor = self.note_cursor.saturating_sub(1),
            KeyCode::Right => self.note_cursor = (self.note_cursor + 1).min(char_count),
            KeyCode::Home => self.note_cursor = 0,
            KeyCode::End => self.note_cursor = char_count,
            _ => {}
        }
    }

    fn confirm_note(&mut self) {
        let text = self.note_input.trim().to_string();
        if let Some(target) = self.note_target.take() {
            if !text.is_empty() {
                let note = match target.line {
                    Some(line) => Note::on_line(target.path.clone(), line, target.anchor, text),
                    None => Note::on_file(target.path.clone(), text),
                };
                self.notes.push(note);
                if let Some(f) = self.project.files.iter_mut().find(|f| f.path == target.path) {
                    f.notes += 1;
                }
            }
        }
        self.overlay = Overlay::None;
    }

    // -------------------------------------------------------------
    // QUIT CONFIRMATION
    // -------------------------------------------------------------

    /// `None` if quitting right now loses nothing; otherwise a short
    /// description of what would be lost. Notes, flags, and a drafted
    /// commit message all live only in memory — nothing here is ever
    /// written to disk until it's actually sent to the agent or committed
    /// — so an instant, unconfirmed quit can silently throw away real work.
    pub(crate) fn quit_risk(&self) -> Option<String> {
        let mut parts = Vec::new();
        let notes = self.notes_queued();
        if notes > 0 {
            parts.push(format!("{notes} note{}", if notes == 1 { "" } else { "s" }));
        }
        let flagged = self.files_flagged();
        if flagged > 0 {
            parts.push(format!("{flagged} flagged file{}", if flagged == 1 { "" } else { "s" }));
        }
        if !self.commit_message.trim().is_empty() {
            parts.push("a drafted commit message".to_string());
        }
        if self.agent_running {
            parts.push("an agent turn still running".to_string());
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(", "))
        }
    }

    fn on_key_quit_confirm(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter | KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => self.overlay = Overlay::None,
            _ => {}
        }
    }

    fn on_key_agent_trust_confirm(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.agent_trust_acknowledged = true;
                crate::trust::acknowledge(self.agent_backend, self.demo);
                self.overlay = Overlay::None;
                if let Some((prompt, cwd, tools, purpose)) = self.pending_turn.take() {
                    self.spawn_turn_now(prompt, cwd, tools, purpose);
                }
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                // Declining just drops the held turn — it never ran, so
                // there's nothing to undo. The prompt that led here was
                // already cleared from its input field by the caller
                // before spawn_turn was ever reached, same as if it had
                // actually been sent; retyping it is the cost of a
                // one-time safety prompt, not something worth engineering
                // a restore path for.
                self.pending_turn = None;
                self.overlay = Overlay::None;
            }
            _ => {}
        }
    }

    // -------------------------------------------------------------
    // AGENT
    // -------------------------------------------------------------
    /// Returns true if the key was consumed as text input/send for the
    /// prompt field (so the caller shouldn't fall through to anything else).
    fn on_key_agent_input(&mut self, key: KeyEvent) -> bool {
        if self.keymap.is(&key, Action::AgentSend) {
            // Checked here, not just inside spawn_turn: that check happens
            // after the input's already been taken, so pressing Enter
            // while a turn is running used to silently clear whatever was
            // typed — spawn_turn would no-op on `agent_running`, but the
            // text was already gone by then. Only take (and clear) the
            // input once a turn will actually start.
            if self.agent_running || self.agent_input.trim().is_empty() {
                return true;
            }
            let prompt = std::mem::take(&mut self.agent_input);
            self.agent_cursor = 0;
            self.start_agent_turn(prompt);
            return true;
        }
        let char_count = self.agent_input.chars().count();
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let byte = char_boundary(&self.agent_input, self.agent_cursor);
                self.agent_input.insert(byte, c);
                self.agent_cursor += 1;
                true
            }
            KeyCode::Backspace => {
                if self.agent_cursor > 0 {
                    let start = char_boundary(&self.agent_input, self.agent_cursor - 1);
                    let end = char_boundary(&self.agent_input, self.agent_cursor);
                    self.agent_input.replace_range(start..end, "");
                    self.agent_cursor -= 1;
                }
                true
            }
            KeyCode::Delete => {
                if self.agent_cursor < char_count {
                    let start = char_boundary(&self.agent_input, self.agent_cursor);
                    let end = char_boundary(&self.agent_input, self.agent_cursor + 1);
                    self.agent_input.replace_range(start..end, "");
                }
                true
            }
            KeyCode::Left => {
                self.agent_cursor = self.agent_cursor.saturating_sub(1);
                true
            }
            KeyCode::Right => {
                self.agent_cursor = (self.agent_cursor + 1).min(char_count);
                true
            }
            KeyCode::Home => {
                self.agent_cursor = 0;
                true
            }
            KeyCode::End => {
                self.agent_cursor = char_count;
                true
            }
            _ => false,
        }
    }

    fn on_key_agent(&mut self, key: KeyEvent) {
        let k = &self.keymap;
        if k.is(&key, Action::AgentScrollUp) {
            self.agent_scroll = self.agent_scroll.saturating_add(PAGE_SIZE);
        } else if k.is(&key, Action::AgentScrollDown) {
            self.agent_scroll = self.agent_scroll.saturating_sub(PAGE_SIZE);
        }
    }

    // -------------------------------------------------------------
    // CURATION
    // -------------------------------------------------------------
    fn on_key_curation(&mut self, key: KeyEvent) {
        let n = self.curation_files.len();
        let k = &self.keymap;
        if k.is(&key, Action::CurateUp) {
            self.curation_index = self.curation_index.saturating_sub(1);
            self.curation_hunk_index = 0;
        } else if k.is(&key, Action::CurateDown) && n > 0 {
            self.curation_index = (self.curation_index + 1).min(n - 1);
            self.curation_hunk_index = 0;
        } else if k.is(&key, Action::CurateHunkPrev) {
            self.curation_hunk_index = self.curation_hunk_index.saturating_sub(1);
        } else if k.is(&key, Action::CurateHunkNext) {
            if let Some(f) = self.curation_files.get(self.curation_index) {
                self.curation_hunk_index = (self.curation_hunk_index + 1).min(f.total().saturating_sub(1) as usize);
            }
        } else if k.is(&key, Action::CurateToggleHunk) {
            if let Some(f) = self.curation_files.get_mut(self.curation_index) {
                if let Some(s) = f.hunk_selected.get_mut(self.curation_hunk_index) {
                    *s = !*s;
                    // A `Stale` file's whole point is "look at this again
                    // before trusting it" — touching a hunk at all is that
                    // look, so the badge has done its job.
                    if f.status == Some(crate::theme::FileStatus::Stale) {
                        f.status = None;
                    }
                }
            }
        } else if k.is(&key, Action::CurateDiscardHunk) {
            self.request_discard();
        } else if k.is(&key, Action::CurateOpenInReview) {
            if let Some(cf) = self.curation_files.get(self.curation_index) {
                let path = self.target_dir.join(&cf.path);
                // No line for a file with nothing to curate (binary, a
                // pure rename, ...) — `hunks` is empty there, so this
                // naturally falls back to just opening the file with no
                // specific line to jump to, same as file-finder does.
                let shown_index = self.curation_hunk_index.min(cf.total().saturating_sub(1) as usize);
                let line = self
                    .project
                    .files
                    .iter()
                    .find(|f| f.path == cf.path)
                    .and_then(|f| f.hunks.get(shown_index))
                    .and_then(|h| h.new_file_start_line());
                self.open_in_review(path, line);
            }
        } else if k.is(&key, Action::CurateEditMessage) {
            // Straight to $EDITOR on the real buffer — no in-TUI editing
            // mode, same as the post-generation flow below.
            self.open_editor_requested = Some(EditorTarget::CommitMessage);
        } else if k.is(&key, Action::CurateGenerateMessage) {
            self.generate_commit_message();
        } else if k.is(&key, Action::CurateCommit) {
            self.commit_selected();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn scratch_repo(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hoot-app-test-{label}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        for args in [["init", "-q"].as_slice(), &["config", "user.email", "test@example.com"], &["config", "user.name", "test"]] {
            assert!(Command::new("git").args(args).current_dir(&dir).status().unwrap().success());
        }
        dir
    }

    fn commit_file(dir: &PathBuf, name: &str, content: &str) {
        fs::write(dir.join(name), content).unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(dir).status().unwrap();
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// Two files, each with one hunk, so Review/Curate navigation and
    /// selection have more than one row to move between.
    fn two_file_app(label: &str) -> (App, PathBuf) {
        let dir = scratch_repo(label);
        commit_file(&dir, "a.txt", "a1\na2\n");
        commit_file(&dir, "b.txt", "b1\nb2\n");
        fs::write(dir.join("a.txt"), "a1-changed\na2\n").unwrap();
        fs::write(dir.join("b.txt"), "b1-changed\nb2\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        // Leave both staged-but-uncommitted so `git diff HEAD` still sees them.
        let app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        (app, dir)
    }

    #[test]
    fn f_keys_switch_mode_from_anywhere() {
        let (mut app, dir) = two_file_app("fkeys");
        assert!(app.mode == Mode::Review, "App::new starts in Review");
        app.on_key(key(KeyCode::F(2)));
        assert!(app.mode == Mode::Curation, "F2 is Curate — Review and Curate are front and center, Agent is F3");
        app.on_key(key(KeyCode::F(3)));
        assert!(app.mode == Mode::Agent);
        app.on_key(key(KeyCode::F(1)));
        assert!(app.mode == Mode::Review);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ctrl_c_quits_immediately_when_nothing_is_at_risk() {
        // Ctrl+C is checked before the keymap is even consulted (see
        // on_key's first lines) — it's a fixed safety net, not a binding.
        // `quit_key_is_configurable` below covers the actually-configurable
        // `q` binding separately.
        let dir = scratch_repo("ctrlc");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(ctrl('c'));
        assert!(app.should_quit);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ctrl_c_asks_for_confirmation_too_but_a_second_press_always_gets_you_out() {
        // Ctrl+C isn't an unconditional instant quit any more — it can't
        // silently drop unsent work either, same as 'q'. But it must still
        // be a real escape hatch: pressing it again while the dialog it
        // just opened is showing confirms right away, no second key needed
        // beyond the same Ctrl+C.
        let (mut app, dir) = two_file_app("ctrlc-confirm");
        app.notes.push(Note::on_file("a.txt".to_string(), "don't lose me".to_string()));
        app.project.files[0].notes = 1;

        app.on_key(ctrl('c'));
        assert!(!app.should_quit, "should ask first, not quit outright");
        assert_eq!(app.overlay, Overlay::QuitConfirm);

        app.on_key(ctrl('c'));
        assert!(app.should_quit, "a second Ctrl+C must always get you out");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ctrl_c_bypasses_whatever_overlay_is_open() {
        // The whole point of Ctrl+C living outside the keymap is that it's
        // never swallowed by a focused text field or overlay — it must
        // still reach on_key's dispatch and not, say, get typed into
        // whatever's currently being edited. quit_risk() itself doesn't
        // track in-progress overlay text (only saved notes/flags/etc), so
        // this only asserts the routing, not that unsaved overlay text is
        // itself protected — that's a separate question.
        let dir = scratch_repo("ctrlc-overlay");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Review;
        app.overlay = Overlay::NoteInput;
        app.note_input = "some in-progress note text".to_string();

        app.on_key(ctrl('c'));
        assert!(app.should_quit, "Ctrl+C should still cut through an open overlay when nothing quit_risk tracks is at stake");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn quit_key_is_configurable() {
        let dir = scratch_repo("quit-remap");
        let mut keymap = Keymap::defaults();
        keymap.set(Action::Quit, crate::keymap::KeyChord { code: KeyCode::Char('z'), mods: KeyModifiers::NONE });
        let mut app = App::new(dir.clone(), keymap, AgentBackend::Pi, false);

        app.on_key(key(KeyCode::Char('q')));
        assert!(!app.should_quit, "plain q should no longer quit once remapped");
        app.on_key(key(KeyCode::Char('z')));
        assert!(app.should_quit, "the remapped chord should quit");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_diff_navigation_and_toggles() {
        let (mut app, dir) = two_file_app("hoot-nav");
        assert_eq!(app.tree_index, 0);
        // Both a.txt (index 0) and b.txt (index 1) have changes, so the
        // content pane defaults to Diff — meaning Up/Down move the tree
        // cursor (cycle files) even without explicitly focusing Tree.
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.tree_index, 1);
        app.on_key(key(KeyCode::Down)); // clamps at the last file
        assert_eq!(app.tree_index, 1);
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.tree_index, 0);

        let idx = app.current_diff_index().expect("a.txt has a diff");
        assert_eq!(app.project.files[idx].path, "a.txt");

        app.on_key(key(KeyCode::Char('x')));
        assert!(app.project.files[idx].flagged);
        app.on_key(key(KeyCode::Char('c')));
        assert_eq!(app.overlay, Overlay::NoteInput);
        for c in "please fix this".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.project.files[idx].notes, 1);
        assert_eq!(app.notes.len(), 1);
        assert_eq!(app.notes[0].text, "please fix this");
        app.on_key(key(KeyCode::Char('g')));
        assert!(!app.project.files[idx].flagged);
        assert_eq!(app.project.files[idx].notes, 0);

        assert!(!app.split_diff);
        app.on_key(key(KeyCode::Char('s')));
        assert!(app.split_diff);
        app.on_key(key(KeyCode::Char('u')));
        assert!(!app.split_diff);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_view_shows_the_whole_file_and_focused_collapses_it() {
        let dir = scratch_repo("context-vs-focused");
        let content: String = (1..=30).map(|n| format!("line{n}\n")).collect();
        commit_file(&dir, "big.rs", &content);
        let mut lines: Vec<String> = (1..=30).map(|n| format!("line{n}")).collect();
        lines[0] = "line1-CHANGED".to_string();
        fs::write(dir.join("big.rs"), lines.join("\n") + "\n").unwrap();

        let app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        assert!(app.current_diff_index().is_some(), "big.rs should have a diff");
        assert_eq!(app.content_view, ContentView::Context, "Context is always the default");
        // 30 file lines, but a *modified* line1 is a Removed+Added pair
        // (git diff has no "changed line" concept), so Context has 31 rows:
        // 29 unchanged as Context, plus the removed old line1 and the
        // added new line1.
        assert_eq!(app.content_line_count(), 31);

        let mut app = app;
        app.on_key(key(KeyCode::Char('v')));
        assert_eq!(app.content_view, ContentView::Focused);
        assert!(app.content_line_count() < 31, "Focused should collapse the long unchanged stretch");

        app.on_key(key(KeyCode::Char('v')));
        assert_eq!(app.content_view, ContentView::Context, "'v' toggles back");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn line_scoped_comment_targets_the_correct_file_line_in_a_diff() {
        let dir = scratch_repo("diff-line-comment");
        let content: String = (1..=10).map(|n| format!("line{n}\n")).collect();
        commit_file(&dir, "f.rs", &content);
        let mut lines: Vec<String> = (1..=10).map(|n| format!("line{n}")).collect();
        lines[4] = "line5-CHANGED".to_string(); // line 5, 0-indexed 4
        fs::write(dir.join("f.rs"), lines.join("\n") + "\n").unwrap();

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(key(KeyCode::Tab)); // focus content
                                       // Rows: line1..line4 (context, 4 rows) then the old "line5"
                                       // (removed) then "line5-CHANGED" (added, file line 5) — 5 Downs
                                       // from row 0 lands on that added row.
        for _ in 0..5 {
            app.on_key(key(KeyCode::Down));
        }
        assert_eq!(app.current_content_line_number(), Some(5), "cursor should be on file line 5");

        app.on_key(key(KeyCode::Char('c')));
        assert_eq!(app.overlay, Overlay::NoteInput);
        let target = app.note_target.as_ref().expect("a note target");
        assert_eq!((target.path.as_str(), target.line), ("f.rs", Some(5)));
        assert_eq!(
            target.anchor.as_ref().map(|a| a.line_text()),
            Some("line5-CHANGED"),
            "the note should anchor to the line it was left on, not just its number"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn comment_falls_back_to_whole_file_in_split_diff_view() {
        // Regression: draw_diff_split's before/after columns never track
        // nav_line at all, so there's no visible indication of which line
        // a line-scoped comment would attach to — it must fall back to a
        // whole-file comment there instead of silently (and invisibly)
        // targeting whatever nav_line happens to still hold from before
        // split view was turned on.
        let dir = scratch_repo("split-diff-comment");
        let content: String = (1..=10).map(|n| format!("line{n}\n")).collect();
        commit_file(&dir, "f.rs", &content);
        let mut lines: Vec<String> = (1..=10).map(|n| format!("line{n}")).collect();
        lines[4] = "line5-CHANGED".to_string();
        fs::write(dir.join("f.rs"), lines.join("\n") + "\n").unwrap();

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(key(KeyCode::Tab)); // focus content
        for _ in 0..5 {
            app.on_key(key(KeyCode::Down));
        }
        assert_eq!(app.current_content_line_number(), Some(5), "sanity check: same cursor position as the unified-view test above");

        app.on_key(key(KeyCode::Char('s'))); // switch to split view
        app.on_key(key(KeyCode::Char('c')));
        assert_eq!(app.overlay, Overlay::NoteInput);
        let target = app.note_target.as_ref().expect("a note target");
        assert_eq!((target.path.as_str(), target.line), ("f.rs", None), "should be a whole-file comment, not line 5");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_tree_movement_via_arrows_and_vim_keys() {
        let dir = scratch_repo("navigate-tree");
        commit_file(&dir, "a.rs", "fn a() {}\n");
        commit_file(&dir, "b.rs", "fn b() {}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Review;

        assert_eq!(app.tree_index, 0);
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.tree_index, 1, "arrow keys move the tree");
        app.on_key(key(KeyCode::Char('k')));
        assert_eq!(app.tree_index, 0, "k moves the tree too, not just Up");
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.tree_index, 1, "j moves the tree too, not just Down");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_source_cursor_movement_and_hover_toggle() {
        let dir = scratch_repo("navigate-cursor");
        commit_file(&dir, "main.rs", "fn main() {\n    let x = 1;\n    let y = 2;\n}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Review;

        // Up/Down move the tree until the source pane is focused — arrows
        // never move both at once.
        assert!(app.nav_focus == NavFocus::Tree);
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.nav_line, 0, "still tree-focused, shouldn't touch the cursor");

        app.on_key(key(KeyCode::Tab));
        assert!(app.nav_focus == NavFocus::Content);

        let start_line = app.nav_line;
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.nav_line, start_line + 1);
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.nav_line, start_line);

        let show = app.show_hover;
        app.on_key(key(KeyCode::Char('h')));
        assert_eq!(app.show_hover, !show);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_comment_opens_note_input_targeting_the_current_line() {
        let dir = scratch_repo("navigate-comment");
        commit_file(&dir, "main.rs", "fn main() {\n    let x = 1;\n    let y = 2;\n}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Review;
        app.on_key(key(KeyCode::Tab)); // focus source
        app.on_key(key(KeyCode::Down)); // nav_line == 1

        app.on_key(key(KeyCode::Char('c')));
        assert_eq!(app.overlay, Overlay::NoteInput);
        let target = app.note_target.as_ref().expect("a note target");
        assert_eq!((target.path.as_str(), target.line), ("main.rs", Some(2)));

        for c in "extract this".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.notes.len(), 1);
        assert_eq!(app.notes[0].path, "main.rs");
        assert_eq!(app.notes[0].line, Some(2));
        assert_eq!(app.notes[0].text, "extract this");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn note_input_esc_discards_without_saving() {
        let dir = scratch_repo("note-cancel");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Review;
        app.project.files.push(crate::data::FileEntry {
            path: "x.rs".to_string(),
            hunk_count: 0,
            notes: 0,
            flagged: false,
            hunks: vec![],
            unsupported: None,
            meta: crate::data::FileMeta::modified("x.rs"),
        });
        app.nav_file = dir.join("x.rs"); // so current_diff_index() resolves to it

        app.on_key(key(KeyCode::Char('c')));
        assert_eq!(app.overlay, Overlay::NoteInput);
        app.on_key(key(KeyCode::Char('x')));
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.overlay, Overlay::None);
        assert!(app.notes.is_empty());
        assert_eq!(app.note_target, None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn note_input_editing_supports_cursor_movement_and_backspace() {
        let dir = scratch_repo("note-edit");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Review;
        app.project.files.push(crate::data::FileEntry {
            path: "x.rs".to_string(),
            hunk_count: 0,
            notes: 0,
            flagged: false,
            hunks: vec![],
            unsupported: None,
            meta: crate::data::FileMeta::modified("x.rs"),
        });
        app.nav_file = dir.join("x.rs"); // so current_diff_index() resolves to it
        app.on_key(key(KeyCode::Char('c')));
        for c in "helo".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        // cursor is after "helo"; move left once and insert 'l' -> "hello"
        app.on_key(key(KeyCode::Left));
        app.on_key(key(KeyCode::Char('l')));
        assert_eq!(app.note_input, "hello");

        app.on_key(key(KeyCode::End));
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.note_input, "hell");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_left_right_scroll_the_source_pane_only_when_focused() {
        let dir = scratch_repo("navigate-scroll");
        commit_file(&dir, "main.rs", "fn main() {}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Review;

        app.on_key(key(KeyCode::Right));
        assert_eq!(app.nav_scroll_x, 0, "tree-focused: arrows shouldn't scroll source");

        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Right));
        assert!(app.nav_scroll_x > 0, "source-focused: Right should scroll");
        let scrolled = app.nav_scroll_x;
        app.on_key(key(KeyCode::Left));
        assert!(app.nav_scroll_x < scrolled, "Left should scroll back");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_left_right_also_scroll_while_viewing_a_diff() {
        // Regression: horizontal scroll used to be disabled whenever the
        // open file had a diff, which meant it was disabled for basically
        // every file under review — the diff-in-context view is the
        // default content pane for any changed file, not an edge case.
        let dir = scratch_repo("navigate-scroll-diff");
        commit_file(&dir, "long.rs", "short\n");
        fs::write(dir.join("long.rs"), "a very much longer line than before, changed\n").unwrap();
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Review;
        assert!(app.current_diff_index().is_some(), "long.rs should have a diff");

        app.on_key(key(KeyCode::Tab)); // focus content
        app.on_key(key(KeyCode::Right));
        assert!(app.nav_scroll_x > 0, "content-focused: Right should scroll even while viewing a diff");
        let scrolled = app.nav_scroll_x;
        app.on_key(key(KeyCode::Left));
        assert!(app.nav_scroll_x < scrolled, "Left should scroll back");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_page_up_down_and_home_end_in_source() {
        let dir = scratch_repo("navigate-page");
        let content: String = (1..=60).map(|n| format!("line{n}\n")).collect();
        commit_file(&dir, "big.rs", &content);
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Review;
        app.on_key(key(KeyCode::Tab)); // focus source
        assert!(app.nav_focus == NavFocus::Content);

        app.on_key(key(KeyCode::PageDown));
        assert_eq!(app.nav_line, PAGE_SIZE);
        app.on_key(key(KeyCode::PageDown));
        assert_eq!(app.nav_line, PAGE_SIZE * 2);
        app.on_key(key(KeyCode::PageUp));
        assert_eq!(app.nav_line, PAGE_SIZE);

        app.on_key(key(KeyCode::End));
        assert_eq!(app.nav_line, 59); // 60 lines, 0-indexed
        app.on_key(key(KeyCode::Home));
        assert_eq!(app.nav_line, 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_page_down_clamps_at_the_end_of_a_short_file() {
        let dir = scratch_repo("navigate-page-short");
        commit_file(&dir, "small.rs", "line1\nline2\nline3\n");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Review;
        app.on_key(key(KeyCode::Tab));

        app.on_key(key(KeyCode::PageDown));
        assert_eq!(app.nav_line, 2, "should clamp to the last line, not overshoot");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_enter_focuses_source_and_opening_a_file_resets_scroll() {
        let dir = scratch_repo("navigate-open-focus");
        commit_file(&dir, "a.rs", "fn a() {}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Review;
        app.nav_scroll_x = 12;

        app.on_key(key(KeyCode::Enter));
        assert!(app.nav_focus == NavFocus::Content);
        assert_eq!(app.nav_scroll_x, 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn app_new_opens_on_the_first_changed_file_not_the_alphabetically_first_tree_entry() {
        // Regression: nav_file used to just be the first tree entry
        // (alphabetical order), regardless of whether it had any changes
        // — so Review could easily open on a completely unrelated,
        // unchanged file instead of the one thing actually worth looking
        // at first.
        let dir = scratch_repo("open-on-changed-file");
        commit_file(&dir, "a-unchanged.rs", "fn a() {}\n"); // alphabetically first, but clean
        commit_file(&dir, "z-changed.rs", "fn z() {}\n");
        fs::write(dir.join("z-changed.rs"), "fn z() { /* edited */ }\n").unwrap();

        let app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        assert_eq!(app.nav_file, dir.join("z-changed.rs"));
        assert_eq!(app.tree[app.tree_index].path, dir.join("z-changed.rs"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_enter_opens_the_selected_tree_file() {
        let dir = scratch_repo("navigate-open");
        commit_file(&dir, "a.rs", "fn a() {}\n");
        commit_file(&dir, "b.rs", "fn b() {}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Review;

        // No subdirectories, so the tree is just [a.rs, b.rs] in that order;
        // App::new() opens a.rs by default. Moving down and pressing Enter
        // should switch the open file to b.rs.
        assert_eq!(app.nav_file.file_name().unwrap(), "a.rs");
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.nav_file.file_name().unwrap(), "b.rs");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn curation_navigation_and_edit_message_requests_the_editor() {
        let (mut app, dir) = two_file_app("curate-edit");
        app.mode = Mode::Curation;
        assert_eq!(app.curation_index, 0);
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.curation_index, 1);
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.curation_index, 0);

        let was_fully_selected = app.curation_files[0].selected() == app.curation_files[0].total();
        app.on_key(key(KeyCode::Char(' ')));
        let now_fully_selected = app.curation_files[0].selected() == app.curation_files[0].total();
        assert_eq!(now_fully_selected, !was_fully_selected);

        // 'e' opens $EDITOR directly on the real buffer — no in-TUI typing
        // mode; main.rs's event loop is what actually suspends the TUI and
        // runs the editor, so here we just check the request was made.
        assert_eq!(app.open_editor_requested, None);
        app.on_key(key(KeyCode::Char('e')));
        assert_eq!(app.open_editor_requested, Some(EditorTarget::CommitMessage));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn curate_open_in_review_positions_both_the_tree_and_content_panes() {
        let (mut app, dir) = two_file_app("curate-open-in-review");
        app.mode = Mode::Curation;
        assert_eq!(app.curation_files[1].path, "b.txt");
        app.curation_index = 1; // b.txt, not whatever App::new happened to open in Review

        app.on_key(key(KeyCode::Char('r')));

        assert!(app.mode == Mode::Review, "expected Review mode");
        assert_eq!(app.nav_file, dir.join("b.txt"), "content pane should have opened b.txt");
        assert_eq!(app.tree[app.tree_index].path, dir.join("b.txt"), "tree/explorer pane should be positioned on b.txt too");
        assert_eq!(app.content_view, ContentView::Context);
        assert!(app.nav_focus == NavFocus::Content);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn curate_open_in_review_on_a_deleted_file_lands_on_its_directory_not_a_stale_row() {
        // Regression: a deleted file still has a real diff for Curate to
        // show (all-removed hunks), but fsnav::build_tree walks the live
        // filesystem, so the file itself is never in `self.tree` to
        // position on. Falling back to whatever `tree_index` happened to
        // be before the jump left an unrelated file looking selected/
        // highlighted in the sidebar while the content pane had already
        // moved to the deleted file. The containing directory (still on
        // disk, still in the tree) is the fix's fallback target.
        let dir = scratch_repo("open-in-review-deleted");
        commit_file(&dir, "a.txt", "a1\na2\n");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/gone.txt"), "g1\ng2\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "add sub/gone.txt"]).current_dir(&dir).status().unwrap();

        fs::write(dir.join("a.txt"), "a1-changed\na2\n").unwrap();
        fs::remove_file(dir.join("sub/gone.txt")).unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        assert!(!app.tree.iter().any(|e| e.path == dir.join("sub/gone.txt")), "deleted file shouldn't be in the tree");
        assert!(app.tree.iter().any(|e| e.path == dir.join("sub") && e.is_dir), "its directory should still be in the tree");

        app.mode = Mode::Curation;
        let gone_index = app.curation_files.iter().position(|cf| cf.path == "sub/gone.txt").expect("sub/gone.txt in curation_files");
        app.curation_index = gone_index;
        // Park the tree cursor somewhere unrelated first, so a fallback to
        // "leave it wherever it was" would be observably wrong.
        app.tree_index = app.tree.iter().position(|e| e.path == dir.join("a.txt")).unwrap();

        app.on_key(key(KeyCode::Char('r')));

        assert!(app.mode == Mode::Review, "expected Review mode");
        assert_eq!(app.nav_file, dir.join("sub/gone.txt"), "content pane should have opened the deleted file");
        assert_eq!(app.tree[app.tree_index].path, dir.join("sub"), "tree should fall back to the containing directory");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn curate_open_in_review_lands_on_the_hunks_own_line_not_just_line_one() {
        // two_file_app's single-hunk files don't distinguish "opened the
        // file" from "opened it at the right line" — a file with two
        // far-apart hunks does.
        let dir = scratch_repo("curate-open-in-review-line");
        let original: String = (1..=20).map(|n| format!("line{n}\n")).collect();
        commit_file(&dir, "f.txt", &original);
        let mut lines: Vec<String> = (1..=20).map(|n| format!("line{n}")).collect();
        lines[0] = "line1-CHANGED".to_string();
        lines[19] = "line20-CHANGED".to_string();
        fs::write(dir.join("f.txt"), lines.join("\n") + "\n").unwrap();

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Curation;
        assert_eq!(app.curation_files[0].total(), 2, "expected two separate hunks");
        app.curation_hunk_index = 1; // the second hunk, around line20

        app.on_key(key(KeyCode::Char('r')));

        assert!(app.mode == Mode::Review, "expected Review mode");
        assert_eq!(app.nav_file, dir.join("f.txt"));
        // f.txt has an active diff, so `nav_line` indexes the Context
        // view's diff-line reconstruction, not `app.source` directly —
        // `current_content_line_number` resolves either case to the real
        // file line. The hunk's own declared start (17, confirmed against
        // a real `git diff`: 3 lines of leading context before line 20
        // itself) is the thing actually worth asserting on here — landed
        // on the *second* hunk, not silently defaulted to the first (1).
        assert_eq!(app.current_content_line_number(), Some(17));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn curation_toggles_only_the_currently_shown_hunk_not_all_of_them() {
        let dir = scratch_repo("curate-multi-hunk");
        let content: String = (1..=20).map(|n| format!("line{n}\n")).collect();
        commit_file(&dir, "f.txt", &content);
        // Two far-apart single-line changes -> two separate hunks.
        let mut lines: Vec<String> = (1..=20).map(|n| format!("line{n}")).collect();
        lines[0] = "line1-CHANGED".to_string();
        lines[19] = "line20-CHANGED".to_string();
        fs::write(dir.join("f.txt"), lines.join("\n") + "\n").unwrap();

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Curation;
        assert_eq!(app.curation_files[0].total(), 2, "expected two separate hunks");
        assert_eq!(app.curation_hunk_index, 0);
        assert!(app.curation_files[0].hunk_selected[0], "both start selected");
        assert!(app.curation_files[0].hunk_selected[1], "both start selected");

        // Deselecting hunk 0 shouldn't touch hunk 1.
        app.on_key(key(KeyCode::Char(' ')));
        assert!(!app.curation_files[0].hunk_selected[0]);
        assert!(app.curation_files[0].hunk_selected[1], "the other hunk should be untouched");

        // Move to hunk 1 and deselect it independently.
        app.on_key(key(KeyCode::Right));
        assert_eq!(app.curation_hunk_index, 1);
        app.on_key(key(KeyCode::Char(' ')));
        assert!(!app.curation_files[0].hunk_selected[1]);

        // Right again clamps at the last hunk.
        app.on_key(key(KeyCode::Right));
        assert_eq!(app.curation_hunk_index, 1);

        app.on_key(key(KeyCode::Left));
        assert_eq!(app.curation_hunk_index, 0);
        app.on_key(key(KeyCode::Left)); // clamps at 0
        assert_eq!(app.curation_hunk_index, 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_input_accumulates_text_without_spawning_pi() {
        let dir = scratch_repo("agent-input");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Agent;

        for c in "hello".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.agent_input, "hello");
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.agent_input, "hell");
        // Deliberately not pressing Enter here — that would spawn a real
        // `pi` subprocess, which belongs in a slower, opt-in test if ever.

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn enter_while_a_turn_is_running_does_not_discard_the_typed_prompt() {
        // Regression: the input used to be taken (and thus cleared)
        // *before* checking whether a turn could actually start, so typing
        // a follow-up while one was still running silently lost it — Enter
        // cleared the box, spawn_turn no-op'd on agent_running, and the
        // text was already gone.
        let dir = scratch_repo("agent-input-race");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Agent;
        app.agent_running = true; // simulate a turn already in flight

        for c in "still typing".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.agent_input, "still typing");

        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.agent_input, "still typing", "Enter shouldn't clear the prompt while a turn is running");
        assert!(app.agent_running, "and definitely shouldn't have started a second turn");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn esc_cancels_a_running_turn_from_review_mode_too() {
        // Regression: Esc only cancelled a running turn from Agent or
        // Curate — switching to Review to look something up while a turn
        // from Agent kept running in the background left no way to stop
        // it again without switching back first. Cancellation is now
        // checked globally, before mode dispatch, whenever a turn is
        // actually running.
        let (mut app, dir) = two_file_app("esc-cancel-review");
        app.mode = Mode::Review;
        app.agent_running = true; // simulate a turn already in flight

        app.on_key(key(KeyCode::Esc));

        // cancel_agent_turn() only ever signals the kill and marks intent
        // — agent_running itself only flips once the backend process
        // actually exits and the event channel disconnects (finish_turn),
        // which nothing drives here without a real spawned session.
        assert!(app.agent_cancelled, "Esc should have marked the turn as cancelling");
        assert!(app.mode == Mode::Review, "cancelling shouldn't itself change modes");
        assert!(
            app.transcript.iter().any(|l| l.text.contains("Cancelling")),
            "expected cancellation feedback in the transcript: {:?}",
            app.transcript.iter().map(|l| &l.text).collect::<Vec<_>>()
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn symbol_jump_close_discards_filter_state_choice() {
        let dir = scratch_repo("symjump-close");
        commit_file(&dir, "lib.rs", "fn foo() {}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(ctrl('k'));
        app.on_key(key(KeyCode::Char('f')));
        assert_eq!(app.symbol_filter, "f");
        app.on_key(key(KeyCode::Esc));
        assert!(app.overlay == Overlay::None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn generate_message_with_nothing_selected_does_not_spawn_pi() {
        let (mut app, dir) = two_file_app("gen-none-selected");
        app.mode = Mode::Curation;
        for cf in &mut app.curation_files {
            cf.hunk_selected.fill(false);
        }
        app.on_key(key(KeyCode::Char('g')));
        assert!(!app.agent_running);
        assert_eq!(app.commit_message_status.as_deref(), Some("Nothing selected to summarize."));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn generate_message_in_demo_mode_does_not_spawn_a_real_agent_turn() {
        // Pressing `g` out of curiosity while exploring --demo must not
        // spend a real API call on a throwaway repo. The guard is the
        // explicit demo flag now, not "the review data looks fake" — the
        // demo's diff is a real git diff, which is the whole point of it.
        let (_dir, repo) = crate::demo::create_repo().expect("demo repo");
        let mut app = App::new(repo, Keymap::defaults(), AgentBackend::Pi, true);
        assert!(!app.curation_files.is_empty(), "the demo repo should have something selected to summarize");

        app.mode = Mode::Curation;
        app.on_key(key(KeyCode::Char('g')));

        assert!(!app.agent_running, "demo mode must never spawn a real agent turn");
        assert!(app.commit_message_status.as_deref().is_some_and(|s| s.starts_with("Demo mode")), "{:?}", app.commit_message_status);
    }

    #[test]
    fn an_unacknowledged_trust_prompt_holds_the_turn_instead_of_spawning() {
        // App::new defaults agent_trust_acknowledged to true (see its doc
        // comment — real per-machine state is only applied by main.rs) —
        // forcing it false here is what actually exercises the gate, the
        // same way a genuinely first-ever launch would.
        let (mut app, dir) = two_file_app("trust-gate-hold");
        app.mode = Mode::Agent;
        app.agent_trust_acknowledged = false;

        for c in "hello".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));

        assert_eq!(app.overlay, Overlay::AgentTrustConfirm, "should hold for confirmation, not proceed");
        assert!(!app.agent_running, "must not have attempted a real spawn yet");
        assert!(app.transcript.is_empty(), "the prompt shouldn't appear in the transcript until it actually sends");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn declining_the_trust_prompt_discards_the_held_turn() {
        let (mut app, dir) = two_file_app("trust-gate-decline");
        app.mode = Mode::Agent;
        app.agent_trust_acknowledged = false;

        for c in "hello".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.overlay, Overlay::AgentTrustConfirm);

        app.on_key(key(KeyCode::Esc));

        assert_eq!(app.overlay, Overlay::None);
        assert!(!app.agent_trust_acknowledged, "declining shouldn't count as acknowledging");
        assert!(!app.agent_running);
        assert!(app.transcript.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn generate_commit_message_is_also_held_for_trust_confirmation() {
        // The gate lives in spawn_turn, shared by both call paths — this
        // covers the Curate 'g' path specifically, not just Agent chat.
        let (mut app, dir) = two_file_app("trust-gate-curate");
        app.mode = Mode::Curation;
        app.agent_trust_acknowledged = false;

        app.on_key(key(KeyCode::Char('g')));

        assert_eq!(app.overlay, Overlay::AgentTrustConfirm);
        assert!(!app.agent_running);
        assert_eq!(app.commit_message_status, None, "no real turn should have started yet");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn selected_diff_text_includes_only_selected_hunks() {
        let (mut app, dir) = two_file_app("diff-text");
        // a.txt selected (default), b.txt deselected.
        app.curation_files[1].hunk_selected.fill(false);
        let text = app.selected_diff_text();
        assert!(text.contains("a.txt"), "{text}");
        assert!(text.contains("a1-changed"), "{text}");
        assert!(!text.contains("b1-changed"), "{text}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn selected_diff_text_carries_gits_own_header_so_a_pure_rename_isnt_contentless() {
        // A rename with no content change has no hunks at all — everything
        // that says what happened lives in the extended header. Drafting a
        // commit message from a reconstructed `diff --git`/`---`/`+++`
        // triple threw exactly that away and asked the model to summarize
        // a stub.
        let dir = scratch_repo("rename-header");
        commit_file(&dir, "before.txt", "unchanged contents\n");
        Command::new("git").args(["mv", "before.txt", "after.txt"]).current_dir(&dir).status().unwrap();
        let app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);

        let text = app.selected_diff_text();
        assert!(text.contains("rename from before.txt"), "{text}");
        assert!(text.contains("rename to after.txt"), "{text}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn iterate_prompt_groups_all_notes_by_file_with_line_annotations() {
        // Notes are sent unconditionally, for every file with notes — no
        // per-file "selected" gate to remember to toggle first.
        let (mut app, dir) = two_file_app("iterate-prompt");

        app.notes.push(Note::on_line("a.txt".to_string(), 2, None, "tighten this up".to_string()));
        app.notes.push(Note::on_file("a.txt".to_string(), "consider a rename".to_string()));
        app.notes.push(Note::on_line("b.txt".to_string(), 1, None, "this one too".to_string()));

        let prompt = app.build_iterate_prompt();
        assert!(prompt.contains("File: a.txt"), "{prompt}");
        assert!(prompt.contains("Line 2: tighten this up"), "{prompt}");
        assert!(prompt.contains("consider a rename"), "{prompt}");
        assert!(prompt.contains("File: b.txt"), "{prompt}");
        assert!(prompt.contains("Line 1: this one too"), "{prompt}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn iterate_prompt_includes_all_flagged_files_and_honest_empty_state() {
        let (mut app, dir) = two_file_app("iterate-prompt-flagged");
        app.project.files[0].flagged = true;

        let prompt = app.build_iterate_prompt();
        assert!(prompt.contains("File: a.txt"), "{prompt}");
        assert!(prompt.contains("Flagged for rework"), "{prompt}");
        assert!(!prompt.contains("b.txt"), "{prompt}");

        app.project.files[0].flagged = false;
        let empty_prompt = app.build_iterate_prompt();
        assert!(empty_prompt.contains("No specific notes were left"), "{empty_prompt}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_prompt_key_sets_a_clipboard_status() {
        // For running the real agent in a separate terminal instead of
        // hoot's embedded one: 'y' builds the same prompt as Iterate but
        // copies it instead of sending it. Whether the environment
        // actually has a clipboard tool on PATH varies (CI, headless
        // Linux), so this only checks that the attempt is made and
        // recorded — clipboard::copy's own fallback/error behavior is
        // covered directly in clipboard.rs.
        let (mut app, dir) = two_file_app("copy-prompt");
        assert!(app.review_status.is_none());

        app.mode = Mode::Review;
        app.on_key(key(KeyCode::Char('y')));
        assert!(app.review_status.is_some());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_file_notes_only_touches_the_open_files_notes_and_not_its_flag() {
        let (mut app, dir) = two_file_app("clear-file-notes");
        app.mode = Mode::Review;
        app.notes.push(Note::on_file("a.txt".to_string(), "on a".to_string()));
        app.notes.push(Note::on_file("b.txt".to_string(), "on b".to_string()));
        app.project.files[0].notes = 1;
        app.project.files[0].flagged = true;
        app.project.files[1].notes = 1;
        app.nav_file = dir.join("a.txt"); // so current_diff_index() resolves to a.txt

        app.on_key(key(KeyCode::Char('d')));

        assert_eq!(app.project.files[0].notes, 0, "a.txt's notes should be cleared");
        assert!(app.project.files[0].flagged, "clearing notes shouldn't touch the flag");
        assert_eq!(app.project.files[1].notes, 1, "b.txt's notes are untouched");
        assert_eq!(app.notes.len(), 1);
        assert_eq!(app.notes[0].path, "b.txt");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_all_notes_wipes_every_file_but_leaves_flags_alone() {
        let (mut app, dir) = two_file_app("clear-all-notes");
        app.mode = Mode::Review;
        app.notes.push(Note::on_file("a.txt".to_string(), "on a".to_string()));
        app.notes.push(Note::on_file("b.txt".to_string(), "on b".to_string()));
        app.project.files[0].notes = 1;
        app.project.files[0].flagged = true;
        app.project.files[1].notes = 1;

        app.on_key(key(KeyCode::Char('D')));

        assert_eq!(app.project.files[0].notes, 0);
        assert_eq!(app.project.files[1].notes, 0);
        assert!(app.project.files[0].flagged, "clearing notes shouldn't touch flags");
        assert!(app.notes.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn quit_is_immediate_when_nothing_is_at_risk() {
        let (mut app, dir) = two_file_app("quit-nothing-at-risk");
        assert!(app.quit_risk().is_none());

        app.on_key(key(KeyCode::Char('q')));
        assert!(app.should_quit, "no notes/flags/draft — q should quit immediately");
        assert_eq!(app.overlay, Overlay::None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn quit_asks_for_confirmation_when_notes_are_queued() {
        let (mut app, dir) = two_file_app("quit-with-notes");
        app.notes.push(Note::on_file("a.txt".to_string(), "don't lose me".to_string()));
        app.project.files[0].notes = 1;
        assert!(app.quit_risk().is_some());

        app.on_key(key(KeyCode::Char('q')));
        assert!(!app.should_quit, "should ask first, not quit outright");
        assert_eq!(app.overlay, Overlay::QuitConfirm);

        // Esc backs out without quitting or losing the note.
        app.on_key(key(KeyCode::Esc));
        assert!(!app.should_quit);
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.notes.len(), 1);

        // Asking again and confirming (either Enter or 'q' again) quits.
        app.on_key(key(KeyCode::Char('q')));
        app.on_key(key(KeyCode::Enter));
        assert!(app.should_quit);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn quit_risk_flags_a_drafted_commit_message_and_a_running_agent_too() {
        let (mut app, dir) = two_file_app("quit-risk-other-state");
        assert!(app.quit_risk().is_none());

        app.commit_message = "fix: the thing".to_string();
        assert!(app.quit_risk().unwrap().contains("drafted commit message"));
        app.commit_message.clear();

        app.agent_running = true;
        assert!(app.quit_risk().unwrap().contains("agent turn"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn notes_queued_counts_a_note_on_a_file_with_no_diff() {
        // Regression: notes_queued() used to sum FileEntry.notes, a
        // counter that only exists on files currently in self.project.files
        // (i.e. ones with an active diff) — but a line-scoped comment can
        // be left on any file browsed in source view, changed or not. That
        // silently undercounted, which let quit_risk (and the "N notes
        // queued" status line) miss real queued notes on clean files.
        let (mut app, dir) = two_file_app("notes-queued-clean-file");
        assert_eq!(app.notes_queued(), 0);

        app.notes.push(Note::on_line("clean-file-with-no-diff.txt".to_string(), 3, None, "a note".to_string()));
        assert_eq!(app.notes_queued(), 1, "should count a note even on a file with no active diff");
        assert!(app.quit_risk().is_some(), "and quit_risk should see it too");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_review_from_disk_picks_up_an_external_change_and_keeps_flags() {
        let (mut app, dir) = two_file_app("sync-review");
        app.project.files[0].flagged = true;
        let before = app.project.files[0].hunks[0].lines.len();

        // Simulate a change made outside hoot (another `pi` run, an
        // editor, plain `git`) — a plain fs::write, not through the app.
        fs::write(dir.join("a.txt"), "a1-changed\na2\na3-new-line\n").unwrap();

        app.sync_review_from_disk();

        assert!(app.project.files[0].hunks[0].lines.len() > before, "should pick up the new line");
        assert!(app.project.files[0].flagged, "flagged should survive an external content change");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_review_from_disk_is_a_noop_when_the_diff_is_unchanged() {
        let (mut app, dir) = two_file_app("sync-review-noop");
        app.curation_files[0].hunk_selected[0] = false;

        app.sync_review_from_disk();

        assert!(!app.curation_files[0].hunk_selected[0], "an unchanged diff shouldn't reset curation toggles");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_review_from_disk_preserves_a_deselected_hunk_in_a_file_that_did_not_change() {
        // Regression: any diff change anywhere in the repo used to replace
        // curation_files wholesale, silently reselecting every hunk in
        // every file — including ones a user had deliberately deselected
        // in a file the external change never touched at all.
        let (mut app, dir) = two_file_app("sync-review-preserve-selection");
        assert_eq!(app.curation_files[1].path, "b.txt");
        app.curation_files[1].hunk_selected[0] = false; // deselect b.txt's hunk

        // Change only a.txt externally — b.txt's hunks are untouched.
        fs::write(dir.join("a.txt"), "a1-changed\na2\na3-new-line\n").unwrap();
        app.sync_review_from_disk();

        assert_eq!(app.curation_files[1].path, "b.txt");
        assert!(!app.curation_files[1].hunk_selected[0], "b.txt's deselected hunk should survive a.txt changing");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_review_from_disk_marks_a_changed_file_stale_instead_of_reselecting_everything() {
        // Regression: a file whose *own* hunks shift shape (not just an
        // unrelated file elsewhere in the repo) used to fall through to
        // gitreview::load's normal "everything selected" default — so a
        // background edit to the file the user was actively curating could
        // silently re-include content they never reviewed. Index-for-index
        // hunk matching genuinely can't be trusted across a real shape
        // change, so the safer response is deselect-and-flag, not
        // reselect-everything.
        let (mut app, dir) = two_file_app("sync-review-stale");
        assert_eq!(app.curation_files[0].path, "a.txt");
        assert!(app.curation_files[0].hunk_selected[0], "starts selected");
        assert_eq!(app.curation_files[0].status, None);

        // Change a.txt's own content enough to shift its hunk shape.
        fs::write(dir.join("a.txt"), "a1-changed\na2\na3-new-line\n").unwrap();
        app.sync_review_from_disk();

        assert_eq!(app.curation_files[0].path, "a.txt");
        assert!(!app.curation_files[0].hunk_selected[0], "a changed file's hunks should not silently reselect");
        assert_eq!(app.curation_files[0].status, Some(crate::theme::FileStatus::Stale));

        // Touching any hunk is the re-review the badge was asking for.
        app.mode = Mode::Curation;
        app.curation_index = 0;
        app.curation_hunk_index = 0;
        app.on_key(key(KeyCode::Char(' ')));
        assert_eq!(app.curation_files[0].status, None, "toggling a hunk should clear the stale badge");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn committing_is_refused_while_an_agent_turn_is_running() {
        // The agent writes to the same working tree Curate is about to
        // commit from, and nothing it has written mid-turn has been
        // reviewed yet. gitcommit's freshness check would catch a file that
        // had already changed, but the turn is still in flight — the next
        // write may land between the check and the commit. The only honest
        // answer is not to start.
        let dir = scratch_repo("commit-during-turn");
        fs::write(dir.join("f.txt"), "one\n").unwrap();
        for args in [["add", "-A"].as_slice(), &["commit", "-q", "-m", "init"]] {
            Command::new("git").args(args).current_dir(&dir).status().unwrap();
        }
        fs::write(dir.join("f.txt"), "one\ntwo\n").unwrap();

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Curation;
        app.commit_message = "Add a line".to_string();
        app.agent_running = true;

        app.on_key(key(KeyCode::Char('c')));

        let err = app.last_commit.as_ref().expect("a refusal, not silence").as_ref().unwrap_err();
        assert!(err.contains("agent turn is running"), "{err}");

        let log = Command::new("git").args(["log", "--oneline"]).current_dir(&dir).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).lines().count(), 1, "no commit should have landed");
        let staged = Command::new("git").args(["diff", "--cached"]).current_dir(&dir).output().unwrap().stdout;
        assert!(staged.is_empty(), "and nothing should have been staged either");

        // Once the turn is over, the same keypress commits normally.
        app.agent_running = false;
        app.on_key(key(KeyCode::Char('c')));
        assert!(app.last_commit.as_ref().unwrap().is_ok(), "{:?}", app.last_commit);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_end_refreshes_review_immediately() {
        let (mut app, dir) = two_file_app("agent-end-edit-mode");
        app.mode = Mode::Agent;

        // Every turn writes straight to target_dir — simulate that by
        // editing the file directly, the same as what a real `pi` write
        // tool call would have just done.
        fs::write(dir.join("a.txt"), "a1-changed\na2\na3-written-by-the-agent\n").unwrap();
        let before = app.project.files[0].hunks[0].lines.len();

        // Real backend processes signal "the turn is over" by exiting (the
        // event channel disconnecting), not through any particular JSON
        // event — pi sends AgentEnd and opencode doesn't, so this is
        // triggered by `finish_turn` directly rather than by feeding
        // AgentEnd through `apply_agent_event`. See finish_turn's doc
        // comment.
        app.finish_turn();

        assert!(!app.agent_running);
        assert!(
            app.project.files[0].hunks[0].lines.len() > before,
            "Review should already reflect the write, no waiting for the next poll"
        );
        assert!(
            app.transcript.iter().any(|l| l.text.contains("file") && l.text.contains("changed")),
            "{:?}",
            app.transcript.iter().map(|l| &l.text).collect::<Vec<_>>()
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_tree_from_disk_picks_up_an_external_edit_to_the_open_file() {
        let dir = scratch_repo("sync-nav-source");
        commit_file(&dir, "main.rs", "fn main() {\n    let x = 1;\n}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Review;
        assert_eq!(app.source.len(), 3);

        fs::write(dir.join("main.rs"), "fn main() {\n    let x = 1;\n    let y = 2;\n}\n").unwrap();
        app.sync_tree_from_disk();

        assert_eq!(app.source.len(), 4, "should re-read the file that changed on disk");
        assert!(app.source.iter().any(|l| l.contains("let y = 2")), "{:?}", app.source);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_tree_from_disk_picks_up_a_new_file_added_externally() {
        let dir = scratch_repo("sync-nav-tree");
        commit_file(&dir, "a.rs", "fn a() {}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Review;
        let before = app.tree.len();

        fs::write(dir.join("b.rs"), "fn b() {}\n").unwrap();
        app.sync_tree_from_disk();

        assert_eq!(app.tree.len(), before + 1, "should pick up the new file on disk");
        assert!(app.tree.iter().any(|e| e.label == "b.rs"), "{:?}", app.tree.iter().map(|e| &e.label).collect::<Vec<_>>());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_message_purpose_routes_text_and_end_away_from_the_transcript() {
        let dir = scratch_repo("purpose-text");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        let transcript_len_before = app.transcript.len();

        app.agent_purpose = TurnPurpose::CommitMessage;
        app.commit_message_status = Some("Generating\u{2026}".to_string());
        app.apply_agent_event(AgentEvent::Text("feat: add the thing".to_string()));
        assert_eq!(app.commit_message, "feat: add the thing");
        assert_eq!(app.transcript.len(), transcript_len_before, "should not touch the chat transcript");

        app.finish_turn();
        assert_eq!(app.open_editor_requested, Some(EditorTarget::CommitMessage), "completing generation should request the editor");
        assert!(app.commit_message_status.is_none());
        assert!(!app.agent_running);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_message_purpose_routes_errors_to_status_not_transcript() {
        let dir = scratch_repo("purpose-error");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        let transcript_len_before = app.transcript.len();

        app.agent_purpose = TurnPurpose::CommitMessage;
        app.apply_agent_event(AgentEvent::Error("pi exploded".to_string()));
        assert!(app.commit_message_status.as_deref().unwrap().contains("pi exploded"));
        assert_eq!(app.transcript.len(), transcript_len_before);
        assert_eq!(app.open_editor_requested, None, "an error shouldn't open the editor");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_commit_message_generation_keeps_the_error_visible_and_never_opens_a_blank_editor() {
        // Regression: finish_turn (fired once the backend process exits)
        // used to unconditionally clear commit_message_status and open
        // $EDITOR, even when the turn had just errored out — clobbering
        // the error the user was still looking at and popping up an editor
        // on an empty buffer with no explanation.
        let dir = scratch_repo("purpose-error-finish");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.agent_purpose = TurnPurpose::CommitMessage;
        app.agent_running = true;

        app.apply_agent_event(AgentEvent::Error("model unavailable".to_string()));
        app.finish_turn();

        assert!(!app.agent_running);
        assert!(
            app.commit_message_status.as_deref().unwrap_or_default().contains("model unavailable"),
            "the error should still be visible: {:?}",
            app.commit_message_status
        );
        assert_eq!(app.open_editor_requested, None, "shouldn't open the editor on a failed generation");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn opencode_session_id_is_captured_from_the_session_event() {
        // opencode has no equivalent of pi's upfront session file — it
        // assigns a session id itself and hands it back over the stream,
        // which then has to be threaded into every later call. Confirms
        // that capture happens regardless of which turn purpose is active.
        let dir = scratch_repo("opencode-session-capture");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::OpenCode, false);
        assert!(app.opencode_session_id.is_none());

        app.apply_agent_event(AgentEvent::Session("ses_abc123".to_string()));
        assert_eq!(app.opencode_session_id.as_deref(), Some("ses_abc123"));

        app.agent_purpose = TurnPurpose::CommitMessage;
        app.apply_agent_event(AgentEvent::Session("ses_def456".to_string()));
        assert_eq!(app.opencode_session_id.as_deref(), Some("ses_def456"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_finder_fuzzy_filters_and_opens_the_selected_file() {
        let dir = scratch_repo("finder");
        commit_file(&dir, "main.rs", "fn main() {}\n");
        commit_file(&dir, "readme.md", "# hi\n");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);

        app.on_key(ctrl('f'));
        assert!(app.overlay == Overlay::FileFinder);

        // "mnrs" should subsequence-match "main.rs" but not "readme.md".
        for c in "mnrs".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        let results = app.file_finder_results();
        assert_eq!(results.len(), 1, "results = {:?}", results.iter().map(|e| &e.label).collect::<Vec<_>>());
        assert_eq!(results[0].label, "main.rs");

        app.on_key(key(KeyCode::Enter));
        assert!(app.overlay == Overlay::None);
        assert!(app.mode == Mode::Review);
        assert_eq!(app.nav_file.file_name().unwrap(), "main.rs");
        assert!(app.nav_focus == NavFocus::Content);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_finder_esc_closes_without_opening_anything() {
        let dir = scratch_repo("finder-close");
        commit_file(&dir, "a.rs", "fn a() {}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        let original_file = app.nav_file.clone();

        app.on_key(ctrl('f'));
        app.on_key(key(KeyCode::Char('z')));
        app.on_key(key(KeyCode::Esc));
        assert!(app.overlay == Overlay::None);
        assert_eq!(app.nav_file, original_file);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn char_boundary_handles_multibyte_characters() {
        // "café" — the é is 2 bytes, so the boundary for index 4 (past all
        // 4 chars) must land after it, at byte offset 5, not split it.
        let s = "caf\u{e9}"; // "café"
        assert_eq!(char_boundary(s, 0), 0);
        assert_eq!(char_boundary(s, 3), 3); // right before é
        assert_eq!(char_boundary(s, 4), s.len()); // past the end
        assert_eq!(s.len(), 5); // 3 ascii + 2-byte é
    }

    #[test]
    fn agent_input_types_at_cursor_not_just_appends() {
        let dir = scratch_repo("agent-cursor");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Agent;

        for c in "ac".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.agent_input, "ac");
        assert_eq!(app.agent_cursor, 2);

        app.on_key(key(KeyCode::Left));
        assert_eq!(app.agent_cursor, 1);
        app.on_key(key(KeyCode::Char('b')));
        assert_eq!(app.agent_input, "abc", "should insert at the cursor, not append");
        assert_eq!(app.agent_cursor, 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_input_backspace_and_delete() {
        let dir = scratch_repo("agent-bs-del");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Agent;
        for c in "abc".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Left)); // cursor between b and c

        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.agent_input, "ac", "backspace removes the char before the cursor");
        assert_eq!(app.agent_cursor, 1);

        app.on_key(key(KeyCode::Delete));
        assert_eq!(app.agent_input, "a", "delete removes the char at the cursor");
        assert_eq!(app.agent_cursor, 1, "delete shouldn't move the cursor");

        // Backspace/Delete at the boundaries are no-ops, not panics.
        app.on_key(key(KeyCode::Delete));
        assert_eq!(app.agent_input, "a");
        app.on_key(key(KeyCode::Left));
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.agent_input, "a");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_input_home_end_and_arrow_clamping() {
        let dir = scratch_repo("agent-home-end");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Agent;
        for c in "hello".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.agent_cursor, 5);

        app.on_key(key(KeyCode::Right));
        assert_eq!(app.agent_cursor, 5, "Right shouldn't overshoot the end");

        app.on_key(key(KeyCode::Home));
        assert_eq!(app.agent_cursor, 0);
        app.on_key(key(KeyCode::Left));
        assert_eq!(app.agent_cursor, 0, "Left shouldn't underflow past the start");

        app.on_key(key(KeyCode::End));
        assert_eq!(app.agent_cursor, 5);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_input_editing_is_utf8_safe() {
        let dir = scratch_repo("agent-utf8");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Agent;
        for c in "caf\u{e9}".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.agent_input, "caf\u{e9}");
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.agent_input, "caf", "should remove the whole é, not split its bytes");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_send_clears_input_and_resets_cursor() {
        let dir = scratch_repo("agent-send-reset");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Agent;
        for c in "hello".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.agent_input, "");
        assert_eq!(app.agent_cursor, 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_transcript_scroll_up_and_down() {
        // Sending a real turn (which also resets scroll to 0) is
        // deliberately not exercised here, same as elsewhere in this file —
        // it would spawn a real, un-cleaned-up `pi` subprocess.
        let dir = scratch_repo("agent-scroll");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Agent;

        assert_eq!(app.agent_scroll, 0);
        app.on_key(key(KeyCode::PageUp));
        assert_eq!(app.agent_scroll, PAGE_SIZE);
        app.on_key(key(KeyCode::PageUp));
        assert_eq!(app.agent_scroll, PAGE_SIZE * 2);
        app.on_key(key(KeyCode::PageDown));
        assert_eq!(app.agent_scroll, PAGE_SIZE);

        let _ = fs::remove_dir_all(&dir);
    }

    // -------------------------------------------------------------
    // TURN-SCOPED REVIEW
    // -------------------------------------------------------------

    #[test]
    fn turn_scope_is_what_moved_since_the_turn_started_not_the_whole_dirty_tree() {
        // b.txt is already dirty when the turn begins. The turn touches
        // only a.txt. "What this turn wrote" is a.txt — the whole point of
        // the baseline is that b.txt's pre-existing work isn't credited to
        // a turn that never opened it.
        let (mut app, dir) = two_file_app("turn-scope-basic");
        app.run_fake_turn(|| {});
        assert!(app.turn_scope.is_empty(), "a turn that wrote nothing owns nothing");

        app.run_fake_turn(|| fs::write(dir.join("a.txt"), "a1-changed\na2\na3-by-the-agent\n").unwrap());

        assert_eq!(app.turn_scope, vec!["a.txt".to_string()]);
        assert_eq!(app.project.files.len(), 2, "the whole changeset is still two files");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_the_turn_creates_counts_as_this_turns_work() {
        let (mut app, dir) = two_file_app("turn-scope-new-file");
        app.run_fake_turn(|| fs::write(dir.join("c.txt"), "written by the agent\n").unwrap());

        assert_eq!(app.turn_scope, vec!["c.txt".to_string()], "entering the changeset is a change like any other");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn turn_scope_hides_the_files_the_turn_never_touched_from_the_tree() {
        let (mut app, dir) = two_file_app("turn-scope-tree");
        app.run_fake_turn(|| fs::write(dir.join("a.txt"), "a1-changed\na2\na3-by-the-agent\n").unwrap());

        let all_rows = app.visible_tree_rows().len();
        assert_eq!(all_rows, app.tree.len(), "nothing is hidden until the scope says so");

        app.on_key(key(KeyCode::Char('t')));
        assert_eq!(app.review_scope, ReviewScope::Turn);

        let shown: Vec<String> = app.visible_tree_rows().into_iter().map(|i| app.tree[i].label.clone()).collect();
        assert_eq!(shown, vec!["a.txt".to_string()], "b.txt is dirty but not this turn's work: {shown:?}");

        app.on_key(key(KeyCode::Char('t')));
        assert_eq!(app.review_scope, ReviewScope::All);
        assert_eq!(app.visible_tree_rows().len(), all_rows, "toggling back restores the whole tree");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn narrowing_to_a_turn_before_one_has_run_says_so_rather_than_emptying_the_tree() {
        // With no baseline, every file would fall outside "this turn" and
        // the tree would go blank for a reason nothing on screen explains.
        let (mut app, dir) = two_file_app("turn-scope-no-baseline");
        app.on_key(key(KeyCode::Char('t')));

        assert_eq!(app.review_scope, ReviewScope::All, "the scope must not change");
        assert!(app.visible_tree_rows().len() == app.tree.len());
        let status = app.review_status.as_ref().expect("a status message");
        assert!(status.as_ref().err().is_some_and(|e| e.contains("no agent turn has run")), "{status:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_narrowed_tree_moves_one_visible_row_per_keypress() {
        // Regression risk in the filtered tree: `tree_index` still indexes
        // the full scan, so a naive +1 would walk through hidden rows and
        // look like a dead arrow key.
        let dir = scratch_repo("turn-scope-nav");
        for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
            commit_file(&dir, name, "one\n");
        }
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.run_fake_turn(|| {
            for name in ["a.txt", "d.txt"] {
                fs::write(dir.join(name), "one\ntwo\n").unwrap();
            }
        });
        app.on_key(key(KeyCode::Char('t')));

        let rows = app.visible_tree_rows();
        assert_eq!(rows.len(), 2, "only a.txt and d.txt are this turn's");
        app.on_key(key(KeyCode::Home));
        assert_eq!(app.tree_index, rows[0]);
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.tree_index, rows[1], "one Down should cross b.txt and c.txt in a single step");
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.tree_index, rows[1], "and stop at the last visible row");
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.tree_index, rows[0]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn finishing_a_turn_scopes_review_to_that_turns_files_and_says_how_many() {
        let (mut app, dir) = two_file_app("turn-scope-finish");
        app.mode = Mode::Agent;
        app.turn_baseline = Some(crate::gitreview::diff_files(&dir).unwrap());
        fs::write(dir.join("a.txt"), "a1-changed\na2\na3-by-the-agent\n").unwrap();

        app.finish_turn();

        assert_eq!(app.review_scope, ReviewScope::Turn, "F1 should land on this turn's work");
        let summary = app.transcript.last().expect("a summary line");
        assert!(summary.text.starts_with("1 file changed"), "{:?}", summary.text);
        assert!(summary.text.contains('t'), "the way back out has to be on the line too: {:?}", summary.text);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancelled_turn_is_scoped_the_same_way_as_a_finished_one() {
        // A killed turn can have written plenty before it died, and that
        // half-finished output is exactly what you want isolated.
        let (mut app, dir) = two_file_app("turn-scope-cancelled");
        app.mode = Mode::Agent;
        app.agent_running = true;
        app.turn_baseline = Some(crate::gitreview::diff_files(&dir).unwrap());
        fs::write(dir.join("a.txt"), "a1-changed\na2\na3-half-written\n").unwrap();
        app.agent_cancelled = true;

        app.finish_turn();

        assert_eq!(app.review_scope, ReviewScope::Turn);
        let texts: Vec<&str> = app.transcript.iter().map(|l| l.text.as_str()).collect();
        assert!(texts.contains(&"Cancelled."), "{texts:?}");
        assert!(texts.last().is_some_and(|t| t.contains("cancelled")), "{texts:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn committing_drops_the_turn_baseline_instead_of_reinterpreting_it() {
        // The baseline is a changeset measured against the old HEAD. Once
        // HEAD moves, every entry in it describes a comparison that no
        // longer exists.
        let dir = scratch_repo("turn-scope-after-commit");
        commit_file(&dir, "a.txt", "one\n");
        fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.run_fake_turn(|| {});
        app.review_scope = ReviewScope::Turn;

        app.mode = Mode::Curation;
        app.commit_message = "a change".to_string();
        app.on_key(key(KeyCode::Char('c')));
        assert!(app.last_commit.as_ref().unwrap().is_ok(), "{:?}", app.last_commit);

        assert!(app.turn_baseline.is_none());
        assert_eq!(app.review_scope, ReviewScope::All);
        assert!(app.turn_scope.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    // -------------------------------------------------------------
    // NOTES THAT RE-ANCHOR
    // -------------------------------------------------------------

    /// A repo with one committed multi-line file, opened with the content
    /// pane focused so `c` leaves a line-scoped note.
    fn note_app(label: &str, content: &str) -> (App, PathBuf) {
        let dir = scratch_repo(label);
        commit_file(&dir, "f.rs", content);
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(key(KeyCode::Tab)); // focus the content pane
        (app, dir)
    }

    fn leave_note_on_line(app: &mut App, line: usize, text: &str) {
        app.nav_line = line - 1;
        app.on_key(key(KeyCode::Char('c')));
        assert_eq!(app.overlay, Overlay::NoteInput);
        for ch in text.chars() {
            app.on_key(key(KeyCode::Char(ch)));
        }
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn a_note_follows_its_line_when_the_agent_inserts_code_above_it() {
        let (mut app, dir) = note_app("note-follows", "fn one() {\n    let a = 1;\n}\n\nfn two() {\n    let b = 2;\n}\n");
        leave_note_on_line(&mut app, 6, "this shadows a");
        assert_eq!(app.notes[0].line, Some(6));

        // The agent adds a use statement and a doc comment at the top.
        fs::write(dir.join("f.rs"), "use std::fmt;\n\n/// Docs.\nfn one() {\n    let a = 1;\n}\n\nfn two() {\n    let b = 2;\n}\n")
            .unwrap();
        app.sync_review_from_disk();

        assert_eq!(app.notes[0].line, Some(9), "the note should point at where its line went");
        assert!(!app.notes[0].stale);
        assert_eq!(app.notes_stale(), 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_note_whose_line_is_gone_is_kept_and_marked_rather_than_dropped() {
        let (mut app, dir) = note_app("note-stale", "fn one() {\n    let a = 1;\n}\n\nfn two() {\n    let b = 2;\n}\n");
        leave_note_on_line(&mut app, 6, "this shadows a");

        // The agent rewrote the line the note was about, and everything
        // around it — there is nowhere left to put the note.
        fs::write(dir.join("f.rs"), "fn one() {\n    let a = 1;\n}\n").unwrap();
        app.sync_review_from_disk();

        assert_eq!(app.notes.len(), 1, "a note the user wrote is never thrown away on their behalf");
        assert!(app.notes[0].stale);
        assert_eq!(app.notes_stale(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_notes_prompt_says_the_line_is_gone_instead_of_naming_one() {
        let (mut app, dir) = note_app("note-stale-prompt", "fn one() {\n    let a = 1;\n}\n\nfn two() {\n    let b = 2;\n}\n");
        leave_note_on_line(&mut app, 6, "this shadows a");
        fs::write(dir.join("f.rs"), "fn one() {\n    let a = 1;\n}\n").unwrap();
        app.sync_review_from_disk();

        let prompt = app.build_iterate_prompt();
        assert!(prompt.contains("no longer exists"), "{prompt}");
        assert!(prompt.contains("this shadows a"), "the note itself still goes: {prompt}");
        assert!(!prompt.contains("- Line 6:"), "and it must not be presented as still being line 6: {prompt}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_re_anchored_note_reaches_the_agent_with_its_new_line_number() {
        let (mut app, dir) = note_app("note-prompt-moved", "fn one() {\n    let a = 1;\n}\n\nfn two() {\n    let b = 2;\n}\n");
        leave_note_on_line(&mut app, 6, "this shadows a");
        fs::write(dir.join("f.rs"), "use std::fmt;\n\n/// Docs.\nfn one() {\n    let a = 1;\n}\n\nfn two() {\n    let b = 2;\n}\n")
            .unwrap();
        app.sync_review_from_disk();

        let prompt = app.build_iterate_prompt();
        assert!(prompt.contains("- Line 9: this shadows a"), "{prompt}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_whole_file_note_is_left_alone_when_the_file_moves() {
        let (mut app, dir) = note_app("note-whole-file", "fn one() {\n    let a = 1;\n}\n");
        app.notes.push(Note::on_file("f.rs".to_string(), "split this module".to_string()));

        fs::write(dir.join("f.rs"), "fn renamed() {\n    let a = 2;\n}\n").unwrap();
        app.sync_review_from_disk();

        assert!(!app.notes[0].stale, "a note with no line can't have lost one");
        assert_eq!(app.notes[0].line, None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_curation_badge_survives_a_poll_that_finds_nothing_new() {
        // Regression: `gitreview::load` rebuilds every CurationFile with
        // `status: None`, and the byte-identical early return that used to
        // hide that never fires once a note or a flag exists — so a file
        // marked stale kept its emptied selection but silently lost the
        // badge explaining it, one poll tick later.
        let (mut app, dir) = two_file_app("stale-badge-survives-poll");
        app.notes.push(Note::on_file("b.txt".to_string(), "unrelated note".to_string()));

        fs::write(dir.join("a.txt"), "a1-changed\na2\na3-new\n").unwrap();
        app.sync_review_from_disk();
        assert_eq!(app.curation_files[0].status, Some(crate::theme::FileStatus::Stale));

        app.sync_review_from_disk(); // nothing changed on disk this time
        assert_eq!(
            app.curation_files[0].status,
            Some(crate::theme::FileStatus::Stale),
            "the badge has to outlive the poll that follows it"
        );
        assert!(!app.curation_files[0].hunk_selected[0], "and still match the selection it explains");

        let _ = fs::remove_dir_all(&dir);
    }

    // -------------------------------------------------------------
    // DISCARD
    // -------------------------------------------------------------

    /// One committed file, edited in two places far enough apart to parse
    /// as two separate hunks, and left unstaged.
    fn two_hunk_app(label: &str) -> (App, PathBuf) {
        let dir = scratch_repo(label);
        let base: String = (1..=20).map(|n| format!("line{n}\n")).collect();
        commit_file(&dir, "f.txt", &base);
        let mut edited: Vec<String> = (1..=20).map(|n| format!("line{n}")).collect();
        edited[1] = "line2-FIRST-EDIT".to_string();
        edited[17] = "line18-SECOND-EDIT".to_string();
        fs::write(dir.join("f.txt"), edited.join("\n") + "\n").unwrap();
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Curation;
        assert_eq!(app.project.files[0].hunks.len(), 2, "the fixture needs two separate hunks");
        (app, dir)
    }

    #[test]
    fn discarding_a_hunk_reverses_only_that_hunk_on_disk() {
        let (mut app, dir) = two_hunk_app("discard-one-hunk");
        app.curation_hunk_index = 0;

        app.on_key(key(KeyCode::Char('D')));
        assert_eq!(app.overlay, Overlay::DiscardConfirm, "an irreversible action asks first");
        assert!(app.pending_discard.as_ref().unwrap().what.contains("hunk 1/2 of f.txt"), "{:?}", app.pending_discard);
        app.on_key(key(KeyCode::Enter));

        assert_eq!(app.overlay, Overlay::None);
        assert!(app.last_discard.as_ref().unwrap().is_ok(), "{:?}", app.last_discard);
        let on_disk = fs::read_to_string(dir.join("f.txt")).unwrap();
        assert!(on_disk.contains("line2\n"), "the discarded hunk should be back to what HEAD holds:\n{on_disk}");
        assert!(on_disk.contains("line18-SECOND-EDIT"), "the other hunk must be untouched:\n{on_disk}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_discarded_file_comes_back_marked_stale_with_nothing_selected() {
        // Its hunks genuinely shifted shape — "look at the rest of this
        // again" is the correct reading, and it's the same vocabulary an
        // outside edit gets.
        let (mut app, dir) = two_hunk_app("discard-then-stale");
        app.on_key(key(KeyCode::Char('D')));
        app.on_key(key(KeyCode::Enter));

        assert_eq!(app.curation_files[0].status, Some(crate::theme::FileStatus::Stale));
        assert!(app.curation_files[0].hunk_selected.iter().all(|s| !*s));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn esc_at_the_discard_prompt_keeps_the_hunk() {
        let (mut app, dir) = two_hunk_app("discard-cancel");
        let before = fs::read_to_string(dir.join("f.txt")).unwrap();

        app.on_key(key(KeyCode::Char('D')));
        app.on_key(key(KeyCode::Esc));

        assert_eq!(app.overlay, Overlay::None);
        assert!(app.pending_discard.is_none());
        assert!(app.last_discard.is_none(), "cancelling isn't an outcome worth reporting");
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), before);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pressing_d_again_at_the_discard_prompt_does_not_confirm_it() {
        // The confirmation opens on `D`; accepting on `D` would mean a
        // held or double-tapped key destroys content on its own.
        let (mut app, dir) = two_hunk_app("discard-double-tap");
        let before = fs::read_to_string(dir.join("f.txt")).unwrap();

        app.on_key(key(KeyCode::Char('D')));
        app.on_key(key(KeyCode::Char('D')));

        assert_eq!(app.overlay, Overlay::DiscardConfirm, "still waiting for a real answer");
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), before);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discarding_is_refused_while_an_agent_turn_is_running() {
        // Same reasoning as committing: the agent writes to this exact
        // tree, and reversing a patch out from under a live writer takes
        // out lines nobody has looked at.
        let (mut app, dir) = two_hunk_app("discard-during-turn");
        let before = fs::read_to_string(dir.join("f.txt")).unwrap();
        app.agent_running = true;

        app.on_key(key(KeyCode::Char('D')));

        assert_eq!(app.overlay, Overlay::None, "it never even gets as far as asking");
        let err = app.last_discard.as_ref().unwrap().as_ref().unwrap_err();
        assert!(err.contains("agent turn is running"), "{err}");
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), before);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_turn_started_during_the_confirmation_cancels_the_discard() {
        let (mut app, dir) = two_hunk_app("discard-turn-races-prompt");
        let before = fs::read_to_string(dir.join("f.txt")).unwrap();

        app.on_key(key(KeyCode::Char('D')));
        app.agent_running = true; // a turn started from another mode while it was open
        app.on_key(key(KeyCode::Enter));

        let err = app.last_discard.as_ref().unwrap().as_ref().unwrap_err();
        assert!(err.contains("nothing was discarded"), "{err}");
        assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), before);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discarding_a_brand_new_file_says_plainly_that_it_is_a_deletion() {
        // The one case where "discard" understates it: an untracked file
        // has no previous version to fall back to.
        let dir = scratch_repo("discard-new-file");
        commit_file(&dir, "committed.txt", "one\n");
        fs::write(dir.join("fresh.txt"), "written by the agent\n").unwrap();
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Curation;
        app.curation_index = app.curation_files.iter().position(|c| c.path == "fresh.txt").expect("the new file");

        app.on_key(key(KeyCode::Char('D')));
        let what = &app.pending_discard.as_ref().unwrap().what;
        assert!(what.contains("delete fresh.txt"), "{what}");
        assert!(what.contains("nothing to fall back to"), "{what}");
        app.on_key(key(KeyCode::Enter));

        assert!(app.last_discard.as_ref().unwrap().is_ok(), "{:?}", app.last_discard);
        assert!(!dir.join("fresh.txt").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn opening_a_file_the_turn_scope_hides_widens_the_scope_instead_of_hiding_the_cursor() {
        // Ctrl+F to a file the last turn never opened is an unambiguous
        // "show me this" — landing the cursor on a row nothing draws would
        // look like the finder had simply ignored it.
        let (mut app, dir) = two_file_app("scope-widens-on-open");
        app.run_fake_turn(|| fs::write(dir.join("a.txt"), "a1-changed\na2\na3-by-the-agent\n").unwrap());
        app.on_key(key(KeyCode::Char('t')));
        assert_eq!(app.review_scope, ReviewScope::Turn);

        app.on_key(ctrl('f'));
        app.on_key(key(KeyCode::Char('b')));
        app.on_key(key(KeyCode::Enter));

        assert_eq!(app.review_scope, ReviewScope::All, "the scope gets out of the way");
        assert!(app.nav_file.ends_with("b.txt"), "{:?}", app.nav_file);
        assert!(app.tree_row_visible(app.tree_index), "and the cursor lands somewhere that is actually drawn");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_poll_does_not_drag_the_cursor_off_the_row_you_are_reading() {
        // The scope set moves on its own as the tree changes; the cursor
        // must not be re-homed once a second because of it.
        let (mut app, dir) = two_file_app("scope-poll-keeps-cursor");
        app.run_fake_turn(|| {
            fs::write(dir.join("a.txt"), "a1-changed\na2\na3-by-the-agent\n").unwrap();
            fs::write(dir.join("b.txt"), "b1-changed\nb2\nb3-by-the-agent\n").unwrap();
        });
        app.on_key(key(KeyCode::Char('t')));
        app.on_key(key(KeyCode::Home));
        let parked = app.tree_index;

        // b.txt leaves the turn scope — reverted to what the baseline held.
        fs::write(dir.join("b.txt"), "b1-changed\nb2\n").unwrap();
        app.sync_review_from_disk();

        assert_eq!(app.tree_index, parked, "a background refresh must not move the cursor");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_note_on_a_file_the_agent_deleted_is_marked_rather_than_left_quoting_a_line() {
        let (mut app, dir) = note_app("note-file-deleted", "fn one() {\n    let a = 1;\n}\n");
        leave_note_on_line(&mut app, 2, "inline this");

        app.run_fake_turn(|| fs::remove_file(dir.join("f.rs")).unwrap());

        assert!(app.notes[0].stale, "there is no line 2 in a file that isn't there");
        let prompt = app.build_iterate_prompt();
        assert!(prompt.contains("no longer exists"), "{prompt}");

        let _ = fs::remove_dir_all(&dir);
    }
}
