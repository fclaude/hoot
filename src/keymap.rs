//! Named key bindings, with defaults and `~/.steer.toml` overrides.
//!
//! Every *command* key (mode switches, navigation, toggles, submit actions)
//! goes through here. Raw text entry — typing into the agent prompt, the
//! symbol filter, or the commit message editor, and Backspace within those —
//! is deliberately NOT part of this table: those aren't "bindings" to remap,
//! they're literal character input. Ctrl+C-to-quit is also intentionally
//! fixed outside the keymap, as a safety net that can't be remapped away.
//!
//! [`BINDINGS`] is the single source of truth: it drives the runtime
//! defaults, the `~/.steer.toml` override parser, and the generated
//! `KEYBINDINGS.md` (see `--print-keymap`), so the doc can't drift from the
//! code.

use std::collections::HashMap;
use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Action {
    Quit,
    SwitchSteer,
    SwitchNavigate,
    SwitchAgent,
    SwitchCurate,
    OpenSymbolJump,
    OpenPermissionDemo,

    SteerUp,
    SteerDown,
    SteerNextFile,
    SteerToggleSelect,
    SteerMarkGood,
    SteerFlagRework,
    SteerComment,
    SteerSplitView,
    SteerUnifiedView,
    SteerIterate,

    NavUp,
    NavDown,
    NavOpen,
    NavCursorDown,
    NavCursorUp,
    NavToggleHover,
    NavOpenSymbolJump,

    SymbolUp,
    SymbolDown,
    SymbolJumpTo,
    SymbolClose,

    AgentSend,
    AgentAcceptAll,
    AgentModify,
    AgentReject,
    AgentSwitchBackend,
    AgentToggleEditMode,

    PermCycle,
    PermConfirm,
    PermCancel,

    CurateUp,
    CurateDown,
    CurateToggleHunk,
    CurateEditMessage,
    CurateCommit,
    CurateStopEditing,
}

pub struct Binding {
    pub action: Action,
    /// snake_case key used in ~/.steer.toml
    pub name: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub default: &'static str,
    pub help: &'static str,
}

macro_rules! b {
    ($action:expr, $name:expr, $label:expr, $group:expr, $default:expr, $help:expr) => {
        Binding { action: $action, name: $name, label: $label, group: $group, default: $default, help: $help }
    };
}

pub const BINDINGS: &[Binding] = &[
    b!(Action::Quit, "quit", "Quit", "Global", "q", "Exit steer"),
    b!(Action::SwitchSteer, "switch_steer", "Switch: Steer", "Global", "f1", "Jump to the Steer/Review screen"),
    b!(Action::SwitchNavigate, "switch_navigate", "Switch: Navigate", "Global", "f2", "Jump to the Navigate screen"),
    b!(Action::SwitchAgent, "switch_agent", "Switch: Agent", "Global", "f3", "Jump to the Agent screen"),
    b!(Action::SwitchCurate, "switch_curate", "Switch: Curate", "Global", "f4", "Jump to the Curation screen"),
    b!(Action::OpenSymbolJump, "open_symbol_jump", "Open Symbol Jump", "Global", "ctrl+k", "Open the fuzzy symbol-jump overlay from anywhere"),
    b!(Action::OpenPermissionDemo, "open_permission_demo", "Open Permission Prompt (demo)", "Global", "ctrl+p", "Open the permission-prompt overlay"),

    b!(Action::SteerUp, "steer_up", "Move up", "Steer", "up", "Select the previous file"),
    b!(Action::SteerDown, "steer_down", "Move down", "Steer", "down", "Select the next file"),
    b!(Action::SteerNextFile, "steer_next_file", "Next file", "Steer", "tab", "Select the next file"),
    b!(Action::SteerToggleSelect, "steer_toggle_select", "Toggle select", "Steer", "space", "Toggle the file into/out of the next batch"),
    b!(Action::SteerMarkGood, "steer_mark_good", "Mark good", "Steer", "g", "Clear notes/flag on this file"),
    b!(Action::SteerFlagRework, "steer_flag_rework", "Flag rework", "Steer", "x", "Flag this file as needing a redo"),
    b!(Action::SteerComment, "steer_comment", "Comment", "Steer", "c", "Queue a review note on this file"),
    b!(Action::SteerSplitView, "steer_split_view", "Split view", "Steer", "s", "Switch the diff panel to before/after columns"),
    b!(Action::SteerUnifiedView, "steer_unified_view", "Unified view", "Steer", "u", "Switch the diff panel back to unified"),
    b!(Action::SteerIterate, "steer_iterate", "Iterate", "Steer", "ctrl+enter", "Send queued notes to the real pi agent"),

    b!(Action::NavUp, "nav_up", "Move up", "Navigate", "up", "Move the tree selection up"),
    b!(Action::NavDown, "nav_down", "Move down", "Navigate", "down", "Move the tree selection down"),
    b!(Action::NavOpen, "nav_open", "Open file", "Navigate", "enter", "Open the selected file"),
    b!(Action::NavCursorDown, "nav_cursor_down", "Cursor down", "Navigate", "j", "Move the source cursor down a line"),
    b!(Action::NavCursorUp, "nav_cursor_up", "Cursor up", "Navigate", "k", "Move the source cursor up a line"),
    b!(Action::NavToggleHover, "nav_toggle_hover", "Toggle hover", "Navigate", "h", "Show/hide symbol info for the current line"),
    b!(Action::NavOpenSymbolJump, "nav_open_symbol_jump", "Open symbol jump", "Navigate", "/", "Open the fuzzy symbol-jump overlay"),

    b!(Action::SymbolUp, "symbol_up", "Move up", "Symbol Jump", "up", "Move the result selection up"),
    b!(Action::SymbolDown, "symbol_down", "Move down", "Symbol Jump", "down", "Move the result selection down"),
    b!(Action::SymbolJumpTo, "symbol_jump_to", "Jump", "Symbol Jump", "enter", "Jump to the selected symbol's definition"),
    b!(Action::SymbolClose, "symbol_close", "Close", "Symbol Jump", "esc", "Close the overlay without jumping"),

    b!(Action::AgentSend, "agent_send", "Send prompt", "Agent", "enter", "Send the typed prompt to the real pi agent"),
    b!(Action::AgentAcceptAll, "agent_accept_all", "Accept all", "Agent", "ctrl+a", "Open the real diff from Edit mode for approval"),
    b!(Action::AgentModify, "agent_modify", "Modify", "Agent", "ctrl+m", "Keep revising in the same sandbox before re-approving"),
    b!(Action::AgentReject, "agent_reject", "Reject", "Agent", "ctrl+r", "Discard the sandbox's changes"),
    b!(Action::AgentSwitchBackend, "agent_switch_backend", "Switch backend", "Agent", "ctrl+b", "Toggle pi \u{2194} pi/openai-codex"),
    b!(Action::AgentToggleEditMode, "agent_toggle_edit_mode", "Toggle edit mode", "Agent", "ctrl+e", "Chat (read-only) \u{2194} Edit (sandboxed writes)"),

    b!(Action::PermCycle, "perm_cycle", "Cycle option", "Permission", "tab", "Cycle the focused option"),
    b!(Action::PermConfirm, "perm_confirm", "Confirm", "Permission", "enter", "Confirm the focused option"),
    b!(Action::PermCancel, "perm_cancel", "Cancel", "Permission", "esc", "Close without confirming"),

    b!(Action::CurateUp, "curate_up", "Move up", "Curate", "up", "Select the previous file"),
    b!(Action::CurateDown, "curate_down", "Move down", "Curate", "down", "Select the next file"),
    b!(Action::CurateToggleHunk, "curate_toggle_hunk", "Toggle hunks", "Curate", "space", "Select/deselect all hunks for this file"),
    b!(Action::CurateEditMessage, "curate_edit_message", "Edit message", "Curate", "e", "Start editing the commit message"),
    b!(Action::CurateCommit, "curate_commit", "Commit", "Curate", "c", "git commit the selected hunks with the drafted message"),
    b!(Action::CurateStopEditing, "curate_stop_editing", "Stop editing", "Curate", "esc", "Stop editing the commit message"),
];

fn binding_by_name(name: &str) -> Option<&'static Binding> {
    BINDINGS.iter().find(|b| b.name == name)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyChord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl KeyChord {
    pub fn matches(&self, key: &KeyEvent) -> bool {
        key.code == self.code && key.modifiers == self.mods
    }

    /// Parses chord strings like "ctrl+enter", "f1", "space", "/", "g".
    pub fn parse(s: &str) -> Option<KeyChord> {
        let mut mods = KeyModifiers::NONE;
        let parts: Vec<&str> = s.split('+').map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
        let (key_part, mod_parts) = parts.split_last()?;
        for m in mod_parts {
            match m.to_lowercase().as_str() {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "shift" => mods |= KeyModifiers::SHIFT,
                "alt" | "opt" | "option" => mods |= KeyModifiers::ALT,
                _ => return None,
            }
        }
        let code = match key_part.to_lowercase().as_str() {
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "space" => KeyCode::Char(' '),
            "tab" => KeyCode::Tab,
            "backspace" => KeyCode::Backspace,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            f if f.len() >= 2 && f.starts_with('f') && f[1..].chars().all(|c| c.is_ascii_digit()) => {
                KeyCode::F(f[1..].parse().ok()?)
            }
            single if single.chars().count() == 1 => KeyCode::Char(single.chars().next().unwrap()),
            _ => return None,
        };
        Some(KeyChord { code, mods })
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.contains(KeyModifiers::CONTROL) {
            write!(f, "Ctrl+")?;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            write!(f, "Alt+")?;
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            write!(f, "Shift+")?;
        }
        match self.code {
            KeyCode::Enter => write!(f, "Enter"),
            KeyCode::Esc => write!(f, "Esc"),
            KeyCode::Char(' ') => write!(f, "Space"),
            KeyCode::Tab => write!(f, "Tab"),
            KeyCode::Backspace => write!(f, "Backspace"),
            KeyCode::Up => write!(f, "\u{2191}"),
            KeyCode::Down => write!(f, "\u{2193}"),
            KeyCode::Left => write!(f, "\u{2190}"),
            KeyCode::Right => write!(f, "\u{2192}"),
            KeyCode::F(n) => write!(f, "F{n}"),
            KeyCode::Char(c) => write!(f, "{c}"),
            other => write!(f, "{other:?}"),
        }
    }
}

pub struct Keymap {
    chords: HashMap<Action, KeyChord>,
}

impl Keymap {
    pub fn defaults() -> Keymap {
        let mut chords = HashMap::new();
        for b in BINDINGS {
            let chord = KeyChord::parse(b.default)
                .unwrap_or_else(|| panic!("bad default chord {:?} for {}", b.default, b.name));
            chords.insert(b.action, chord);
        }
        Keymap { chords }
    }

    /// Loads defaults, then applies `~/.steer.toml` overrides on top. Returns
    /// warnings for unknown action names or unparseable chord strings —
    /// invalid entries are skipped rather than failing the whole load.
    pub fn load() -> (Keymap, Vec<String>) {
        let mut keymap = Keymap::defaults();
        let mut warnings = Vec::new();

        let Some(home) = std::env::var_os("HOME") else { return (keymap, warnings) };
        let path = std::path::Path::new(&home).join(".steer.toml");
        let Ok(content) = std::fs::read_to_string(&path) else { return (keymap, warnings) };

        let table = match content.parse::<toml::Table>() {
            Ok(t) => t,
            Err(e) => {
                warnings.push(format!("{}: {e}", path.display()));
                return (keymap, warnings);
            }
        };

        for (name, value) in table {
            let Some(binding) = binding_by_name(&name) else {
                warnings.push(format!("{}: unknown binding {name:?}", path.display()));
                continue;
            };
            let Some(chord_str) = value.as_str() else {
                warnings.push(format!("{}: {name} must be a string", path.display()));
                continue;
            };
            match KeyChord::parse(chord_str) {
                Some(chord) => {
                    keymap.chords.insert(binding.action, chord);
                }
                None => warnings.push(format!("{}: {name} = {chord_str:?} isn't a recognized chord", path.display())),
            }
        }

        (keymap, warnings)
    }

    pub fn is(&self, key: &KeyEvent, action: Action) -> bool {
        self.chords.get(&action).map(|c| c.matches(key)).unwrap_or(false)
    }

    pub fn chord(&self, action: Action) -> KeyChord {
        self.chords[&action]
    }
}

/// Generates KEYBINDINGS.md content from [`BINDINGS`] (+ `keymap`'s current,
/// possibly-overridden chords) so the doc can never drift from the code.
pub fn generate_markdown(keymap: &Keymap) -> String {
    let mut out = String::new();
    out.push_str("# steer keybindings\n\n");
    out.push_str(
        "This is generated from `src/keymap.rs` (run `steer --print-keymap` to regenerate) — \
         it always matches what the binary actually does.\n\n",
    );
    out.push_str(
        "## Overriding\n\n\
         Create `~/.steer.toml` and set any binding name below to a new chord, e.g.:\n\n\
         ```toml\n\
         quit = \"ctrl+q\"\n\
         steer_iterate = \"ctrl+enter\"\n\
         nav_toggle_hover = \"shift+h\"\n\
         ```\n\n\
         Chords are `mod+mod+key`, e.g. `ctrl+enter`, `shift+tab`, `f1`, `space`, `/`, `g`. \
         Modifiers: `ctrl`, `shift`, `alt`. Unknown binding names or unparsable chords are \
         reported as warnings on startup and otherwise ignored — they never prevent steer from \
         starting.\n\n\
         Not overridable: `Ctrl+C` (always quits), and raw text entry (typing/Backspace) in the \
         agent prompt, symbol filter, and commit message editor.\n\n",
    );

    let mut groups: Vec<&'static str> = Vec::new();
    for b in BINDINGS {
        if !groups.contains(&b.group) {
            groups.push(b.group);
        }
    }

    for group in groups {
        out.push_str(&format!("## {group}\n\n"));
        out.push_str("| Binding name | Action | Default | Current | Description |\n");
        out.push_str("|---|---|---|---|---|\n");
        for b in BINDINGS.iter().filter(|b| b.group == group) {
            let default_chord = KeyChord::parse(b.default).unwrap();
            let current = keymap.chord(b.action);
            let current_col = if current == default_chord { String::new() } else { current.to_string() };
            out.push_str(&format!(
                "| `{}` | {} | `{}` | {} | {} |\n",
                b.name, b.label, default_chord, current_col, b.help
            ));
        }
        out.push('\n');
    }

    out
}
