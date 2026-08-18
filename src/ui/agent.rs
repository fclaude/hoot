use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;
use crate::data::AgentLineKind;
use crate::theme;

/// Where a bottom-anchored scrolling window of `visible` rows over `total`
/// lines should start. `scroll_from_bottom` 0 means "pinned to the latest
/// content"; larger values scroll up into history.
fn transcript_scroll_start(total: usize, visible: usize, scroll_from_bottom: usize) -> usize {
    if total <= visible {
        return 0;
    }
    let max_scroll = total - visible;
    let effective = scroll_from_bottom.min(max_scroll);
    max_scroll - effective
}

/// Renders `text` with a visible block cursor at char index `cursor` —
/// highlights the character under the cursor, or shows a trailing block if
/// the cursor is past the end (the common case, right after typing).
fn input_spans(text: &str, cursor: usize) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let before: String = chars.iter().take(cursor).collect();
    let mut spans = vec![Span::styled(before, Style::default().fg(theme::FG))];
    if cursor < chars.len() {
        spans.push(Span::styled(
            chars[cursor].to_string(),
            Style::default().fg(theme::BG_PANEL).bg(theme::CYAN),
        ));
        let after: String = chars.iter().skip(cursor + 1).collect();
        spans.push(Span::styled(after, Style::default().fg(theme::FG)));
    } else {
        spans.push(Span::styled("\u{2588}", Style::default().fg(theme::CYAN)));
    }
    spans
}

pub fn draw(f: &mut Frame, app: &App, area: Rect, _narrow: bool) {
    let status = if app.agent_running { "running\u{2026}" } else { "idle" };
    let status_color = if app.agent_running { theme::ORANGE } else { theme::DIM };
    let (mode_label, mode_color) =
        if app.edit_mode { ("Edit (sandboxed writes)", theme::ORANGE) } else { ("Chat (read-only)", theme::CYAN) };

    let header = vec![
        Line::from(vec![
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
        ]),
        Line::from(vec![
            Span::styled("Mode: ", Style::default().fg(theme::CYAN)),
            Span::styled(mode_label, Style::default().fg(mode_color).add_modifier(Modifier::BOLD)),
            Span::raw("        "),
            Span::styled("Dir: ", Style::default().fg(theme::CYAN)),
            Span::styled(app.target_dir.display().to_string(), Style::default().fg(theme::FG)),
        ]),
    ];

    let mut transcript: Vec<Line<'static>> = Vec::new();
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
        transcript.push(line);
    }
    if app.demo_transcript {
        transcript.push(Line::raw(""));
        transcript.push(Line::from(vec![
            Span::styled("  \u{25b6} ", Style::default().fg(theme::DIM)),
            Span::styled("src/query/parser.rs (1 hunk)", Style::default().fg(theme::FG)),
            Span::styled("  [demo only]", Style::default().fg(theme::DIM)),
        ]));
        transcript.push(Line::from(Span::styled(
            "      -   for &doc_id in phrase.docs() {",
            Style::default().fg(theme::RED),
        )));
        transcript.push(Line::from(Span::styled(
            "      +   for &doc_id in phrase.docs().iter() {",
            Style::default().fg(theme::GREEN),
        )));
    }

    let hints = vec![super::key_hints(&[
        ("Ctrl+E", "Edit mode"),
        ("Ctrl+A", "Accept"),
        ("Ctrl+M", "Modify"),
        ("Ctrl+R", "Reject"),
        ("Ctrl+B", "Backend"),
        ("PgUp/PgDn", "Scroll"),
        ("Enter", "Send"),
    ])];

    let title = if app.agent_running { "agent \u{2014} running" } else { "agent" };
    let block = super::panel_block(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header.len() as u16),
            Constraint::Length(1), // blank
            Constraint::Min(0),    // scrollable transcript
            Constraint::Length(1), // divider
            Constraint::Length(1), // input — always visible, never scrolled away
            Constraint::Length(1), // divider
            Constraint::Length(hints.len() as u16),
        ])
        .split(inner);

    f.render_widget(Paragraph::new(header), rows[0]);

    let visible = rows[2].height as usize;
    let total = transcript.len();
    let start = transcript_scroll_start(total, visible, app.agent_scroll);
    let scrolled_up = start + visible < total;
    f.render_widget(Paragraph::new(transcript.into_iter().skip(start).collect::<Vec<_>>()), rows[2]);

    let divider_text = if scrolled_up {
        let hidden_below = total - (start + visible);
        format!(
            "\u{2500}\u{2500} scrolled up \u{2014} PgDn to catch up ({hidden_below} newer line{} below) ",
            if hidden_below == 1 { "" } else { "s" }
        )
    } else {
        String::new()
    };
    let divider = format!("{divider_text}{}", "\u{2500}".repeat(rows[3].width.saturating_sub(divider_text.chars().count() as u16) as usize));
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(divider, Style::default().fg(if scrolled_up { theme::ORANGE } else { theme::DIM })))),
        rows[3],
    );

    let mut input_line = vec![Span::styled("> ", Style::default().fg(theme::FG))];
    input_line.extend(input_spans(&app.agent_input, app.agent_cursor));
    f.render_widget(Paragraph::new(Line::from(input_line)), rows[4]);

    let divider2 = "\u{2500}".repeat(rows[5].width as usize);
    f.render_widget(Paragraph::new(divider2).style(Style::default().fg(theme::DIM)), rows[5]);
    f.render_widget(Paragraph::new(hints), rows[6]);
}
