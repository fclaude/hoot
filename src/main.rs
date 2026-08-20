mod agent_client;
mod app;
mod clipboard;
mod data;
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

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut target_arg: Option<String> = None;
    // opencode is the default backend — see agent_client.rs for why each
    // is wired the way it is.
    let mut agent_backend = AgentBackend::OpenCode;
    while let Some(arg) = args.next() {
        if arg == "--print-keymap" {
            let (keymap, warnings) = Keymap::load();
            for w in &warnings {
                eprintln!("~/.hoot.toml: {w}");
            }
            print!("{}", keymap::generate_markdown(&keymap));
            return Ok(());
        }
        if arg == "--agent" {
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
        target_arg = Some(arg);
    }

    let target_dir = target_arg.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let target_dir = target_dir.canonicalize().unwrap_or(target_dir);

    let (keymap, warnings) = Keymap::load();
    for w in &warnings {
        eprintln!("~/.hoot.toml: {w}");
    }
    if !warnings.is_empty() {
        eprintln!("(continuing with defaults for the above)");
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, target_dir, keymap, agent_backend);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    target_dir: PathBuf,
    keymap: Keymap,
    agent_backend: AgentBackend,
) -> io::Result<()> {
    let mut app = App::new(target_dir, keymap, agent_backend);

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
