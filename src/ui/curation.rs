use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;
use crate::theme;

use super::agent::input_spans;

pub fn draw(f: &mut Frame, app: &App, area: Rect, narrow: bool) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let sidebar_width = if narrow { 20 } else { 30 };
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(sidebar_width), Constraint::Min(0)])
        .split(rows[0]);

    draw_sidebar(f, app, cols[0]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(45), Constraint::Min(0)])
        .split(cols[1]);

    draw_commit_box(f, app, right[0]);
    draw_hunk_box(f, app, right[1]);

    let hints = super::key_hints(&[
        ("\u{2190}\u{2192}", "Hunk"),
        ("Space", "Toggle hunk"),
        ("g", "Generate message"),
        ("e", "Quick edit"),
        ("c", "Commit"),
    ]);
    let mut spans = vec![Span::styled("\u{258c} ", Style::default().fg(theme::DIM))];
    spans.extend(hints.spans);
    match &app.last_commit {
        Some(Ok(summary)) => {
            spans.push(Span::raw("   "));
            spans.push(Span::styled(
                format!("\u{2714} {summary}"),
                Style::default().fg(theme::GREEN).add_modifier(Modifier::BOLD),
            ));
        }
        Some(Err(e)) => {
            spans.push(Span::raw("   "));
            spans.push(Span::styled(format!("\u{2717} {e}"), Style::default().fg(theme::RED).add_modifier(Modifier::BOLD)));
        }
        None => {}
    }
    f.render_widget(Paragraph::new(Line::from(spans)), rows[1]);
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(app.project.root.clone(), Style::default().fg(theme::FG))));

    for (i, cf) in app.curation_files.iter().enumerate() {
        let mut spans = vec![Span::styled("\u{258c} ", Style::default().fg(theme::DIM))];
        if let Some(status) = cf.status {
            spans.push(Span::styled(format!("{} ", status.glyph()), Style::default().fg(status.color())));
        } else {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(cf.path.clone(), Style::default().fg(theme::FG)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(format!("{}/{} sel", cf.selected(), cf.total()), Style::default().fg(theme::DIM)));
        if i == app.curation_index {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("\u{25cf}", Style::default().fg(theme::CYAN)));
        }
        let style = if i == app.curation_index { Style::default().bg(theme::BG_SELECTION) } else { Style::default() };
        lines.push(Line::from(spans).style(style));
    }

    lines.push(Line::raw(""));
    let (files, sel, total) = (
        app.curation_files.len(),
        app.curation_files.iter().map(|f| f.selected()).sum::<u32>(),
        app.curation_files.iter().map(|f| f.total()).sum::<u32>(),
    );
    lines.push(Line::from(vec![
        Span::styled("\u{258c} ", Style::default().fg(theme::DIM)),
        Span::styled(format!("Files: {files}  Selected hunks: {sel}/{total}"), Style::default().fg(theme::FG)),
    ]));

    f.render_widget(Paragraph::new(lines), area);
}

fn draw_commit_box(f: &mut Frame, app: &App, area: Rect) {
    let title = if app.review_is_real { "Commit message" } else { "Commit message (editable, drafted by local model)" };
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(status) = &app.commit_message_status {
        lines.push(Line::from(Span::styled(status.clone(), Style::default().fg(theme::ORANGE))));
        lines.push(Line::raw(""));
    }
    if app.commit_message.is_empty() && !app.editing_commit {
        lines.push(Line::from(Span::styled(
            "(empty — press g to draft one with pi, or e to write your own)",
            Style::default().fg(theme::DIM),
        )));
    } else if app.editing_commit {
        lines.extend(commit_message_lines_with_cursor(&app.commit_message, app.commit_message_cursor));
    } else {
        for l in app.commit_message.split('\n') {
            lines.push(Line::from(Span::styled(l.to_string(), Style::default().fg(theme::FG))));
        }
    }
    let hints = if app.editing_commit {
        vec![super::key_hints(&[("\u{2190}\u{2192}", "Move"), ("Home/End", "Line start/end"), ("Enter", "Newline"), ("Esc", "Stop editing")])]
    } else {
        vec![super::key_hints(&[("g", "Generate + open $EDITOR"), ("e", "Quick edit")])]
    };
    super::draw_panel(f, area, title, Paragraph::new(lines), &hints);
}

/// Renders `text` as one `Line` per `\n`-separated row, with a visible
/// block cursor on whichever row `cursor` (a char index into the whole
/// buffer, not just one row) actually falls on — everything else in
/// plain text.
fn commit_message_lines_with_cursor(text: &str, cursor: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    let mut placed = false;
    let rows: Vec<&str> = text.split('\n').collect();
    for row in &rows {
        let row_char_count = row.chars().count();
        let row_end = offset + row_char_count;
        if !placed && cursor >= offset && cursor <= row_end {
            out.push(Line::from(input_spans(row, cursor - offset)));
            placed = true;
        } else {
            out.push(Line::from(Span::styled(row.to_string(), Style::default().fg(theme::FG))));
        }
        offset = row_end + 1; // +1 skips the '\n' consumed between rows
    }
    out
}

fn draw_hunk_box(f: &mut Frame, app: &App, area: Rect) {
    let Some(cf) = app.curation_files.get(app.curation_index) else {
        let body = Paragraph::new(vec![Line::from(Span::styled("No changes to curate.", Style::default().fg(theme::DIM)))]);
        super::draw_panel(f, area, "hunk", body, &[]);
        return;
    };
    let file = app.project.files.iter().find(|f| f.path == cf.path);
    let shown_index = app.curation_hunk_index.min(cf.total().saturating_sub(1) as usize);
    let this_selected = cf.hunk_selected.get(shown_index).copied().unwrap_or(false);

    let title = format!(
        "{} \u{2014} hunk {}/{} ({}) \u{2014} {}/{} selected",
        cf.path,
        shown_index + 1,
        cf.total(),
        if this_selected { "selected" } else { "not selected" },
        cf.selected(),
        cf.total(),
    );

    let mut lines: Vec<Line<'static>> = Vec::new();
    match file.and_then(|f| f.hunks.get(shown_index)) {
        Some(hunk) => {
            for dl in &hunk.lines {
                lines.push(Line::from(Span::styled(dl.text.clone(), super::diff_line_style(dl.kind))));
            }
        }
        None => {
            lines.push(Line::from(Span::styled(
                "No cached diff content for this hunk.",
                Style::default().fg(theme::DIM),
            )));
        }
    }

    let hints = vec![super::key_hints(&[("\u{2190}\u{2192}", "Prev/next hunk"), ("Space", "Select/deselect this hunk")])];
    super::draw_panel(f, area, &title, Paragraph::new(lines), &hints);
}
