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
mod trust;
mod ui;

use std::io;
use std::path::{Path, PathBuf};
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

/// Writes placeholder source files under `dir`, matching the file paths
/// `data::mock_project` names, so `--demo` has something real on disk for
/// Navigate to browse and an agent to point its `--dir` at. Their content
/// has nothing to do with the mock review's fabricated hunks/notes — those
/// come from `data::mock_project`/`mock_curation_files` regardless of what
/// is or isn't on disk; this only needs to look like a plausible small
/// project when opened.
fn write_demo_files(dir: &Path) {
    const FILES: &[(&str, &str)] = &[
        ("main.rs", "mod index;\nmod query;\n\nfn main() {\n    println!(\"search-index demo\");\n}\n"),
        ("lib.rs", "pub mod index;\npub mod query;\n"),
        ("index/mod.rs", "pub mod postings;\n\npub struct Index {\n    pub postings: postings::Postings,\n}\n"),
        (
            "index/postings.rs",
            "pub struct Postings {\n    docs: Vec<u32>,\n}\n\nimpl Postings {\n    pub fn iter(&self) -> PostingsIter {\n        PostingsIter { docs: &self.docs }\n    }\n}\n\npub struct PostingsIter<'a> {\n    docs: &'a [u32],\n}\n",
        ),
        (
            "query/parser.rs",
            "pub struct QueryParser;\n\nimpl QueryParser {\n    pub fn parse(&mut self) -> Result<Query, ParseError> {\n        self.token()\n    }\n}\n",
        ),
        ("tests/integration_test.rs", "#[test]\nfn finds_matching_documents() {\n    // demo placeholder\n}\n"),
    ];
    for (rel, content) in FILES {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, content);
    }
}

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
        // Force gitreview::load's mock-data fallback regardless of what
        // the real cwd happens to be — otherwise `--demo` run from inside
        // an actual git repo (this one, say) would just show real data,
        // since App::new loads whatever target_dir resolves to with no
        // way to ask for fake data on top of a real repo. Deliberately NOT
        // a git repo itself: `gitreview::load` only takes the mock-data
        // branch while `is_git_repo` is false, so a real `.git` here would
        // replace the fabricated demo review with a real (empty) diff on
        // the very next poll tick. It does need to be a real, populated
        // directory though — Navigate/Agent read and run against whatever
        // `target_dir` resolves to on disk regardless of demo mode, so a
        // path that doesn't exist at all used to surface as a raw
        // "couldn't read" error and an agent that couldn't even spawn.
        //
        // Uses `tempfile::TempDir`, not a predictable `$TMPDIR/hoot-demo-<pid>`
        // path built by hand: a guessable name in a world-writable temp dir
        // is plantable — another local user pre-creates that exact path as a
        // symlink before hoot does, and the `fs::create_dir_all`/`fs::write`
        // calls below follow it straight into wherever they pointed it.
        // `TempDir` picks an unpredictable name and creates it atomically, so
        // there's nothing to plant in advance, and it deletes itself when
        // dropped instead of accumulating forever across runs.
        match tempfile::Builder::new().prefix("hoot-demo-").tempdir() {
            Ok(dir) => {
                write_demo_files(dir.path());
                let canonical = dir.path().canonicalize().unwrap_or_else(|_| dir.path().to_path_buf());
                (canonical, Some(dir))
            }
            Err(e) => {
                eprintln!("error: couldn't create a demo directory: {e}");
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
