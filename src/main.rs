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

const HELP: &str = "\
hoot — a terminal UI for reviewing a git working tree alongside an AI coding agent

Usage:
  hoot [path]                    review the repo at <path> (default: current directory)
  hoot --demo                    explore the UI with fake demo data, no git repo needed
  hoot --agent <pi|opencode>     pick the agent backend (default: opencode)
  hoot --print-keymap            print the generated keybindings reference and exit
  hoot --help                    print this message and exit
  hoot --version                 print the version and exit

<path> must exist and be a git repository, or hoot exits with an error —
pass --demo instead if you just want to look around the UI.
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
    let target_dir = if demo {
        // Force gitreview::load's mock-data fallback regardless of what
        // the real cwd happens to be — otherwise `--demo` run from inside
        // an actual git repo (this one, say) would just show real data,
        // since App::new loads whatever target_dir resolves to with no
        // way to ask for fake data on top of a real repo. A path that
        // can't possibly be a git repo makes App::new's own `is_git_repo`
        // check do the rest, same as the old implicit fallback did.
        std::env::temp_dir().join(format!("hoot-demo-{}", std::process::id()))
    } else {
        target_dir.canonicalize().unwrap_or(target_dir)
    };

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
