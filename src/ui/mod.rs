mod agent;
mod agent_trust_confirm;
mod curation;
mod file_finder;
mod note_input;
mod quit_confirm;
mod review;
mod symbol_jump;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{App, Mode, Overlay};
use crate::theme;

#[cfg(test)]
use crate::agent_client::AgentBackend;

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    f.render_widget(ratatui::widgets::Block::default().style(Style::default().bg(theme::BG_OUTER)), area);

    let narrow = area.width < 100;

    let chunks = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(1), Constraint::Min(0)]).split(area);

    draw_status_line(f, app, chunks[0], narrow);

    match app.mode {
        Mode::Review => review::draw(f, app, chunks[1], narrow),
        Mode::Agent => agent::draw(f, app, chunks[1], narrow),
        Mode::Curation => curation::draw(f, app, chunks[1], narrow),
    }

    match app.overlay {
        Overlay::SymbolJump => symbol_jump::draw(f, app, area),
        Overlay::FileFinder => file_finder::draw(f, app, area),
        Overlay::NoteInput => note_input::draw(f, app, area),
        Overlay::QuitConfirm => quit_confirm::draw(f, app, area),
        Overlay::AgentTrustConfirm => agent_trust_confirm::draw(f, app, area),
        Overlay::None => {}
    }
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Review => "review",
        Mode::Agent => "agent",
        Mode::Curation => "curate",
    }
}

fn draw_status_line(f: &mut Frame, app: &App, area: Rect, narrow: bool) {
    // The demo's diff is a real git diff of a real repo — it just isn't
    // *your* repo, and it evaporates when hoot exits. Worth saying so at a
    // glance, since everything else on screen behaves exactly as it would
    // against real work.
    let demo_tag = if app.demo { "\u{26a0} DEMO \u{2014} " } else { "" };
    let left = format!(" {demo_tag}{} — {}", mode_label(app.mode), app.project.name);

    let center = match app.mode {
        Mode::Review => {
            let name = crate::gitreview::display_path_of(app.nav_file.strip_prefix(&app.target_dir).unwrap_or(&app.nav_file));
            if narrow {
                format!("{} changed", app.project.files.len())
            } else {
                let notes = app.notes_queued();
                format!(
                    "{name}:{}  \u{b7}  {} changed  \u{b7}  {notes} note{}",
                    app.nav_line + 1,
                    app.project.files.len(),
                    if notes == 1 { "" } else { "s" }
                )
            }
        }
        Mode::Agent => {
            let model = app.agent_model_live.as_deref().unwrap_or("\u{2014}");
            let status = if app.agent_running { "running" } else { "idle" };
            format!("{}   model: {}   {}", app.agent_backend.label(), model, status)
        }
        Mode::Curation => {
            let (sel, total): (u32, u32) = app.curation_files.iter().fold((0, 0), |(s, t), f| (s + f.selected(), t + f.total()));
            format!("{}/{} hunks selected", sel, total)
        }
    };

    let right = match app.mode {
        Mode::Review => format!("{} symbols scanned", app.symbols.len()),
        _ => String::new(),
    };

    let (left_pad, right_pad) = status_line_padding(area.width as usize, &left, &center, &right);

    let text = format!("{left}{:lw$}{center}{:rw$}{right} ", "", "", lw = left_pad, rw = right_pad);

    let para = Paragraph::new(text).style(Style::default().bg(theme::BG_SELECTION).fg(theme::FG));
    f.render_widget(para, area);
}

/// Splits `mid_gap` columns of padding between `left`/`right`, whatever's
/// left over after `left` + `center` + `right` themselves. Uses display
/// width (`unicode_width`), not byte length — a project name or file path
/// with CJK characters or other wide glyphs has more UTF-8 bytes per
/// column than plain ASCII, and sizing the gap off byte count would
/// overcount how much room the text actually needs, unevenly shrinking or
/// (via `saturating_sub`) fully collapsing the padding well before the
/// line is actually full. Cosmetic only — nothing panics either way — but
/// wrong regardless.
fn status_line_padding(mid_gap: usize, left: &str, center: &str, right: &str) -> (usize, usize) {
    use unicode_width::UnicodeWidthStr;

    // Never below a real gap, even when the segments already overflow the
    // line. Letting the padding collapse to nothing ran the project name
    // straight into the file path — `search-indexsrc/index/mod.rs:1` —
    // which reads as corrupted text rather than as a line that ran out of
    // room. Keeping the separator pushes the overflow off the right edge
    // instead, where a clipped tail at least looks clipped.
    const MIN_GAP: usize = 2;
    let used = left.width() + center.width() + right.width() + 2;
    let pad = mid_gap.saturating_sub(used);
    let left_pad = pad / 2;
    (left_pad.max(MIN_GAP), (pad - left_pad).max(MIN_GAP))
}

/// Bold key glyph + dim action label — the shape every footer hint in the
/// app uses, so one helper keeps them all consistent.
pub fn key_hints(items: &[(&str, &str)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, (key, label)) in items.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(key.to_string(), Style::default().fg(theme::FG).add_modifier(Modifier::BOLD)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(label.to_string(), Style::default().fg(theme::DIM)));
    }
    Line::from(spans)
}

/// Greedily word-wraps `text` to `width` columns. Pre-wrapping into
/// separate `Line`s (rather than relying on `Paragraph`'s own wrap) keeps
/// each rendered row known ahead of time — needed wherever a caller does
/// its own row-count math (Agent's bottom-anchored transcript scroll,
/// dialog box sizing, ...) instead of just handing ratatui a wrapping
/// widget and trusting whatever it decides to draw. A single word longer
/// than `width` is left on its own (slightly overflowing) line rather than
/// hard-split — simpler, and rare for natural-language text.
///
/// Measures in terminal columns (`unicode_width`), not `char`s: a CJK
/// character or an emoji occupies two cells, so a char count says a line
/// fits when it is in fact up to twice the width of the pane it is being
/// laid into — and callers here do their own row math on the result.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width < 4 {
        return vec![text.to_string()];
    }
    use unicode_width::UnicodeWidthStr;

    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let would_be = if current.is_empty() { word.width() } else { current.width() + 1 + word.width() };
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

/// Expands tab characters to spaces, advancing to the next multiple-of-4
/// column — matching how a real terminal renders a tab, not a fixed-width
/// substitution that would misalign wherever a tab isn't at the very start
/// of the line. Diff line text carries raw tabs straight from `git diff`'s
/// output (tab-indented source is extremely common — Go, Makefiles, ...)
/// and is kept byte-exact in `DiffLine.text` itself —
/// `gitcommit::build_patch` reconstructs a real patch from it for
/// `git apply`, so mutating the stored text would silently turn tabs into
/// spaces in a committed file. Expansion only ever happens here, right
/// before something gets drawn: left as `\t`, the terminal renders each
/// one as a jump to its own next tab stop while ratatui's cell-buffer math
/// assumes a fixed, much smaller width, so anything drawn after a tab
/// lands at the wrong column and visually overlaps whatever was already
/// there (confirmed — this is exactly what a real tab-indented Go diff
/// looked like before this existed).
///
/// Tracks column with `unicode_width`, not a plain char count: a wide
/// character (CJK, most emoji, ...) occupies two terminal columns, and a
/// tab stop after one needs to account for that same way ratatui's own
/// cell-buffer math does (ratatui depends on this exact crate itself) —
/// counting chars instead would land the tab one column short whenever a
/// wide character preceded it on the line.
pub fn expand_tabs_for_display(s: &str) -> String {
    use unicode_width::UnicodeWidthChar;

    const TAB_WIDTH: usize = 4;
    if !s.chars().any(|c| c == '\t' || c.is_control()) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut col = 0usize;
    for c in s.chars() {
        if c == '\t' {
            let spaces = TAB_WIDTH - (col % TAB_WIDTH);
            out.push_str(&" ".repeat(spaces));
            col += spaces;
        } else if c.is_control() {
            out.push(control_picture(c));
            col += 1;
        } else {
            out.push(c);
            col += c.width().unwrap_or(0);
        }
    }
    out
}

/// A visible stand-in for a control character, from Unicode's Control
/// Pictures block — `\r` becomes `␍`, an escape becomes `␛`.
///
/// Diff and source lines are file content, and file content can hold any
/// byte at all. Writing a raw control character into the cell buffer means
/// handing it to the terminal: a carriage return jumps the cursor back to
/// the start of the line and overwrites what was just drawn, and an escape
/// begins a sequence the terminal will happily interpret. Neither is
/// something a file being reviewed should be able to do to the screen. A
/// one-column glyph keeps the layout honest as well — every one of these
/// is exactly one cell wide.
fn control_picture(c: char) -> char {
    match c as u32 {
        0x7f => '\u{2421}',
        n @ 0..=0x1f => char::from_u32(0x2400 + n).unwrap_or('\u{fffd}'),
        _ => '\u{fffd}',
    }
}

/// Truncates `s` to at most `max_width` display columns, replacing the
/// tail with an ellipsis if it doesn't fit — so a too-narrow area loses a
/// clearly-marked suffix of the text instead of silently having it cut off
/// mid-character by the renderer with no indication anything's missing.
///
/// "Columns" here means real terminal columns (`unicode_width`), the same
/// measure ratatui's own cell buffer uses. This counted `char`s before,
/// which is the same number only for single-width text: a CJK path or an
/// emoji in a filename measured as half its real width and overflowed the
/// area anyway — the exact failure the ellipsis exists to prevent.
pub fn truncate_with_ellipsis(s: &str, max_width: usize) -> String {
    use unicode_width::UnicodeWidthStr;

    if s.width() <= max_width {
        return s.to_string();
    }
    match max_width {
        0 => String::new(),
        1 => "\u{2026}".to_string(),
        n => take_columns(s, n - 1) + "\u{2026}",
    }
}

/// The longest prefix of `s` that fits in `max_width` terminal columns.
///
/// Character-by-character rather than `chars().take(n)`: the two agree
/// only for text that is entirely single-width. A wide character is never
/// split across the boundary — if it would straddle it, it is left out,
/// so the result is always *at most* `max_width` columns and never
/// overflows the area it was measured for.
fn take_columns(s: &str, max_width: usize) -> String {
    use unicode_width::UnicodeWidthChar;

    let mut out = String::with_capacity(s.len());
    let mut used = 0usize;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > max_width {
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

/// The longest *suffix* of `s` that fits in `max_width` terminal columns —
/// `take_columns` from the other end, with the same no-split guarantee.
fn take_columns_from_end(s: &str, max_width: usize) -> String {
    use unicode_width::UnicodeWidthChar;

    let mut kept: Vec<char> = Vec::new();
    let mut used = 0usize;
    for c in s.chars().rev() {
        let w = c.width().unwrap_or(0);
        if used + w > max_width {
            break;
        }
        kept.push(c);
        used += w;
    }
    kept.into_iter().rev().collect()
}

/// Truncates `s` to `max_width` columns by dropping characters off the
/// *front*, marking the cut with a leading ellipsis.
///
/// For paths specifically. `truncate_with_ellipsis` keeps the head, which
/// is the wrong half of a path to save: everything that identifies it —
/// the repo, the file — is at the end, and the front is boilerplate.
/// A temp directory is the clearest case, since `--demo` runs in one:
/// keeping the head leaves `/private/var/folders/yc/2l75gz293hj1m_`, and
/// keeping the tail leaves `\u{2026}hoot-demo-VZhR5A/search-index`.
pub fn truncate_start_with_ellipsis(s: &str, max_width: usize) -> String {
    use unicode_width::UnicodeWidthStr;

    if s.width() <= max_width {
        return s.to_string();
    }
    match max_width {
        0 => String::new(),
        1 => "\u{2026}".to_string(),
        n => "\u{2026}".to_string() + &take_columns_from_end(s, n - 1),
    }
}

/// The span-list equivalent of `truncate_with_ellipsis`, for a styled
/// footer/hint line built from several differently-styled `Span`s (bold
/// key glyphs, dim labels, an appended status message, ...) rather than one
/// plain string — keeps each span's own style for whatever fits, and
/// ellipsis-marks the cut instead of losing all styling by flattening to
/// plain text first.
///
/// Whenever anything is dropped the result ends in an ellipsis, and the
/// result is never wider than `max_width`.
pub fn truncate_spans(spans: Vec<Span<'static>>, max_width: usize) -> Vec<Span<'static>> {
    use unicode_width::UnicodeWidthStr;

    let total: usize = spans.iter().map(|s| UnicodeWidthStr::width(s.content.as_ref())).sum();
    if total <= max_width {
        return spans;
    }
    if max_width == 0 {
        return Vec::new();
    }

    // One column is reserved for the marker up front, rather than trying to
    // fit it into whatever the last span happens to leave over. That
    // leftover can be exactly zero — a run that ends flush against the
    // boundary, which is what a hint row built from fixed-width labels
    // does regularly — and the old shape then dropped the remaining spans
    // with nothing at all to show for them. Content disappeared and the
    // row looked complete.
    let budget = max_width - 1;
    let mut out = Vec::new();
    let mut used = 0usize;
    let mut cut_style = None;
    for span in spans {
        let span_width = UnicodeWidthStr::width(span.content.as_ref());
        if used + span_width <= budget {
            used += span_width;
            cut_style = Some(span.style);
            out.push(span);
            continue;
        }
        let remaining = budget - used;
        if remaining > 0 {
            out.push(Span::styled(take_columns(&span.content, remaining), span.style));
        }
        cut_style = Some(span.style);
        break;
    }
    out.push(Span::styled("\u{2026}", cut_style.unwrap_or_default()));
    out
}

pub fn panel_block(title: &str) -> Block<'static> {
    Block::default()
        .title(Span::styled(format!(" {title} "), Style::default().fg(theme::CYAN)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::DIM))
        .style(Style::default().bg(theme::BG_PANEL).fg(theme::FG))
}

pub fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect { x, y, width, height }
}

/// The trailing note that marks a diff line as the last one in a file that
/// doesn't end in a newline — git's own `\ No newline at end of file`,
/// appended to the line it belongs to rather than given a row of its own.
///
/// A row of its own would read more like git, but three different
/// renderers show the same `DiffLine`s, and two of them anchor real
/// indices to the row list: the cursor position and line-scoped comments.
/// An inserted row would shift both. The reason it has to appear at all:
/// without it, a change that only adds or removes a file's final newline
/// shows up as an identical line removed and re-added, with nothing
/// anywhere on screen to explain what the difference is.
pub fn no_newline_span() -> Span<'static> {
    Span::styled("  \\ No newline at end of file", Style::default().fg(theme::DIM))
}

/// `dl` rendered as one row: its text, plus the no-newline marker when it
/// carries one.
pub fn diff_line_spans(dl: &crate::data::DiffLine, style: Style) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(expand_tabs_for_display(&dl.text), style)];
    if dl.no_newline {
        spans.push(no_newline_span());
    }
    spans
}

pub fn diff_line_style(kind: crate::data::DiffLineKind) -> Style {
    use crate::data::DiffLineKind::*;
    match kind {
        Context => Style::default().fg(theme::FG),
        Added => Style::default().fg(theme::GREEN),
        Removed => Style::default().fg(theme::RED),
        HunkHeader => Style::default().fg(theme::PURPLE).add_modifier(Modifier::BOLD),
    }
}

/// Renders a bordered panel with an optional divider + key-hint footer
/// inside the border — the standard box every screen's panes are built from.
/// Draws hint rows into `area`, each ellipsis-truncated to its width.
///
/// Every hint row in the app goes through here. Review's wide rows are long
/// enough to overrun the content pane at any terminal width short of about
/// 160 columns, and a bare `Paragraph` simply stops drawing where it runs
/// out — leaving "x Fla" and "y ...or copy the prompt to the " on screen,
/// which reads as a rendering fault rather than as a row with more in it.
/// Three separate render sites in `review` had their own copy of that bug.
pub fn draw_hints(f: &mut Frame, area: Rect, hints: Vec<Line<'static>>) {
    let width = area.width as usize;
    let rows: Vec<Line<'static>> = hints.into_iter().map(|l| Line::from(truncate_spans(l.spans, width))).collect();
    f.render_widget(Paragraph::new(rows), area);
}

pub fn draw_panel(f: &mut Frame, area: Rect, title: &str, body: Paragraph<'static>, hints: &[Line<'static>]) {
    let block = panel_block(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if !hints.is_empty() {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1), Constraint::Length(hints.len() as u16)])
            .split(inner);
        f.render_widget(body, chunks[0]);
        let divider = "─".repeat(chunks[1].width as usize);
        f.render_widget(Paragraph::new(divider).style(Style::default().fg(theme::DIM)), chunks[1]);
        draw_hints(f, chunks[2], hints.to_vec());
    } else {
        f.render_widget(body, inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::Keymap;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::fs;
    use std::path::PathBuf;

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
    fn expand_tabs_for_display_advances_to_the_next_stop() {
        // Regression: a real tab-indented Go diff rendered as visibly
        // garbled, overlapping text — ratatui doesn't expand or specially
        // measure a raw '\t', so its cell-buffer math assumed a much
        // smaller width than the terminal actually used once it hit the
        // tab, and everything after landed at the wrong column.
        assert_eq!(expand_tabs_for_display("\tif true {"), "    if true {");
        assert_eq!(expand_tabs_for_display("\t\treturn"), "        return");
        // A tab mid-line still advances to *its own* next stop, not a
        // flat substitution — "ab" occupies columns 0-1, so the tab here
        // only needs 2 spaces to reach column 4, not 4.
        assert_eq!(expand_tabs_for_display("ab\tc"), "ab  c");
    }

    #[test]
    fn expand_tabs_for_display_accounts_for_wide_characters() {
        // Regression: counting every char as one column undershoots by one
        // wherever a double-width character (CJK, most emoji, ...)
        // precedes a tab — "中" occupies two terminal columns, not one
        // (confirmed: ratatui itself depends on unicode-width for exactly
        // this), so a tab right after it only needs two more spaces to
        // reach the next stop (column 4), not three.
        assert_eq!(expand_tabs_for_display("中\tc"), "中  c");
    }

    #[test]
    fn expand_tabs_for_display_leaves_tab_free_text_untouched() {
        assert_eq!(expand_tabs_for_display("no tabs here"), "no tabs here");
        assert_eq!(expand_tabs_for_display(""), "");
    }

    #[test]
    fn status_line_segments_never_run_together() {
        // A long file path overflows the line at ordinary widths; when it
        // does, the segments must still be separated. Collapsing to zero
        // produced "search-indexsrc/index/postings.rs:1" on screen.
        let (left_pad, right_pad) = status_line_padding(
            40,
            " \u{26a0} DEMO \u{2014} review \u{2014} search-index",
            "src/index/postings.rs:1",
            "20 symbols scanned",
        );
        assert!(left_pad >= 2, "left gap collapsed: {left_pad}");
        assert!(right_pad >= 2, "right gap collapsed: {right_pad}");
    }

    #[test]
    fn status_line_padding_accounts_for_wide_characters() {
        // Regression: sizing the gap off byte length instead of display
        // width overcounts a project name (or file path) with CJK
        // characters — "中" is 3 bytes but occupies 2 terminal columns —
        // so a byte-based `used` here would come out to 19 (15 for
        // "中中中中中" + 1 + 1 + 2), leaving only 1 column of padding to
        // split for a 20-column line that, by actual display width, has
        // room for 6. The old logic would return (0, 1); this checks the
        // real, display-width-correct split instead.
        let (left_pad, right_pad) = status_line_padding(20, "中中中中中", "c", "r");
        assert_eq!((left_pad, right_pad), (3, 3));
    }

    use std::process::Command;

    fn scratch_repo(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hoot-ui-test-{label}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        for args in [["init", "-q"].as_slice(), &["config", "user.email", "test@example.com"], &["config", "user.name", "test"]] {
            assert!(Command::new("git").args(args).current_dir(&dir).status().unwrap().success());
        }
        dir
    }

    fn commit_file(dir: &std::path::Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(dir).status().unwrap();
        Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(dir).status().unwrap();
    }

    #[test]
    fn wide_characters_are_measured_in_columns_not_chars() {
        use unicode_width::UnicodeWidthStr;

        // Each of these is two terminal cells wide, so eight chars is
        // sixteen columns — a char count called this "fits in 10" and let
        // it overflow the pane it was measured for.
        let cjk = "\u{4f60}\u{597d}\u{4e16}\u{754c}\u{4f60}\u{597d}\u{4e16}\u{754c}";
        assert_eq!(cjk.chars().count(), 8);
        assert_eq!(cjk.width(), 16);

        for width in 0..20usize {
            let head = truncate_with_ellipsis(cjk, width);
            assert!(head.width() <= width, "head {head:?} is {} columns, over {width}", head.width());
            let tail = truncate_start_with_ellipsis(cjk, width);
            assert!(tail.width() <= width, "tail {tail:?} is {} columns, over {width}", tail.width());
        }

        // And the ellipsis really is a marker of loss, not decoration.
        assert!(truncate_with_ellipsis(cjk, 10).ends_with('\u{2026}'));
        assert!(truncate_start_with_ellipsis(cjk, 10).starts_with('\u{2026}'));
        // Text that genuinely fits is returned untouched.
        assert_eq!(truncate_with_ellipsis(cjk, 16), cjk);
    }

    #[test]
    fn wrapped_lines_respect_the_column_width_they_were_given() {
        use unicode_width::UnicodeWidthStr;

        let text = "\u{4f60}\u{597d} \u{4e16}\u{754c} \u{4f60}\u{597d} \u{4e16}\u{754c} \u{4f60}\u{597d}";
        for width in 4..20usize {
            for line in wrap_text(text, width) {
                // A single word wider than the pane is left overflowing by
                // design (documented on `wrap_text`); everything else must
                // fit the width it was asked for.
                assert!(line.width() <= width || line.split_whitespace().count() == 1, "{line:?} exceeds {width} columns");
            }
        }
    }

    #[test]
    fn truncate_with_ellipsis_leaves_short_text_alone() {
        assert_eq!(truncate_with_ellipsis("hi", 10), "hi");
        assert_eq!(truncate_with_ellipsis("exact", 5), "exact");
    }

    #[test]
    fn truncate_start_keeps_the_end_of_a_path() {
        // The identifying half of a path is its tail, so that's the half
        // that survives.
        assert_eq!(truncate_start_with_ellipsis("/a/b/c/repo", 20), "/a/b/c/repo");
        // Fills the width it's given: 19 characters of tail plus the ellipsis.
        assert_eq!(truncate_start_with_ellipsis("/private/var/folders/yc/xyz/search-index", 20), "\u{2026}yc/xyz/search-index");
        assert_eq!(truncate_start_with_ellipsis("/a/bcd", 1), "\u{2026}");
        assert_eq!(truncate_start_with_ellipsis("/a/bcd", 0), "");
    }

    #[test]
    fn truncate_start_never_exceeds_the_width_it_was_given() {
        let path = "/private/var/folders/yc/2l75gz293hj1m_/T/hoot-demo-VZhR5A/search-index";
        for width in [0usize, 1, 2, 12, 24, 38] {
            let out = truncate_start_with_ellipsis(path, width);
            assert!(out.chars().count() <= width, "{out:?} exceeds {width} columns");
        }
        assert_eq!(truncate_start_with_ellipsis(path, 500), path, "left alone when it already fits");
    }

    #[test]
    fn truncate_with_ellipsis_marks_where_text_was_cut() {
        assert_eq!(truncate_with_ellipsis("hello world", 8), "hello w\u{2026}");
        assert_eq!(truncate_with_ellipsis("hello world", 1), "\u{2026}");
        assert_eq!(truncate_with_ellipsis("hello world", 0), "");
    }

    /// Renders `app` into an in-memory buffer and flattens it to plain text
    /// (row by row, no styling) so tests can assert on visible content
    /// without a real terminal.
    fn render(app: &App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        flatten(&terminal)
    }

    /// Draws `app` once, applies `mutate`, then draws it again on the
    /// *same* `Terminal` and flattens that second frame. A one-shot
    /// `render()` always starts from a freshly zero-initialized buffer, so
    /// it can't reproduce a bug that only shows up between two real
    /// consecutive frames: `Terminal::draw` diffs the newly-rendered
    /// buffer against the *previous* one and only sends the real backend
    /// (a real terminal, or `TestBackend` here — both go through the same
    /// diffing) writes for cells that actually changed. A widget that
    /// doesn't fill its whole render area leaves the untouched cells
    /// exactly as the previous frame left them — invisible to a
    /// single-draw test, real on screen and in a two-draw test alike.
    fn render_after_transition(app: &mut App, width: u16, height: u16, mutate: impl FnOnce(&mut App)) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        mutate(app);
        terminal.draw(|f| draw(f, app)).unwrap();
        flatten(&terminal)
    }

    fn flatten(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn a_repo_git_cannot_read_says_so_instead_of_looking_clean() {
        // The user-facing half of the "unreadable is not clean" fix: a
        // failed diff read produces an empty file list, which renders
        // exactly like a repo with nothing to review unless the footer
        // says otherwise.
        let dir = scratch_repo("unreadable");
        commit_file(&dir, "f.txt", "line1\n");
        fs::write(dir.join("f.txt"), "line1-changed\n").unwrap();

        // A corrupt index leaves `is_git_repo` happy and breaks `git diff`.
        fs::write(dir.join(".git").join("index"), b"not an index at all").unwrap();
        let app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);

        assert!(app.review_error.is_some(), "the failure must reach the app");
        let screen = render(&app, 120, 30);
        assert!(screen.contains("couldn't read changes"), "the failure must reach the screen:\n{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn overlong_hint_rows_are_ellipsised_rather_than_cut_mid_word() {
        // Review's wide hint rows don't fit the content pane at any
        // terminal width short of ~160 columns. Before this, a Paragraph
        // just stopped drawing where it ran out, leaving "x Fla" and
        // "y ...or copy the prompt to the " on screen — indistinguishable
        // from a rendering fault.
        let dir = scratch_repo("hint-clip");
        commit_file(&dir, "f.txt", "line1\nline2\n");
        fs::write(dir.join("f.txt"), "line1-changed\nline2\n").unwrap();
        let app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);

        let screen = render(&app, 120, 32);
        let row =
            screen.lines().find(|l| l.contains("Tab Switch pane")).unwrap_or_else(|| panic!("no hint row on screen:\n{screen}")).trim_end();
        // The panel's own right border sits past the hint text.
        let row = row.trim_end_matches('\u{2502}').trim_end();
        assert!(row.ends_with('\u{2026}'), "hint row was cut instead of ellipsised: {row:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_screen_survives_a_tiny_terminal() {
        // Regression: Curate sized its commit panel with
        // `clamp(6, height * 3 / 5)`, and below a pane height of about ten
        // that ceiling drops under the floor — `clamp` panics when
        // min > max, so the whole app went down instead of drawing a
        // cramped panel. Nothing here asserts on content: the assertion is
        // that drawing completes at all, at sizes a real user produces by
        // dragging a window edge.
        let dir = scratch_repo("tiny-term");
        commit_file(&dir, "f.txt", "line1\nline2\n");
        fs::write(dir.join("f.txt"), "line1-changed\nline2\n").unwrap();
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);

        for mode in [Mode::Review, Mode::Curation, Mode::Agent] {
            app.mode = mode;
            for height in 1..=14u16 {
                for width in [1u16, 2, 8, 20, 40, 80] {
                    let _ = render(&app, width, height);
                }
            }
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_screen_shows_real_file_and_diff() {
        let dir = scratch_repo("hoot");
        commit_file(&dir, "f.txt", "line1\nline2\n");
        fs::write(dir.join("f.txt"), "line1-changed\nline2\n").unwrap();

        let app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        let screen = render(&app, 120, 30);
        assert!(screen.contains("f.txt"), "{screen}");
        assert!(screen.contains("line1-changed"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_refused_commit_says_why_at_a_normal_terminal_width() {
        // Regression: the outcome shared the hint row, and the hints alone
        // run to roughly 110 columns — so at any ordinary width the message
        // was truncated to a character or two. A refusal nobody can read is
        // indistinguishable from nothing happening.
        let dir = scratch_repo("commit-refusal-visible");
        commit_file(&dir, "f.txt", "one\n");
        fs::write(dir.join("f.txt"), "one\ntwo\n").unwrap();

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Curation;
        app.last_commit = Some(Err("an agent turn is running \u{2014} wait for it to finish".to_string()));

        let screen = render(&app, 120, 30);
        assert!(screen.contains("an agent turn is running"), "{screen}");
        assert!(screen.contains("wait for it to finish"), "the whole sentence, not a fragment of it: {screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_final_newline_change_says_so_instead_of_showing_two_identical_lines() {
        // Removing a file's trailing newline is a real change with no
        // visible difference: the diff shows `-b` and `+b`, character for
        // character identical. Without the marker git prints, the screen
        // gives no way at all to tell what changed — three renderers show
        // this data and none of them used to include it.
        let dir = scratch_repo("no-newline-visible");
        commit_file(&dir, "f.txt", "a\nb\n");
        fs::write(dir.join("f.txt"), "a\nb").unwrap();

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        let review = render(&app, 120, 30);
        assert!(review.contains("No newline at end of file"), "Review's diff must say so: {review}");

        app.mode = Mode::Curation;
        let curate = render(&app, 120, 30);
        assert!(curate.contains("No newline at end of file"), "and so must Curate's hunk view: {curate}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_mode_change_is_visible_in_curate_before_it_can_be_committed() {
        // It gets staged along with the file's content, so it has to be on
        // screen. It used to be nowhere: the parser dropped it, and
        // whole-file staging picked it up off disk anyway.
        let dir = scratch_repo("mode-visible");
        commit_file(&dir, "run.sh", "echo hi\n");
        fs::write(dir.join("run.sh"), "echo hi\necho there\n").unwrap();
        fs::set_permissions(dir.join("run.sh"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Curation;
        let screen = render(&app, 120, 30);
        assert!(screen.contains("100755"), "the new mode must be shown: {screen}");
        assert!(screen.contains("executable"), "in terms a person can act on: {screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_mode_only_change_reads_as_a_selectable_change_not_an_empty_hunk_view() {
        let dir = scratch_repo("mode-only-visible");
        commit_file(&dir, "run.sh", "echo hi\n");
        fs::set_permissions(dir.join("run.sh"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.mode = Mode::Curation;
        let screen = render(&app, 120, 30);
        assert!(screen.contains("change 1/1"), "it's a change, not a \"hunk\": {screen}");
        assert!(screen.contains("no changed lines"), "{screen}");
        assert!(!screen.contains("No cached diff content"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_context_view_shows_the_whole_file_not_just_the_hunk() {
        // A change on line 1 of a much longer file — under git's default
        // -U3 this wouldn't show line 30 at all, but Context mode loads
        // with a huge window specifically so the whole file is visible in
        // place around the change, not just a few lines of context.
        let dir = scratch_repo("context-whole-file");
        let content: String = (1..=30).map(|n| format!("line{n}\n")).collect();
        commit_file(&dir, "big.rs", &content);
        let mut lines: Vec<String> = (1..=30).map(|n| format!("line{n}")).collect();
        lines[0] = "line1-CHANGED".to_string();
        fs::write(dir.join("big.rs"), lines.join("\n") + "\n").unwrap();

        let app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        let screen = render(&app, 120, 40);
        assert!(screen.contains("line1-CHANGED"), "{screen}");
        assert!(screen.contains("line30"), "whole file should be visible, not just a window around the change: {screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_focused_view_collapses_unchanged_stretches() {
        let dir = scratch_repo("focused-collapse");
        let content: String = (1..=30).map(|n| format!("line{n}\n")).collect();
        commit_file(&dir, "big.rs", &content);
        let mut lines: Vec<String> = (1..=30).map(|n| format!("line{n}")).collect();
        lines[0] = "line1-CHANGED".to_string();
        fs::write(dir.join("big.rs"), lines.join("\n") + "\n").unwrap();

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        let screen = render(&app, 120, 40);
        assert!(screen.contains("line1-CHANGED"), "{screen}");
        assert!(screen.contains("unchanged"), "far-away unchanged lines should collapse: {screen}");
        assert!(!screen.contains("line30"), "line30 is far from the only change, should be collapsed away: {screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_screen_falls_back_to_source_browsing_on_a_clean_repo() {
        // Unlike the earlier review-only screen (which showed a blocking
        // "nothing to review" message), the merged screen stays useful on a
        // clean repo: there's no diff to show, so the content pane falls
        // back to plain source browsing instead of going empty.
        let dir = scratch_repo("hoot-clean");
        commit_file(&dir, "f.txt", "line1\n");

        let app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        let screen = render(&app, 120, 30);
        assert!(screen.contains("f.txt"), "{screen}");
        assert!(screen.contains("line1"), "{screen}");
        assert!(!screen.contains("[x]") && !screen.contains("[ ]"), "no diff, so no select checkbox: {screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_screen_shows_real_source_via_f1() {
        let dir = scratch_repo("nav");
        commit_file(&dir, "main.rs", "fn main() {\n    println!(\"hi\");\n}\n");

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        let screen = render(&app, 120, 30);
        assert!(screen.contains("main.rs"), "{screen}");
        assert!(screen.contains("println"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_source_viewport_follows_the_cursor_on_a_long_file() {
        let dir = scratch_repo("nav-scroll");
        let content: String = (1..=200).map(|n| format!("UNIQUE_LINE_MARKER_{n}\n")).collect();
        commit_file(&dir, "big.rs", &content);

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)); // focus source

        // Before jumping: line 1 is visible, line 150 is nowhere near the
        // top of a freshly opened file, so it shouldn't be on screen yet.
        let before = render(&app, 120, 30);
        assert!(before.contains("UNIQUE_LINE_MARKER_1\n") || before.contains("UNIQUE_LINE_MARKER_1 "), "{before}");
        assert!(!before.contains("UNIQUE_LINE_MARKER_150"), "{before}");

        for _ in 0..7 {
            app.on_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        }
        assert_eq!(app.nav_line, 140); // 7 * PAGE_SIZE(20)

        let after = render(&app, 120, 30);
        assert!(after.contains("UNIQUE_LINE_MARKER_141"), "viewport should have scrolled to follow the cursor: {after}");
        assert!(!after.contains("UNIQUE_LINE_MARKER_1\n"), "the original top of the file should have scrolled off: {after}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_screen_shows_pi_status_via_f3() {
        let dir = scratch_repo("agent");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE));
        let screen = render(&app, 120, 30);
        assert!(screen.contains("pi"), "{screen}");
        assert!(screen.contains("idle"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_screen_shows_the_active_backends_label() {
        let dir = scratch_repo("agent-opencode");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::OpenCode, false);
        app.on_key(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE));
        let screen = render(&app, 120, 30);
        assert!(screen.contains("opencode"), "the header/status line should reflect the selected backend: {screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_input_line_stays_visible_with_a_long_transcript() {
        use crate::data::{AgentLine, AgentLineKind};

        let dir = scratch_repo("agent-long");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE));
        app.transcript = (1..=100).map(|n| AgentLine { kind: AgentLineKind::Text, text: format!("TRANSCRIPT_LINE_{n}") }).collect();
        for c in "MY_TYPED_PROMPT".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        let screen = render(&app, 120, 30);
        assert!(screen.contains("MY_TYPED_PROMPT"), "the input line should stay on screen, not scroll off: {screen}");
        assert!(screen.contains("TRANSCRIPT_LINE_100"), "the latest transcript content should be visible: {screen}");
        assert!(!screen.contains("TRANSCRIPT_LINE_1\n") && !screen.contains("TRANSCRIPT_LINE_1 "), "early history shouldn't fit: {screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_page_up_reveals_earlier_transcript_history() {
        use crate::data::{AgentLine, AgentLineKind};

        let dir = scratch_repo("agent-scrollback");
        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE));
        app.transcript = (1..=100).map(|n| AgentLine { kind: AgentLineKind::Text, text: format!("TRANSCRIPT_LINE_{n}") }).collect();

        for _ in 0..8 {
            app.on_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        }
        let screen = render(&app, 120, 30);
        assert!(
            screen.contains("TRANSCRIPT_LINE_1\n") || screen.contains("TRANSCRIPT_LINE_1 "),
            "scrolling up should reach the start: {screen}"
        );
        assert!(screen.contains("scrolled up"), "should indicate we're not pinned to the bottom: {screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn curation_screen_shows_real_hunk_selection_via_f2() {
        let dir = scratch_repo("curate");
        commit_file(&dir, "f.txt", "a\nb\n");
        fs::write(dir.join("f.txt"), "a-changed\nb\n").unwrap();

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        let screen = render(&app, 120, 30);
        assert!(screen.contains("f.txt"), "{screen}");
        assert!(screen.contains("1/1 sel"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn curate_sidebar_marks_each_file_with_its_review_state() {
        // Regression: the three non-stale `FileStatus` variants were never
        // constructed anywhere outside the old fabricated demo data, so on
        // a real repo every row in Curate's sidebar rendered with a blank
        // where its status glyph belongs. They're derived from the file's
        // own review state now, matching what Review already marks.
        let dir = scratch_repo("curate-status");
        commit_file(&dir, "clean.txt", "a\n");
        commit_file(&dir, "flagged.txt", "b\n");
        fs::write(dir.join("clean.txt"), "a-changed\n").unwrap();
        fs::write(dir.join("flagged.txt"), "b-changed\n").unwrap();

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        let flagged = app.project.files.iter().position(|f| f.path == "flagged.txt").expect("flagged.txt");
        app.project.files[flagged].flagged = true;
        app.mode = Mode::Curation;

        let screen = render(&app, 120, 30);
        let row = |name: &str| screen.lines().find(|l| l.contains(name)).unwrap_or("").to_string();
        assert!(row("clean.txt").contains('\u{2714}'), "a clean file should be ticked: {:?}", row("clean.txt"));
        assert!(row("flagged.txt").contains('\u{2717}'), "a flagged file should be crossed: {:?}", row("flagged.txt"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn curate_sidebar_shows_queued_notes_before_a_clean_tick() {
        let dir = scratch_repo("curate-status-notes");
        commit_file(&dir, "noted.txt", "a\n");
        fs::write(dir.join("noted.txt"), "a-changed\n").unwrap();

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.project.files[0].notes = 1;
        app.mode = Mode::Curation;

        let screen = render(&app, 120, 30);
        let row = screen.lines().find(|l| l.contains("noted.txt")).unwrap_or("").to_string();
        assert!(row.contains('\u{29d6}'), "a file with queued notes should say so: {row:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn curation_hunk_view_renders_tab_indented_source_without_garbling() {
        // Regression: a real tab-indented Go diff rendered as visibly
        // corrupted, overlapping text in Curate's hunk box — ratatui
        // doesn't expand a raw '\t' before measuring/drawing it, so its
        // cell-buffer math landed everything after the tab at the wrong
        // column. Reproduces the exact shape (nested tabs, a removed
        // block, unchanged context above and below) and checks the
        // rendered screen for the tell-tale garbling instead of just
        // exercising the tab-expansion helper in isolation.
        let dir = scratch_repo("curate-tabs");
        let original = "func f() {\n\tif true {\n\t\thttp.NotFound(w, r)\n\t\treturn\n\t}\n\tbefore := 1\n\tif err != nil {\n\t}\n}\n";
        commit_file(&dir, "f.go", original);
        let changed = "func f() {\n\tif true {\n\t\thttp.NotFound(w, r)\n\t\treturn\n\t}\n\tif item.ContentType == \"static\" {\n\t\thttp.Error(w, \"x\", 1)\n\t\treturn\n\t}\n\tbefore := 1\n\tif err != nil {\n\t}\n}\n";
        fs::write(dir.join("f.go"), changed).unwrap();

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        let screen = render(&app, 178, 40);

        // Every line should render as clean, correctly-indented source —
        // no character-level overlap from a mispositioned tab. (git's
        // default 3-line context window means the hunk starts at
        // http.NotFound, not the enclosing "if true {" two lines above.)
        assert!(screen.contains("        http.NotFound(w, r)"), "{screen}");
        assert!(screen.contains("        return"), "{screen}");
        // Added lines keep their '+' diff marker ahead of the expanded tab.
        assert!(screen.contains("+   if item.ContentType == \"static\""), "{screen}");
        assert!(screen.contains("    before := 1"), "{screen}");
        assert!(screen.contains("    if err != nil {"), "{screen}");
        // The exact garbled fragments this bug used to produce.
        assert!(!screen.contains("return             a"), "{screen}");
        assert!(!screen.contains("iferr !=}nil {"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn symbol_jump_hint_row_has_no_background_bleed_through() {
        // Regression: the hint line rendered just above the overlay's own
        // box sat outside that box's `Clear` — a Paragraph only overwrites
        // cells its own text actually reaches, so whatever the screen
        // underneath had drawn at that row kept showing past wherever the
        // hint text ended. Looked exactly like real content from the
        // screen behind the overlay spliced in right after "Esc Close".
        let dir = scratch_repo("symjump-hint-bleed");
        // Every visible row needs its own full-width, distinctive content
        // — a single long line only fills the one row it's on, and
        // whichever row the hint lands on (depends on exact layout math)
        // would otherwise just be blank already, with nothing to bleed
        // through regardless of whether the bug is present.
        let content: String = (0..60).map(|i| format!("{}\n", "x".repeat(60) + &i.to_string())).collect();
        commit_file(&dir, "lib.rs", &content);

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        // First frame: the long background line, no overlay. Second frame
        // (after mutate): the overlay open on top of it — the exact
        // sequence a real keypress produces, and the only way this bug
        // shows up at all.
        let screen = render_after_transition(&mut app, 178, 50, |app| {
            app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        });

        let mut found_hint_row = false;
        for line in screen.lines() {
            if let Some(after) = line.split("Esc Close").nth(1) {
                found_hint_row = true;
                assert!(after.trim().trim_end_matches('\u{2502}').trim().is_empty(), "hint row has leftover background content: {line:?}");
            }
        }
        assert!(found_hint_row, "the hint row should have rendered at all: {screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_finder_hint_row_has_no_background_bleed_through() {
        let dir = scratch_repo("filefinder-hint-bleed");
        let content: String = (0..60).map(|i| format!("{}\n", "x".repeat(60) + &i.to_string())).collect();
        commit_file(&dir, "lib.rs", &content);

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        let screen = render_after_transition(&mut app, 178, 50, |app| {
            app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        });

        let mut found_hint_row = false;
        for line in screen.lines() {
            if let Some(after) = line.split("Esc Close").nth(1) {
                found_hint_row = true;
                assert!(after.trim().trim_end_matches('\u{2502}').trim().is_empty(), "hint row has leftover background content: {line:?}");
            }
        }
        assert!(found_hint_row, "the hint row should have rendered at all: {screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn symbol_jump_overlay_shows_real_matches_via_ctrl_k() {
        let dir = scratch_repo("symjump");
        commit_file(&dir, "lib.rs", "pub fn parse_query(s: &str) {}\n");

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        for c in "parse_q".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let screen = render(&app, 150, 40);
        assert!(screen.contains("parse_query"), "{screen}");
        assert!(screen.contains("lib.rs"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn symbol_jump_enter_navigates_to_the_real_definition() {
        let dir = scratch_repo("symjump-goto");
        commit_file(&dir, "lib.rs", "fn unrelated() {}\npub fn parse_query(s: &str) {}\n");

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        for c in "parse_query".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.nav_file.file_name().unwrap(), "lib.rs");
        assert_eq!(app.nav_line, 1); // 0-indexed line of the `pub fn parse_query` definition
                                     // Regression: jumping used to move the content pane only — the
                                     // tree/explorer pane's selection silently stayed wherever it had
                                     // been, so it could point at a completely different file than
                                     // what was actually on screen.
        assert_eq!(app.tree[app.tree_index].path, app.nav_file, "the tree selection should follow the jump too");
        let screen = render(&app, 150, 40);
        assert!(screen.contains("parse_query"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_finder_shows_fuzzy_matches_with_a_live_preview() {
        let dir = scratch_repo("finder-ui");
        commit_file(&dir, "main.rs", "fn main() {\n    println!(\"finder preview\");\n}\n");
        commit_file(&dir, "readme.md", "# unrelated\n");

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        for c in "mnrs".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let screen = render(&app, 150, 40);
        assert!(screen.contains("main.rs"), "{screen}");
        assert!(screen.contains("finder preview"), "live preview should show the file's real content: {screen}");
        assert!(!screen.contains("unrelated"), "readme.md shouldn't match \"mnrs\": {screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn note_input_overlay_shows_target_and_typed_text() {
        let dir = scratch_repo("note-overlay");
        commit_file(&dir, "a.rs", "a1\na2\n");
        fs::write(dir.join("a.rs"), "a1-changed\na2\n").unwrap();
        Command::new("git").args(["add", "-A"]).current_dir(&dir).status().unwrap();

        let mut app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        for c in "extract this".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let screen = render(&app, 150, 30);
        assert!(screen.contains("a.rs"), "{screen}");
        assert!(screen.contains("extract this"), "{screen}");
        assert!(screen.contains("Save"), "{screen}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn narrow_terminal_uses_compact_review_header() {
        let dir = scratch_repo("narrow");
        let app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        let screen = render(&app, 80, 30);
        // Narrow layout abbreviates the status line's full
        // "path:line · N changed · N notes" center down to just "N changed"
        // — check the status line (the first rendered row) specifically,
        // since the tree sidebar's own footer always shows a notes count
        // regardless of terminal width.
        let status_line = screen.lines().next().unwrap_or("");
        assert!(status_line.contains("changed"), "{status_line}");
        assert!(!status_line.contains("notes"), "{status_line}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn wide_terminal_uses_full_review_header() {
        let dir = scratch_repo("wide");
        let app = App::new(dir.clone(), Keymap::defaults(), AgentBackend::Pi, false);
        let screen = render(&app, 150, 30);
        let status_line = screen.lines().next().unwrap_or("");
        assert!(status_line.contains("changed"), "{status_line}");
        assert!(status_line.contains("notes"), "{status_line}");
        assert!(status_line.contains("symbols scanned"), "{status_line}");

        let _ = fs::remove_dir_all(&dir);
    }
}
