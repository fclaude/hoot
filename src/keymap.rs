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

    /// Overrides a single binding programmatically. The `~/.steer.toml`
    /// loader doesn't need this (it builds the map directly) — it exists so
    /// tests can exercise a specific remap without touching the real
    /// filesystem.
    #[cfg(test)]
    pub fn set(&mut self, action: Action, chord: KeyChord) {
        self.chords.insert(action, chord);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_binding_name_is_unique() {
        let mut names: Vec<&str> = BINDINGS.iter().map(|b| b.name).collect();
        names.sort();
        let mut deduped = names.clone();
        deduped.dedup();
        assert_eq!(names, deduped, "duplicate binding name in BINDINGS");
    }

    #[test]
    fn every_default_chord_parses() {
        // Keymap::defaults() already panics on a bad default; this just
        // makes the failure message point at the specific binding.
        for b in BINDINGS {
            assert!(KeyChord::parse(b.default).is_some(), "binding {:?} has an unparsable default {:?}", b.name, b.default);
        }
    }

    #[test]
    fn parses_plain_key() {
        let c = KeyChord::parse("g").unwrap();
        assert_eq!(c, KeyChord { code: KeyCode::Char('g'), mods: KeyModifiers::NONE });
    }

    #[test]
    fn parses_modified_key_case_insensitively() {
        let c = KeyChord::parse("Ctrl+Enter").unwrap();
        assert_eq!(c, KeyChord { code: KeyCode::Enter, mods: KeyModifiers::CONTROL });
    }

    #[test]
    fn parses_stacked_modifiers() {
        let c = KeyChord::parse("ctrl+shift+tab").unwrap();
        assert_eq!(c, KeyChord { code: KeyCode::Tab, mods: KeyModifiers::CONTROL | KeyModifiers::SHIFT });
    }

    #[test]
    fn parses_named_keys() {
        assert_eq!(KeyChord::parse("space").unwrap().code, KeyCode::Char(' '));
        assert_eq!(KeyChord::parse("esc").unwrap().code, KeyCode::Esc);
        assert_eq!(KeyChord::parse("escape").unwrap().code, KeyCode::Esc);
        assert_eq!(KeyChord::parse("f4").unwrap().code, KeyCode::F(4));
        assert_eq!(KeyChord::parse("f12").unwrap().code, KeyCode::F(12));
        assert_eq!(KeyChord::parse("/").unwrap().code, KeyCode::Char('/'));
    }

    #[test]
    fn rejects_garbage() {
        assert!(KeyChord::parse("").is_none());
        assert!(KeyChord::parse("ctrl+").is_none());
        assert!(KeyChord::parse("banana").is_none());
        assert!(KeyChord::parse("foo+g").is_none());
        assert!(KeyChord::parse("f99x").is_none());
    }

    #[test]
    fn display_round_trips_through_parse() {
        for s in ["g", "ctrl+enter", "space", "f1", "shift+tab", "/"] {
            let chord = KeyChord::parse(s).unwrap();
            let reparsed = KeyChord::parse(&chord.to_string()).unwrap();
            assert_eq!(chord, reparsed, "{s} did not round-trip through Display");
        }
    }

    #[test]
    fn matches_checks_code_and_modifiers() {
        let chord = KeyChord::parse("ctrl+a").unwrap();
        let hit = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        let wrong_mods = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        let wrong_code = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        assert!(chord.matches(&hit));
        assert!(!chord.matches(&wrong_mods));
        assert!(!chord.matches(&wrong_code));
    }

    #[test]
    fn defaults_is_populated_and_matches_binding_table() {
        let keymap = Keymap::defaults();
        assert_eq!(keymap.chords.len(), BINDINGS.len());
        let steer_up = keymap.chord(Action::SteerUp);
        assert_eq!(steer_up, KeyChord::parse("up").unwrap());
    }

    #[test]
    fn is_uses_the_configured_chord() {
        let keymap = Keymap::defaults();
        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(keymap.is(&key, Action::Quit));
        assert!(!keymap.is(&key, Action::SteerMarkGood));
    }

    #[test]
    fn generated_markdown_lists_every_binding_name() {
        let keymap = Keymap::defaults();
        let md = generate_markdown(&keymap);
        for b in BINDINGS {
            assert!(md.contains(&format!("`{}`", b.name)), "KEYBINDINGS.md missing entry for {}", b.name);
        }
    }
}
