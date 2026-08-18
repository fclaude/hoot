use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;
use crate::theme;

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
        ("Space", "Toggle"),
        ("e", "Edit message"),
        ("c", "Commit"),
        ("h", "Help"),
        ("Esc", "Back"),
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
    let title = "Commit message (editable, drafted by local model)";
    let mut lines: Vec<Line<'static>> = Vec::new();
    for l in app.commit_message.split('\n') {
        lines.push(Line::from(Span::styled(l.to_string(), Style::default().fg(theme::FG))));
    }
    if app.editing_commit {
        lines.push(Line::from(Span::styled("\u{2588}", Style::default().fg(theme::CYAN))));
    }
    let hints = if app.editing_commit {
        vec![super::key_hints(&[("Esc", "Stop editing"), ("Enter", "Newline")])]
    } else {
        vec![]
    };
    super::draw_panel(f, area, title, Paragraph::new(lines), &hints);
}

fn draw_hunk_box(f: &mut Frame, app: &App, area: Rect) {
    let Some(cf) = app.curation_files.get(app.curation_index) else {
        let body = Paragraph::new(vec![Line::from(Span::styled("No changes to curate.", Style::default().fg(theme::DIM)))]);
        super::draw_panel(f, area, "hunk", body, &[]);
        return;
    };
    let file = app.project.files.iter().find(|f| f.path == cf.path);
    let shown_index = cf.hunk_selected.iter().position(|s| *s).unwrap_or(0);

    let title = format!("{} \u{2014} hunk {}/{} ({})", cf.path, shown_index + 1, cf.total(), if cf.selected() > 0 { "selected" } else { "none selected" });

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

    super::draw_panel(f, area, &title, Paragraph::new(lines), &[]);
}
