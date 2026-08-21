//! Minimal inline-markdown rendering for agent transcript text.
//!
//! Both backends deliver an answer as one complete block of text (not
//! deltas) that routinely uses `**bold**`, `` `code` ``, `#` headers, and
//! `-`/`*`/`1.` bullet lists. Rendering that as plain wrapped text left the
//! literal `**`/`` ` ``/`-` markers in place and — worse — collapsed every
//! newline into a single run-on paragraph, since word-wrapping split on all
//! whitespace including `\n`. This renders each source line on its own,
//! turns list/header markers into real formatting, and word-wraps the
//! result while keeping each token's style intact across the wrap.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme;

struct Token {
    text: String,
    style: Style,
}

/// Renders freeform markdown-ish `text` into wrapped, styled `Line`s using
/// `base` as the default prose style (callers pass a dim style for the
/// agent's private "thinking" text, full brightness for its answers).
pub fn render(text: &str, width: usize, base: Style) -> Vec<Line<'static>> {
    let width = width.max(4);
    let mut out = Vec::new();
    for raw_line in text.split('\n') {
        if raw_line.trim().is_empty() {
            out.push(Line::raw(""));
            continue;
        }
        let (prefix, indent, content, content_style) = classify_line(raw_line, base);
        let tokens = tokenize_inline(content, content_style);
        let avail = width.saturating_sub(prefix.chars().count()).max(4);
        let wrapped = wrap_tokens(&tokens, avail);
        for (i, spans) in wrapped.into_iter().enumerate() {
            let mut line_spans = Vec::new();
            if i == 0 {
                if !prefix.is_empty() {
                    line_spans.push(Span::styled(prefix.clone(), base));
                }
            } else if !indent.is_empty() {
                line_spans.push(Span::raw(indent.clone()));
            }
            line_spans.extend(spans);
            out.push(Line::from(line_spans));
        }
    }
    out
}

/// Splits a raw source line into `(prefix, continuation_indent, content,
/// content_style)` — headers get a bold/colored style and their `#`
/// markers stripped, bullets get a glyph prefix (with the wrapped
/// continuation lines indented to match), everything else passes through
/// as plain prose under `base`.
fn classify_line(raw: &str, base: Style) -> (String, String, &str, Style) {
    let trimmed = raw.trim_start();
    let leading_ws = &raw[..raw.len() - trimmed.len()];

    for marker in ["### ", "## ", "# "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return (String::new(), String::new(), rest, base.fg(theme::CYAN).add_modifier(Modifier::BOLD));
        }
    }

    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            let prefix = format!("{leading_ws}\u{2022} ");
            let indent = " ".repeat(prefix.chars().count());
            return (prefix, indent, rest, base);
        }
    }

    if let Some(dot) = trimmed.find(". ") {
        let (num, rest) = trimmed.split_at(dot);
        if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
            let prefix = format!("{leading_ws}{num}. ");
            let indent = " ".repeat(prefix.chars().count());
            return (prefix, indent, &rest[2..], base);
        }
    }

    (leading_ws.to_string(), " ".repeat(leading_ws.chars().count()), trimmed, base)
}

/// Tokenizes `text` into whitespace-separated words, toggling bold/code
/// styling on `**`/`` ` `` markers (consumed, not kept in the output).
fn tokenize_inline(text: &str, base: Style) -> Vec<Token> {
    let bold_style = base.add_modifier(Modifier::BOLD);
    let code_style = Style::default().fg(theme::YELLOW);

    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut in_bold = false;
    let mut in_code = false;

    let current_style = |in_bold: bool, in_code: bool| -> Style {
        if in_code {
            code_style
        } else if in_bold {
            bold_style
        } else {
            base
        }
    };

    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if !in_code && chars[i] == '*' && chars.get(i + 1) == Some(&'*') {
            if !word.is_empty() {
                tokens.push(Token { text: std::mem::take(&mut word), style: current_style(in_bold, in_code) });
            }
            in_bold = !in_bold;
            i += 2;
            continue;
        }
        if chars[i] == '`' {
            if !word.is_empty() {
                tokens.push(Token { text: std::mem::take(&mut word), style: current_style(in_bold, in_code) });
            }
            in_code = !in_code;
            i += 1;
            continue;
        }
        if chars[i].is_whitespace() {
            if !word.is_empty() {
                tokens.push(Token { text: std::mem::take(&mut word), style: current_style(in_bold, in_code) });
            }
            i += 1;
            continue;
        }
        word.push(chars[i]);
        i += 1;
    }
    if !word.is_empty() {
        tokens.push(Token { text: word, style: current_style(in_bold, in_code) });
    }
    tokens
}

/// Greedily wraps styled `tokens` into lines of at most `width` columns,
/// one space between tokens, each token keeping its own style.
fn wrap_tokens(tokens: &[Token], width: usize) -> Vec<Vec<Span<'static>>> {
    if tokens.is_empty() {
        return vec![Vec::new()];
    }
    let mut lines = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut current_width = 0usize;
    for tok in tokens {
        let tok_width = tok.text.chars().count();
        let sep = if current_width == 0 { 0 } else { 1 };
        if current_width + sep + tok_width > width && current_width > 0 {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        if current_width > 0 {
            current.push(Span::raw(" "));
            current_width += 1;
        }
        current.push(Span::styled(tok.text.clone(), tok.style));
        current_width += tok_width;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    #[test]
    fn preserves_explicit_line_breaks() {
        let out = render("first line\nsecond line", 80, Style::default());
        assert_eq!(plain(&out), vec!["first line", "second line"]);
    }

    #[test]
    fn blank_line_becomes_a_paragraph_break() {
        let out = render("a\n\nb", 80, Style::default());
        assert_eq!(plain(&out), vec!["a", "", "b"]);
    }

    #[test]
    fn strips_bold_markers_and_bolds_the_span() {
        let out = render("**hoot** is a TUI", 80, Style::default());
        assert_eq!(plain(&out), vec!["hoot is a TUI"]);
        let bold_span = out[0].spans.iter().find(|s| s.content.as_ref() == "hoot").unwrap();
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn strips_code_markers_and_colors_the_span() {
        let out = render("run `cargo test` now", 80, Style::default());
        assert_eq!(plain(&out), vec!["run cargo test now"]);
        let code_span = out[0].spans.iter().find(|s| s.content.as_ref() == "cargo").unwrap();
        assert_eq!(code_span.style.fg, Some(theme::YELLOW));
    }

    #[test]
    fn bullet_list_items_get_a_glyph_and_land_on_separate_lines() {
        let out = render("- first\n- second", 80, Style::default());
        assert_eq!(plain(&out), vec!["\u{2022} first", "\u{2022} second"]);
    }

    #[test]
    fn wrapped_bullet_continuation_lines_up_under_the_text() {
        let out = render("- one two three four five", 12, Style::default());
        assert!(out.len() > 1, "expected the bullet to wrap onto more than one line: {:?}", plain(&out));
        assert!(plain(&out)[1].starts_with("  "), "continuation should be indented under the bullet text: {:?}", plain(&out));
    }

    #[test]
    fn header_marker_is_stripped_and_bolded() {
        let out = render("## Summary", 80, Style::default());
        assert_eq!(plain(&out), vec!["Summary"]);
        assert!(out[0].spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn respects_width_when_wrapping_plain_prose() {
        let out = render("the quick brown fox jumps over", 11, Style::default());
        for line in &out {
            let len: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            assert!(len <= 11, "{:?} exceeds width", plain(&out));
        }
    }
}
