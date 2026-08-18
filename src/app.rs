use std::path::PathBuf;
use std::sync::mpsc::TryRecvError;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::data::{
    self, AgentLine, AgentLineKind, CurationFile, FileEntry, FileWrite, HoverInfo, Project,
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    SymbolJump,
    Permission,
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
    pub nav_file: PathBuf,
    pub source: Vec<String>,
    pub nav_line: usize,
    pub hover: Option<HoverInfo>,
    pub show_hover: bool,

    // SYMBOL JUMP (overlay)
    pub symbols: Vec<SymbolResult>,
    pub symbol_filter: String,
    pub symbol_index: usize,

    // AGENT
    pub target_dir: PathBuf,
    pub session_id: String,
    pub transcript: Vec<AgentLine>,
    pub agent_input: String,
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
            nav_file,
            source,
            nav_line: 0,
            hover: None,
            show_hover: true,

            symbols,
            symbol_filter: String::new(),
            symbol_index: 0,

            target_dir,
            session_id: format!("steer-{}", std::process::id()),
            transcript: data::mock_transcript(),
            agent_input: String::new(),
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
            Thinking(t) => self.transcript.push(AgentLine { kind: AgentLineKind::Text, text: format!("  {t}") }),
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
            let f = &mut self.project.files[self.steer_selected];
            f.flagged = false;
            f.notes = 0;
        } else if k.is(&key, Action::SteerFlagRework) {
            self.project.files[self.steer_selected].flagged = true;
        } else if k.is(&key, Action::SteerComment) {
            self.project.files[self.steer_selected].notes += 1;
        } else if k.is(&key, Action::SteerSplitView) {
            self.steer_split = true;
        } else if k.is(&key, Action::SteerUnifiedView) {
            self.steer_split = false;
        } else if k.is(&key, Action::SteerIterate) {
            let prompt = self.build_iterate_prompt();
            self.mode = Mode::Agent;
            self.start_agent_turn(prompt);
        }
    }

    /// Turns the queued review notes into a real prompt for `pi`.
    fn build_iterate_prompt(&self) -> String {
        let mut prompt = String::from(
            "You're iterating on review feedback for this repo. Please address the following notes:\n\n",
        );
        let mut any = false;
        for file in &self.project.files {
            if !file.selected {
                continue;
            }
            for hunk in &file.hunks {
                if let Some(note) = &hunk.note {
                    any = true;
                    prompt.push_str(&format!("- {}: {}\n", file.path, note));
                }
            }
            if file.flagged {
                any = true;
                prompt.push_str(&format!("- {}: flagged for rework, please redo this file's change.\n", file.path));
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
    fn on_key_navigate(&mut self, key: KeyEvent) {
        let n = self.tree.len();
        let k = &self.keymap;
        // j/k always move the tree too, regardless of NavUp/NavDown's
        // configured chord — vim muscle memory shouldn't require a remap.
        if k.is(&key, Action::NavUp) || key.code == KeyCode::Char('k') {
            self.tree_index = self.tree_index.saturating_sub(1);
        } else if k.is(&key, Action::NavDown) || key.code == KeyCode::Char('j') {
            self.tree_index = (self.tree_index + 1).min(n.saturating_sub(1));
        } else if k.is(&key, Action::NavOpen) {
            if let Some(entry) = self.tree.get(self.tree_index) {
                if !entry.is_dir {
                    let path = entry.path.clone();
                    self.open_file(path);
                }
            }
        } else if k.is(&key, Action::NavCursorDown) {
            self.nav_line = (self.nav_line + 1).min(self.source.len().saturating_sub(1));
            if self.show_hover {
                self.refresh_hover();
            }
        } else if k.is(&key, Action::NavCursorUp) {
            self.nav_line = self.nav_line.saturating_sub(1);
            if self.show_hover {
                self.refresh_hover();
            }
        } else if k.is(&key, Action::NavToggleHover) {
            self.show_hover = !self.show_hover;
            if self.show_hover {
                self.refresh_hover();
            }
        } else if k.is(&key, Action::NavOpenSymbolJump) {
            self.overlay = Overlay::SymbolJump;
        }
    }

    // -------------------------------------------------------------
    // SYMBOL JUMP
    // -------------------------------------------------------------
    fn filtered_symbols(&self) -> Vec<usize> {
        let needle = self.symbol_filter.to_lowercase();
        self.symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| needle.is_empty() || s.name.to_lowercase().contains(&needle))
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
    // AGENT
    // -------------------------------------------------------------
    /// Returns true if the key was consumed as text input/send for the
    /// prompt field (so the caller shouldn't fall through to anything else).
    fn on_key_agent_input(&mut self, key: KeyEvent) -> bool {
        if self.keymap.is(&key, Action::AgentSend) {
            let prompt = std::mem::take(&mut self.agent_input);
            self.start_agent_turn(prompt);
            return true;
        }
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.agent_input.push(c);
                true
            }
            KeyCode::Backspace => {
                self.agent_input.pop();
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
        assert_eq!(app.project.files[0].notes, 1);
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

        let start_line = app.nav_line;
        app.on_key(key(KeyCode::Char(']')));
        assert_eq!(app.nav_line, start_line + 1);
        app.on_key(key(KeyCode::Char('[')));
        assert_eq!(app.nav_line, start_line);

        let show = app.show_hover;
        app.on_key(key(KeyCode::Char('h')));
        assert_eq!(app.show_hover, !show);

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
}
