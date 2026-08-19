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

/// Greedily word-wraps `text` to `width` columns. Pre-wrapping into
/// separate `Line`s (rather than relying on `Paragraph`'s own wrap) keeps
/// each transcript entry's row count known ahead of render, so the
/// bottom-anchored scroll math above stays exact. A single word longer
/// than `width` is left on its own (slightly overflowing) line rather than
/// hard-split — simpler, and rare for natural-language transcript text.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width < 4 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let would_be = if current.is_empty() { word.chars().count() } else { current.chars().count() + 1 + word.chars().count() };
        if would_be > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// Renders `text` with a visible block cursor at char index `cursor` —
/// highlights the character under the cursor, or shows a trailing block if
/// the cursor is past the end (the common case, right after typing).
pub(crate) fn input_spans(text: &str, cursor: usize) -> Vec<Span<'static>> {
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

/// Builds the full scrollable transcript, word-wrapped to `width` so
/// nothing is silently clipped — each returned `Line` is exactly one
/// rendered row, keeping the caller's scroll math exact.
fn build_transcript_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    fn wrapped(prefix: &str, prefix_color: ratatui::style::Color, text: &str, style: Style, width: usize) -> Vec<Line<'static>> {
        let indent = " ".repeat(prefix.chars().count());
        let chunks = wrap_text(text, width.saturating_sub(prefix.chars().count()).max(4));
        chunks
            .into_iter()
            .enumerate()
            .map(|(i, chunk)| {
                if i == 0 {
                    Line::from(vec![Span::styled(prefix.to_string(), Style::default().fg(prefix_color)), Span::styled(chunk, style)])
                } else {
                    Line::from(vec![Span::raw(indent.clone()), Span::styled(chunk, style)])
                }
            })
            .collect()
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    for tl in &app.transcript {
        let fg = Style::default().fg(theme::FG);
        let dim = Style::default().fg(theme::DIM);
        let mut wrapped_lines = match tl.kind {
            AgentLineKind::Done => wrapped("\u{2714} ", theme::GREEN, &tl.text, dim, width),
            AgentLineKind::InProgress => wrapped("\u{21bb} ", theme::ORANGE, &tl.text, fg, width),
            AgentLineKind::ToolCall => wrapped("\u{29d6} ", theme::ORANGE, &tl.text, dim, width),
            AgentLineKind::Proposal => {
                wrapped("\u{1f4dd} ", theme::FG, &tl.text, fg.add_modifier(Modifier::BOLD), width)
            }
            AgentLineKind::UserPrompt => wrapped("\u{276f} ", theme::CYAN, &tl.text, fg.add_modifier(Modifier::BOLD), width),
            AgentLineKind::Thinking => {
                if tl.text.is_empty() {
                    vec![Line::raw("")]
                } else {
                    wrap_text(&tl.text, width.max(4)).into_iter().map(|c| Line::from(Span::styled(c, dim))).collect()
                }
            }
            AgentLineKind::Text | AgentLineKind::Blank => {
                if tl.text.is_empty() {
                    vec![Line::raw("")]
                } else {
                    wrap_text(&tl.text, width.max(4)).into_iter().map(|c| Line::from(Span::styled(c, fg))).collect()
                }
            }
        };
        lines.append(&mut wrapped_lines);
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
    lines
}

pub fn draw(f: &mut Frame, app: &App, area: Rect, _narrow: bool) {
    let status = if app.agent_running { "running\u{2026}" } else { "idle" };
    let status_color = if app.agent_running { theme::ORANGE } else { theme::DIM };

    let header = vec![Line::from(vec![
        Span::styled("pi", Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(
            app.agent_model_live.clone().unwrap_or_else(|| "\u{2014}".to_string()),
            Style::default().fg(theme::DIM),
        ),
        Span::raw("        "),
        Span::styled(status, Style::default().fg(status_color)),
        Span::raw("        "),
        Span::styled(app.target_dir.display().to_string(), Style::default().fg(theme::DIM)),
    ])];

    let hints = vec![super::key_hints(&[
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

    let transcript = build_transcript_lines(app, rows[2].width as usize);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_text_breaks_at_word_boundaries_within_width() {
        let lines = wrap_text("the quick brown fox jumps", 11);
        for l in &lines {
            assert!(l.chars().count() <= 11, "{l:?} exceeds width");
        }
        assert_eq!(lines.join(" "), "the quick brown fox jumps");
    }

    #[test]
    fn wrap_text_leaves_an_overlong_word_on_its_own_line() {
        let lines = wrap_text("supercalifragilisticexpialidocious short", 10);
        assert_eq!(lines[0], "supercalifragilisticexpialidocious");
        assert_eq!(lines[1], "short");
    }

    #[test]
    fn wrap_text_empty_input_yields_one_empty_line() {
        assert_eq!(wrap_text("", 10), vec![""]);
    }

    #[test]
    fn wrap_text_short_text_is_a_single_line() {
        assert_eq!(wrap_text("hi", 80), vec!["hi"]);
    }

    #[test]
    fn transcript_scroll_start_pins_to_bottom_by_default() {
        assert_eq!(transcript_scroll_start(100, 20, 0), 80);
    }

    #[test]
    fn transcript_scroll_start_reaches_the_top_when_fully_scrolled() {
        assert_eq!(transcript_scroll_start(100, 20, 80), 0);
        assert_eq!(transcript_scroll_start(100, 20, 9999), 0, "should clamp, not go negative");
    }

    #[test]
    fn transcript_scroll_start_is_zero_when_content_fits() {
        assert_eq!(transcript_scroll_start(5, 20, 0), 0);
    }
}
