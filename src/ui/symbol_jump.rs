use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::theme;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let rect = super::centered_rect(area.width.saturating_sub(10).min(110), area.height.saturating_sub(6).min(30), area);
    f.render_widget(Clear, rect);

    let results = app.symbol_results();
    let title = format!("Symbol jump \u{2014} {} result{}", results.len(), if results.len() == 1 { "" } else { "s" });
    let block = super::panel_block(&title);
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("> ", Style::default().fg(theme::FG)),
        Span::styled(app.symbol_filter.clone(), Style::default().fg(theme::FG)),
        Span::styled("\u{2588}", Style::default().fg(theme::CYAN)),
    ]));
    lines.push(Line::raw(""));

    if results.is_empty() {
        lines.push(Line::from(Span::styled("No matches.", Style::default().fg(theme::DIM))));
    }

    for (i, sym) in results.iter().enumerate().take(15) {
        let marker = if i == app.symbol_index { "\u{25b6}" } else { " " };
        let name_style = if i == app.symbol_index {
            Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::FG)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker} "), Style::default().fg(theme::DIM)),
            Span::styled(sym.name.clone(), name_style),
            Span::raw("  "),
            Span::styled(sym.location(&app.target_dir), Style::default().fg(theme::DIM)),
        ]));
        lines.push(Line::from(vec![Span::raw("    "), Span::styled(sym.preview.clone(), Style::default().fg(theme::FG))]));
        lines.push(Line::raw(""));
    }
    if results.len() > 15 {
        lines.push(Line::from(Span::styled(format!("  \u{2026} and {} more", results.len() - 15), Style::default().fg(theme::DIM))));
    }

    f.render_widget(Paragraph::new(lines), inner);

    let hint_area = Rect { x: rect.x, y: rect.y.saturating_sub(1).max(area.y), width: rect.width, height: 1 };
    if hint_area.y < rect.y {
        // This row sits just above `rect`, outside the `Clear` above — a
        // Paragraph only overwrites cells its own text actually reaches,
        // so without its own `Clear` here, whatever the screen underneath
        // drew at this row shows through past wherever the hint text ends.
        f.render_widget(Clear, hint_area);
        f.render_widget(
            Paragraph::new(super::key_hints(&[("/", "Filter"), ("\u{2191}\u{2193}", "Navigate"), ("Enter", "Jump"), ("Esc", "Close")])),
            hint_area,
        );
    }
}
