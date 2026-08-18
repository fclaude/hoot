mod agent;
mod curation;
mod navigate;
mod permission;
mod steer;
mod symbol_jump;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{App, Mode, Overlay};
use crate::theme;

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    f.render_widget(
        ratatui::widgets::Block::default().style(Style::default().bg(theme::BG_OUTER)),
        area,
    );

    let narrow = area.width < 100;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    draw_status_line(f, app, chunks[0], narrow);

    match app.mode {
        Mode::Steer => steer::draw(f, app, chunks[1], narrow),
        Mode::Navigate => navigate::draw(f, app, chunks[1], narrow),
        Mode::Agent => agent::draw(f, app, chunks[1], narrow),
        Mode::Curation => curation::draw(f, app, chunks[1], narrow),
    }

    match app.overlay {
        Overlay::SymbolJump => symbol_jump::draw(f, app, area),
        Overlay::Permission => permission::draw(f, app, area),
        Overlay::None => {}
    }
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Steer => "steer",
        Mode::Navigate => "navigate",
        Mode::Agent => "agent",
        Mode::Curation => "curate",
    }
}

fn draw_status_line(f: &mut Frame, app: &App, area: Rect, narrow: bool) {
    let left = format!(" {} — {}", mode_label(app.mode), app.project.name);

    let center = match app.mode {
        Mode::Steer => {
            if narrow {
                format!("{}/{} sel", app.files_selected(), app.project.files.len())
            } else {
                format!("{} notes queued · {} files selected", app.notes_queued(), app.files_selected())
            }
        }
        Mode::Navigate => {
            let name = app.nav_file.strip_prefix(&app.target_dir).unwrap_or(&app.nav_file).display().to_string();
            format!("{name}:{}", app.nav_line + 1)
        }
        Mode::Agent => {
            let model = app.agent_model_live.as_deref().unwrap_or("\u{2014}");
            let status = if app.agent_running { "running" } else { "idle" };
            format!("backend: {}   model: {}   {}", app.backend.label(), model, status)
        }
        Mode::Curation => {
            let (sel, total): (u32, u32) =
                app.curation_files.iter().fold((0, 0), |(s, t), f| (s + f.selected(), t + f.total()));
            format!("{}/{} hunks selected", sel, total)
        }
    };

    let right = match app.mode {
        Mode::Steer => {
            if narrow {
                "^Enter iterate".to_string()
            } else {
                "Ctrl+Enter iterate".to_string()
            }
        }
        Mode::Navigate => format!("{} symbols scanned", app.symbols.len()),
        _ => String::new(),
    };

    let mid_gap = area.width as usize;
    let used = left.len() + center.len() + right.len() + 2;
    let pad = mid_gap.saturating_sub(used);
    let left_pad = pad / 2;
    let right_pad = pad - left_pad;

    let text = format!(
        "{left}{:lw$}{center}{:rw$}{right} ",
        "",
        "",
        lw = left_pad,
        rw = right_pad
    );

    let para = Paragraph::new(text).style(Style::default().bg(theme::BG_SELECTION).fg(theme::FG));
    f.render_widget(para, area);
}

/// Bold key glyph + dim action label, per the design system's key-hint grammar.
pub fn key_hints(items: &[(&str, &str)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, (key, label)) in items.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(
            key.to_string(),
            Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(label.to_string(), Style::default().fg(theme::DIM)));
    }
    Line::from(spans)
}

pub fn panel_block(title: &str) -> Block<'static> {
    Block::default()
        .title(Span::styled(format!(" {title} "), Style::default().fg(theme::CYAN)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::DIM))
        .style(Style::default().bg(theme::BG_PANEL).fg(theme::FG))
}

pub fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect { x, y, width, height }
}

pub fn diff_line_style(kind: crate::data::DiffLineKind) -> Style {
    use crate::data::DiffLineKind::*;
    match kind {
        Context => Style::default().fg(theme::FG),
        Added => Style::default().fg(theme::GREEN),
        Removed => Style::default().fg(theme::RED),
        HunkHeader => Style::default().fg(theme::PURPLE).add_modifier(Modifier::BOLD),
    }
}

/// Renders a bordered panel with an optional divider + key-hint footer inside
/// the border, matching the box-drawn panels throughout the mockups.
pub fn draw_panel(
    f: &mut Frame,
    area: Rect,
    title: &str,
    body: Paragraph<'static>,
    hints: &[Line<'static>],
) {
    let block = panel_block(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if !hints.is_empty() {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(hints.len() as u16),
            ])
            .split(inner);
        f.render_widget(body, chunks[0]);
        let divider = "─".repeat(chunks[1].width as usize);
        f.render_widget(Paragraph::new(divider).style(Style::default().fg(theme::DIM)), chunks[1]);
        f.render_widget(Paragraph::new(hints.to_vec()), chunks[2]);
    } else {
        f.render_widget(body, inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::Keymap;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn scratch_repo(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "steer-ui-test-{label}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        for args in [["init", "-q"].as_slice(), &["config", "user.email", "test@example.com"], &["config", "user.name", "test"]] {
            assert!(Command::new("git").args(args).current_dir(&dir).status().unwrap().success());
        }
        dir
    }

    fn commit_file(dir: &std::path::Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(dir).status().unwrap();
    }

    /// Renders `app` into an in-memory buffer and flattens it to plain text
    /// (row by row, no styling) so tests can assert on visible content
    /// without a real terminal.
    fn render(app: &App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn steer_screen_shows_real_file_and_diff() {
        let dir = scratch_repo("steer");
        commit_file(&dir, "f.txt", "line1\nline2\n");
        fs::write(dir.join("f.txt"), "line1-changed\nline2\n").unwrap();

        let app = App::new(dir.clone(), Keymap::defaults());
        let screen = render(&app, 120, 30);
        assert!(screen.contains("f.txt"), "{screen}");
        assert!(screen.contains("line1-changed"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn steer_screen_reports_honest_empty_state_on_a_clean_repo() {
        let dir = scratch_repo("steer-clean");
        commit_file(&dir, "f.txt", "line1\n");

        let app = App::new(dir.clone(), Keymap::defaults());
        let screen = render(&app, 120, 30);
        assert!(screen.contains("No uncommitted changes"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn navigate_screen_shows_real_source_via_f2() {
        let dir = scratch_repo("nav");
        commit_file(&dir, "main.rs", "fn main() {\n    println!(\"hi\");\n}\n");

        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.on_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        let screen = render(&app, 120, 30);
        assert!(screen.contains("main.rs"), "{screen}");
        assert!(screen.contains("println"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_screen_shows_backend_and_chat_mode_via_f3() {
        let dir = scratch_repo("agent");
        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.on_key(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE));
        let screen = render(&app, 120, 30);
        assert!(screen.contains("Backend"), "{screen}");
        assert!(screen.contains("Chat (read-only)"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_edit_mode_toggle_is_reflected_on_screen() {
        let dir = scratch_repo("agent-edit");
        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.on_key(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        let screen = render(&app, 120, 30);
        assert!(screen.contains("Edit (sandboxed writes)"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn curation_screen_shows_real_hunk_selection_via_f4() {
        let dir = scratch_repo("curate");
        commit_file(&dir, "f.txt", "a\nb\n");
        fs::write(dir.join("f.txt"), "a-changed\nb\n").unwrap();

        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.on_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
        let screen = render(&app, 120, 30);
        assert!(screen.contains("f.txt"), "{screen}");
        assert!(screen.contains("1/1 sel"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn symbol_jump_overlay_shows_real_matches_via_ctrl_k() {
        let dir = scratch_repo("symjump");
        commit_file(&dir, "lib.rs", "pub fn parse_query(s: &str) {}\n");

        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        for c in "parse_q".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let screen = render(&app, 150, 40);
        assert!(screen.contains("parse_query"), "{screen}");
        assert!(screen.contains("lib.rs"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn symbol_jump_enter_navigates_to_the_real_definition() {
        let dir = scratch_repo("symjump-goto");
        commit_file(&dir, "lib.rs", "fn unrelated() {}\npub fn parse_query(s: &str) {}\n");

        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        for c in "parse_query".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.nav_file.file_name().unwrap(), "lib.rs");
        assert_eq!(app.nav_line, 1); // 0-indexed line of the `pub fn parse_query` definition
        let screen = render(&app, 150, 40);
        assert!(screen.contains("parse_query"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn permission_overlay_demo_via_ctrl_p() {
        let dir = scratch_repo("perm");
        let mut app = App::new(dir.clone(), Keymap::defaults());
        app.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        let screen = render(&app, 150, 30);
        assert!(screen.contains("wants to apply"), "{screen}");
        assert!(screen.contains("Approve all"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn narrow_terminal_uses_compact_steer_header() {
        let dir = scratch_repo("narrow");
        let app = App::new(dir.clone(), Keymap::defaults());
        let screen = render(&app, 80, 30);
        // Narrow layout abbreviates "Ctrl+Enter" to "^Enter" in the header.
        assert!(screen.contains("^Enter"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn wide_terminal_uses_full_steer_header() {
        let dir = scratch_repo("wide");
        let app = App::new(dir.clone(), Keymap::defaults());
        let screen = render(&app, 150, 30);
        assert!(screen.contains("Ctrl+Enter iterate"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }
}
