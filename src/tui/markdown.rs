use ansi_to_tui::IntoText;
use ratatui::text::Text;
use termimad::MadSkin;

pub fn make_skin() -> MadSkin {
    MadSkin::default_dark()
}

pub fn render_markdown(skin: &MadSkin, source: &str, width: usize) -> Text<'static> {
    let sanitized = strip_control_chars(source);
    let formatted = skin.text(&sanitized, Some(width));
    let rendered = format!("{formatted}");
    rendered
        .into_text()
        .unwrap_or_else(|_| Text::from(rendered))
}

fn strip_control_chars(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_control() && !matches!(ch, '\n' | '\t') {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_plain_text() {
        let skin = make_skin();
        let text = render_markdown(&skin, "hello world", 40);
        assert!(!text.lines.is_empty());
    }

    #[test]
    fn renders_headings_without_panic() {
        let skin = make_skin();
        let text = render_markdown(&skin, "# Heading\n\nbody", 40);
        assert!(!text.lines.is_empty());
    }

    #[test]
    fn strips_control_chars_from_source() {
        assert!(!strip_control_chars("before\x1b[5Dafter\x07end").contains('\x1b'));
        assert!(!strip_control_chars("osc\x1b]0;title\x07rest").contains('\x07'));
        assert!(strip_control_chars("line1\nline2\tcol").contains('\n'));
        assert!(strip_control_chars("line1\nline2\tcol").contains('\t'));
    }

    #[test]
    fn render_markdown_strips_embedded_escapes() {
        let skin = make_skin();
        let text = render_markdown(&skin, "hello\x1b[31mworld\x1b[0m", 40);
        let flat: String = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(!flat.contains('\x1b'));
    }
}
