use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;
use crate::data::AgentLineKind;
use crate::theme;

pub fn draw(f: &mut Frame, app: &App, area: Rect, _narrow: bool) {
    let mut lines: Vec<Line<'static>> = Vec::new();

    let status = if app.agent_running { "running\u{2026}" } else { "idle" };
    let status_color = if app.agent_running { theme::ORANGE } else { theme::DIM };
    let (mode_label, mode_color) =
        if app.edit_mode { ("Edit (sandboxed writes)", theme::ORANGE) } else { ("Chat (read-only)", theme::CYAN) };
    lines.push(Line::from(vec![
        Span::styled("Backend: ", Style::default().fg(theme::CYAN)),
        Span::styled(app.backend.label(), Style::default().fg(theme::FG)),
        Span::raw("        "),
        Span::styled("Model: ", Style::default().fg(theme::CYAN)),
        Span::styled(
            app.agent_model_live.clone().unwrap_or_else(|| "\u{2014}".to_string()),
            Style::default().fg(theme::FG),
        ),
        Span::raw("        "),
        Span::styled("Status: ", Style::default().fg(theme::CYAN)),
        Span::styled(status, Style::default().fg(status_color)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Mode: ", Style::default().fg(theme::CYAN)),
        Span::styled(mode_label, Style::default().fg(mode_color).add_modifier(Modifier::BOLD)),
        Span::raw("        "),
        Span::styled("Dir: ", Style::default().fg(theme::CYAN)),
        Span::styled(app.target_dir.display().to_string(), Style::default().fg(theme::FG)),
    ]));
    lines.push(Line::raw(""));

    for tl in &app.transcript {
        let line = match tl.kind {
            AgentLineKind::Done => Line::from(vec![
                Span::styled("\u{2714} ", Style::default().fg(theme::GREEN)),
                Span::styled(tl.text.clone(), Style::default().fg(theme::FG)),
            ]),
            AgentLineKind::InProgress => Line::from(vec![
                Span::styled("\u{21bb} ", Style::default().fg(theme::ORANGE)),
                Span::styled(tl.text.clone(), Style::default().fg(theme::FG)),
            ]),
            AgentLineKind::ToolCall => Line::from(vec![
                Span::styled("\u{29d6} ", Style::default().fg(theme::ORANGE)),
                Span::styled(tl.text.clone(), Style::default().fg(theme::FG)),
            ]),
            AgentLineKind::Proposal => Line::from(Span::styled(
                format!("\u{1f4dd} {}", tl.text),
                Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
            )),
            AgentLineKind::Text | AgentLineKind::Blank => {
                Line::from(Span::styled(tl.text.clone(), Style::default().fg(theme::FG)))
            }
        };
        lines.push(line);
    }

    if app.demo_transcript {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("  \u{25b6} ", Style::default().fg(theme::DIM)),
            Span::styled("src/query/parser.rs (1 hunk)", Style::default().fg(theme::FG)),
            Span::styled("  [demo only]", Style::default().fg(theme::DIM)),
        ]));
        lines.push(Line::from(Span::styled(
            "      -   for &doc_id in phrase.docs() {",
            Style::default().fg(theme::RED),
        )));
        lines.push(Line::from(Span::styled(
            "      +   for &doc_id in phrase.docs().iter() {",
            Style::default().fg(theme::GREEN),
        )));
    }

    lines.push(Line::raw(""));
    let divider = "\u{2500}".repeat(area.width.saturating_sub(2) as usize);
    lines.push(Line::from(Span::styled(divider, Style::default().fg(theme::DIM))));
    lines.push(Line::from(vec![
        Span::styled("> ", Style::default().fg(theme::FG)),
        Span::styled(app.agent_input.clone(), Style::default().fg(theme::FG)),
        Span::styled("\u{2588}", Style::default().fg(theme::CYAN)),
    ]));

    let hints = vec![super::key_hints(&[
        ("Ctrl+E", "Toggle edit mode"),
        ("Ctrl+A", "Accept all"),
        ("Ctrl+M", "Modify"),
        ("Ctrl+R", "Reject"),
        ("Ctrl+B", "Switch backend"),
        ("Enter", "Send prompt"),
    ])];

    let title = if app.agent_running { "agent \u{2014} running" } else { "agent" };
    super::draw_panel(f, area, title, Paragraph::new(lines), &hints);
}
