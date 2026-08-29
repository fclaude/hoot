use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::theme;

/// The one confirmation in hoot that guards something git can't give back.
///
/// Everything else Curate does is either recoverable (a staged hunk can be
/// unstaged, a commit can be reset) or additive. Reverse-applying a hunk
/// removes uncommitted content, which by definition exists in no git
/// object anywhere — so the prompt names the exact unit and says plainly
/// that it isn't coming back, rather than asking a generic "are you sure".
pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let rect = super::centered_rect(72.min(area.width.saturating_sub(4)), 8.min(area.height), area);
    f.render_widget(Clear, rect);

    let block = super::panel_block("Discard?").border_style(Style::default().fg(theme::RED));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let what = app.pending_discard.as_ref().map(|d| d.what.clone()).unwrap_or_default();

    let lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled("This will ", Style::default().fg(theme::FG))),
        Line::from(Span::styled(
            super::truncate_with_ellipsis(&what, inner.width as usize),
            Style::default().fg(theme::RED).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled("It was never committed, so git has no copy to restore.", Style::default().fg(theme::ORANGE))),
        Line::raw(""),
        super::key_hints(&[("Enter", "Discard it"), ("Esc/n", "Keep it")]),
    ];

    f.render_widget(Paragraph::new(lines).alignment(Alignment::Left), inner);
}
