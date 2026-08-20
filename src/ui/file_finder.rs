use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::theme;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let rect = super::centered_rect(area.width.saturating_sub(6).min(150), area.height.saturating_sub(4).min(40), area);
    f.render_widget(Clear, rect);

    let results = app.file_finder_results();
    let title = format!("Find files \u{2014} {} result{}", results.len(), if results.len() == 1 { "" } else { "s" });
    let block = super::panel_block(&title);
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    draw_results(f, app, &results, cols[0]);

    let rule_lines: Vec<Line<'static>> =
        (0..cols[1].height).map(|_| Line::from(Span::styled("\u{2502}", Style::default().fg(theme::DIM)))).collect();
    f.render_widget(Paragraph::new(rule_lines), cols[1]);

    draw_preview(f, app, results.get(app.file_finder_index).copied(), cols[2]);

    let hint_area = Rect { x: rect.x, y: rect.y.saturating_sub(1).max(area.y), width: rect.width, height: 1 };
    if hint_area.y < rect.y {
        // This row sits just above `rect`, outside the `Clear` above — a
        // Paragraph only overwrites cells its own text actually reaches,
        // so without its own `Clear` here, whatever the screen underneath
        // drew at this row shows through past wherever the hint text ends.
        f.render_widget(Clear, hint_area);
        f.render_widget(
            Paragraph::new(super::key_hints(&[
                ("type", "Fuzzy filter"),
                ("\u{2191}\u{2193}", "Navigate"),
                ("Enter", "Open"),
                ("Esc", "Close"),
            ])),
            hint_area,
        );
    }
}

fn draw_results(f: &mut Frame, app: &App, results: &[&crate::data::TreeEntry], area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("> ", Style::default().fg(theme::FG)),
        Span::styled(app.file_finder_filter.clone(), Style::default().fg(theme::FG)),
        Span::styled("\u{2588}", Style::default().fg(theme::CYAN)),
    ]));
    lines.push(Line::raw(""));

    if results.is_empty() {
        lines.push(Line::from(Span::styled("No matches.", Style::default().fg(theme::DIM))));
    }

    for (i, entry) in results.iter().enumerate().take(area.height.saturating_sub(2) as usize) {
        let rel = entry.path.strip_prefix(&app.target_dir).unwrap_or(&entry.path).display().to_string();
        let marker = if i == app.file_finder_index { "\u{25b6} " } else { "  " };
        let style = if i == app.file_finder_index {
            Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::FG)
        };
        lines.push(Line::from(vec![Span::styled(marker, Style::default().fg(theme::DIM)), Span::styled(rel, style)]));
    }

    f.render_widget(Paragraph::new(lines), area);
}

fn draw_preview(f: &mut Frame, app: &App, entry: Option<&crate::data::TreeEntry>, area: Rect) {
    let Some(entry) = entry else {
        f.render_widget(Paragraph::new(Span::styled("(no file selected)", Style::default().fg(theme::DIM))), area);
        return;
    };

    let rel = entry.path.strip_prefix(&app.target_dir).unwrap_or(&entry.path).display().to_string();
    let mut lines: Vec<Line<'static>> =
        vec![Line::from(Span::styled(rel, Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD))), Line::raw("")];

    let ext = crate::syntax::ext_for(&entry.path);
    let content = crate::fsnav::read_file(&entry.path);
    let visible = area.height.saturating_sub(2) as usize;
    for src_line in content.iter().take(visible) {
        lines.push(super::review::highlighted_line(&ext, src_line, None));
    }

    f.render_widget(Paragraph::new(lines), area);
}
