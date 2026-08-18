use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;
use crate::data::DiffLineKind;
use crate::theme;

pub fn draw(f: &mut Frame, app: &App, area: Rect, narrow: bool) {
    let sidebar_width = if narrow { 20 } else { 36 };
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(sidebar_width), Constraint::Min(0)])
        .split(area);

    draw_sidebar(f, app, chunks[0], narrow);

    if app.project.files.is_empty() {
        let msg = if app.review_is_real {
            format!("No uncommitted changes in {}.", app.target_dir.display())
        } else {
            "No changes to review (not a git repository — showing nothing to diff).".to_string()
        };
        let body = Paragraph::new(vec![Line::raw(""), Line::from(Span::styled(msg, Style::default().fg(theme::DIM)))]);
        super::draw_panel(f, chunks[1], "review", body, &[]);
        return;
    }

    if app.steer_split {
        draw_diff_split(f, app, chunks[1], narrow);
    } else {
        draw_diff_unified(f, app, chunks[1], narrow);
    }
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect, narrow: bool) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(app.project.root.clone(), Style::default().fg(theme::FG))));

    for (i, file) in app.project.files.iter().enumerate() {
        let mut spans = vec![Span::styled("\u{258c} ", Style::default().fg(theme::DIM))];

        let (checkbox, checkbox_style) = if file.selected {
            ("[x] ", Style::default().fg(theme::CYAN))
        } else {
            ("[ ] ", Style::default().fg(theme::DIM))
        };
        spans.push(Span::styled(checkbox, checkbox_style));

        let name = if narrow {
            file.path.rsplit('/').next().unwrap_or(&file.path).to_string()
        } else {
            file.path.to_string()
        };
        spans.push(Span::styled(name, Style::default().fg(theme::FG)));

        spans.push(Span::raw("  "));
        let hunks_label = if narrow {
            format!("{}h", file.hunk_count)
        } else {
            format!("{} hunk{}", file.hunk_count, if file.hunk_count == 1 { "" } else { "s" })
        };
        spans.push(Span::styled(hunks_label, Style::default().fg(theme::DIM)));

        if file.notes > 0 {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(format!("\u{270e}{}", file.notes), Style::default().fg(theme::ORANGE)));
        }
        if file.flagged {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("\u{2717}", Style::default().fg(theme::RED)));
        }
        if i == app.steer_selected {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("\u{25cf}", Style::default().fg(theme::CYAN)));
        }

        let style = if i == app.steer_selected {
            Style::default().bg(theme::BG_SELECTION)
        } else {
            Style::default()
        };
        lines.push(Line::from(spans).style(style));
    }

    lines.push(Line::raw(""));
    if narrow {
        lines.push(Line::from(vec![
            Span::styled("\u{258c} ", Style::default().fg(theme::DIM)),
            Span::styled(
                format!("{}/{} sel \u{b7} {} notes", app.files_selected(), app.project.files.len(), app.notes_queued()),
                Style::default().fg(theme::FG),
            ),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled("\u{258c} ", Style::default().fg(theme::DIM)),
            Span::styled(
                format!("Selected {}/{} files", app.files_selected(), app.project.files.len()),
                Style::default().fg(theme::FG),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("\u{258c} ", Style::default().fg(theme::DIM)),
            Span::styled(format!("Notes queued: {}", app.notes_queued()), Style::default().fg(theme::FG)),
        ]));
    }

    let para = Paragraph::new(lines).style(Style::default().bg(theme::BG_OUTER));
    f.render_widget(para, area);
}

fn hunk_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let file = &app.project.files[app.steer_selected];
    let mut lines: Vec<Line<'static>> = Vec::new();

    if file.hunks.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "No diff content in this demo for this file.",
            Style::default().fg(theme::DIM),
        )));
        lines.push(Line::from(Span::styled(
            "Try index/postings.rs or query/parser.rs.",
            Style::default().fg(theme::DIM),
        )));
        return lines;
    }

    for hunk in &file.hunks {
        for dl in &hunk.lines {
            let style = super::diff_line_style(dl.kind);
            lines.push(Line::from(Span::styled(dl.text.clone(), style)));
        }
        if let Some(note) = &hunk.note {
            lines.push(Line::raw(""));
            let prefix = "\u{bb} note to agent: ";
            let wrapped = wrap_note(note, prefix, width.max(20));
            for l in wrapped {
                lines.push(Line::from(Span::styled(l, Style::default().fg(theme::PINK).add_modifier(Modifier::BOLD))));
            }
        }
        lines.push(Line::raw(""));
    }
    lines
}

fn wrap_note(note: &str, prefix: &str, width: usize) -> Vec<String> {
    let note = note.strip_prefix("note to agent: ").unwrap_or(note);
    let mut out = Vec::new();
    let mut current = prefix.to_string();
    for word in note.split_whitespace() {
        if current.len() + word.len() + 1 > width {
            out.push(current);
            current = "  ".to_string();
        }
        if !current.ends_with(' ') && !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

fn diff_title(app: &App) -> String {
    app.project.files[app.steer_selected].path.clone()
}

fn draw_diff_unified(f: &mut Frame, app: &App, area: Rect, narrow: bool) {
    let content_width = area.width.saturating_sub(2) as usize;
    let body = Paragraph::new(hunk_lines(app, content_width));
    let hints = if narrow {
        vec![super::key_hints(&[("Space", "Sel"), ("c", "Comment")]), super::key_hints(&[("^Enter", "Iterate")])]
    } else {
        vec![
            super::key_hints(&[
                ("Space", "Select"),
                ("c", "Comment"),
                ("g", "Mark good"),
                ("x", "Flag rework"),
                ("\u{2191}\u{2193}", "Navigate"),
            ]),
            super::key_hints(&[
                ("Ctrl+Enter", &format!("Send {} notes \u{2192} iterate with agent", app.notes_queued())),
                ("Tab", "Next file"),
            ]),
        ]
    };
    super::draw_panel(f, area, &diff_title(app), body, &hints);
}

fn draw_diff_split(f: &mut Frame, app: &App, area: Rect, _narrow: bool) {
    let file = &app.project.files[app.steer_selected];
    let title = format!("{} \u{2014} before / after", file.path);
    let column_width = (area.width.saturating_sub(3) / 2) as usize;

    let mut before: Vec<Line<'static>> = Vec::new();
    let mut after: Vec<Line<'static>> = Vec::new();

    if file.hunks.is_empty() {
        before.push(Line::from(Span::styled("No diff content in this demo.", Style::default().fg(theme::DIM))));
    } else {
        for hunk in &file.hunks {
            for dl in &hunk.lines {
                match dl.kind {
                    DiffLineKind::HunkHeader => {
                        before.push(Line::from(Span::styled(dl.text.clone(), super::diff_line_style(dl.kind))));
                        after.push(Line::from(Span::styled(dl.text.clone(), super::diff_line_style(dl.kind))));
                    }
                    DiffLineKind::Context => {
                        before.push(Line::from(Span::styled(dl.text.clone(), super::diff_line_style(dl.kind))));
                        after.push(Line::from(Span::styled(dl.text.clone(), super::diff_line_style(dl.kind))));
                    }
                    DiffLineKind::Removed => {
                        before.push(Line::from(Span::styled(dl.text.clone(), super::diff_line_style(dl.kind))));
                    }
                    DiffLineKind::Added => {
                        after.push(Line::from(Span::styled(dl.text.clone(), super::diff_line_style(dl.kind))));
                    }
                }
            }
            let max = before.len().max(after.len());
            before.resize(max, Line::raw(""));
            after.resize(max, Line::raw(""));
            if let Some(note) = &hunk.note {
                let prefix = "\u{bb} note to agent: ";
                for l in wrap_note(note, prefix, column_width.max(20)) {
                    before.push(Line::from(Span::styled(l, Style::default().fg(theme::PINK).add_modifier(Modifier::BOLD))));
                }
                let max = before.len().max(after.len());
                before.resize(max, Line::raw(""));
                after.resize(max, Line::raw(""));
            }
        }
    }

    let block = super::panel_block(&title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1), Constraint::Length(2)])
        .split(inner);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Length(1), Constraint::Percentage(50)])
        .split(rows[0]);

    let mut before_full = vec![Line::from(Span::styled("BEFORE", Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD)))];
    before_full.extend(before);
    let mut after_full = vec![Line::from(Span::styled("AFTER", Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD)))];
    after_full.extend(after);

    f.render_widget(Paragraph::new(before_full), cols[0]);
    let rule_lines: Vec<Line<'static>> = (0..cols[1].height)
        .map(|_| Line::from(Span::styled("\u{2502}", Style::default().fg(theme::DIM))))
        .collect();
    f.render_widget(Paragraph::new(rule_lines), cols[1]);
    f.render_widget(Paragraph::new(after_full), cols[2]);

    let divider = "\u{2500}".repeat(rows[1].width as usize);
    f.render_widget(Paragraph::new(divider).style(Style::default().fg(theme::DIM)), rows[1]);

    let hints = vec![
        super::key_hints(&[
            ("Space", "Select"),
            ("c", "Comment"),
            ("g", "Mark good"),
            ("x", "Flag rework"),
            ("s", "Split"),
            ("u", "Unified"),
        ]),
        super::key_hints(&[("Ctrl+Enter", "Send notes \u{2192} iterate with agent")]),
    ];
    f.render_widget(Paragraph::new(hints), rows[2]);
}
