use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::theme;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let rect = super::centered_rect(64.min(area.width.saturating_sub(4)), 7.min(area.height), area);
    f.render_widget(Clear, rect);

    let block = super::panel_block("Quit?").border_style(Style::default().fg(theme::ORANGE));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let risk = app.quit_risk().unwrap_or_default();

    let lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            "Quitting now loses:",
            Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(risk, Style::default().fg(theme::ORANGE))),
        Line::raw(""),
        super::key_hints(&[("Enter/q/Ctrl+C", "Quit anyway"), ("Esc", "Cancel")]),
    ];

    f.render_widget(Paragraph::new(lines).alignment(Alignment::Left), inner);
}
