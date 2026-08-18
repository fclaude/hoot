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
