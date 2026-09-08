use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, ContentView, NavFocus, ReviewScope};
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

    draw_tree(f, app, chunks[0]);
    draw_content(f, app, chunks[1], narrow);
}

/// The tree pane's footer counts, in the longest form that fits `width`.
///
/// Fitted against the sidebar's own width, not the whole-screen `narrow`
/// flag. The sidebar is a fixed 38 columns on any terminal wide enough not
/// to count as narrow, and the wordier version this used to print there
/// ("5 files changed \u{b7} 0 notes queued \u{b7} 0 flagged") needs 44 — so it was
/// cut off mid-word, with no ellipsis and nothing to indicate anything was
/// missing, at every width above the narrow threshold. The fallback drops
/// the flagged tally only once even that can't fit, which in practice is
/// just the 24-column narrow sidebar; the final truncate is a backstop for
/// anything narrower still.
///
/// Takes the counts rather than an `&App` so the thing it actually does —
/// fit text into a width — is testable without a repo on disk behind it.
///
/// `subject` is what the leading count is counting: "changed" for the
/// whole uncommitted changeset, "this turn" when Review is narrowed to
/// what the last agent turn wrote. It has to travel with the number,
/// because "3 changed" and "3 this turn" are different claims about the
/// same repo and only one of them is true at a time.
fn tree_footer_counts(files: usize, subject: &str, notes: u32, flagged: usize, width: usize) -> String {
    let budget = width.saturating_sub(2);
    let plural = if notes == 1 { "" } else { "s" };
    let candidates = [
        format!("{files} {subject} \u{b7} {notes} note{plural} \u{b7} {flagged} flagged"),
        format!("{files} {subject} \u{b7} {notes} note{plural}"),
    ];
    match candidates.iter().find(|c| c.chars().count() <= budget) {
        Some(c) => c.clone(),
        None => super::truncate_with_ellipsis(&candidates[1], budget),
    }
}

/// The "you are not seeing everything" line under a turn-scoped tree,
/// in the longest form that fits `width`.
///
/// The way back out (`t`) is the half that has to survive the squeeze, not
/// the uncommitted tally — a notice that says the tree is filtered but not
/// how to unfilter it leaves the user hunting through KEYBINDINGS.md for a
/// key they pressed by accident. Fitted rather than truncated for the same
/// reason `tree_footer_counts` is: at the sidebar's real 38 columns the
/// long form runs over, and truncation ate exactly the useful end of it.
fn tree_scope_notice(uncommitted: usize, width: usize) -> String {
    let budget = width.saturating_sub(2);
    let candidates = [
        format!("\u{21b3} this turn only ({uncommitted} uncommitted) \u{b7} t: all"),
        "\u{21b3} this turn only \u{b7} t: all".to_string(),
        "\u{21b3} turn only \u{b7} t: all".to_string(),
    ];
    match candidates.iter().find(|c| c.chars().count() <= budget) {
        Some(c) => c.clone(),
        None => super::truncate_with_ellipsis(&candidates[2], budget),
    }
}

/// The change indicator owns a fixed-width column in every tree row.
///
/// Omitting the span for unchanged files made `├─` start two columns
/// earlier than it did for changed files, so adjacent files appeared to sit
/// at different depths (or under one another) even though their `depth` was
/// identical.
fn tree_change_marker(changed: bool) -> &'static str {
    if changed {
        "± "
    } else {
        "  "
    }
}

fn draw_tree(f: &mut Frame, app: &App, area: Rect) {
    let tick_color = if app.nav_focus == NavFocus::Tree { theme::CYAN } else { theme::DIM };
    // Counts + blank separator, plus a row for each optional notice that is
    // actually showing. Fixed at 2 before, which meant a git error or a
    // truncation notice silently pushed the counts row out of the pane.
    let scoped = app.review_scope == ReviewScope::Turn;
    let optional_rows = app.review_error.is_some() as u16
        + app.tree_truncated as u16
        + app.review_status.is_some() as u16
        + (scoped || app.has_turn_baseline()) as u16
        + (app.notes_stale() > 0) as u16;
    let footer_lines: u16 = 2 + optional_rows;
    // 1 row for the root path, 1 blank separator, then the footer.
    let visible = area.height.saturating_sub(2 + footer_lines) as usize;
    // Scrolling is over the rows actually being drawn, not over the whole
    // scan: under a turn scope most of `app.tree` isn't on screen at all,
    // and offsetting by its indices would scroll the pane off into a run
    // of hidden rows.
    let rows = app.visible_tree_rows();
    let cursor = rows.iter().position(|i| *i == app.tree_index).unwrap_or(0);
    let scroll = scroll_offset(cursor, rows.len(), visible);

    let mut lines: Vec<Line<'static>> = Vec::new();
    // Ellipsised from the front, not hard-clipped: the sidebar is far
    // narrower than a real absolute path, and the end of a path is the half
    // worth keeping. Left unclipped, a Paragraph simply cut it wherever the
    // pane ran out, which read as a corrupted path rather than a shortened
    // one — most visibly under `--demo`, whose repo lives in a temp
    // directory with a long generated prefix.
    lines.push(Line::from(Span::styled(
        super::truncate_start_with_ellipsis(&app.target_dir.display().to_string(), area.width as usize),
        Style::default().fg(theme::FG),
    )));

    for &i in rows.iter().skip(scroll).take(visible) {
        let entry = &app.tree[i];
        let indent = "\u{2502}  ".repeat(entry.depth as usize);
        let mut spans =
            vec![Span::styled("\u{258c} ", Style::default().fg(tick_color)), Span::styled(indent, Style::default().fg(theme::DIM))];

        let diff_idx = if entry.is_dir { None } else { app.diff_index_for(&entry.path) };
        spans.push(Span::styled(tree_change_marker(diff_idx.is_some()), Style::default().fg(theme::CYAN)));

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
                if app.notes.iter().any(|n| n.path == file.path && n.stale) {
                    spans.push(Span::styled("\u{26a0}", Style::default().fg(theme::ORANGE)));
                }
            }
            if file.flagged {
                spans.push(Span::raw(" "));
                spans.push(Span::styled("\u{2717}", Style::default().fg(theme::RED)));
            }
        } else if !entry.is_dir {
            let rel = crate::gitreview::display_path_of(entry.path.strip_prefix(&app.target_dir).unwrap_or(&entry.path));
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

    if rows.is_empty() {
        let empty = if scoped { "(that turn changed nothing)" } else { "(empty directory)" };
        lines.push(Line::from(Span::styled(empty, Style::default().fg(theme::DIM))));
    }

    lines.push(Line::raw(""));
    let (count, subject) = if scoped { (app.turn_scope_len(), "this turn") } else { (app.project.files.len(), "changed") };
    lines.push(Line::from(vec![
        Span::styled("\u{258c} ", Style::default().fg(theme::DIM)),
        Span::styled(
            tree_footer_counts(count, subject, app.notes_queued(), app.files_flagged(), area.width as usize),
            Style::default().fg(theme::FG),
        ),
    ]));

    // A filtered tree is missing files that really are dirty, so it has to
    // say so outright — the same rule the truncation notice below follows.
    // A quiet filter and a clean repo look identical otherwise.
    if scoped {
        lines.push(Line::from(vec![
            Span::styled("\u{258c} ", Style::default().fg(theme::DIM)),
            Span::styled(tree_scope_notice(app.project.files.len(), area.width as usize), Style::default().fg(theme::CYAN)),
        ]));
    } else if app.has_turn_baseline() {
        // The other half of the same row: once a turn has run there is
        // somewhere to narrow to, and `t` is otherwise only discoverable
        // from the transcript line that scrolls away.
        lines.push(Line::from(vec![
            Span::styled("\u{258c} ", Style::default().fg(theme::DIM)),
            Span::styled(
                super::truncate_with_ellipsis(
                    &format!("\u{21b3} t: this turn ({})", app.turn_scope_len()),
                    area.width.saturating_sub(2) as usize,
                ),
                Style::default().fg(theme::DIM),
            ),
        ]));
    }

    // Notes whose line is gone still go to the agent — they just go
    // without a line number (see App::reanchor_notes). Saying how many
    // here is what keeps `d`/`D` a decision rather than a guess.
    let stale_notes = app.notes_stale();
    if stale_notes > 0 {
        lines.push(Line::from(vec![
            Span::styled("\u{258c} ", Style::default().fg(theme::DIM)),
            Span::styled(
                super::truncate_with_ellipsis(
                    &format!("\u{26a0} {stale_notes} note{} lost its line", if stale_notes == 1 { "" } else { "s" }),
                    area.width.saturating_sub(2) as usize,
                ),
                Style::default().fg(theme::ORANGE),
            ),
        ]));
    }

    // An unreadable repo and a clean one produce the same empty file list,
    // so the difference has to be stated outright rather than left to the
    // counts above — this is the one message that must never be missed.
    if let Some(e) = &app.review_error {
        lines.push(Line::from(vec![
            Span::styled("\u{258c} ", Style::default().fg(theme::DIM)),
            Span::styled(
                super::truncate_with_ellipsis(&format!("\u{2717} couldn't read changes: {e}"), area.width.saturating_sub(2) as usize),
                Style::default().fg(theme::RED),
            ),
        ]));
    }
    if app.tree_truncated {
        lines.push(Line::from(vec![
            Span::styled("\u{258c} ", Style::default().fg(theme::DIM)),
            Span::styled(
                super::truncate_with_ellipsis(
                    "\u{2026} tree truncated \u{2014} some files aren't listed or findable",
                    area.width.saturating_sub(2) as usize,
                ),
                Style::default().fg(theme::ORANGE),
            ),
        ]));
    }

    match &app.review_status {
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
                draw_diff_split(f, app, area, file);
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
    let mut hints = if narrow {
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
                // Pluralized like the status line and the tree footer: this
                // hint had kept "1 notes" after both of those were fixed.
                (
                    "i",
                    &format!("Review {} note{} \u{2192} send to agent", app.notes_queued(), if app.notes_queued() == 1 { "" } else { "s" }),
                ),
                ("y", "...or copy the prompt to the clipboard"),
            ]),
        ]
    };
    // A turn started from Agent keeps running in the background if you
    // switch here to look something up — Esc still reaches it.
    if app.agent_running {
        hints.push(super::key_hints(&[("Esc", "Cancel agent turn")]));
    }

    let block = super::panel_block(&title).border_style(Style::default().fg(if focused { theme::CYAN } else { theme::DIM }));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1), Constraint::Length(hints.len() as u16)])
        .split(inner);

    // Everything about the change that isn't line content — renamed from
    // where, an executable bit that flipped. The diff body below can't show
    // any of it (a pure rename has no lines at all), so it gets its own
    // rows above, and the scrollable area shrinks to make room rather than
    // pushing the last line off the bottom.
    let meta_notes = file.meta.describe();
    let visible_height = (rows[0].height as usize).saturating_sub(meta_notes.len());
    let scroll_y = scroll_offset(app.nav_line, numbered.len(), visible_height);
    let scroll_x = app.nav_scroll_x as usize;

    let rel = file.path.as_str();
    let mut lines: Vec<Line<'static>> =
        meta_notes.iter().map(|n| Line::from(Span::styled(n.clone(), Style::default().fg(theme::CYAN)))).collect();
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
        if dl.no_newline {
            spans.push(super::no_newline_span());
        }
        lines.push(Line::from(spans).style(Style::default().bg(bg.unwrap_or(theme::BG_PANEL))));
    }
    if numbered.is_empty() {
        lines.push(Line::from(Span::styled("(no diff content)", Style::default().fg(theme::DIM))));
    }

    f.render_widget(Paragraph::new(lines), rows[0]);
    let divider = "\u{2500}".repeat(rows[1].width as usize);
    f.render_widget(Paragraph::new(divider).style(Style::default().fg(theme::DIM)), rows[1]);
    super::draw_hints(f, rows[2], hints);
}

fn draw_diff_split(f: &mut Frame, app: &App, area: Rect, file: &FileEntry) {
    let notes = file.meta.describe();
    let title = if notes.is_empty() {
        format!("{} \u{2014} before / after", file.path)
    } else {
        format!("{} ({}) \u{2014} before / after", file.path, notes.join(", "))
    };

    let mut before: Vec<Line<'static>> = Vec::new();
    let mut after: Vec<Line<'static>> = Vec::new();

    for hunk in &file.hunks {
        for dl in &hunk.lines {
            let row = |side: &mut Vec<Line<'static>>| side.push(Line::from(super::diff_line_spans(dl, super::diff_line_style(dl.kind))));
            match dl.kind {
                DiffLineKind::HunkHeader | DiffLineKind::Context => {
                    row(&mut before);
                    row(&mut after);
                }
                DiffLineKind::Removed => row(&mut before),
                DiffLineKind::Added => row(&mut after),
            }
        }
        let max = before.len().max(after.len());
        before.resize(max, Line::raw(""));
        after.resize(max, Line::raw(""));
    }

    let mut hints = vec![
        super::key_hints(&[("c", "Comment"), ("g", "Mark good"), ("x", "Flag rework"), ("d/D", "Clear file/all notes"), ("u", "Unified")]),
        super::key_hints(&[("i", "Review notes \u{2192} send to agent"), ("y", "Copy prompt")]),
    ];
    // A turn started from Agent keeps running in the background if you
    // switch here to look something up — Esc still reaches it.
    if app.agent_running {
        hints.push(super::key_hints(&[("Esc", "Cancel agent turn")]));
    }

    let block = super::panel_block(&title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1), Constraint::Length(hints.len() as u16)])
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

    super::draw_hints(f, rows[2], hints);
}

fn draw_source(f: &mut Frame, app: &App, area: Rect) {
    let name = crate::gitreview::display_path_of(app.nav_file.strip_prefix(&app.target_dir).unwrap_or(&app.nav_file));

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
    // No `v` hint here. This pane only ever draws for a file with no diff
    // to show (see `draw_content`), so the guard that used to gate one —
    // `current_diff_index().is_some()` — could never be true, and the
    // "View diff" label it carried described a source/diff toggle `v`
    // stopped being some time ago. Both were leftovers.
    let mut hints = vec![super::key_hints(&hints)];
    // Its own row, not appended to the line above: that line already
    // packs in enough hints to fill a typical terminal width with nothing
    // left over (there's no wrap/truncate on it), so anything appended to
    // it was silently invisible regardless of what it was.
    // A turn started from Agent keeps running in the background if you
    // switch here to look something up — Esc still reaches it.
    if app.agent_running {
        hints.push(super::key_hints(&[("Esc", "Cancel agent turn")]));
    }
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
    super::draw_hints(f, rows[2], hints);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_marker_does_not_shift_the_tree_connector() {
        use unicode_width::UnicodeWidthStr;

        let changed = tree_change_marker(true);
        let unchanged = tree_change_marker(false);
        assert_eq!(UnicodeWidthStr::width(changed), 2);
        assert_eq!(UnicodeWidthStr::width(unchanged), 2);

        let changed_prefix = format!("▌ │  {changed}├─ ");
        let unchanged_prefix = format!("▌ │  {unchanged}├─ ");
        let changed_before_connector = changed_prefix.split_once('├').unwrap().0;
        let unchanged_before_connector = unchanged_prefix.split_once('├').unwrap().0;
        assert_eq!(UnicodeWidthStr::width(changed_before_connector), UnicodeWidthStr::width(unchanged_before_connector));
        assert_eq!(UnicodeWidthStr::width(changed_prefix.as_str()), UnicodeWidthStr::width(unchanged_prefix.as_str()));
    }

    #[test]
    fn tree_footer_never_exceeds_the_sidebar_width() {
        // 38 is the wide sidebar and 24 the narrow one; the rest are widths
        // in between and below, where the long form used to be cut off
        // mid-word with nothing to indicate it.
        for width in [10usize, 16, 24, 30, 36, 38, 44] {
            for files in [0usize, 1, 12] {
                let s = tree_footer_counts(files, "changed", 3, 2, width);
                assert!(s.chars().count() <= width.saturating_sub(2), "{s:?} overflows a {width}-column sidebar");
            }
        }
    }

    #[test]
    fn tree_footer_reports_every_count_at_the_wide_sidebar_width() {
        // 38 is the sidebar `draw` renders on any non-narrow terminal, and
        // it's where the old text was silently cut off. All three counts
        // have to survive at that width, including for two-digit tallies.
        for (files, notes, flagged) in [(5usize, 3u32, 2usize), (12, 10, 3)] {
            let s = tree_footer_counts(files, "changed", notes, flagged, 38);
            assert_eq!(s, format!("{files} changed \u{b7} {notes} notes \u{b7} {flagged} flagged"), "at 38 columns");
            assert!(s.chars().count() <= 36, "{s:?}");
        }
    }

    #[test]
    fn tree_footer_drops_the_flagged_count_only_when_it_cannot_fit() {
        // The 24-column narrow sidebar: no room for all three, so the
        // flagged tally goes rather than the text being cut mid-word.
        assert_eq!(tree_footer_counts(5, "changed", 3, 2, 24), "5 changed \u{b7} 3 notes");
    }

    #[test]
    fn tree_footer_pluralizes_the_note_count() {
        // Every other count line in the app gets this right; this one used
        // to read "1 notes queued".
        assert!(tree_footer_counts(2, "changed", 1, 0, 38).contains("1 note \u{b7}"), "{:?}", tree_footer_counts(2, "changed", 1, 0, 38));
        assert!(tree_footer_counts(2, "changed", 0, 0, 38).contains("0 notes"), "{:?}", tree_footer_counts(2, "changed", 0, 0, 38));
    }

    #[test]
    fn the_scope_notice_keeps_the_way_out_when_it_has_to_drop_something() {
        // 38 is the real sidebar width, and the long form doesn't fit it —
        // which is exactly the case where truncation used to eat "t: all"
        // and leave a filtered tree with no visible way back.
        for width in [10usize, 16, 24, 30, 36, 38, 44, 80] {
            let s = tree_scope_notice(12, width);
            assert!(s.chars().count() <= width.saturating_sub(2), "{s:?} overflows a {width}-column sidebar");
            if width >= 24 {
                assert!(s.contains("t: all"), "{s:?} at {width} columns doesn't say how to get back");
            }
        }
        assert_eq!(tree_scope_notice(12, 80), "\u{21b3} this turn only (12 uncommitted) \u{b7} t: all");
        assert_eq!(tree_scope_notice(12, 38), "\u{21b3} this turn only \u{b7} t: all", "the tally is what goes first");
    }

    #[test]
    fn tree_footer_falls_back_to_an_ellipsis_when_nothing_fits() {
        let tiny = tree_footer_counts(5, "changed", 3, 2, 8);
        assert!(tiny.ends_with('\u{2026}'), "{tiny:?}");
        assert!(tiny.chars().count() <= 6, "{tiny:?}");
    }

    #[test]
    fn scroll_offset_keeps_the_selection_on_screen() {
        assert_eq!(scroll_offset(0, 100, 10), 0);
        assert_eq!(scroll_offset(50, 100, 10), 45, "centered when there is room on both sides");
        assert_eq!(scroll_offset(99, 100, 10), 90, "clamped at the end");
        assert_eq!(scroll_offset(3, 5, 10), 0, "everything fits, no scrolling");
    }
}
