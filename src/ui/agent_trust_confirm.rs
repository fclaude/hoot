use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::agent_client::AgentBackend;
use crate::app::App;
use crate::theme;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let width = 70.min(area.width.saturating_sub(4)).max(20);
    let body_width = (width as usize).saturating_sub(2).max(4);

    let trust_line = match app.agent_backend {
        AgentBackend::OpenCode => "auto-approved tool calls (opencode's --auto) \u{2014} no sandbox, no approval prompt.".to_string(),
        AgentBackend::Pi => "auto-approved tool calls, plus --approve: real code execution from repo-local files, every turn.".to_string(),
    };
    let intro = format!("hoot is about to run a real `{}` process for the first time.", app.agent_backend.label());
    let scope = format!("It reads and writes this repo directly, with {trust_line}");

    let paragraphs: Vec<(String, ratatui::style::Color, bool)> = vec![
        (intro, theme::FG, true),
        (scope, theme::FG, false),
        ("git diff/git log let you review and revert tracked-file edits \u{2014} not a full undo.".to_string(), theme::DIM, false),
        ("Asked once per machine \u{2014} not shown again after this.".to_string(), theme::DIM, false),
    ];

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (text, color, bold) in &paragraphs {
        let mut style = Style::default().fg(*color);
        if *bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        for chunk in super::wrap_text(text, body_width) {
            lines.push(Line::from(Span::styled(chunk, style)));
        }
        lines.push(Line::raw(""));
    }
    lines.push(super::key_hints(&[("Enter/y", "Continue"), ("Esc/n", "Cancel this turn")]));

    let height = (lines.len() as u16 + 2).min(area.height);
    let rect = super::centered_rect(width, height, area);
    f.render_widget(Clear, rect);

    let block = super::panel_block("Before the first real agent turn").border_style(Style::default().fg(theme::ORANGE));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    f.render_widget(Paragraph::new(lines).alignment(Alignment::Left), inner);
}
