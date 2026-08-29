use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::theme;

use super::agent::input_spans;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let rect = super::centered_rect(90.min(area.width.saturating_sub(4)), 9.min(area.height), area);
    f.render_widget(Clear, rect);

    let block = super::panel_block("Note");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let target = match &app.note_target {
        Some(t) => match t.line {
            Some(line) => format!("{}:{line}", t.path),
            None => t.path.clone(),
        },
        None => String::new(),
    };

    let mut lines: Vec<Line<'static>> = vec![
        Line::from(vec![
            Span::styled("On: ", Style::default().fg(theme::CYAN)),
            Span::styled(target, Style::default().fg(theme::FG).add_modifier(Modifier::BOLD)),
        ]),
        Line::raw(""),
    ];
    let mut input_line = vec![Span::styled("> ", Style::default().fg(theme::FG))];
    input_line.extend(input_spans(&app.note_input, app.note_cursor));
    lines.push(Line::from(input_line));
    lines.push(Line::raw(""));
    lines.push(super::key_hints(&[("Enter", "Save"), ("Esc", "Cancel")]));

    f.render_widget(Paragraph::new(lines).alignment(Alignment::Left), inner);
}
