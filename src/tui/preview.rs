pub fn preview(body: &str, max_width: usize) -> String {
    let first_line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if first_line.is_empty() {
        return "(no content)".to_string();
    }
    let stripped = strip_markdown_prefix(first_line);
    let collapsed = collapse_whitespace(&stripped);
    truncate_with_ellipsis(&collapsed, max_width)
}

fn strip_markdown_prefix(line: &str) -> String {
    let trimmed = line.trim_start();
    if let Some(rest) = strip_heading(trimmed) {
        return rest.to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("> ") {
        return rest.trim_start().to_string();
    }
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return rest.trim_start().to_string();
        }
    }
    if let Some(rest) = strip_ordered_list(trimmed) {
        return rest.to_string();
    }
    trimmed.to_string()
}

fn strip_heading(line: &str) -> Option<&str> {
    let count = line.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&count) {
        let after = &line[count..];
        if let Some(rest) = after.strip_prefix(' ') {
            return Some(rest.trim_start());
        }
    }
    None
}

fn strip_ordered_list(line: &str) -> Option<&str> {
    let digit_end = line.chars().take_while(|c| c.is_ascii_digit()).count();
    if digit_end == 0 {
        return None;
    }
    let after_digits = &line[digit_end..];
    for sep in [". ", ") "] {
        if let Some(rest) = after_digits.strip_prefix(sep) {
            return Some(rest.trim_start());
        }
    }
    None
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        // Treat control chars as whitespace. This prevents raw ANSI escape
        // bytes or other C0/C1 controls inside an entry body from leaking
        // into the terminal during rendering (where they can move the cursor
        // and overwrite adjacent panes).
        if ch.is_whitespace() || ch.is_control() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

fn truncate_with_ellipsis(s: &str, max_width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if max_width == 0 {
        return String::new();
    }
    let total: usize = s.chars().map(|c| c.width().unwrap_or(0)).sum();
    if total <= max_width {
        return s.to_string();
    }
    if max_width == 1 {
        return "…".to_string();
    }
    // Reserve 1 cell for the ellipsis.
    let budget = max_width.saturating_sub(1);
    let mut used = 0usize;
    let mut out = String::new();
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_body_returns_no_content_marker() {
        assert_eq!(preview("", 40), "(no content)");
        assert_eq!(preview("   \n\n  \t ", 40), "(no content)");
    }

    #[test]
    fn strips_heading_marker() {
        assert_eq!(preview("# Hello world", 40), "Hello world");
        assert_eq!(preview("### Deep heading", 40), "Deep heading");
    }

    #[test]
    fn strips_blockquote_and_list_markers() {
        assert_eq!(preview("> quoted", 40), "quoted");
        assert_eq!(preview("- bullet", 40), "bullet");
        assert_eq!(preview("* star", 40), "star");
        assert_eq!(preview("+ plus", 40), "plus");
        assert_eq!(preview("1. numbered", 40), "numbered");
        assert_eq!(preview("12) parens", 40), "parens");
    }

    #[test]
    fn picks_first_non_empty_line() {
        assert_eq!(preview("\n\n  \nactual text", 40), "actual text");
    }

    #[test]
    fn collapses_internal_whitespace() {
        assert_eq!(preview("foo   bar\tbaz", 40), "foo bar baz");
    }

    #[test]
    fn truncates_with_ellipsis() {
        assert_eq!(preview("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn no_truncation_when_within_width() {
        assert_eq!(preview("abcdef", 10), "abcdef");
    }

    #[test]
    fn handles_multibyte_chars_without_panic() {
        let s = "héllo wörld 日本";
        let result = preview(s, 8);
        assert!(result.chars().count() <= 8);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn strips_ansi_escape_and_other_control_chars() {
        // A body containing a raw CSI "move cursor 5 left" escape must not
        // survive into the rendered preview; if it did, the terminal would
        // move the cursor and subsequent cells would overwrite other panes.
        let s = "before\x1b[5Dafter\x07\x1blegit";
        let result = preview(s, 40);
        assert!(!result.contains('\x1b'));
        assert!(!result.contains('\x07'));
        assert!(result.contains("before"));
        assert!(result.contains("after"));
        assert!(result.contains("legit"));
    }
}
