# steer keybindings

This is generated from `src/keymap.rs` (run `steer --print-keymap` to regenerate) — it always matches what the binary actually does.

## Overriding

Create `~/.steer.toml` and set any binding name below to a new chord, e.g.:

```toml
quit = "ctrl+q"
steer_iterate = "ctrl+enter"
nav_toggle_hover = "shift+h"
```

Chords are `mod+mod+key`, e.g. `ctrl+enter`, `shift+tab`, `f1`, `space`, `/`, `g`. Modifiers: `ctrl`, `shift`, `alt`. Unknown binding names or unparsable chords are reported as warnings on startup and otherwise ignored — they never prevent steer from starting.

Not overridable: `Ctrl+C` (always quits), and raw text entry (typing/Backspace) in the agent prompt, symbol filter, and commit message editor.

## Global

| Binding name | Action | Default | Current | Description |
|---|---|---|---|---|
| `quit` | Quit | `q` |  | Exit steer |
| `switch_steer` | Switch: Steer | `F1` |  | Jump to the Steer/Review screen |
| `switch_navigate` | Switch: Navigate | `F2` |  | Jump to the Navigate screen |
| `switch_agent` | Switch: Agent | `F3` |  | Jump to the Agent screen |
| `switch_curate` | Switch: Curate | `F4` |  | Jump to the Curation screen |
| `open_symbol_jump` | Open Symbol Jump | `Ctrl+k` |  | Open the fuzzy symbol-jump overlay from anywhere |
| `open_permission_demo` | Open Permission Prompt (demo) | `Ctrl+p` |  | Open the permission-prompt overlay |

## Steer

| Binding name | Action | Default | Current | Description |
|---|---|---|---|---|
| `steer_up` | Move up | `↑` |  | Select the previous file |
| `steer_down` | Move down | `↓` |  | Select the next file |
| `steer_next_file` | Next file | `Tab` |  | Select the next file |
| `steer_toggle_select` | Toggle select | `Space` |  | Toggle the file into/out of the next batch |
| `steer_mark_good` | Mark good | `g` |  | Clear notes/flag on this file |
| `steer_flag_rework` | Flag rework | `x` |  | Flag this file as needing a redo |
| `steer_comment` | Comment | `c` |  | Queue a review note on this file |
| `steer_split_view` | Split view | `s` |  | Switch the diff panel to before/after columns |
| `steer_unified_view` | Unified view | `u` |  | Switch the diff panel back to unified |
| `steer_iterate` | Iterate | `Ctrl+Enter` |  | Send queued notes to the real pi agent |

## Navigate

| Binding name | Action | Default | Current | Description |
|---|---|---|---|---|
| `nav_up` | Move up | `↑` |  | Move the tree selection up (k also always works) |
| `nav_down` | Move down | `↓` |  | Move the tree selection down (j also always works) |
| `nav_open` | Open file | `Enter` |  | Open the selected file |
| `nav_cursor_down` | Cursor down | `]` |  | Move the source cursor down a line (for hover) |
| `nav_cursor_up` | Cursor up | `[` |  | Move the source cursor up a line (for hover) |
| `nav_toggle_hover` | Toggle hover | `h` |  | Show/hide symbol info for the current line |
| `nav_open_symbol_jump` | Open symbol jump | `/` |  | Open the fuzzy symbol-jump overlay |

## Symbol Jump

| Binding name | Action | Default | Current | Description |
|---|---|---|---|---|
| `symbol_up` | Move up | `↑` |  | Move the result selection up |
| `symbol_down` | Move down | `↓` |  | Move the result selection down |
| `symbol_jump_to` | Jump | `Enter` |  | Jump to the selected symbol's definition |
| `symbol_close` | Close | `Esc` |  | Close the overlay without jumping |

## Agent

| Binding name | Action | Default | Current | Description |
|---|---|---|---|---|
| `agent_send` | Send prompt | `Enter` |  | Send the typed prompt to the real pi agent |
| `agent_accept_all` | Accept all | `Ctrl+a` |  | Open the real diff from Edit mode for approval |
| `agent_modify` | Modify | `Ctrl+m` |  | Keep revising in the same sandbox before re-approving |
| `agent_reject` | Reject | `Ctrl+r` |  | Discard the sandbox's changes |
| `agent_switch_backend` | Switch backend | `Ctrl+b` |  | Toggle pi ↔ pi/openai-codex |
| `agent_toggle_edit_mode` | Toggle edit mode | `Ctrl+e` |  | Chat (read-only) ↔ Edit (sandboxed writes) |

## Permission

| Binding name | Action | Default | Current | Description |
|---|---|---|---|---|
| `perm_cycle` | Cycle option | `Tab` |  | Cycle the focused option |
| `perm_confirm` | Confirm | `Enter` |  | Confirm the focused option |
| `perm_cancel` | Cancel | `Esc` |  | Close without confirming |

## Curate

| Binding name | Action | Default | Current | Description |
|---|---|---|---|---|
| `curate_up` | Move up | `↑` |  | Select the previous file |
| `curate_down` | Move down | `↓` |  | Select the next file |
| `curate_toggle_hunk` | Toggle hunks | `Space` |  | Select/deselect all hunks for this file |
| `curate_edit_message` | Quick edit | `e` |  | In-TUI quick edit of the commit message |
| `curate_generate_message` | Generate message | `g` |  | Draft a message from the real diff with pi, then open it in $EDITOR for a last pass |
| `curate_commit` | Commit | `c` |  | git commit the selected hunks with the drafted message |
| `curate_stop_editing` | Stop editing | `Esc` |  | Stop editing the commit message |

