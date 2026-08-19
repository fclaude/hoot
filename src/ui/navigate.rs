use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, NavFocus};
use crate::syntax::TokenKind;
use crate::theme;

pub(crate) fn highlighted_line(ext: &str, src_line: &str, bg: Option<ratatui::style::Color>) -> Line<'static> {
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

/// Where a scrolling window of `visible` rows over `total` items should
/// start so that `selected` stays on screen — centered when there's room,
/// clamped at both ends otherwise.
fn scroll_offset(selected: usize, total: usize, visible: usize) -> usize {
    if visible == 0 || total <= visible {
        return 0;
    }
    let max_start = total - visible;
    selected.saturating_sub(visible / 2).min(max_start)
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
    let tick_color = if app.nav_focus == NavFocus::Tree { theme::CYAN } else { theme::DIM };
    let visible = area.height.saturating_sub(1) as usize; // 1 row for the root path
    let scroll = scroll_offset(app.tree_index, app.tree.len(), visible);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        app.target_dir.display().to_string(),
        Style::default().fg(theme::FG),
    )));

    for (i, entry) in app.tree.iter().enumerate().skip(scroll).take(visible) {
        let indent = "\u{2502}  ".repeat(entry.depth as usize);
        let mut spans = vec![
            Span::styled("\u{258c} ", Style::default().fg(tick_color)),
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
        if !entry.is_dir {
            let rel = entry.path.strip_prefix(&app.target_dir).unwrap_or(&entry.path).display().to_string();
            if app.notes.iter().any(|n| n.path == rel) {
                spans.push(Span::raw(" "));
                spans.push(Span::styled("\u{1f4cc}", Style::default().fg(theme::PINK)));
            }
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

    let focused = app.nav_focus == NavFocus::Source;
    let scroll_x = app.nav_scroll_x as usize;
    let title = if !app.source.is_empty() {
        format!("{name} \u{2014} line {}/{}", app.nav_line + 1, app.source.len())
    } else {
        format!("{name} \u{2014} 0 lines")
    };
    let hints = vec![super::key_hints(&[
        ("Tab", "Switch pane"),
        ("\u{2191}\u{2193}", "Move"),
        ("PgUp/PgDn", "Page"),
        ("\u{2190}\u{2192}", "Scroll"),
        ("Enter", "Open"),
        ("h", "Hover"),
        ("/", "Symbols"),
    ])];
    let block = super::panel_block(&title).border_style(Style::default().fg(if focused { theme::CYAN } else { theme::DIM }));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1), Constraint::Length(hints.len() as u16)])
        .split(inner);

    let visible_height = rows[0].height as usize;
    let scroll_y = scroll_offset(app.nav_line, app.source.len(), visible_height);

    let ext = crate::syntax::ext_for(&app.nav_file);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, src_line) in app.source.iter().enumerate().skip(scroll_y).take(visible_height) {
        let bg = if i == app.nav_line { Some(theme::BG_SELECTION) } else { None };
        let visible_line: String = src_line.chars().skip(scroll_x).collect();
        let mut line = highlighted_line(&ext, &visible_line, bg);
        if app.notes.iter().any(|n| n.path == name && n.line == Some(i + 1)) {
            let marker_bg = bg.unwrap_or(theme::BG_PANEL);
            line.spans.insert(0, Span::styled("\u{1f4cc}", Style::default().fg(theme::PINK).bg(marker_bg)));
        }
        lines.push(line);
    }
    if app.source.is_empty() {
        lines.push(Line::from(Span::styled("(select a file and press Enter)", Style::default().fg(theme::DIM))));
    }

    f.render_widget(Paragraph::new(lines), rows[0]);
    let divider = "\u{2500}".repeat(rows[1].width as usize);
    f.render_widget(Paragraph::new(divider).style(Style::default().fg(theme::DIM)), rows[1]);
    f.render_widget(Paragraph::new(hints), rows[2]);

    if app.show_hover {
        if let Some(hover) = &app.hover {
            let on_screen_line = app.nav_line.saturating_sub(scroll_y);
            draw_hover(f, hover, on_screen_line, rows[0]);
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
