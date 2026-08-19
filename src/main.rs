mod app;
mod data;
mod editor;
mod fsnav;
mod gitcommit;
mod gitreview;
mod keymap;
mod pi_client;
mod sandbox;
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

use app::App;
use keymap::Keymap;

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut target_arg: Option<String> = None;
    for arg in &mut args {
        if arg == "--print-keymap" {
            let (keymap, warnings) = Keymap::load();
            for w in &warnings {
                eprintln!("~/.steer.toml: {w}");
            }
            print!("{}", keymap::generate_markdown(&keymap));
            return Ok(());
        }
        target_arg = Some(arg);
    }

    let target_dir = target_arg.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let target_dir = target_dir.canonicalize().unwrap_or(target_dir);

    let (keymap, warnings) = Keymap::load();
    for w in &warnings {
        eprintln!("~/.steer.toml: {w}");
    }
    if !warnings.is_empty() {
        eprintln!("(continuing with defaults for the above)");
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, target_dir, keymap);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, target_dir: PathBuf, keymap: Keymap) -> io::Result<()> {
    let mut app = App::new(target_dir, keymap);

    loop {
        app.poll_agent();
        app.sync_from_disk();
        terminal.draw(|f| ui::draw(f, &app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                app.on_key(key);
            }
        }

        if app.open_editor_requested {
            app.open_editor_requested = false;
            open_external_editor(terminal, &mut app)?;
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

/// Suspends the TUI (raw mode + alternate screen) so an interactive editor
/// can draw directly to the real terminal, runs it on `app.commit_message`,
/// then restores the TUI and forces a full redraw.
fn open_external_editor(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    let result = editor::edit_text(&app.commit_message);

    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()?;

    match result {
        Ok(text) => app.commit_message = text.trim_end().to_string(),
        Err(e) => app.commit_message_status = Some(e),
    }

    Ok(())
}
