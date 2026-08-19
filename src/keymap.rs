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
    SwitchReview,
    SwitchAgent,
    SwitchCurate,
    OpenSymbolJump,
    OpenFileFinder,

    ReviewUp,
    ReviewDown,
    ReviewPageUp,
    ReviewPageDown,
    ReviewHome,
    ReviewEnd,
    ReviewOpen,
    ReviewToggleFocus,
    ReviewScrollLeft,
    ReviewScrollRight,
    ReviewToggleHover,
    ReviewOpenSymbolJump,
    ReviewToggleView,
    ReviewMarkGood,
    ReviewFlagRework,
    ReviewComment,
    ReviewSplitView,
    ReviewUnifiedView,
    ReviewIterate,

    SymbolUp,
    SymbolDown,
    SymbolJumpTo,
    SymbolClose,

    FinderUp,
    FinderDown,
    FinderOpen,
    FinderClose,

    NoteConfirm,
    NoteCancel,

    AgentSend,
    AgentScrollUp,
    AgentScrollDown,

    CurateUp,
    CurateDown,
    CurateHunkPrev,
    CurateHunkNext,
    CurateToggleHunk,
    CurateEditMessage,
    CurateGenerateMessage,
    CurateCommit,
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
    // Two chords each: F-keys are the traditional binding, but laptop
    // keyboards (MacBooks especially) map F1-F3 to brightness/Mission
    // Control/etc by default and need Fn held to send the real F-key, so a
    // Ctrl+letter fallback that always reaches the app is bound alongside.
    // (Ctrl+digit was tried first but doesn't work: outside the Kitty
    // keyboard protocol, terminals encode Ctrl+2/Ctrl+3/etc as legacy C0
    // control bytes that either collide with other keys — Ctrl+3 is
    // indistinguishable from plain Escape — or don't reach the app with
    // the modifier intact at all. Ctrl+letter avoids both problems: it's
    // real C0 range (0x01-0x1A), universally supported, and distinct from
    // every other key.)
    b!(Action::SwitchReview, "switch_review", "Switch: Review", "Global", "f1,ctrl+r", "Jump to the Review screen (file tree + diff/source)"),
    b!(Action::SwitchAgent, "switch_agent", "Switch: Agent", "Global", "f2,ctrl+a", "Jump to the Agent screen"),
    b!(Action::SwitchCurate, "switch_curate", "Switch: Curate", "Global", "f3,ctrl+u", "Jump to the Curation screen"),
    b!(Action::OpenSymbolJump, "open_symbol_jump", "Open Symbol Jump", "Global", "ctrl+k", "Open the fuzzy symbol-jump overlay from anywhere"),
    b!(Action::OpenFileFinder, "open_file_finder", "Open File Finder", "Global", "ctrl+f", "Open the fuzzy file finder (with live preview) from anywhere"),

    b!(Action::ReviewUp, "review_up", "Move up", "Review", "up", "Move up in whichever pane is focused (k also always works)"),
    b!(Action::ReviewDown, "review_down", "Move down", "Review", "down", "Move down in whichever pane is focused (j also always works)"),
    b!(Action::ReviewPageUp, "review_page_up", "Page up", "Review", "pageup", "Move up a page in whichever pane is focused"),
    b!(Action::ReviewPageDown, "review_page_down", "Page down", "Review", "pagedown", "Move down a page in whichever pane is focused"),
    b!(Action::ReviewHome, "review_home", "Jump to top", "Review", "home", "Jump to the first entry/line in the focused pane"),
    b!(Action::ReviewEnd, "review_end", "Jump to bottom", "Review", "end", "Jump to the last entry/line in the focused pane"),
    b!(Action::ReviewOpen, "review_open", "Open file", "Review", "enter", "Open the selected file and focus the content pane"),
    b!(Action::ReviewToggleFocus, "review_toggle_focus", "Switch pane", "Review", "tab", "Switch focus between the tree and content panes"),
    b!(Action::ReviewScrollLeft, "review_scroll_left", "Scroll left", "Review", "left", "Scroll the content pane left (source view only, while it's focused)"),
    b!(Action::ReviewScrollRight, "review_scroll_right", "Scroll right", "Review", "right", "Scroll the content pane right (source view only, while it's focused)"),
    b!(Action::ReviewToggleHover, "review_toggle_hover", "Toggle hover", "Review", "h", "Show/hide symbol info for the current line (source view only)"),
    b!(Action::ReviewOpenSymbolJump, "review_open_symbol_jump", "Open symbol jump", "Review", "/", "Open the fuzzy symbol-jump overlay"),
    b!(Action::ReviewToggleView, "review_toggle_view", "Toggle diff/source", "Review", "v", "Switch the content pane between diff and source (only if the file has changes)"),
    b!(Action::ReviewMarkGood, "review_mark_good", "Mark good", "Review", "g", "Clear notes/flag on the open file"),
    b!(Action::ReviewFlagRework, "review_flag_rework", "Flag rework", "Review", "x", "Flag the open file as needing a redo"),
    b!(Action::ReviewComment, "review_comment", "Comment", "Review", "c", "In source view: comment on the current line. Otherwise: comment on the whole file"),
    b!(Action::ReviewSplitView, "review_split_view", "Split view", "Review", "s", "Switch the diff view to before/after columns"),
    b!(Action::ReviewUnifiedView, "review_unified_view", "Unified view", "Review", "u", "Switch the diff view back to unified"),
    // Plain Enter, not Ctrl+Enter: most terminals collapse Ctrl+Enter to the
    // same bare CR byte as Enter (no modifier bit survives), so a chord that
    // requires the Ctrl modifier silently never matches outside terminals
    // with the Kitty keyboard protocol enabled. Enter already means "open
    // file" here, so iterate gets its own mnemonic letter instead.
    b!(Action::ReviewIterate, "review_iterate", "Iterate", "Review", "i", "Send queued notes to the real pi agent"),

    b!(Action::SymbolUp, "symbol_up", "Move up", "Symbol Jump", "up", "Move the result selection up"),
    b!(Action::SymbolDown, "symbol_down", "Move down", "Symbol Jump", "down", "Move the result selection down"),
    b!(Action::SymbolJumpTo, "symbol_jump_to", "Jump", "Symbol Jump", "enter", "Jump to the selected symbol's definition"),
    b!(Action::SymbolClose, "symbol_close", "Close", "Symbol Jump", "esc", "Close the overlay without jumping"),

    b!(Action::FinderUp, "finder_up", "Move up", "File Finder", "up", "Move the result selection up"),
    b!(Action::FinderDown, "finder_down", "Move down", "File Finder", "down", "Move the result selection down"),
    b!(Action::FinderOpen, "finder_open", "Open", "File Finder", "enter", "Open the selected file"),
    b!(Action::FinderClose, "finder_close", "Close", "File Finder", "esc", "Close the overlay without opening"),

    b!(Action::NoteConfirm, "note_confirm", "Save note", "Note", "enter", "Save the note and close the overlay"),
    b!(Action::NoteCancel, "note_cancel", "Cancel", "Note", "esc", "Discard and close without saving"),

    b!(Action::AgentSend, "agent_send", "Send prompt", "Agent", "enter", "Send the typed prompt to the real pi agent"),
    b!(Action::AgentScrollUp, "agent_scroll_up", "Scroll up", "Agent", "pageup", "Scroll the transcript up to review history"),
    b!(Action::AgentScrollDown, "agent_scroll_down", "Scroll down", "Agent", "pagedown", "Scroll the transcript back down toward the latest"),

    b!(Action::CurateUp, "curate_up", "Move up", "Curate", "up", "Select the previous file"),
    b!(Action::CurateDown, "curate_down", "Move down", "Curate", "down", "Select the next file"),
    b!(Action::CurateHunkPrev, "curate_hunk_prev", "Previous hunk", "Curate", "left", "View the previous hunk in this file"),
    b!(Action::CurateHunkNext, "curate_hunk_next", "Next hunk", "Curate", "right", "View the next hunk in this file"),
    b!(Action::CurateToggleHunk, "curate_toggle_hunk", "Toggle hunk", "Curate", "space", "Select/deselect the hunk currently shown"),
    b!(Action::CurateEditMessage, "curate_edit_message", "Edit message", "Curate", "e", "Open the commit message in $EDITOR"),
    b!(Action::CurateGenerateMessage, "curate_generate_message", "Generate message", "Curate", "g", "Draft a message from the real diff with pi, then open it in $EDITOR for a last pass"),
    b!(Action::CurateCommit, "curate_commit", "Commit", "Curate", "c", "git commit the selected hunks with the drafted message"),
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
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            f if f.len() >= 2 && f.starts_with('f') && f[1..].chars().all(|c| c.is_ascii_digit()) => {
                KeyCode::F(f[1..].parse().ok()?)
            }
            single if single.chars().count() == 1 => KeyCode::Char(single.chars().next().unwrap()),
            _ => return None,
        };
        Some(KeyChord { code, mods })
    }

    /// Parses a comma-separated list of chords (e.g. `"f1,ctrl+r"`), so one
    /// action can be reachable by more than one key. Fails if any entry
    /// fails to parse.
    pub fn parse_list(s: &str) -> Option<Vec<KeyChord>> {
        s.split(',').map(|part| KeyChord::parse(part.trim())).collect()
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
            KeyCode::PageUp => write!(f, "PgUp"),
            KeyCode::PageDown => write!(f, "PgDn"),
            KeyCode::Home => write!(f, "Home"),
            KeyCode::End => write!(f, "End"),
            KeyCode::F(n) => write!(f, "F{n}"),
            KeyCode::Char(c) => write!(f, "{c}"),
            other => write!(f, "{other:?}"),
        }
    }
}

pub struct Keymap {
    chords: HashMap<Action, Vec<KeyChord>>,
}

impl Keymap {
    pub fn defaults() -> Keymap {
        let mut chords = HashMap::new();
        for b in BINDINGS {
            let list = KeyChord::parse_list(b.default)
                .unwrap_or_else(|| panic!("bad default chord {:?} for {}", b.default, b.name));
            chords.insert(b.action, list);
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
            match KeyChord::parse_list(chord_str) {
                Some(list) => {
                    keymap.chords.insert(binding.action, list);
                }
                None => warnings.push(format!("{}: {name} = {chord_str:?} isn't a recognized chord", path.display())),
            }
        }

        (keymap, warnings)
    }

    pub fn is(&self, key: &KeyEvent, action: Action) -> bool {
        self.chords.get(&action).map(|list| list.iter().any(|c| c.matches(key))).unwrap_or(false)
    }

    pub fn chords(&self, action: Action) -> &[KeyChord] {
        &self.chords[&action]
    }

    /// Overrides a binding programmatically to a single chord. The
    /// `~/.steer.toml` loader doesn't need this (it builds the map
    /// directly) — it exists so tests can exercise a specific remap without
    /// touching the real filesystem.
    #[cfg(test)]
    pub fn set(&mut self, action: Action, chord: KeyChord) {
        self.chords.insert(action, vec![chord]);
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
         review_comment = \"ctrl+enter\"\n\
         review_toggle_hover = \"shift+h\"\n\
         ```\n\n\
         Chords are `mod+mod+key`, e.g. `ctrl+enter`, `shift+tab`, `f1`, `space`, `/`, `g`. \
         Modifiers: `ctrl`, `shift`, `alt`. Bind more than one chord to the same action with a \
         comma, e.g. `f1,ctrl+r`. Unknown binding names or unparsable chords are \
         reported as warnings on startup and otherwise ignored — they never prevent steer from \
         starting.\n\n\
         Not overridable: `Ctrl+C` (always quits), and raw text entry (typing/Backspace) in the \
         agent prompt, symbol filter, and commit message editor.\n\n\
         Note: `ctrl+enter`, `ctrl+tab`, and similar Ctrl-plus-whitespace-key chords don't work \
         in most terminals — the terminal collapses them to the same byte sequence as the bare \
         key, so no modifier survives for steer to see. Prefer a plain letter or `ctrl+<letter>` \
         chord instead.\n\n",
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
            let default_chords = KeyChord::parse_list(b.default).unwrap();
            let current = keymap.chords(b.action);
            let default_str = default_chords.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(" / ");
            let current_col = if current == default_chords.as_slice() {
                String::new()
            } else {
                current.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(" / ")
            };
            out.push_str(&format!(
                "| `{}` | {} | `{}` | {} | {} |\n",
                b.name, b.label, default_str, current_col, b.help
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
            assert!(KeyChord::parse_list(b.default).is_some(), "binding {:?} has an unparsable default {:?}", b.name, b.default);
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
        let review_up = keymap.chords(Action::ReviewUp);
        assert_eq!(review_up, [KeyChord::parse("up").unwrap()]);
    }

    #[test]
    fn parse_list_splits_on_comma() {
        let list = KeyChord::parse_list("f1,ctrl+r").unwrap();
        assert_eq!(list, vec![KeyChord::parse("f1").unwrap(), KeyChord::parse("ctrl+r").unwrap()]);
    }

    #[test]
    fn parse_list_rejects_a_bad_entry_anywhere_in_the_list() {
        assert!(KeyChord::parse_list("f1,banana").is_none());
    }

    #[test]
    fn is_matches_any_chord_bound_to_an_action() {
        let keymap = Keymap::defaults();
        let f1 = KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE);
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert!(keymap.is(&f1, Action::SwitchReview));
        assert!(keymap.is(&ctrl_r, Action::SwitchReview));
    }

    #[test]
    fn is_uses_the_configured_chord() {
        let keymap = Keymap::defaults();
        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(keymap.is(&key, Action::Quit));
        assert!(!keymap.is(&key, Action::ReviewMarkGood));
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
