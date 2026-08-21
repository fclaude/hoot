mod agent_client;
mod app;
mod clipboard;
mod data;
mod demo;
mod editor;
mod fsnav;
mod gitcommit;
mod gitreview;
mod keymap;
mod markdown;
mod opencode_client;
mod pi_client;
mod syntax;
mod theme;
mod trust;
mod ui;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use agent_client::AgentBackend;
use app::App;
use keymap::Keymap;

const HELP: &str = "\
hoot — a terminal UI for reviewing a git working tree alongside an AI coding agent

Usage:
  hoot [path]                    review the repo at <path> (default: current directory)
  hoot --demo                    explore the UI on a throwaway demo repo, no setup needed
  hoot --agent <pi|opencode>     pick the agent backend (default: opencode)
  hoot --print-keymap            print the generated keybindings reference and exit
  hoot --help                    print this message and exit
  hoot --version                 print the version and exit

<path> must exist and be a git repository, or hoot exits with an error —
pass --demo instead if you just want to look around the UI. --demo builds a
small git repository in a temp directory and deletes it again on exit.
";

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut target_arg: Option<String> = None;
    // opencode is the default backend — see agent_client.rs for why each
    // is wired the way it is.
    let mut agent_backend = AgentBackend::OpenCode;
    let mut demo = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(());
            }
            "--version" | "-V" => {
                println!("hoot {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--demo" => {
                demo = true;
                continue;
            }
            "--print-keymap" => {
                let (keymap, warnings) = Keymap::load();
                for w in &warnings {
                    eprintln!("~/.hoot.toml: {w}");
                }
                print!("{}", keymap::generate_markdown(&keymap));
                return Ok(());
            }
            "--agent" => {
                let Some(value) = args.next() else {
                    eprintln!("--agent needs a value: pi or opencode");
                    std::process::exit(1);
                };
                let Some(backend) = AgentBackend::parse(&value) else {
                    eprintln!("unknown --agent value {value:?}: expected pi or opencode");
                    std::process::exit(1);
                };
                agent_backend = backend;
                continue;
            }
            _ => target_arg = Some(arg),
        }
    }

    let target_dir = target_arg.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));

    // A bad or non-existent target used to fall through silently to
    // gitreview::load's mock-data fallback — meaning a typo'd path, or
    // even a bare `--help`-shaped typo landing here as a "path", launched
    // a full TUI full of fabricated demo content with no indication
    // anything was wrong. Only --demo should ever see fake data now.
    if !demo {
        if !target_dir.exists() {
            eprintln!("error: {} doesn't exist", target_dir.display());
            std::process::exit(1);
        }
        let canonical = target_dir.canonicalize().unwrap_or_else(|_| target_dir.clone());
        if !gitreview::is_git_repo(&canonical) {
            eprintln!("error: {} isn't a git repository (pass --demo to explore the UI without one)", canonical.display());
            std::process::exit(1);
        }
    }
    // `_demo_guard` has to outlive `run()` below — its Drop impl is what
    // deletes the demo directory, so it's bound here and just left alone
    // rather than immediately discarded.
    let (target_dir, _demo_guard) = if demo {
        // A real git repository in a temp directory, not a hand-written
        // `Project` of invented hunks — see `demo.rs` for why that matters.
        // Every screen then runs the same code it would against real work,
        // and the directory deletes itself on exit.
        match demo::create_repo() {
            Ok((guard, repo)) => {
                let canonical = repo.canonicalize().unwrap_or(repo);
                (canonical, Some(guard))
            }
            Err(e) => {
                eprintln!("error: couldn't create the demo repository: {e}");
                std::process::exit(1);
            }
        }
    } else {
        (target_dir.canonicalize().unwrap_or(target_dir), None)
    };

    let (keymap, warnings) = Keymap::load();
    for w in &warnings {
        eprintln!("~/.hoot.toml: {w}");
    }
    if !warnings.is_empty() {
        eprintln!("(continuing with defaults for the above)");
    }

    // A panic anywhere after this point would otherwise unwind straight past
    // the `disable_raw_mode`/`LeaveAlternateScreen`/`show_cursor` cleanup at
    // the bottom of this function, leaving the terminal in raw mode with no
    // visible echo and the panic message itself swallowed into the alternate
    // screen — the user's left staring at a dead prompt with no indication
    // anything went wrong, and no way out short of blindly typing `reset`.
    // Best-effort restore the terminal before handing off to the default
    // hook so the panic message actually reaches a normal, readable screen.
    let default_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
        default_panic_hook(info);
    }));

    enable_raw_mode()?;
    // Ownership of the terminal's state transfers to this guard the moment
    // raw mode is on, and its Drop is the *only* path that gives it back.
    // Two things went wrong without it. An error between here and a fully
    // built `Terminal` — `EnterAlternateScreen` failing, say — returned
    // straight out of main with `?`, past cleanup that hadn't been reached
    // yet, leaving the shell in raw mode with no echo. And on the normal
    // exit path the three restore steps each ended in `?`, so the first one
    // to fail skipped the two after it: a failed `disable_raw_mode` meant
    // the alternate screen was never left and the cursor never came back.
    // Drop can't use `?`, which is exactly the property wanted here — every
    // step is attempted, independently, however the function ends.
    let _terminal_guard = TerminalGuard;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    run(&mut terminal, target_dir, keymap, agent_backend, demo)
}

/// Restores the terminal on the way out, whatever "the way out" turns out
/// to be: a clean return, an early `?` on a startup error, or an unwinding
/// panic.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    }
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    target_dir: PathBuf,
    keymap: Keymap,
    agent_backend: AgentBackend,
    demo: bool,
) -> io::Result<()> {
    let mut app = App::new(target_dir, keymap, agent_backend, demo);
    // Only the real production entry point reads real on-disk state for
    // this — see the field's doc comment on why App::new itself doesn't.
    app.agent_trust_acknowledged = trust::is_acknowledged(agent_backend);

    loop {
        app.poll_agent();
        app.sync_from_disk();
        terminal.draw(|f| ui::draw(f, &app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                app.on_key(key);
            }
        }

        if let Some(target) = app.open_editor_requested.take() {
            open_external_editor(terminal, &mut app, target)?;
        }

        if app.should_quit {
            // Every quit path lands here — not each individual keypress
            // that can set should_quit (q, Ctrl+C, the confirmation
            // dialog's Enter/q) — so a running turn's process (and
            // anything it itself spawned; see cancel_agent_turn) always
            // gets killed on the way out. Without this, quitting mid-turn
            // just orphaned the real pi/opencode subprocess: nothing kills
            // a child a Rust process spawned when that process exits, so
            // it kept running — reading and writing the repo — with no UI
            // left to show it was still happening.
            if app.agent_running {
                app.cancel_agent_turn();
            }
            return Ok(());
        }
    }
}

/// Suspends the TUI (raw mode + alternate screen) so an interactive editor
/// can draw directly to the real terminal, runs it on whichever buffer
/// `target` names, then restores the TUI and forces a full redraw.
fn open_external_editor(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App, target: app::EditorTarget) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    let initial = match target {
        app::EditorTarget::CommitMessage => app.commit_message.clone(),
        app::EditorTarget::IteratePrompt => app.iterate_draft.clone(),
    };
    let result = editor::edit_text(&initial);

    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()?;

    match target {
        app::EditorTarget::CommitMessage => app.finish_editing_commit_message(result),
        app::EditorTarget::IteratePrompt => app.finish_editing_iterate_prompt(result),
    }

    Ok(())
}
