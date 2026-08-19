# steer keybindings

This is generated from `src/keymap.rs` (run `steer --print-keymap` to regenerate) — it always matches what the binary actually does.

## Overriding

Create `~/.steer.toml` and set any binding name below to a new chord, e.g.:

```toml
quit = "ctrl+q"
review_comment = "ctrl+enter"
review_toggle_hover = "shift+h"
```

Chords are `mod+mod+key`, e.g. `ctrl+enter`, `shift+tab`, `f1`, `space`, `/`, `g`. Modifiers: `ctrl`, `shift`, `alt`. Bind more than one chord to the same action with a comma, e.g. `f1,ctrl+r`. Unknown binding names or unparsable chords are reported as warnings on startup and otherwise ignored — they never prevent steer from starting.

Not overridable: `Ctrl+C` (always quits), and raw text entry (typing/Backspace) in the agent prompt, symbol filter, and commit message editor.

Note: `ctrl+enter`, `ctrl+tab`, and similar Ctrl-plus-whitespace-key chords don't work in most terminals — the terminal collapses them to the same byte sequence as the bare key, so no modifier survives for steer to see. Prefer a plain letter or `ctrl+<letter>` chord instead.

## Global

| Binding name | Action | Default | Current | Description |
|---|---|---|---|---|
| `quit` | Quit | `q` |  | Exit steer |
| `switch_review` | Switch: Review | `F1 / Ctrl+r` |  | Jump to the Review screen (file tree + diff/source) |
| `switch_agent` | Switch: Agent | `F2 / Ctrl+a` |  | Jump to the Agent screen |
| `switch_curate` | Switch: Curate | `F3 / Ctrl+u` |  | Jump to the Curation screen |
| `open_symbol_jump` | Open Symbol Jump | `Ctrl+k` |  | Open the fuzzy symbol-jump overlay from anywhere |
| `open_file_finder` | Open File Finder | `Ctrl+f` |  | Open the fuzzy file finder (with live preview) from anywhere |

## Review

| Binding name | Action | Default | Current | Description |
|---|---|---|---|---|
| `review_up` | Move up | `↑` |  | Move up in whichever pane is focused (k also always works) |
| `review_down` | Move down | `↓` |  | Move down in whichever pane is focused (j also always works) |
| `review_page_up` | Page up | `PgUp` |  | Move up a page in whichever pane is focused |
| `review_page_down` | Page down | `PgDn` |  | Move down a page in whichever pane is focused |
| `review_home` | Jump to top | `Home` |  | Jump to the first entry/line in the focused pane |
| `review_end` | Jump to bottom | `End` |  | Jump to the last entry/line in the focused pane |
| `review_open` | Open file | `Enter` |  | Open the selected file and focus the content pane |
| `review_toggle_focus` | Switch pane | `Tab` |  | Switch focus between the tree and content panes |
| `review_scroll_left` | Scroll left | `←` |  | Scroll the content pane left (source view only, while it's focused) |
| `review_scroll_right` | Scroll right | `→` |  | Scroll the content pane right (source view only, while it's focused) |
| `review_toggle_hover` | Toggle hover | `h` |  | Show/hide symbol info for the current line (source view only) |
| `review_open_symbol_jump` | Open symbol jump | `/` |  | Open the fuzzy symbol-jump overlay |
| `review_toggle_view` | Toggle diff/source | `v` |  | Switch the content pane between diff and source (only if the file has changes) |
| `review_mark_good` | Mark good | `g` |  | Clear notes/flag on the open file |
| `review_flag_rework` | Flag rework | `x` |  | Flag the open file as needing a redo |
| `review_comment` | Comment | `c` |  | In source view: comment on the current line. Otherwise: comment on the whole file |
| `review_split_view` | Split view | `s` |  | Switch the diff view to before/after columns |
| `review_unified_view` | Unified view | `u` |  | Switch the diff view back to unified |
| `review_iterate` | Iterate | `i` |  | Send queued notes to the real pi agent |

## Symbol Jump

| Binding name | Action | Default | Current | Description |
|---|---|---|---|---|
| `symbol_up` | Move up | `↑` |  | Move the result selection up |
| `symbol_down` | Move down | `↓` |  | Move the result selection down |
| `symbol_jump_to` | Jump | `Enter` |  | Jump to the selected symbol's definition |
| `symbol_close` | Close | `Esc` |  | Close the overlay without jumping |

## File Finder

| Binding name | Action | Default | Current | Description |
|---|---|---|---|---|
| `finder_up` | Move up | `↑` |  | Move the result selection up |
| `finder_down` | Move down | `↓` |  | Move the result selection down |
| `finder_open` | Open | `Enter` |  | Open the selected file |
| `finder_close` | Close | `Esc` |  | Close the overlay without opening |

## Note

| Binding name | Action | Default | Current | Description |
|---|---|---|---|---|
| `note_confirm` | Save note | `Enter` |  | Save the note and close the overlay |
| `note_cancel` | Cancel | `Esc` |  | Discard and close without saving |

## Agent

| Binding name | Action | Default | Current | Description |
|---|---|---|---|---|
| `agent_send` | Send prompt | `Enter` |  | Send the typed prompt to the real pi agent |
| `agent_scroll_up` | Scroll up | `PgUp` |  | Scroll the transcript up to review history |
| `agent_scroll_down` | Scroll down | `PgDn` |  | Scroll the transcript back down toward the latest |

## Curate

| Binding name | Action | Default | Current | Description |
|---|---|---|---|---|
| `curate_up` | Move up | `↑` |  | Select the previous file |
| `curate_down` | Move down | `↓` |  | Select the next file |
| `curate_hunk_prev` | Previous hunk | `←` |  | View the previous hunk in this file |
| `curate_hunk_next` | Next hunk | `→` |  | View the next hunk in this file |
| `curate_toggle_hunk` | Toggle hunk | `Space` |  | Select/deselect the hunk currently shown |
| `curate_edit_message` | Edit message | `e` |  | Open the commit message in $EDITOR |
| `curate_generate_message` | Generate message | `g` |  | Draft a message from the real diff with pi, then open it in $EDITOR for a last pass |
| `curate_commit` | Commit | `c` |  | git commit the selected hunks with the drafted message |

