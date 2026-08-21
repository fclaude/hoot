use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;
use crate::theme;

pub fn draw(f: &mut Frame, app: &App, area: Rect, narrow: bool) {
    // The result of the last commit gets a row to itself rather than being
    // appended to the hint row. It used to share that row, and lost: the
    // hints alone run to about 110 columns, so on any ordinary terminal the
    // outcome was truncated down to a character or two. That was survivable
    // while the only outcome was success (the file list emptying says as
    // much), but a *refusal* — an agent turn still running, an index staged
    // outside hoot, a selection the index didn't match — is a sentence the
    // user has to be able to read, and it was the part being cut off.
    let result_rows = u16::from(app.last_commit.is_some());
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(result_rows), Constraint::Length(1)])
        .split(area);

    // Wide enough that the summary line ("Files: N  Selected hunks: N/N")
    // fits without clipping on its own — it was clipping even at 140
    // columns total width, since this is a *fixed* width regardless of
    // how much room the terminal actually has. File paths still get
    // truncated with an ellipsis in draw_sidebar when they don't fit,
    // since no fixed width accommodates an arbitrarily long path.
    let sidebar_width = if narrow { 26 } else { 36 };
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(sidebar_width), Constraint::Min(0)])
        .split(rows[0]);

    draw_sidebar(f, app, cols[0]);

    // Sized to what the message actually needs (status line + message
    // body + the panel's own border/divider/hint chrome), not a flat 45% —
    // that wasted most of the screen on an empty box showing one line of
    // placeholder text, especially on a clean repo with nothing to
    // curate. Still capped at 60% so a genuinely long drafted message
    // can't crowd the hunk view out entirely, and never shrinks below
    // enough room for the placeholder/hint to read cleanly.
    let message_lines = app.commit_message.split('\n').count().max(1);
    let status_lines = if app.commit_message_status.is_some() { 2 } else { 0 };
    const CHROME: usize = 4; // border (2) + divider (1) + hint row (1)
    let commit_height = (message_lines + status_lines + CHROME) as u16;
    let commit_height = commit_height.clamp(6, (cols[1].height * 3) / 5);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(commit_height), Constraint::Min(0)])
        .split(cols[1]);

    draw_commit_box(f, app, right[0]);
    draw_hunk_box(f, app, right[1]);

    let hints = if app.agent_running {
        super::key_hints(&[
            ("\u{2191}\u{2193}", "File"),
            ("\u{2190}\u{2192}", "Prev/next hunk"),
            ("Space", "Toggle hunk"),
            ("r", "Open in Review"),
            ("Esc", "Cancel agent turn"),
            ("e", "Edit in $EDITOR"),
            ("c", "Commit"),
        ])
    } else {
        super::key_hints(&[
            ("\u{2191}\u{2193}", "File"),
            ("\u{2190}\u{2192}", "Prev/next hunk"),
            ("Space", "Toggle hunk"),
            ("r", "Open in Review"),
            ("g", "Generate message"),
            ("e", "Edit in $EDITOR"),
            ("c", "Commit"),
        ])
    };
    if let Some(result) = &app.last_commit {
        let (text, color) = match result {
            Ok(summary) => (format!("\u{2714} {summary}"), theme::GREEN),
            Err(e) => (format!("\u{2717} {e}"), theme::RED),
        };
        let text = super::truncate_with_ellipsis(&text, (rows[1].width as usize).saturating_sub(2));
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("\u{258c} ", Style::default().fg(theme::DIM)),
                Span::styled(text, Style::default().fg(color).add_modifier(Modifier::BOLD)),
            ])),
            rows[1],
        );
    }

    let mut spans = vec![Span::styled("\u{258c} ", Style::default().fg(theme::DIM))];
    spans.extend(hints.spans);
    let spans = super::truncate_spans(spans, rows[2].width as usize);
    f.render_widget(Paragraph::new(Line::from(spans)), rows[2]);
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(app.project.root.clone(), Style::default().fg(theme::FG))));

    for (i, cf) in app.curation_files.iter().enumerate() {
        // The selection count/dot matter more than seeing the whole path —
        // reserve room for them first and truncate the path (which can be
        // arbitrarily long) into whatever's left, rather than letting a
        // long path silently push the actually-important count off the
        // edge of the sidebar.
        let prefix_width = 4; // "▌ " + glyph-or-two-spaces
        let suffix = format!(" {}/{} sel", cf.selected(), cf.total());
        let dot_suffix = if i == app.curation_index { " \u{25cf}" } else { "" };
        let reserved = prefix_width + suffix.chars().count() + dot_suffix.chars().count();
        let path_budget = (area.width as usize).saturating_sub(reserved).max(1);
        let path = super::truncate_with_ellipsis(&cf.path, path_budget);

        let mut spans = vec![Span::styled("\u{258c} ", Style::default().fg(theme::DIM))];
        if let Some(status) = cf.status {
            spans.push(Span::styled(format!("{} ", status.glyph()), Style::default().fg(status.color())));
        } else {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(path, Style::default().fg(theme::FG)));
        spans.push(Span::styled(suffix, Style::default().fg(theme::DIM)));
        if i == app.curation_index {
            spans.push(Span::styled(dot_suffix, Style::default().fg(theme::CYAN)));
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
    let summary = super::truncate_with_ellipsis(
        &format!("Files: {files}  Selected hunks: {sel}/{total}"),
        (area.width as usize).saturating_sub(2).max(1),
    );
    lines.push(Line::from(vec![
        Span::styled("\u{258c} ", Style::default().fg(theme::DIM)),
        Span::styled(summary, Style::default().fg(theme::FG)),
    ]));

    f.render_widget(Paragraph::new(lines), area);
}

fn draw_commit_box(f: &mut Frame, app: &App, area: Rect) {
    // The demo message is static fabricated text (data::mock_commit_message),
    // not something any model actually drafted — say so plainly rather than
    // implying a real local inference call happened.
    let title = if app.review_is_real { "Commit message" } else { "Commit message (editable — fictional demo text)" };
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(status) = &app.commit_message_status {
        lines.push(Line::from(Span::styled(status.clone(), Style::default().fg(theme::ORANGE))));
        lines.push(Line::raw(""));
    }
    let body_width = (area.width as usize).saturating_sub(2);
    if app.curation_files.is_empty() {
        lines.push(Line::from(Span::styled(
            super::truncate_with_ellipsis("Nothing to commit \u{2014} your working tree is clean.", body_width),
            Style::default().fg(theme::DIM),
        )));
    } else if app.commit_message.is_empty() {
        lines.push(Line::from(Span::styled(
            super::truncate_with_ellipsis(
                &format!("(empty \u{2014} press g to draft one with {}, or e to write your own)", app.agent_backend.label()),
                body_width,
            ),
            Style::default().fg(theme::DIM),
        )));
    } else {
        for l in app.commit_message.split('\n') {
            lines.push(Line::from(Span::styled(l.to_string(), Style::default().fg(theme::FG))));
        }
    }
    let hints = vec![super::key_hints(&[("g", "Generate + open $EDITOR"), ("e", "Edit in $EDITOR")])];
    super::draw_panel(f, area, title, Paragraph::new(lines), &hints);
}

fn draw_hunk_box(f: &mut Frame, app: &App, area: Rect) {
    let Some(cf) = app.curation_files.get(app.curation_index) else {
        // Truncated defensively rather than hand-fit to one screen size —
        // the border eats 2 columns, and this box's width isn't fixed
        // (the sidebar next to it is), so there's no single width these
        // sentences are guaranteed to fit at.
        let body_width = (area.width as usize).saturating_sub(2);
        let lines = vec![
            Line::from(Span::styled(
                super::truncate_with_ellipsis("Nothing to curate \u{2014} the working tree is clean.", body_width),
                Style::default().fg(theme::FG),
            )),
            Line::raw(""),
            Line::from(Span::styled(
                super::truncate_with_ellipsis("Switch to Review (F1) or Agent (F3) to make some changes first.", body_width),
                Style::default().fg(theme::DIM),
            )),
        ];
        super::draw_panel(f, area, "Curate", Paragraph::new(lines), &[]);
        return;
    };
    let file = app.project.files.iter().find(|f| f.path == cf.path);

    // Binary content or a submodule pointer: hoot can neither show it as
    // selectable lines nor stage it faithfully, so it says so plainly
    // instead of showing a confusing "hunk 1/0" with no explanation, and
    // offers no selectable unit at all — nothing here can end up in a
    // commit by accident. (Renames and mode changes used to land here too;
    // those are real, stageable changes now, not dead ends.) Committing
    // this one is still possible with plain `git add` outside hoot.
    if let Some(reason) = file.and_then(|f| f.unsupported) {
        let title = format!("{} \u{2014} not curatable here", cf.path);
        let body_width = (area.width as usize).saturating_sub(2);
        let lines = vec![
            Line::from(Span::styled(
                super::truncate_with_ellipsis(&format!("This change is {reason}."), body_width),
                Style::default().fg(theme::ORANGE),
            )),
            Line::raw(""),
            Line::from(Span::styled(
                super::truncate_with_ellipsis(
                    "There's no hunk-level content to select \u{2014} stage it with plain `git add` instead.",
                    body_width,
                ),
                Style::default().fg(theme::DIM),
            )),
        ];
        super::draw_panel(f, area, &title, Paragraph::new(lines), &[]);
        return;
    }

    let shown_index = app.curation_hunk_index.min(cf.total().saturating_sub(1) as usize);
    let this_selected = cf.hunk_selected.get(shown_index).copied().unwrap_or(false);

    // A change with no hunks of its own — a pure rename, a mode flip, a
    // new empty file — is still a real change, and still one selectable
    // unit. Calling it a "hunk" would be a lie about something the user is
    // about to commit.
    let metadata_only = file.is_some_and(|f| f.is_metadata_only());
    let unit = if metadata_only { "change" } else { "hunk" };
    let title = format!(
        "{} \u{2014} {unit} {}/{} ({}) \u{2014} {}/{} selected",
        cf.path,
        shown_index + 1,
        cf.total(),
        if this_selected { "selected" } else { "not selected" },
        cf.selected(),
        cf.total(),
    );

    let mut lines: Vec<Line<'static>> = Vec::new();
    // Everything about this change that isn't line content: which path it
    // was renamed from, an executable bit that flipped. All of it is staged
    // along with the hunks below, so all of it has to be *visible* — a mode
    // change used to ride into a commit without ever appearing on screen.
    for note in file.map(|f| f.meta.describe()).unwrap_or_default() {
        lines.push(Line::from(Span::styled(
            super::truncate_with_ellipsis(&note, (area.width as usize).saturating_sub(2)),
            Style::default().fg(theme::CYAN),
        )));
    }
    if metadata_only {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            super::truncate_with_ellipsis(
                "There are no changed lines \u{2014} selecting this commits the change above on its own.",
                (area.width as usize).saturating_sub(2),
            ),
            Style::default().fg(theme::DIM),
        )));
    } else if !lines.is_empty() {
        lines.push(Line::raw(""));
    }
    // The sidebar's ⚠ glyph alone doesn't explain itself — spell out why
    // this file's hunks all came back deselected right where the user is
    // about to review them, not just as a badge they might not notice.
    if cf.status == Some(theme::FileStatus::Stale) {
        lines.push(Line::from(Span::styled(
            "File changed since your last review. All hunks were deselected.",
            Style::default().fg(theme::ORANGE),
        )));
        lines.push(Line::raw(""));
    }
    match file.and_then(|f| f.hunks.get(shown_index)) {
        Some(hunk) => {
            for dl in &hunk.lines {
                lines.push(Line::from(super::diff_line_spans(dl, super::diff_line_style(dl.kind))));
            }
        }
        None if metadata_only => {}
        None => {
            lines.push(Line::from(Span::styled("No cached diff content for this hunk.", Style::default().fg(theme::DIM))));
        }
    }

    let hints =
        vec![super::key_hints(&[("\u{2190}\u{2192}", &format!("Prev/next {unit}")), ("Space", &format!("Select/deselect this {unit}"))])];
    super::draw_panel(f, area, &title, Paragraph::new(lines), &hints);
}
