use std::path::PathBuf;
use std::sync::mpsc::TryRecvError;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::data::{
    self, AgentLine, AgentLineKind, CurationFile, FileEntry, FileWrite, HoverInfo, Note, Project,
    SymbolResult, TreeEntry,
};
use crate::fsnav;
use crate::keymap::{Action, Keymap};
use crate::pi_client::{self, AgentEvent, Backend, PiSession, ToolProfile};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Steer,
    Navigate,
    Agent,
    Curation,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overlay {
    None,
    SymbolJump,
    Permission,
    FileFinder,
    NoteInput,
}

/// Which Navigate pane arrow keys currently move.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NavFocus {
    Tree,
    Source,
}

/// What the Permission overlay is currently showing: the static Ctrl+P demo
/// data, or a real sandboxed diff pending approval.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PermTarget {
    Demo,
    SandboxApply,
}

/// What an in-flight `pi` turn is for. `Chat` (the normal Agent-pane
/// conversation, in either read-only or sandboxed-edit mode) streams into
/// the visible transcript as usual. `CommitMessage` is a silent background
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

    // STEER
    pub project: Project,
    pub review_is_real: bool,
    pub steer_selected: usize,
    pub steer_split: bool,

    // NAVIGATE
    pub tree: Vec<TreeEntry>,
    pub tree_index: usize,
    pub nav_focus: NavFocus,
    pub nav_file: PathBuf,
    pub source: Vec<String>,
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

    // NOTES (real free-text review notes, from Steer or Navigate)
    pub notes: Vec<Note>,
    pub(crate) note_target: Option<(String, Option<usize>)>,
    pub note_input: String,
    pub note_cursor: usize,

    // AGENT
    pub target_dir: PathBuf,
    pub session_id: String,
    pub transcript: Vec<AgentLine>,
    pub agent_input: String,
    /// Char index into `agent_input` (not a byte offset — see `char_boundary`).
    pub agent_cursor: usize,
    /// Lines scrolled up from the bottom of the transcript; 0 = pinned to
    /// the latest content (and stays pinned as new lines arrive).
    pub agent_scroll: usize,
    pub agent_model_live: Option<String>,
    pub backend: Backend,
    pub agent_running: bool,
    pub demo_transcript: bool,
    pi_session: Option<PiSession>,
    agent_purpose: TurnPurpose,
    /// Chat (read-only) vs Edit (sandboxed writes) — see `sandbox.rs`.
    pub edit_mode: bool,
    sandbox: Option<PathBuf>,
    pub pending_changes: Vec<FileEntry>,

    // PERMISSION (overlay)
    pub perm_writes: Vec<FileWrite>,
    pub perm_commands: Vec<&'static str>,
    pub perm_focus: usize,
    perm_target: PermTarget,

    // CURATION
    pub curation_files: Vec<CurationFile>,
    pub curation_index: usize,
    pub commit_message: String,
    pub commit_message_status: Option<String>,
    pub editing_commit: bool,
    /// Set true to ask main.rs's event loop to suspend the TUI and open
    /// $EDITOR on `commit_message` — App itself doesn't own the Terminal.
    pub open_editor_requested: bool,
    pub last_commit: Option<Result<String, String>>,
}

const PERM_OPTIONS: [&str; 4] = ["Review files", "Approve all", "Modify", "Reject"];

/// Lines per Page Up/Page Down in Navigate. app.rs doesn't know the actual
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

impl App {
    pub fn new(target_dir: PathBuf, keymap: Keymap) -> Self {
        let review = crate::gitreview::load(&target_dir);
        let tree = fsnav::build_tree(&target_dir);
        let symbols = fsnav::scan_symbols(&target_dir);
        let nav_file = tree.iter().find(|e| !e.is_dir).map(|e| e.path.clone()).unwrap_or_else(|| target_dir.clone());
        let source = fsnav::read_file(&nav_file);

        let mut app = App {
            mode: Mode::Steer,
            overlay: Overlay::None,
            should_quit: false,
            keymap,

            project: review.project,
            review_is_real: review.is_real,
            steer_selected: 0,
            steer_split: false,

            tree,
            tree_index: 0,
            nav_focus: NavFocus::Tree,
            nav_file,
            source,
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

            target_dir,
            session_id: format!("steer-{}", std::process::id()),
            transcript: data::mock_transcript(),
            agent_input: String::new(),
            agent_cursor: 0,
            agent_scroll: 0,
            agent_model_live: None,
            backend: Backend::Pi,
            agent_running: false,
            demo_transcript: true,
            pi_session: None,
            agent_purpose: TurnPurpose::Chat,
            edit_mode: false,
            sandbox: None,
            pending_changes: Vec::new(),

            perm_writes: data::mock_permission_writes(),
            perm_commands: data::mock_permission_commands(),
            perm_focus: 1,
            perm_target: PermTarget::Demo,

            curation_files: review.curation_files,
            curation_index: 0,
            // Real diffs get an empty message the user must actually write —
            // the drafted mock text belongs only to the non-git demo path.
            commit_message: if review.is_real { String::new() } else { data::mock_commit_message() },
            commit_message_status: None,
            editing_commit: false,
            open_editor_requested: false,
            last_commit: None,
        };
        app.refresh_hover();
        app
    }

    /// Reloads the git-diff-backed review data (file list, hunks, curation
    /// selections) — used after a real commit changes what's outstanding.
    fn refresh_review(&mut self) {
        let review = crate::gitreview::load(&self.target_dir);
        self.project = review.project;
        self.review_is_real = review.is_real;
        self.curation_files = review.curation_files;
        self.steer_selected = 0;
        self.curation_index = 0;
        self.commit_message.clear();
        self.commit_message_status = None;
    }

    /// Commits whatever's currently selected in Curation, for real.
    fn commit_selected(&mut self) {
        let result = crate::gitcommit::commit(&self.target_dir, &self.project, &self.curation_files, &self.commit_message);
        let ok = result.is_ok();
        self.last_commit = Some(result);
        if ok {
            self.refresh_review();
        }
    }

    fn open_file(&mut self, path: PathBuf) {
        self.source = fsnav::read_file(&path);
        self.nav_file = path;
        self.nav_line = 0;
        self.nav_scroll_x = 0;
        self.refresh_hover();
    }

    fn refresh_hover(&mut self) {
        self.hover = self.source.get(self.nav_line).and_then(|line| {
            let sym = fsnav::hover_for_line(&self.symbols, line)?;
            let references = fsnav::reference_count(&self.target_dir, &sym.name);
            Some(HoverInfo {
                signature: sym.preview.clone(),
                location: sym.location(&self.target_dir),
                references,
            })
        });
    }

    pub fn notes_queued(&self) -> u32 {
        self.project.files.iter().map(|f| f.notes).sum()
    }

    pub fn files_selected(&self) -> usize {
        self.project.files.iter().filter(|f| f.selected).count()
    }

    /// Sends `prompt` to `pi`. In Chat mode this is a real, read-only turn
    /// against `self.target_dir`. In Edit mode it runs read+write against a
    /// disposable sandbox worktree instead (creating one on first use) —
    /// never the real target directory directly.
    pub fn start_agent_turn(&mut self, prompt: String) {
        if !self.edit_mode {
            let target_dir = self.target_dir.clone();
            self.spawn_turn(prompt, target_dir, ToolProfile::ReadOnly, TurnPurpose::Chat);
            return;
        }

        let sandbox = match &self.sandbox {
            Some(s) => s.clone(),
            None => match crate::sandbox::create(&self.target_dir) {
                Ok(s) => {
                    self.sandbox = Some(s.clone());
                    s
                }
                Err(e) => {
                    self.transcript.push(AgentLine { kind: AgentLineKind::Text, text: format!("Error: {e}") });
                    return;
                }
            },
        };
        self.spawn_turn(prompt, sandbox, ToolProfile::ReadWrite, TurnPurpose::Chat);
    }

    /// Spawns a `pi` turn in `cwd` with the given tool profile. Reuses
    /// `self.session_id` across every call in this run, so pi has real
    /// cross-turn memory via its own session storage. For `TurnPurpose::Chat`
    /// the visible transcript is cleared only once (to drop the initial demo
    /// content) and then appended to on every call; `CommitMessage` turns
    /// never touch the transcript at all — see `apply_agent_event`.
    fn spawn_turn(&mut self, prompt: String, cwd: PathBuf, tools: ToolProfile, purpose: TurnPurpose) {
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() || self.agent_running {
            return;
        }
        self.agent_purpose = purpose;
        self.agent_model_live = None;
        if purpose == TurnPurpose::Chat {
            if self.demo_transcript {
                self.demo_transcript = false;
                self.transcript.clear();
            }
            self.transcript.push(AgentLine { kind: AgentLineKind::Text, text: format!("> {prompt}") });
            self.transcript.push(AgentLine { kind: AgentLineKind::Blank, text: String::new() });
            self.agent_scroll = 0; // jump to the bottom to watch it stream in
        }

        match pi_client::spawn(&prompt, &cwd, self.backend, &self.session_id, tools) {
            Ok(session) => {
                self.pi_session = Some(session);
                self.agent_running = true;
            }
            Err(e) => {
                let msg = format!("Error: couldn't start `pi` ({e}). Is it installed and on PATH?");
                if purpose == TurnPurpose::Chat {
                    self.transcript.push(AgentLine { kind: AgentLineKind::Text, text: msg });
                } else {
                    self.commit_message_status = Some(msg);
                }
            }
        }
    }

    /// Ctrl+G in Curation: asks `pi` to draft a commit message from the real
    /// diff of everything currently selected — a silent, read-only,
    /// no-tools-needed turn that never touches the visible Agent
    /// transcript. Completing it opens `$EDITOR` for a last pass (see
    /// `apply_agent_event`'s `AgentEnd` handling).
    pub fn generate_commit_message(&mut self) {
        if self.agent_running {
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
        self.commit_message_status = Some("Generating\u{2026}".to_string());
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
            out.push_str(&format!("diff --git a/{0} b/{0}\n--- a/{0}\n+++ b/{0}\n", cf.path));
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
        while let Some(session) = &self.pi_session {
            match session.rx.try_recv() {
                Ok(ev) => self.apply_agent_event(ev),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.agent_running = false;
                    self.pi_session = None;
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
                Text(t) => self.commit_message = t.trim().to_string(),
                AgentEnd => {
                    self.agent_running = false;
                    self.pi_session = None;
                    self.commit_message_status = None;
                    self.open_editor_requested = true;
                }
                Error(e) => self.commit_message_status = Some(format!("Error generating message: {e}")),
                Thinking(_) | ToolCall { .. } | ToolResult { .. } | TurnEnd => {}
            }
            return;
        }

        match event {
            Model(m) => self.agent_model_live = Some(m),
            Thinking(t) => {
                // Always followed by a blank line: thinking prose and
                // whatever comes next (a tool call or the final answer)
                // share the same line kind, so without an explicit
                // separator they visually run together.
                self.transcript.push(AgentLine { kind: AgentLineKind::Text, text: format!("  {t}") });
                self.transcript.push(AgentLine { kind: AgentLineKind::Blank, text: String::new() });
            }
            Text(t) => self.transcript.push(AgentLine { kind: AgentLineKind::Text, text: t }),
            ToolCall { name, args } => self.transcript.push(AgentLine {
                kind: AgentLineKind::ToolCall,
                text: format!("Calling: {name}({args})"),
            }),
            ToolResult { name, summary } => self.transcript.push(AgentLine {
                kind: AgentLineKind::Done,
                text: format!("{name}  {summary}"),
            }),
            TurnEnd => self.transcript.push(AgentLine { kind: AgentLineKind::Blank, text: String::new() }),
            AgentEnd => {
                self.agent_running = false;
                self.pi_session = None;
                if let Some(sandbox) = self.sandbox.clone() {
                    self.pending_changes = crate::sandbox::diff(&sandbox);
                    let n = self.pending_changes.len();
                    let text = if n == 0 {
                        "  (no file changes in the sandbox yet)".to_string()
                    } else {
                        format!(
                            "{n} file{} changed in the sandbox \u{2014} Ctrl+A to review & apply, Ctrl+R to discard",
                            if n == 1 { "" } else { "s" }
                        )
                    };
                    self.transcript.push(AgentLine { kind: AgentLineKind::Proposal, text });
                }
            }
            Error(e) => {
                self.transcript.push(AgentLine { kind: AgentLineKind::Text, text: format!("stderr: {e}") })
            }
        }
    }

    /// Ctrl+A: open the real sandboxed diff for approval, if there is one.
    fn review_pending_changes(&mut self) {
        if self.pending_changes.is_empty() {
            return;
        }
        self.perm_writes = self
            .pending_changes
            .iter()
            .map(|f| {
                let (plus, minus) = f.diff_stat();
                FileWrite { path: f.path.clone(), kind: "modify", plus, minus }
            })
            .collect();
        self.perm_commands = Vec::new();
        self.perm_focus = 1; // "Approve all"
        self.perm_target = PermTarget::SandboxApply;
        self.overlay = Overlay::Permission;
    }

    /// Applies the sandbox's diff to the real target directory.
    fn apply_sandbox(&mut self) {
        let Some(sandbox) = self.sandbox.clone() else {
            self.overlay = Overlay::None;
            return;
        };
        match crate::sandbox::apply(&sandbox, &self.target_dir) {
            Ok(()) => {
                crate::sandbox::discard(&self.target_dir, &sandbox);
                self.sandbox = None;
                let n = self.pending_changes.len();
                self.pending_changes.clear();
                self.transcript.push(AgentLine {
                    kind: AgentLineKind::Done,
                    text: format!("Applied {n} file{} to the real project.", if n == 1 { "" } else { "s" }),
                });
                self.refresh_review();
            }
            Err(e) => {
                self.transcript.push(AgentLine { kind: AgentLineKind::Text, text: format!("Error applying changes: {e}") });
            }
        }
        self.overlay = Overlay::None;
    }

    /// Ctrl+R: discard the sandbox and its changes entirely.
    fn reject_sandbox(&mut self) {
        if let Some(sandbox) = self.sandbox.take() {
            crate::sandbox::discard(&self.target_dir, &sandbox);
        }
        self.pending_changes.clear();
        self.transcript.push(AgentLine { kind: AgentLineKind::Text, text: "\u{2717} Sandbox changes discarded.".to_string() });
        self.overlay = Overlay::None;
    }

    /// Ctrl+M: keep the sandbox alive for a follow-up prompt instead of
    /// approving or discarding yet.
    fn modify_sandbox(&mut self) {
        if self.sandbox.is_none() {
            return;
        }
        self.pending_changes.clear();
        self.transcript.push(AgentLine {
            kind: AgentLineKind::Text,
            text: "\u{270e} Keep typing to revise in the same sandbox, then Ctrl+A when ready.".to_string(),
        });
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        // Fixed, not remappable: always quits, regardless of context.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }

        match self.overlay {
            Overlay::SymbolJump => return self.on_key_symbol_jump(key),
            Overlay::Permission => return self.on_key_permission(key),
            Overlay::FileFinder => return self.on_key_file_finder(key),
            Overlay::NoteInput => return self.on_key_note_input(key),
            Overlay::None => {}
        }

        // Mode switches always work, even mid-text-entry (their default
        // chords are F1-F4, which no text field would otherwise consume).
        if self.keymap.is(&key, Action::SwitchSteer) {
            self.mode = Mode::Steer;
            return;
        }
        if self.keymap.is(&key, Action::SwitchNavigate) {
            self.mode = Mode::Navigate;
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
        if self.mode == Mode::Curation && self.editing_commit {
            return self.on_key_curation_edit(key);
        }
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
        if self.keymap.is(&key, Action::OpenPermissionDemo) {
            self.perm_writes = data::mock_permission_writes();
            self.perm_commands = data::mock_permission_commands();
            self.perm_focus = 1;
            self.perm_target = PermTarget::Demo;
            self.overlay = Overlay::Permission;
            return;
        }
        if self.keymap.is(&key, Action::Quit) {
            self.should_quit = true;
            return;
        }

        match self.mode {
            Mode::Steer => self.on_key_steer(key),
            Mode::Navigate => self.on_key_navigate(key),
            Mode::Agent => self.on_key_agent(key),
            Mode::Curation => self.on_key_curation(key),
        }
    }

    // -------------------------------------------------------------
    // STEER
    // -------------------------------------------------------------
    fn on_key_steer(&mut self, key: KeyEvent) {
        let n = self.project.files.len();
        if n == 0 {
            return;
        }
        let k = &self.keymap;
        if k.is(&key, Action::SteerUp) {
            self.steer_selected = self.steer_selected.saturating_sub(1);
        } else if k.is(&key, Action::SteerDown) || k.is(&key, Action::SteerNextFile) {
            self.steer_selected = (self.steer_selected + 1).min(n - 1);
        } else if k.is(&key, Action::SteerToggleSelect) {
            self.project.files[self.steer_selected].selected ^= true;
        } else if k.is(&key, Action::SteerMarkGood) {
            let path = self.project.files[self.steer_selected].path.clone();
            let f = &mut self.project.files[self.steer_selected];
            f.flagged = false;
            f.notes = 0;
            self.notes.retain(|n| n.path != path);
        } else if k.is(&key, Action::SteerFlagRework) {
            self.project.files[self.steer_selected].flagged = true;
        } else if k.is(&key, Action::SteerComment) {
            let path = self.project.files[self.steer_selected].path.clone();
            self.open_note_input(path, None);
        } else if k.is(&key, Action::SteerSplitView) {
            self.steer_split = true;
        } else if k.is(&key, Action::SteerUnifiedView) {
            self.steer_split = false;
        } else if k.is(&key, Action::SteerIterate) {
            let prompt = self.build_iterate_prompt();
            self.mode = Mode::Agent;
            // Notes ask the agent to change things — iterating through
            // read-only Chat mode would let it see the request but never
            // act on it, so switch to sandboxed Edit mode first.
            self.edit_mode = true;
            self.start_agent_turn(prompt);
        }
    }

    /// Turns the queued review notes into a real prompt for `pi`, pulling
    /// from `self.notes` (the real free-text notes left in Steer/Navigate)
    /// rather than the unused mock-only `Hunk::note` field. Notes are
    /// grouped under their file, with an explicit line annotation, so the
    /// agent can't confuse which file (or line) a piece of feedback is
    /// actually about.
    fn build_iterate_prompt(&self) -> String {
        let mut prompt = String::from(
            "You're iterating on review feedback for this repo. Each note below is attached to a \
             specific file, and a specific line when one is given — address them there, not elsewhere:\n\n",
        );
        let mut any = false;
        let selected_paths: std::collections::HashSet<&str> =
            self.project.files.iter().filter(|f| f.selected).map(|f| f.path.as_str()).collect();

        let mut order: Vec<&str> = Vec::new();
        let mut grouped: std::collections::HashMap<&str, Vec<&Note>> = std::collections::HashMap::new();
        for note in &self.notes {
            if !selected_paths.contains(note.path.as_str()) {
                continue;
            }
            grouped.entry(note.path.as_str()).or_insert_with(|| { order.push(note.path.as_str()); Vec::new() }).push(note);
        }
        for path in &order {
            any = true;
            prompt.push_str(&format!("File: {path}\n"));
            for note in &grouped[path] {
                match note.line {
                    Some(line) => prompt.push_str(&format!("  - Line {line}: {}\n", note.text)),
                    None => prompt.push_str(&format!("  - {}\n", note.text)),
                }
            }
        }
        for file in &self.project.files {
            if file.selected && file.flagged {
                any = true;
                prompt.push_str(&format!("File: {}\n  - Flagged for rework: please redo this file's change.\n", file.path));
            }
        }
        if !any {
            prompt.push_str("No specific notes were left — please review the selected files' current diffs and suggest improvements.\n");
        }
        prompt
    }

    // -------------------------------------------------------------
    // NAVIGATE
    // -------------------------------------------------------------

    /// Moves the focused pane's index by `delta` (negative = up/back),
    /// clamped to its bounds. `tree_len` is passed in since the tree and
    /// source have different lengths and only one is relevant per call.
    fn move_nav_focus(&mut self, delta: i64, tree_len: usize) {
        match self.nav_focus {
            NavFocus::Tree => self.tree_index = clamped_move(self.tree_index, delta, tree_len),
            NavFocus::Source => {
                self.nav_line = clamped_move(self.nav_line, delta, self.source.len());
                if self.show_hover {
                    self.refresh_hover();
                }
            }
        }
    }

    /// Jumps the focused pane's index directly to `target` (clamped to its
    /// bounds) — `usize::MAX` means "the last entry".
    fn jump_nav_focus(&mut self, target: usize, tree_len: usize) {
        match self.nav_focus {
            NavFocus::Tree => self.tree_index = target.min(tree_len.saturating_sub(1)),
            NavFocus::Source => {
                self.nav_line = target.min(self.source.len().saturating_sub(1));
                if self.show_hover {
                    self.refresh_hover();
                }
            }
        }
    }

    fn on_key_navigate(&mut self, key: KeyEvent) {
        let n = self.tree.len();
        let k = &self.keymap;
        // j/k always move too, regardless of NavUp/NavDown's configured
        // chord — vim muscle memory shouldn't require a remap. Which pane
        // they move depends on nav_focus, same as the arrows: only one
        // pane moves at a time, never both.
        if k.is(&key, Action::NavUp) || key.code == KeyCode::Char('k') {
            self.move_nav_focus(-1, n);
        } else if k.is(&key, Action::NavDown) || key.code == KeyCode::Char('j') {
            self.move_nav_focus(1, n);
        } else if k.is(&key, Action::NavPageUp) {
            self.move_nav_focus(-(PAGE_SIZE as i64), n);
        } else if k.is(&key, Action::NavPageDown) {
            self.move_nav_focus(PAGE_SIZE as i64, n);
        } else if k.is(&key, Action::NavHome) {
            self.jump_nav_focus(0, n);
        } else if k.is(&key, Action::NavEnd) {
            self.jump_nav_focus(usize::MAX, n);
        } else if k.is(&key, Action::NavOpen) {
            if let Some(entry) = self.tree.get(self.tree_index) {
                if !entry.is_dir {
                    let path = entry.path.clone();
                    self.open_file(path);
                }
            }
            self.nav_focus = NavFocus::Source;
        } else if k.is(&key, Action::NavToggleFocus) {
            self.nav_focus = match self.nav_focus {
                NavFocus::Tree => NavFocus::Source,
                NavFocus::Source => NavFocus::Tree,
            };
        } else if k.is(&key, Action::NavScrollLeft) {
            if self.nav_focus == NavFocus::Source {
                self.nav_scroll_x = self.nav_scroll_x.saturating_sub(4);
            }
        } else if k.is(&key, Action::NavScrollRight) {
            if self.nav_focus == NavFocus::Source {
                self.nav_scroll_x = self.nav_scroll_x.saturating_add(4);
            }
        } else if k.is(&key, Action::NavToggleHover) {
            self.show_hover = !self.show_hover;
            if self.show_hover {
                self.refresh_hover();
            }
        } else if k.is(&key, Action::NavOpenSymbolJump) {
            self.overlay = Overlay::SymbolJump;
        } else if k.is(&key, Action::NavComment) {
            if !self.source.is_empty() {
                let rel = self.nav_file.strip_prefix(&self.target_dir).unwrap_or(&self.nav_file).display().to_string();
                self.open_note_input(rel, Some(self.nav_line + 1));
            }
        }
    }

    // -------------------------------------------------------------
    // SYMBOL JUMP
    // -------------------------------------------------------------
    fn filtered_symbols(&self) -> Vec<usize> {
        self.symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| fsnav::fuzzy_match(&self.symbol_filter, &s.name))
            .map(|(i, _)| i)
            .collect()
    }

    fn on_key_symbol_jump(&mut self, key: KeyEvent) {
        let k = &self.keymap;
        if k.is(&key, Action::SymbolClose) {
            self.overlay = Overlay::None;
        } else if k.is(&key, Action::SymbolJumpTo) {
            if let Some(&i) = self.filtered_symbols().get(self.symbol_index) {
                let sym = &self.symbols[i];
                let path = sym.path.clone();
                let line = sym.line;
                self.open_file(path);
                self.nav_line = line.saturating_sub(1).min(self.source.len().saturating_sub(1));
                self.refresh_hover();
            }
            self.overlay = Overlay::None;
            self.mode = Mode::Navigate;
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
                let rel = e.path.strip_prefix(&self.target_dir).unwrap_or(&e.path).display().to_string();
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
                self.open_file(path);
                self.nav_focus = NavFocus::Source;
            }
            self.overlay = Overlay::None;
            self.mode = Mode::Navigate;
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
        self.note_target = Some((path, line));
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
        if let Some((path, line)) = self.note_target.take() {
            if !text.is_empty() {
                self.notes.push(Note { path: path.clone(), line, text });
                if let Some(f) = self.project.files.iter_mut().find(|f| f.path == path) {
                    f.notes += 1;
                }
            }
        }
        self.overlay = Overlay::None;
    }

    // -------------------------------------------------------------
    // AGENT
    // -------------------------------------------------------------
    /// Returns true if the key was consumed as text input/send for the
    /// prompt field (so the caller shouldn't fall through to anything else).
    fn on_key_agent_input(&mut self, key: KeyEvent) -> bool {
        if self.keymap.is(&key, Action::AgentSend) {
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
        if k.is(&key, Action::AgentAcceptAll) {
            self.review_pending_changes();
        } else if k.is(&key, Action::AgentReject) {
            self.reject_sandbox();
        } else if k.is(&key, Action::AgentModify) {
            self.modify_sandbox();
        } else if k.is(&key, Action::AgentSwitchBackend) {
            self.backend = self.backend.toggled();
        } else if k.is(&key, Action::AgentToggleEditMode) {
            self.edit_mode = !self.edit_mode;
        } else if k.is(&key, Action::AgentScrollUp) {
            self.agent_scroll = self.agent_scroll.saturating_add(PAGE_SIZE);
        } else if k.is(&key, Action::AgentScrollDown) {
            self.agent_scroll = self.agent_scroll.saturating_sub(PAGE_SIZE);
        }
    }

    // -------------------------------------------------------------
    // PERMISSION
    // -------------------------------------------------------------
    fn on_key_permission(&mut self, key: KeyEvent) {
        let k = &self.keymap;
        if k.is(&key, Action::PermCycle) {
            self.perm_focus = (self.perm_focus + 1) % PERM_OPTIONS.len();
        } else if k.is(&key, Action::PermCancel) {
            self.overlay = Overlay::None;
        } else if k.is(&key, Action::PermConfirm) {
            match self.perm_target {
                PermTarget::Demo => self.overlay = Overlay::None,
                PermTarget::SandboxApply => match self.perm_focus {
                    1 => self.apply_sandbox(),
                    3 => self.reject_sandbox(),
                    _ => self.overlay = Overlay::None, // "Review files" / "Modify": just close
                },
            }
        }
    }

    pub fn perm_options() -> [&'static str; 4] {
        PERM_OPTIONS
    }

    // -------------------------------------------------------------
    // CURATION
    // -------------------------------------------------------------
    fn on_key_curation(&mut self, key: KeyEvent) {
        let n = self.curation_files.len();
        let k = &self.keymap;
        if k.is(&key, Action::CurateUp) {
            self.curation_index = self.curation_index.saturating_sub(1);
        } else if k.is(&key, Action::CurateDown) && n > 0 {
            self.curation_index = (self.curation_index + 1).min(n - 1);
        } else if k.is(&key, Action::CurateToggleHunk) && n > 0 {
            let f = &mut self.curation_files[self.curation_index];
            let all_selected = f.selected() == f.total();
            for s in &mut f.hunk_selected {
                *s = !all_selected;
            }
        } else if k.is(&key, Action::CurateEditMessage) {
            self.editing_commit = true;
        } else if k.is(&key, Action::CurateGenerateMessage) {
            self.generate_commit_message();
        } else if k.is(&key, Action::CurateCommit) {
            self.commit_selected();
        }
    }

    fn on_key_curation_edit(&mut self, key: KeyEvent) {
        if self.keymap.is(&key, Action::CurateStopEditing) {
            self.editing_commit = false;
            return;
        }
        match key.code {
            KeyCode::Enter => self.commit_message.push('\n'),
            KeyCode::Backspace => {
                self.commit_message.pop();
            }
            KeyCode::Char(c) => self.commit_message.push(c),
            _ => {}
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
            "steer-app-test-{label}-{}-{:?}",
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

    /// Two files, each with one hunk, so Steer/Curation navigation and
    /// selection have more than one row to move between.
    fn two_file_app(label: &str) -> (App, PathBuf) {
        let dir = scratch_repo(label);
        commit_file(&dir, "a.txt", "a1\na2\n");
        commit_file(&dir, "b.txt", "b1\nb2\n");
        fs::write(dir.join("a.txt"), "a1-changed\na2\n").unwrap();
        fs::write(dir.join("b.txt"), "b1-changed\nb2\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();
        // Leave both staged-but-uncommitted so `git diff HEAD` still sees them.
        let app = App::new(dir.clone(), Keymap::defaults());
        (app, dir)
    }

    #[test]
    fn f_keys_switch_mode_from_anywhere() {
        let (mut app, dir) = two_file_app("fkeys");
        app.on_key(key(KeyCode::F(2)));
        assert!(app.mode == Mode::Navigate);
        app.on_key(key(KeyCode::F(4)));
        assert!(app.mode == Mode::Curation);
        app.on_key(key(KeyCode::F(3)));
        assert!(app.mode == Mode::Agent);
        app.on_key(key(KeyCode::F(1)));
        assert!(app.mode == Mode::Steer);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ctrl_c_quits_regardless_of_the_keymap() {
        // Ctrl+C is checked before the keymap is even consulted (see
        // on_key's first lines) — it's a fixed safety net, not a binding.
        // `quit_key_is_configurable` below covers the actually-configurable
        // `q` binding separately.
        let dir = scratch_repo("ctrlc");
        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.on_key(ctrl('c'));
        assert!(app.should_quit);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn quit_key_is_configurable() {
        let dir = scratch_repo("quit-remap");
        let mut keymap = Keymap::defaults();
        keymap.set(Action::Quit, crate::keymap::KeyChord { code: KeyCode::Char('z'), mods: KeyModifiers::NONE });
        let mut app = App::new(dir.clone(), keymap);

        app.on_key(key(KeyCode::Char('q')));
        assert!(!app.should_quit, "plain q should no longer quit once remapped");
        app.on_key(key(KeyCode::Char('z')));
        assert!(app.should_quit, "the remapped chord should quit");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn steer_navigation_and_toggles() {
        let (mut app, dir) = two_file_app("steer-nav");
        assert_eq!(app.steer_selected, 0);
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.steer_selected, 1);
        app.on_key(key(KeyCode::Down)); // clamps at the last file
        assert_eq!(app.steer_selected, 1);
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.steer_selected, 0);

        let was_selected = app.project.files[0].selected;
        app.on_key(key(KeyCode::Char(' ')));
        assert_eq!(app.project.files[0].selected, !was_selected);

        app.on_key(key(KeyCode::Char('x')));
        assert!(app.project.files[0].flagged);
        app.on_key(key(KeyCode::Char('c')));
        assert_eq!(app.overlay, Overlay::NoteInput);
        for c in "please fix this".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.project.files[0].notes, 1);
        assert_eq!(app.notes.len(), 1);
        assert_eq!(app.notes[0].text, "please fix this");
        app.on_key(key(KeyCode::Char('g')));
        assert!(!app.project.files[0].flagged);
        assert_eq!(app.project.files[0].notes, 0);

        assert!(!app.steer_split);
        app.on_key(key(KeyCode::Char('s')));
        assert!(app.steer_split);
        app.on_key(key(KeyCode::Char('u')));
        assert!(!app.steer_split);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn navigate_tree_movement_via_arrows_and_vim_keys() {
        let dir = scratch_repo("navigate-tree");
        commit_file(&dir, "a.rs", "fn a() {}\n");
        commit_file(&dir, "b.rs", "fn b() {}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.mode = Mode::Navigate;

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
    fn navigate_source_cursor_movement_and_hover_toggle() {
        let dir = scratch_repo("navigate-cursor");
        commit_file(&dir, "main.rs", "fn main() {\n    let x = 1;\n    let y = 2;\n}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.mode = Mode::Navigate;

        // Up/Down move the tree until the source pane is focused — arrows
        // never move both at once.
        assert!(app.nav_focus == NavFocus::Tree);
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.nav_line, 0, "still tree-focused, shouldn't touch the cursor");

        app.on_key(key(KeyCode::Tab));
        assert!(app.nav_focus == NavFocus::Source);

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
    fn navigate_comment_opens_note_input_targeting_the_current_line() {
        let dir = scratch_repo("navigate-comment");
        commit_file(&dir, "main.rs", "fn main() {\n    let x = 1;\n    let y = 2;\n}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.mode = Mode::Navigate;
        app.on_key(key(KeyCode::Tab)); // focus source
        app.on_key(key(KeyCode::Down)); // nav_line == 1

        app.on_key(key(KeyCode::Char('c')));
        assert_eq!(app.overlay, Overlay::NoteInput);
        assert_eq!(app.note_target, Some(("main.rs".to_string(), Some(2))));

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
        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.mode = Mode::Steer;
        app.project.files.push(crate::data::FileEntry {
            path: "x.rs".to_string(),
            hunk_count: 0,
            notes: 0,
            selected: false,
            flagged: false,
            hunks: vec![],
        });
        app.steer_selected = 0;

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
        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.mode = Mode::Steer;
        app.project.files.push(crate::data::FileEntry {
            path: "x.rs".to_string(),
            hunk_count: 0,
            notes: 0,
            selected: false,
            flagged: false,
            hunks: vec![],
        });
        app.steer_selected = 0;
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
    fn navigate_left_right_scroll_the_source_pane_only_when_focused() {
        let dir = scratch_repo("navigate-scroll");
        commit_file(&dir, "main.rs", "fn main() {}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.mode = Mode::Navigate;

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
    fn navigate_page_up_down_and_home_end_in_source() {
        let dir = scratch_repo("navigate-page");
        let content: String = (1..=60).map(|n| format!("line{n}\n")).collect();
        commit_file(&dir, "big.rs", &content);
        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.mode = Mode::Navigate;
        app.on_key(key(KeyCode::Tab)); // focus source
        assert!(app.nav_focus == NavFocus::Source);

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
    fn navigate_page_down_clamps_at_the_end_of_a_short_file() {
        let dir = scratch_repo("navigate-page-short");
        commit_file(&dir, "small.rs", "line1\nline2\nline3\n");
        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.mode = Mode::Navigate;
        app.on_key(key(KeyCode::Tab));

        app.on_key(key(KeyCode::PageDown));
        assert_eq!(app.nav_line, 2, "should clamp to the last line, not overshoot");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn navigate_enter_focuses_source_and_opening_a_file_resets_scroll() {
        let dir = scratch_repo("navigate-open-focus");
        commit_file(&dir, "a.rs", "fn a() {}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.mode = Mode::Navigate;
        app.nav_scroll_x = 12;

        app.on_key(key(KeyCode::Enter));
        assert!(app.nav_focus == NavFocus::Source);
        assert_eq!(app.nav_scroll_x, 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn navigate_enter_opens_the_selected_tree_file() {
        let dir = scratch_repo("navigate-open");
        commit_file(&dir, "a.rs", "fn a() {}\n");
        commit_file(&dir, "b.rs", "fn b() {}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.mode = Mode::Navigate;

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
    fn curation_navigation_and_edit_message_typing() {
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

        assert!(!app.editing_commit);
        app.on_key(key(KeyCode::Char('e')));
        assert!(app.editing_commit);
        app.on_key(key(KeyCode::Char('h')));
        app.on_key(key(KeyCode::Char('i')));
        assert_eq!(app.commit_message, "hi");
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.commit_message, "h");
        app.on_key(key(KeyCode::Esc));
        assert!(!app.editing_commit);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_input_accumulates_text_without_spawning_pi() {
        let dir = scratch_repo("agent-input");
        let mut app = App::new(dir.clone(), Keymap::defaults());
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
    fn agent_backend_and_edit_mode_toggle() {
        let dir = scratch_repo("agent-toggles");
        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.mode = Mode::Agent;

        assert_eq!(app.backend, Backend::Pi);
        app.on_key(ctrl('b'));
        assert_eq!(app.backend, Backend::PiCodex);
        app.on_key(ctrl('b'));
        assert_eq!(app.backend, Backend::Pi);

        assert!(!app.edit_mode);
        app.on_key(ctrl('e'));
        assert!(app.edit_mode);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn permission_demo_cycle_and_cancel() {
        let dir = scratch_repo("perm-cycle");
        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.on_key(ctrl('p'));
        assert!(app.overlay == Overlay::Permission);
        assert_eq!(app.perm_focus, 1);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.perm_focus, 2);
        app.on_key(key(KeyCode::Esc));
        assert!(app.overlay == Overlay::None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn symbol_jump_close_discards_filter_state_choice() {
        let dir = scratch_repo("symjump-close");
        commit_file(&dir, "lib.rs", "fn foo() {}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults());
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
            for s in &mut cf.hunk_selected {
                *s = false;
            }
        }
        app.on_key(key(KeyCode::Char('g')));
        assert!(!app.agent_running);
        assert_eq!(app.commit_message_status.as_deref(), Some("Nothing selected to summarize."));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn selected_diff_text_includes_only_selected_hunks() {
        let (mut app, dir) = two_file_app("diff-text");
        // a.txt selected (default), b.txt deselected.
        for s in &mut app.curation_files[1].hunk_selected {
            *s = false;
        }
        let text = app.selected_diff_text();
        assert!(text.contains("a.txt"), "{text}");
        assert!(text.contains("a1-changed"), "{text}");
        assert!(!text.contains("b1-changed"), "{text}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn iterate_prompt_groups_notes_by_file_with_line_annotations() {
        let (mut app, dir) = two_file_app("iterate-prompt");
        // a.txt selected (default from two_file_app); b.txt is not.
        app.project.files[1].selected = false;

        app.notes.push(Note { path: "a.txt".to_string(), line: Some(2), text: "tighten this up".to_string() });
        app.notes.push(Note { path: "a.txt".to_string(), line: None, text: "consider a rename".to_string() });
        // Excluded: not a selected file.
        app.notes.push(Note { path: "b.txt".to_string(), line: Some(1), text: "should not appear".to_string() });

        let prompt = app.build_iterate_prompt();
        assert!(prompt.contains("File: a.txt"), "{prompt}");
        assert!(prompt.contains("Line 2: tighten this up"), "{prompt}");
        assert!(prompt.contains("consider a rename"), "{prompt}");
        assert!(!prompt.contains("b.txt"), "{prompt}");
        assert!(!prompt.contains("should not appear"), "{prompt}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn iterate_prompt_includes_flagged_selected_files_and_honest_empty_state() {
        let (mut app, dir) = two_file_app("iterate-prompt-flagged");
        app.project.files[0].flagged = true;
        app.project.files[1].selected = false; // no notes, no flag, excluded

        let prompt = app.build_iterate_prompt();
        assert!(prompt.contains("File: a.txt"), "{prompt}");
        assert!(prompt.contains("Flagged for rework"), "{prompt}");
        assert!(!prompt.contains("b.txt"), "{prompt}");

        app.project.files[0].flagged = false;
        app.project.files[1].selected = false;
        let empty_prompt = app.build_iterate_prompt();
        assert!(empty_prompt.contains("No specific notes were left"), "{empty_prompt}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_message_purpose_routes_text_and_end_away_from_the_transcript() {
        let dir = scratch_repo("purpose-text");
        let mut app = App::new(dir.clone(), Keymap::defaults());
        let transcript_len_before = app.transcript.len();

        app.agent_purpose = TurnPurpose::CommitMessage;
        app.commit_message_status = Some("Generating\u{2026}".to_string());
        app.apply_agent_event(AgentEvent::Text("feat: add the thing".to_string()));
        assert_eq!(app.commit_message, "feat: add the thing");
        assert_eq!(app.transcript.len(), transcript_len_before, "should not touch the chat transcript");

        app.apply_agent_event(AgentEvent::AgentEnd);
        assert!(app.open_editor_requested, "completing generation should request the editor");
        assert!(app.commit_message_status.is_none());
        assert!(!app.agent_running);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_message_purpose_routes_errors_to_status_not_transcript() {
        let dir = scratch_repo("purpose-error");
        let mut app = App::new(dir.clone(), Keymap::defaults());
        let transcript_len_before = app.transcript.len();

        app.agent_purpose = TurnPurpose::CommitMessage;
        app.apply_agent_event(AgentEvent::Error("pi exploded".to_string()));
        assert!(app.commit_message_status.as_deref().unwrap().contains("pi exploded"));
        assert_eq!(app.transcript.len(), transcript_len_before);
        assert!(!app.open_editor_requested, "an error shouldn't open the editor");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_finder_fuzzy_filters_and_opens_the_selected_file() {
        let dir = scratch_repo("finder");
        commit_file(&dir, "main.rs", "fn main() {}\n");
        commit_file(&dir, "readme.md", "# hi\n");
        let mut app = App::new(dir.clone(), Keymap::defaults());

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
        assert!(app.mode == Mode::Navigate);
        assert_eq!(app.nav_file.file_name().unwrap(), "main.rs");
        assert!(app.nav_focus == NavFocus::Source);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_finder_esc_closes_without_opening_anything() {
        let dir = scratch_repo("finder-close");
        commit_file(&dir, "a.rs", "fn a() {}\n");
        let mut app = App::new(dir.clone(), Keymap::defaults());
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
        let mut app = App::new(dir.clone(), Keymap::defaults());
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
        let mut app = App::new(dir.clone(), Keymap::defaults());
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
        let mut app = App::new(dir.clone(), Keymap::defaults());
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
        let mut app = App::new(dir.clone(), Keymap::defaults());
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
        let mut app = App::new(dir.clone(), Keymap::defaults());
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
        let mut app = App::new(dir.clone(), Keymap::defaults());
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
}
