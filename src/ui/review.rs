use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, ContentView, NavFocus};
use crate::data::{DiffLineKind, FileEntry};
use crate::syntax::TokenKind;
use crate::theme;

pub(crate) fn highlighted_line(ext: &str, src_line: &str, bg: Option<ratatui::style::Color>) -> Line<'static> {
    let base = match bg {
        Some(bg) => Style::default().bg(bg),
        None => Style::default(),
    };
    // Expanded before tokenizing, not after: a tab only ever falls inside a
    // whitespace/punctuation run (see syntax::highlight_line), so widening
    // it to spaces here can't shift where a string/keyword/number token
    // starts. Source lines carry raw tabs (tab-indented code is extremely
    // common — Go, Makefiles, ...) and ratatui doesn't expand or specially
    // measure them, so left as '\t' they render at the wrong column and
    // visually overlap whatever else is on the line.
    let src_line = super::expand_tabs_for_display(src_line);
    let src_line = src_line.as_str();
    if !crate::syntax::supported(ext) {
        return Line::from(Span::styled(src_line.to_string(), base.fg(theme::FG)));
    }
    let spans = crate::syntax::highlight_line(ext, src_line)
        .into_iter()
        .map(|tok| {
            let style = match tok.kind {
                TokenKind::Plain => base.fg(theme::FG),
                TokenKind::Keyword => base.fg(theme::PURPLE).add_modifier(Modifier::BOLD),
                TokenKind::String => base.fg(theme::YELLOW),
                TokenKind::Comment => base.fg(theme::DIM),
                TokenKind::Number => base.fg(theme::CYAN),
            };
            Span::styled(tok.text, style)
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

/// Where a scrolling window of `visible` rows over `total` items should
/// start so that `selected` stays on screen — centered when there's room,
/// clamped at both ends otherwise.
fn scroll_offset(selected: usize, total: usize, visible: usize) -> usize {
    if visible == 0 || total <= visible {
        return 0;
    }
    let max_start = total - visible;
    selected.saturating_sub(visible / 2).min(max_start)
}

pub fn draw(f: &mut Frame, app: &App, area: Rect, narrow: bool) {
    let sidebar_width = if narrow { 24 } else { 38 };
    let chunks =
        Layout::default().direction(Direction::Horizontal).constraints([Constraint::Length(sidebar_width), Constraint::Min(0)]).split(area);

    draw_tree(f, app, chunks[0], narrow);
    draw_content(f, app, chunks[1], narrow);
}

fn draw_tree(f: &mut Frame, app: &App, area: Rect, narrow: bool) {
    let tick_color = if app.nav_focus == NavFocus::Tree { theme::CYAN } else { theme::DIM };
    // 2 rows reserved either way: the summary line, plus room for a
    // transient clipboard-copy status line under it when there is one.
    let footer_lines: u16 = 2;
    // 1 row for the root path, 1 blank separator, then the footer.
    let visible = area.height.saturating_sub(2 + footer_lines) as usize;
    let scroll = scroll_offset(app.tree_index, app.tree.len(), visible);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(app.target_dir.display().to_string(), Style::default().fg(theme::FG))));

    for (i, entry) in app.tree.iter().enumerate().skip(scroll).take(visible) {
        let indent = "\u{2502}  ".repeat(entry.depth as usize);
        let mut spans =
            vec![Span::styled("\u{258c} ", Style::default().fg(tick_color)), Span::styled(indent, Style::default().fg(theme::DIM))];

        let diff_idx = if entry.is_dir { None } else { app.diff_index_for(&entry.path) };
        if diff_idx.is_some() {
            spans.push(Span::styled("\u{00b1} ", Style::default().fg(theme::CYAN)));
        }

        spans.push(Span::styled("\u{251c}\u{2500} ", Style::default().fg(theme::DIM)));
        spans.push(Span::styled(entry.label.clone(), Style::default().fg(theme::FG)));
        if let Some(n) = entry.child_count {
            spans.push(Span::styled(format!(" ({n})"), Style::default().fg(theme::DIM)));
        }

        if let Some(idx) = diff_idx {
            let file = &app.project.files[idx];
            spans.push(Span::raw("  "));
            spans.push(Span::styled(format!("{}h", file.hunk_count), Style::default().fg(theme::DIM)));
            if file.notes > 0 {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(format!("\u{270e}{}", file.notes), Style::default().fg(theme::ORANGE)));
            }
            if file.flagged {
                spans.push(Span::raw(" "));
                spans.push(Span::styled("\u{2717}", Style::default().fg(theme::RED)));
            }
        } else if !entry.is_dir {
            let rel = entry.path.strip_prefix(&app.target_dir).unwrap_or(&entry.path).display().to_string();
            if app.notes.iter().any(|n| n.path == rel) {
                spans.push(Span::raw(" "));
                spans.push(Span::styled("\u{1f4cc}", Style::default().fg(theme::PINK)));
            }
        }

        if entry.path == app.nav_file {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("\u{25cf}", Style::default().fg(theme::CYAN)));
        }

        let style = if i == app.tree_index { Style::default().bg(theme::BG_SELECTION) } else { Style::default() };
        lines.push(Line::from(spans).style(style));
    }

    if app.tree.is_empty() {
        lines.push(Line::from(Span::styled("(empty directory)", Style::default().fg(theme::DIM))));
    }

    lines.push(Line::raw(""));
    if narrow {
        lines.push(Line::from(vec![
            Span::styled("\u{258c} ", Style::default().fg(theme::DIM)),
            Span::styled(
                format!("{} changed \u{b7} {} notes", app.project.files.len(), app.notes_queued()),
                Style::default().fg(theme::FG),
            ),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled("\u{258c} ", Style::default().fg(theme::DIM)),
            Span::styled(
                format!(
                    "{} file{} changed \u{b7} {} notes queued \u{b7} {} flagged",
                    app.project.files.len(),
                    if app.project.files.len() == 1 { "" } else { "s" },
                    app.notes_queued(),
                    app.files_flagged()
                ),
                Style::default().fg(theme::FG),
            ),
        ]));
    }

    match &app.review_clipboard_status {
        Some(Ok(msg)) => lines.push(Line::from(vec![
            Span::styled("\u{258c} ", Style::default().fg(theme::DIM)),
            Span::styled(format!("\u{2714} {msg}"), Style::default().fg(theme::GREEN)),
        ])),
        Some(Err(e)) => lines.push(Line::from(vec![
            Span::styled("\u{258c} ", Style::default().fg(theme::DIM)),
            Span::styled(format!("\u{2717} {e}"), Style::default().fg(theme::RED)),
        ])),
        None => {}
    }

    let para = Paragraph::new(lines).style(Style::default().bg(theme::BG_OUTER));
    f.render_widget(para, area);
}

fn draw_content(f: &mut Frame, app: &App, area: Rect, narrow: bool) {
    match app.current_diff_index() {
        Some(idx) => {
            let file = &app.project.files[idx];
            if app.split_diff {
                draw_diff_split(f, area, file);
            } else {
                draw_diff_scrollable(f, app, area, file, narrow);
            }
        }
        None => draw_source(f, app, area),
    }
}

/// The main diff view: the file's changes shown in place, scrollable like
/// source. `Context` (the default) shows the whole file; `Focused`
/// collapses long unchanged stretches down to a placeholder line, for
/// scanning files with many small, scattered changes without paging
/// through everything in between.
fn draw_diff_scrollable(f: &mut Frame, app: &App, area: Rect, file: &crate::data::FileEntry, narrow: bool) {
    let numbered = crate::data::diff_lines_with_file_line_numbers(&app.diff_context);
    let numbered = match app.content_view {
        ContentView::Context => numbered,
        ContentView::Focused => crate::data::focus_diff_lines(&numbered, 3),
    };

    let focused = app.nav_focus == NavFocus::Content;
    let mode_label = match app.content_view {
        ContentView::Context => "context",
        ContentView::Focused => "focused",
    };
    let title =
        format!("{} \u{2014} {mode_label} \u{2014} line {}/{}", file.path, (app.nav_line + 1).min(numbered.len().max(1)), numbered.len());
    let view_toggle_label = match app.content_view {
        ContentView::Context => "Focused",
        ContentView::Focused => "Context",
    };
    let hints = if narrow {
        vec![super::key_hints(&[("c", "Comment"), ("v", view_toggle_label)]), super::key_hints(&[("i", "Iterate"), ("y", "Copy prompt")])]
    } else {
        vec![
            super::key_hints(&[
                ("Tab", "Switch pane"),
                ("\u{2191}\u{2193}", "Move"),
                ("PgUp/PgDn", "Page"),
                ("\u{2190}\u{2192}", "Scroll"),
                ("c", "Comment"),
                ("g", "Mark good"),
                ("x", "Flag rework"),
                ("d/D", "Clear file/all notes"),
            ]),
            super::key_hints(&[
                ("v", view_toggle_label),
                ("s", "Split"),
                ("i", &format!("Review {} notes \u{2192} send to agent", app.notes_queued())),
                ("y", "...or copy the prompt to the clipboard"),
            ]),
        ]
    };

    let block = super::panel_block(&title).border_style(Style::default().fg(if focused { theme::CYAN } else { theme::DIM }));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1), Constraint::Length(hints.len() as u16)])
        .split(inner);

    let visible_height = rows[0].height as usize;
    let scroll_y = scroll_offset(app.nav_line, numbered.len(), visible_height);
    let scroll_x = app.nav_scroll_x as usize;

    let rel = file.path.as_str();
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, (line_no, dl)) in numbered.iter().enumerate().skip(scroll_y).take(visible_height) {
        let bg = if i == app.nav_line { Some(theme::BG_SELECTION) } else { None };
        let mut style = super::diff_line_style(dl.kind);
        if let Some(bg) = bg {
            style = style.bg(bg);
        }
        let mut spans = Vec::new();
        if line_no.is_some_and(|n| app.notes.iter().any(|note| note.path == rel && note.line == Some(n))) {
            spans.push(Span::styled("\u{1f4cc}", Style::default().fg(theme::PINK).bg(bg.unwrap_or(theme::BG_PANEL))));
        }
        let visible_text: String = super::expand_tabs_for_display(&dl.text).chars().skip(scroll_x).collect();
        spans.push(Span::styled(visible_text, style));
        lines.push(Line::from(spans).style(Style::default().bg(bg.unwrap_or(theme::BG_PANEL))));
    }
    if numbered.is_empty() {
        lines.push(Line::from(Span::styled("(no diff content)", Style::default().fg(theme::DIM))));
    }

    f.render_widget(Paragraph::new(lines), rows[0]);
    let divider = "\u{2500}".repeat(rows[1].width as usize);
    f.render_widget(Paragraph::new(divider).style(Style::default().fg(theme::DIM)), rows[1]);
    f.render_widget(Paragraph::new(hints), rows[2]);
}

fn draw_diff_split(f: &mut Frame, area: Rect, file: &FileEntry) {
    let title = format!("{} \u{2014} before / after", file.path);

    let mut before: Vec<Line<'static>> = Vec::new();
    let mut after: Vec<Line<'static>> = Vec::new();

    for hunk in &file.hunks {
        for dl in &hunk.lines {
            let text = super::expand_tabs_for_display(&dl.text);
            match dl.kind {
                DiffLineKind::HunkHeader | DiffLineKind::Context => {
                    before.push(Line::from(Span::styled(text.clone(), super::diff_line_style(dl.kind))));
                    after.push(Line::from(Span::styled(text, super::diff_line_style(dl.kind))));
                }
                DiffLineKind::Removed => {
                    before.push(Line::from(Span::styled(text, super::diff_line_style(dl.kind))));
                }
                DiffLineKind::Added => {
                    after.push(Line::from(Span::styled(text, super::diff_line_style(dl.kind))));
                }
            }
        }
        let max = before.len().max(after.len());
        before.resize(max, Line::raw(""));
        after.resize(max, Line::raw(""));
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
    let rule_lines: Vec<Line<'static>> =
        (0..cols[1].height).map(|_| Line::from(Span::styled("\u{2502}", Style::default().fg(theme::DIM)))).collect();
    f.render_widget(Paragraph::new(rule_lines), cols[1]);
    f.render_widget(Paragraph::new(after_full), cols[2]);

    let divider = "\u{2500}".repeat(rows[1].width as usize);
    f.render_widget(Paragraph::new(divider).style(Style::default().fg(theme::DIM)), rows[1]);

    let hints = vec![
        super::key_hints(&[("c", "Comment"), ("g", "Mark good"), ("x", "Flag rework"), ("d/D", "Clear file/all notes"), ("u", "Unified")]),
        super::key_hints(&[("i", "Review notes \u{2192} send to agent"), ("y", "Copy prompt")]),
    ];
    f.render_widget(Paragraph::new(hints), rows[2]);
}

fn draw_source(f: &mut Frame, app: &App, area: Rect) {
    let name = app.nav_file.strip_prefix(&app.target_dir).unwrap_or(&app.nav_file).display().to_string();

    let focused = app.nav_focus == NavFocus::Content;
    let scroll_x = app.nav_scroll_x as usize;
    let title = if !app.source.is_empty() {
        format!("{name} \u{2014} line {}/{}", app.nav_line + 1, app.source.len())
    } else {
        format!("{name} \u{2014} 0 lines")
    };
    let mut hints = vec![("Tab", "Switch pane"), ("\u{2191}\u{2193}", "Move"), ("PgUp/PgDn", "Page"), ("\u{2190}\u{2192}", "Scroll")];
    hints.push(("Enter", "Open"));
    hints.push(("h", "Hover"));
    hints.push(("/", "Symbols"));
    hints.push(("c", "Comment"));
    if app.current_diff_index().is_some() {
        hints.push(("v", "View diff"));
    }
    let hints = vec![super::key_hints(&hints)];
    let block = super::panel_block(&title).border_style(Style::default().fg(if focused { theme::CYAN } else { theme::DIM }));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1), Constraint::Length(hints.len() as u16)])
        .split(inner);

    let visible_height = rows[0].height as usize;
    let scroll_y = scroll_offset(app.nav_line, app.source.len(), visible_height);

    let ext = crate::syntax::ext_for(&app.nav_file);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, src_line) in app.source.iter().enumerate().skip(scroll_y).take(visible_height) {
        let bg = if i == app.nav_line { Some(theme::BG_SELECTION) } else { None };
        // Expanded before the horizontal-scroll skip, not after: scroll_x
        // counts visual columns, and a raw tab counts as one character but
        // several columns — skipping first would leave the viewport
        // misaligned on any tab-indented line.
        let visible_line: String = super::expand_tabs_for_display(src_line).chars().skip(scroll_x).collect();
        let mut line = highlighted_line(&ext, &visible_line, bg);
        if app.notes.iter().any(|n| n.path == name && n.line == Some(i + 1)) {
            let marker_bg = bg.unwrap_or(theme::BG_PANEL);
            line.spans.insert(0, Span::styled("\u{1f4cc}", Style::default().fg(theme::PINK).bg(marker_bg)));
        }
        lines.push(line);
    }
    if app.source.is_empty() {
        lines.push(Line::from(Span::styled("(select a file and press Enter)", Style::default().fg(theme::DIM))));
    }

    f.render_widget(Paragraph::new(lines), rows[0]);
    let divider = "\u{2500}".repeat(rows[1].width as usize);
    f.render_widget(Paragraph::new(divider).style(Style::default().fg(theme::DIM)), rows[1]);
    f.render_widget(Paragraph::new(hints), rows[2]);

    if app.show_hover {
        if let Some(hover) = &app.hover {
            let on_screen_line = app.nav_line.saturating_sub(scroll_y);
            draw_hover(f, hover, on_screen_line, rows[0]);
        }
    }
}

fn draw_hover(f: &mut Frame, hover: &crate::data::HoverInfo, cursor_line: usize, source_area: Rect) {
    let width = 46u16.min(source_area.width.saturating_sub(2));
    let sig_lines = hover.signature.split('\n').count().max(1) as u16;
    let height = (sig_lines + 4).min(source_area.height);

    let y_offset = (cursor_line as u16 + 1).min(source_area.height.saturating_sub(height));
    let x = source_area.x + (source_area.width / 2).min(source_area.width.saturating_sub(width));
    let y = source_area.y + y_offset;
    let rect = Rect { x, y, width, height };

    f.render_widget(Clear, rect);
    let block = super::panel_block("");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let mut lines: Vec<Line<'static>> = Vec::new();
    for sig_line in hover.signature.split('\n') {
        lines.push(Line::from(Span::styled(sig_line.to_string(), Style::default().fg(theme::FG))));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(hover.location.clone(), Style::default().fg(theme::DIM))));
    lines.push(Line::from(Span::styled(
        format!("{} reference{}", hover.references, if hover.references == 1 { "" } else { "s" }),
        Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD),
    )));

    f.render_widget(Paragraph::new(lines), inner);
}
