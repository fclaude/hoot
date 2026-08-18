use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::theme;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let rect = super::centered_rect(96, 20, area);
    f.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(theme::DIM))
        .style(Style::default().bg(theme::BG_PANEL).fg(theme::FG));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let mut lines: Vec<Line<'static>> = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled("Agent", Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD)),
            Span::styled(" wants to apply the following", Style::default().fg(theme::FG)),
        ]),
        Line::raw(""),
        Line::from(Span::styled("File writes:", Style::default().fg(theme::FG))),
    ];
    if app.perm_writes.is_empty() {
        lines.push(Line::from(Span::styled("  (none)", Style::default().fg(theme::DIM))));
    }
    for w in &app.perm_writes {
        lines.push(Line::from(Span::styled(
            format!("  \u{2022} {:<32} ({}, +{} lines, -{} lines)", w.path, w.kind, w.plus, w.minus),
            Style::default().fg(theme::FG),
        )));
    }
    lines.push(Line::raw(""));
    if !app.perm_commands.is_empty() {
        lines.push(Line::from(Span::styled("Shell commands:", Style::default().fg(theme::FG))));
        for c in &app.perm_commands {
            lines.push(Line::from(Span::styled(format!("  \u{2022} {c}"), Style::default().fg(theme::FG))));
        }
        lines.push(Line::raw(""));
    }

    let options = App::perm_options();
    let mut opt_spans = vec![Span::raw(" ")];
    for (i, opt) in options.iter().enumerate() {
        if i == app.perm_focus {
            opt_spans.push(Span::styled(format!("[ {opt} ]"), Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD)));
        } else {
            opt_spans.push(Span::styled(format!("  {opt}  "), Style::default().fg(theme::FG)));
        }
        opt_spans.push(Span::raw("   "));
    }
    lines.push(Line::from(opt_spans));
    lines.push(Line::raw(""));
    lines.push(super::key_hints(&[("Tab", "cycle"), ("Enter", "confirm"), ("Esc", "cancel")]));

    f.render_widget(Paragraph::new(lines).alignment(Alignment::Left), inner);
}
