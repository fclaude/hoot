# steer

A terminal UI I use for reviewing a git working tree alongside a coding agent — [opencode](https://opencode.ai) by default, or [pi](https://github.com/earendil-works/pi) if you prefer it — built with [ratatui](https://ratatui.rs).

One screen, three modes: look at what changed, chat with the agent, and pick exactly which hunks go into a commit. It's a personal tool, not a polished product — expect rough edges.

## Requirements

- Rust (2021 edition)
- `git` on PATH
- [`opencode`](https://opencode.ai) on PATH (the default agent), or [`pi`](https://github.com/earendil-works/pi) if you run `--agent pi`, for the Agent and commit-message-drafting features
- `$EDITOR` set, for editing prompts and commit messages
- Optional: `pbcopy` (macOS) or `wl-copy`/`xclip`/`xsel` (Linux), if you want the clipboard shortcut

## Build & run

```sh
cargo build --release
./target/release/steer            # review the repo in the current directory
./target/release/steer /path/to/repo
./target/release/steer --agent pi # drive the Agent pane with pi instead of opencode
```

It polls the working tree every second, so changes from elsewhere — another agent run, an editor, plain `git` — show up on their own.

## Modes

Switch with **F1 / F2 / F3**, or **Ctrl+R / Ctrl+A / Ctrl+U** if your laptop maps F1–F3 to brightness and the like.

- **Review** — file tree + diff. Leave notes (`c`), flag files for rework (`x`), clear stale notes with `d` (this file) or `D` (everywhere), then either `i` to send it all to the agent or `y` to copy the same prompt to your clipboard if you're running the agent somewhere else.
- **Agent** — chat with a real agent session (opencode by default, pi with `--agent pi`); it reads and writes the repo directly, so `git diff` is the undo button.
- **Curate** — select hunks per file, draft a commit message with the agent (`g`), give it a last look in `$EDITOR` (`e`), commit (`c`).

Quitting (`q` or `Ctrl+C`) asks for confirmation first if there's anything in-memory that would be lost — queued notes, flagged files, a drafted commit message, or a turn still running.

## Keybindings

Everything's remappable via `~/.steer.toml`:

```toml
quit = "ctrl+q"
review_comment = "ctrl+e"
```

Run `steer --print-keymap` to regenerate [KEYBINDINGS.md](KEYBINDINGS.md), the full reference.

## Layout

| Path | What's there |
|---|---|
| `src/app.rs` | App state and interaction logic |
| `src/ui/` | Rendering — one file per mode/overlay |
| `src/agent_client.rs` | Backend-agnostic agent types shared by both clients |
| `src/opencode_client.rs`, `src/pi_client.rs` | Spawn the chosen agent, stream its event log |
| `src/gitreview.rs`, `src/gitcommit.rs` | Real `git diff` / `git commit` |
| `src/keymap.rs` | Bindings, defaults, `~/.steer.toml` overrides |
| `src/editor.rs`, `src/clipboard.rs` | Shelling out to `$EDITOR` and the system clipboard |
| `packaging/` | Release packaging notes (`.deb`/`.rpm`, Homebrew, COPR, AUR) |
