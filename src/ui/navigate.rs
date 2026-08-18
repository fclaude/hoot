use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::syntax::TokenKind;
use crate::theme;

fn highlighted_line(ext: &str, src_line: &str, bg: Option<ratatui::style::Color>) -> Line<'static> {
    let base = match bg {
        Some(bg) => Style::default().bg(bg),
        None => Style::default(),
    };
    if !crate::syntax::supported(ext) {
        return Line::from(Span::styled(src_line.to_string(), base.fg(theme::FG)));
    }
    let spans = crate::syntax::highlight_line(ext, src_line)
        .into_iter()
        .map(|tok| {
            let style = match tok.kind {
                TokenKind::Plain => base.fg(theme::FG),
                TokenKind::Keyword => base.fg(theme::PURPLE).add_modifier(Modifier::BOLD),
                TokenKind::String => base.fg(theme::YELLOW),
                TokenKind::Comment => base.fg(theme::DIM),
                TokenKind::Number => base.fg(theme::CYAN),
            };
            Span::styled(tok.text, style)
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

pub fn draw(f: &mut Frame, app: &App, area: Rect, narrow: bool) {
    let sidebar_width = if narrow { 22 } else { 34 };
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(sidebar_width), Constraint::Min(0)])
        .split(area);

    draw_tree(f, app, chunks[0]);
    draw_source(f, app, chunks[1]);
}

fn draw_tree(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        app.target_dir.display().to_string(),
        Style::default().fg(theme::FG),
    )));

    for (i, entry) in app.tree.iter().enumerate() {
        let indent = "\u{2502}  ".repeat(entry.depth as usize);
        let mut spans = vec![
            Span::styled("\u{258c} ", Style::default().fg(theme::DIM)),
            Span::styled(indent, Style::default().fg(theme::DIM)),
            Span::styled("\u{251c}\u{2500} ", Style::default().fg(theme::DIM)),
            Span::styled(entry.label.clone(), Style::default().fg(theme::FG)),
        ];
        if let Some(n) = entry.child_count {
            spans.push(Span::styled(format!(" ({n})"), Style::default().fg(theme::DIM)));
        }
        if entry.path == app.nav_file {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("\u{25cf}", Style::default().fg(theme::CYAN)));
        }
        let style = if i == app.tree_index { Style::default().bg(theme::BG_SELECTION) } else { Style::default() };
        lines.push(Line::from(spans).style(style));
    }

    if app.tree.is_empty() {
        lines.push(Line::from(Span::styled("(empty directory)", Style::default().fg(theme::DIM))));
    }

    f.render_widget(Paragraph::new(lines), area);
}

fn draw_source(f: &mut Frame, app: &App, area: Rect) {
    let name = app
        .nav_file
        .strip_prefix(&app.target_dir)
        .unwrap_or(&app.nav_file)
        .display()
        .to_string();

    let ext = crate::syntax::ext_for(&app.nav_file);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, src_line) in app.source.iter().enumerate() {
        let bg = if i == app.nav_line { Some(theme::BG_SELECTION) } else { None };
        lines.push(highlighted_line(&ext, src_line, bg));
    }
    if app.source.is_empty() {
        lines.push(Line::from(Span::styled("(select a file and press Enter)", Style::default().fg(theme::DIM))));
    }

    let title = format!("{name} \u{2014} {} lines", app.source.len());
    let block = super::panel_block(&title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(lines), inner);

    if app.show_hover {
        if let Some(hover) = &app.hover {
            draw_hover(f, hover, app.nav_line, inner);
        }
    }
}

fn draw_hover(f: &mut Frame, hover: &crate::data::HoverInfo, cursor_line: usize, source_area: Rect) {
    let width = 46u16.min(source_area.width.saturating_sub(2));
    let sig_lines = hover.signature.split('\n').count().max(1) as u16;
    let height = (sig_lines + 4).min(source_area.height);

    let y_offset = (cursor_line as u16 + 1).min(source_area.height.saturating_sub(height));
    let x = source_area.x + (source_area.width / 2).min(source_area.width.saturating_sub(width));
    let y = source_area.y + y_offset;
    let rect = Rect { x, y, width, height };

    f.render_widget(Clear, rect);
    let block = super::panel_block("");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let mut lines: Vec<Line<'static>> = Vec::new();
    for sig_line in hover.signature.split('\n') {
        lines.push(Line::from(Span::styled(sig_line.to_string(), Style::default().fg(theme::FG))));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(hover.location.clone(), Style::default().fg(theme::DIM))));
    lines.push(Line::from(Span::styled(
        format!("{} reference{}", hover.references, if hover.references == 1 { "" } else { "s" }),
        Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD),
    )));

    f.render_widget(Paragraph::new(lines), inner);
}
