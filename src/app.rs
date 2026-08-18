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
    pub editing_commit: bool,
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
            editing_commit: false,
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
            self.spawn_turn(prompt, target_dir, ToolProfile::ReadOnly);
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
        self.spawn_turn(prompt, sandbox, ToolProfile::ReadWrite);
    }

    /// Spawns a `pi` turn in `cwd` with the given tool profile. Reuses
    /// `self.session_id` across every call in this run, so pi has real
    /// cross-turn memory via its own session storage. The visible
    /// transcript is cleared only once (to drop the initial demo content);
    /// after that, turns append rather than replace.
    fn spawn_turn(&mut self, prompt: String, cwd: PathBuf, tools: ToolProfile) {
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() || self.agent_running {
            return;
        }
        if self.demo_transcript {
            self.demo_transcript = false;
            self.transcript.clear();
        }
        self.agent_model_live = None;
        self.transcript.push(AgentLine { kind: AgentLineKind::Text, text: format!("> {prompt}") });
        self.transcript.push(AgentLine { kind: AgentLineKind::Blank, text: String::new() });

        match pi_client::spawn(&prompt, &cwd, self.backend, &self.session_id, tools) {
            Ok(session) => {
                self.pi_session = Some(session);
                self.agent_running = true;
            }
            Err(e) => {
                self.transcript.push(AgentLine {
                    kind: AgentLineKind::Text,
                    text: format!("Error: couldn't start `pi` ({e}). Is it installed and on PATH?"),
                });
            }
        }
    }

    /// Drains any events the background reader thread has queued up. Called
    /// once per event-loop tick; never blocks.
    pub fn poll_agent(&mut self) {
        if self.pi_session.is_none() {
            return;
        }
        loop {
            let event = match &self.pi_session {
                Some(s) => s.rx.try_recv(),
                None => break,
            };
            match event {
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
        if self.mode == Mode::Agent {
            if self.on_key_agent_input(key) {
                return;
            }
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
        if k.is(&key, Action::NavUp) {
            self.tree_index = self.tree_index.saturating_sub(1);
        } else if k.is(&key, Action::NavDown) {
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
