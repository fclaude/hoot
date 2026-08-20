# hoot

A terminal UI I use for reviewing a git working tree alongside a coding agent — [opencode](https://opencode.ai) by default, or [pi](https://github.com/earendil-works/pi) if you prefer it — built with [ratatui](https://ratatui.rs).

One screen, three modes: look at what changed, pick exactly which hunks go into a commit, and — optionally — chat with an agent. It's a personal tool, not a polished product — expect rough edges.

## Requirements

- Rust (2021 edition)
- `git` on PATH
- [`opencode`](https://opencode.ai) on PATH (the default agent), or [`pi`](https://github.com/earendil-works/pi) if you run `--agent pi`, for the Agent and commit-message-drafting features
- `$EDITOR` set, for editing prompts and commit messages
- Optional: `pbcopy` (macOS) or `wl-copy`/`xclip`/`xsel` (Linux), if you want the clipboard shortcut

## Build & run

```sh
cargo build --release
./target/release/hoot            # review the repo in the current directory
./target/release/hoot /path/to/repo
./target/release/hoot --agent pi # drive the Agent pane with pi instead of opencode
```

It polls the working tree every second, so changes from elsewhere — another agent run, an editor, plain `git` — show up on their own.

## Modes

Switch with **F1 / F2 / F3**, or **Ctrl+R / Ctrl+U / Ctrl+A** if your laptop maps F1–F3 to brightness and the like. Review and Curate — the two you'll actually live in — get F1/F2; Agent is F3.

- **Review** — file tree + diff. Leave notes (`c`), flag files for rework (`x`), clear stale notes with `d` (this file) or `D` (everywhere), then either `i` to send it all to the agent or `y` to copy the same prompt to your clipboard if you're running the agent somewhere else.
- **Curate** — select hunks per file, draft a commit message with the agent (`g`), give it a last look in `$EDITOR` (`e`), commit (`c`).
- **Agent** — chat with a real agent session (opencode by default, pi with `--agent pi`); it reads and writes the repo directly, with tool-call permissions auto-approved (opencode's `--auto`) — there's no sandbox or approval prompt. pi additionally runs every turn, including read-only ones, with `--approve` ("trust project-local files for this run" — real code execution from whatever's in the repo, independent of the tool permissions). `git diff`/`git log` let you review and revert tracked-file edits, but that's not a full undo: a deleted untracked file, a shell command it ran, or anything it read outside the repo isn't something git can take back. Point it at repos and prompts you'd trust with your own shell. `Esc` kills a turn mid-run (also works while Curate is generating a commit message) — nothing else stops one short of quitting hoot entirely.

The first real agent turn on a given machine asks for an explicit confirmation of the above before it runs — a one-time prompt (`~/.hoot-agent-trust-ack`), not shown again after you accept it.

Quitting (`q` or `Ctrl+C`) asks for confirmation first if there's anything in-memory that would be lost — queued notes, flagged files, a drafted commit message, or a turn still running.

## Keybindings

Everything's remappable via `~/.hoot.toml`:

```toml
quit = "ctrl+q"
review_comment = "ctrl+e"
```

Run `hoot --print-keymap` to regenerate [KEYBINDINGS.md](KEYBINDINGS.md), the full reference.

## Layout

| Path | What's there |
|---|---|
| `src/app.rs` | App state and interaction logic |
| `src/ui/` | Rendering — one file per mode/overlay |
| `src/agent_client.rs` | Backend-agnostic agent types shared by both clients |
| `src/opencode_client.rs`, `src/pi_client.rs` | Spawn the chosen agent, stream its event log |
| `src/gitreview.rs`, `src/gitcommit.rs` | Real `git diff` / `git commit` |
| `src/keymap.rs` | Bindings, defaults, `~/.hoot.toml` overrides |
| `src/editor.rs`, `src/clipboard.rs` | Shelling out to `$EDITOR` and the system clipboard |
| `packaging/` | Release packaging notes (`.deb`/`.rpm`, Homebrew, COPR, AUR) |
