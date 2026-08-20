//! Lightweight, hand-rolled syntax highlighting for Navigate's source view.
//!
//! This tokenizes one line at a time, independent of every other line — not
//! a real lexer. Multi-line constructs (a Rust block comment, a Python
//! triple-quoted string) aren't tracked across lines, so they'll highlight
//! oddly at their boundaries. Good enough for a quick read over a handful
//! of common languages; not a substitute for a real grammar.
//!
//! Deliberately scoped to Navigate's source view only — the Steer/Curation
//! diff panels already use color to mean added/removed/context, and
//! layering a second, unrelated color meaning on top of that would make
//! both harder to read.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenKind {
    Plain,
    Keyword,
    String,
    Comment,
    Number,
}

pub struct Token {
    pub text: String,
    pub kind: TokenKind,
}

fn keywords_for(ext: &str) -> &'static [&'static str] {
    match ext {
        "rs" => &[
            "fn", "let", "mut", "pub", "struct", "enum", "impl", "trait", "match", "if", "else", "for", "while", "loop", "return", "use",
            "mod", "crate", "self", "Self", "as", "in", "move", "ref", "static", "const", "dyn", "where", "async", "await", "unsafe",
            "extern", "true", "false",
        ],
        "py" => &[
            "def", "class", "import", "from", "as", "if", "elif", "else", "for", "while", "return", "yield", "lambda", "None", "True",
            "False", "and", "or", "not", "in", "is", "with", "try", "except", "finally", "raise", "pass", "break", "continue", "global",
            "nonlocal", "async", "await", "self",
        ],
        "go" => &[
            "func",
            "package",
            "import",
            "if",
            "else",
            "for",
            "range",
            "return",
            "var",
            "const",
            "type",
            "struct",
            "interface",
            "map",
            "chan",
            "go",
            "defer",
            "select",
            "switch",
            "case",
            "default",
            "break",
            "continue",
            "fallthrough",
            "nil",
            "true",
            "false",
        ],
        "js" | "jsx" | "ts" | "tsx" => &[
            "function",
            "const",
            "let",
            "var",
            "if",
            "else",
            "for",
            "while",
            "return",
            "class",
            "import",
            "export",
            "from",
            "as",
            "new",
            "this",
            "typeof",
            "instanceof",
            "null",
            "undefined",
            "true",
            "false",
            "async",
            "await",
            "try",
            "catch",
            "finally",
            "throw",
            "switch",
            "case",
            "default",
            "break",
            "continue",
            "extends",
            "super",
            "yield",
        ],
        _ => &[],
    }
}

fn comment_prefix(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" | "go" | "js" | "jsx" | "ts" | "tsx" | "java" | "c" | "h" | "cpp" | "hpp" | "swift" | "kt" => Some("//"),
        "py" | "rb" => Some("#"),
        _ => None,
    }
}

/// Whether `ext` has any highlighting rules at all — callers can use this to
/// skip tokenizing (and just render plain text) for unrecognized languages.
pub fn supported(ext: &str) -> bool {
    !keywords_for(ext).is_empty() || comment_prefix(ext).is_some()
}

pub fn highlight_line(ext: &str, line: &str) -> Vec<Token> {
    let keywords = keywords_for(ext);
    let comment = comment_prefix(ext);
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();

    let mut tokens = Vec::new();
    let mut i = 0;

    while i < n {
        if let Some(prefix) = comment {
            if matches_at(&chars, i, prefix) {
                tokens.push(Token { text: chars[i..].iter().collect(), kind: TokenKind::Comment });
                break;
            }
        }

        let c = chars[i];

        if c == '"' || c == '\'' {
            let start = i;
            i += 1;
            while i < n && chars[i] != c {
                i += if chars[i] == '\\' && i + 1 < n { 2 } else { 1 };
            }
            if i < n {
                i += 1; // closing quote
            }
            tokens.push(Token { text: chars[start..i].iter().collect(), kind: TokenKind::String });
            continue;
        }

        if c.is_ascii_digit() {
            let start = i;
            while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_') {
                i += 1;
            }
            tokens.push(Token { text: chars[start..i].iter().collect(), kind: TokenKind::Number });
            continue;
        }

        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < n && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            let kind = if keywords.contains(&text.as_str()) { TokenKind::Keyword } else { TokenKind::Plain };
            tokens.push(Token { text, kind });
            continue;
        }

        // Run of whitespace/punctuation, stopping before the next token type.
        let start = i;
        while i < n {
            let c = chars[i];
            let starts_comment = comment.map(|p| matches_at(&chars, i, p)).unwrap_or(false);
            if c.is_alphanumeric() || c == '_' || c == '"' || c == '\'' || starts_comment {
                break;
            }
            i += 1;
        }
        tokens.push(Token { text: chars[start..i].iter().collect(), kind: TokenKind::Plain });
    }

    tokens
}

fn matches_at(chars: &[char], i: usize, prefix: &str) -> bool {
    let p: Vec<char> = prefix.chars().collect();
    i + p.len() <= chars.len() && chars[i..i + p.len()] == p[..]
}

pub fn ext_for(path: &std::path::Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(ext: &str, line: &str) -> Vec<(TokenKind, String)> {
        highlight_line(ext, line).into_iter().map(|t| (t.kind, t.text)).collect()
    }

    #[test]
    fn rust_keyword_and_plain_identifier() {
        let toks = kinds("rs", "pub fn draw() {");
        assert_eq!(toks[0].0, TokenKind::Keyword);
        assert_eq!(toks[0].1, "pub");
        assert!(toks.iter().any(|(k, t)| *k == TokenKind::Keyword && *t == "fn"));
        assert!(toks.iter().any(|(k, t)| *k == TokenKind::Plain && *t == "draw"));
    }

    #[test]
    fn rust_line_comment_takes_rest_of_line() {
        let toks = kinds("rs", "let x = 1; // trailing note");
        let comment = toks.iter().find(|(k, _)| *k == TokenKind::Comment).expect("comment token");
        assert_eq!(comment.1, "// trailing note");
    }

    #[test]
    fn string_literal_with_escaped_quote() {
        let toks = kinds("rs", r#"let s = "a \" b";"#);
        assert!(toks.iter().any(|(k, t)| *k == TokenKind::String && *t == r#""a \" b""#));
    }

    #[test]
    fn unterminated_string_consumes_to_end_of_line() {
        // Single-line tokenizer: no multi-line string tracking, so an
        // unterminated quote just runs to the end of this line.
        let toks = kinds("rs", r#"let s = "oops"#);
        assert!(toks.iter().any(|(k, t)| *k == TokenKind::String && *t == r#""oops"#));
    }

    #[test]
    fn number_token() {
        let toks = kinds("rs", "let x = 42;");
        assert!(toks.iter().any(|(k, t)| *k == TokenKind::Number && *t == "42"));
    }

    #[test]
    fn python_async_def_and_hash_comment() {
        let toks = kinds("py", "async def render(self):  # comment");
        assert!(toks.iter().any(|(k, t)| *k == TokenKind::Keyword && *t == "async"));
        assert!(toks.iter().any(|(k, t)| *k == TokenKind::Keyword && *t == "def"));
        assert!(toks.iter().any(|(k, t)| *k == TokenKind::Comment && *t == "# comment"));
    }

    #[test]
    fn go_func_and_type_are_keywords() {
        let toks = kinds("go", "func (s *Server) Handle() {");
        assert!(toks.iter().any(|(k, t)| *k == TokenKind::Keyword && *t == "func"));
        let toks = kinds("go", "type Server struct {");
        assert!(toks.iter().any(|(k, t)| *k == TokenKind::Keyword && *t == "type"));
        assert!(toks.iter().any(|(k, t)| *k == TokenKind::Keyword && *t == "struct"));
    }

    #[test]
    fn unrecognized_extension_is_unsupported() {
        assert!(!supported("xyz"));
        assert!(supported("rs"));
        assert!(supported("py"));
    }

    #[test]
    fn tokens_reassemble_to_the_original_line() {
        let line = "pub fn draw(f: &mut Frame) { let s = \"hi\"; } // done";
        let rebuilt: String = highlight_line("rs", line).into_iter().map(|t| t.text).collect();
        assert_eq!(rebuilt, line);
    }

    #[test]
    fn ext_for_reads_the_extension() {
        assert_eq!(ext_for(std::path::Path::new("src/main.rs")), "rs");
        assert_eq!(ext_for(std::path::Path::new("README")), "");
    }
}
