use super::model::*;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

pub fn rtjson_to_markdown(doc: &RtjDocument) -> String {
    let blocks = rtjson_to_blocks(doc);
    blocks_to_markdown(&blocks)
}

pub(crate) fn rtjson_to_blocks(doc: &RtjDocument) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut pending_spans: Vec<Span> = Vec::new();
    let mut pending_line_attrs: Option<RtjLineAttrs> = None;

    let flush =
        |blocks: &mut Vec<Block>, spans: &mut Vec<Span>, attrs: &mut Option<RtjLineAttrs>| {
            let line = TextLine {
                line_attrs: attrs.take().unwrap_or_default(),
                spans: std::mem::take(spans),
            };
            blocks.push(Block::Text(line));
        };

    for node in &doc.contents {
        match node {
            RtjNode::Text(tn) => {
                let attrs = tn.attributes.as_ref();
                // Capture line attrs from the first node that has them on this line
                if pending_line_attrs.is_none()
                    && let Some(a) = attrs
                    && let Some(la) = &a.line
                {
                    pending_line_attrs = Some(la.clone());
                }
                let span_base = attrs_to_span_base(attrs);
                let mut remaining = tn.text.as_str();
                while !remaining.is_empty() {
                    match remaining.find('\n') {
                        Some(pos) => {
                            let segment = &remaining[..pos];
                            if !segment.is_empty() {
                                pending_spans.push(Span {
                                    text: segment.to_owned(),
                                    ..span_base.clone()
                                });
                            }
                            flush(&mut blocks, &mut pending_spans, &mut pending_line_attrs);
                            remaining = &remaining[pos + 1..];
                        }
                        None => {
                            if !remaining.is_empty() {
                                pending_spans.push(Span {
                                    text: remaining.to_owned(),
                                    ..span_base.clone()
                                });
                            }
                            break;
                        }
                    }
                }
            }
            RtjNode::Embedded(en) => {
                // Flush any pending inline content first
                if !pending_spans.is_empty() || pending_line_attrs.is_some() {
                    flush(&mut blocks, &mut pending_spans, &mut pending_line_attrs);
                }
                for obj in &en.embedded_objects {
                    if let Some(emb) = parse_embedded_object(obj) {
                        blocks.push(Block::Embedded(emb));
                    }
                }
            }
            RtjNode::Unknown(_) => {}
        }
    }

    // Flush any remaining content
    if !pending_spans.is_empty() || pending_line_attrs.is_some() {
        flush(&mut blocks, &mut pending_spans, &mut pending_line_attrs);
    }

    // Strip trailing blank text lines (empty nodes from Day One app)
    while matches!(blocks.last(), Some(Block::Text(l)) if l.is_blank()) {
        blocks.pop();
    }

    blocks
}

fn attrs_to_span_base(attrs: Option<&RtjTextAttrs>) -> Span {
    let Some(a) = attrs else {
        return Span::default();
    };
    Span {
        text: String::new(),
        bold: a.bold,
        italic: a.italic,
        strikethrough: a.strikethrough,
        inline_code: a.inline_code,
        highlight: a.highlighted_color.is_some(),
        link_url: a.link_url.clone(),
        autolink: a.autolink,
    }
}

fn parse_embedded_object(obj: &serde_json::Value) -> Option<EmbeddedContent> {
    let type_str = obj.get("type")?.as_str()?;
    match type_str {
        "photo" | "video" | "audio" | "pdfAttachment" => {
            let identifier = obj.get("identifier")?.as_str()?.to_owned();
            let kind = match type_str {
                "photo" => MediaKind::Photo,
                "video" => MediaKind::Video,
                "audio" => MediaKind::Audio,
                _ => MediaKind::Pdf,
            };
            Some(EmbeddedContent::Media { kind, identifier })
        }
        "externalVideo" => {
            let url = obj.get("url")?.as_str()?.to_owned();
            Some(EmbeddedContent::ExternalMedia {
                kind: ExternalMediaKind::Video,
                url,
            })
        }
        "externalAudio" => {
            let url = obj.get("url")?.as_str()?.to_owned();
            Some(EmbeddedContent::ExternalMedia {
                kind: ExternalMediaKind::Audio,
                url,
            })
        }
        "horizontalLineRule" | "horizontalRuleLine" => Some(EmbeddedContent::HorizontalRule),
        "table" => {
            let markdown_cache = obj
                .get("markdown")
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned());
            let rows = parse_table_rows(obj.get("rows")?);
            Some(EmbeddedContent::Table {
                markdown_cache,
                rows,
            })
        }
        _ => Some(EmbeddedContent::Unknown),
    }
}

fn parse_table_rows(rows_val: &serde_json::Value) -> Vec<Vec<TableCell>> {
    let Some(rows) = rows_val.as_array() else {
        return vec![];
    };
    rows.iter()
        .map(|row| {
            let Some(cells) = row.as_array() else {
                return vec![];
            };
            cells
                .iter()
                .map(|cell| {
                    let header = cell
                        .get("header")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let spans = cell
                        .get("content")
                        .and_then(|c| c.as_array())
                        .map(|content| {
                            content
                                .iter()
                                .filter_map(|seg| {
                                    let text = seg.get("text")?.as_str()?.to_owned();
                                    if text.is_empty() {
                                        return None;
                                    }
                                    let a = seg.get("attributes");
                                    Some(Span {
                                        text,
                                        bold: a
                                            .and_then(|a| a.get("bold"))
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false),
                                        italic: a
                                            .and_then(|a| a.get("italic"))
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false),
                                        strikethrough: a
                                            .and_then(|a| a.get("strikethrough"))
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false),
                                        inline_code: a
                                            .and_then(|a| a.get("inlineCode"))
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false),
                                        highlight: a
                                            .and_then(|a| a.get("highlightedColor"))
                                            .is_some(),
                                        link_url: a
                                            .and_then(|a| a.get("linkURL"))
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_owned()),
                                        autolink: a
                                            .and_then(|a| a.get("autolink"))
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    TableCell { spans, header }
                })
                .collect()
        })
        .collect()
}

pub(crate) fn blocks_to_markdown(blocks: &[Block]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < blocks.len() {
        match &blocks[i] {
            Block::Text(line) if line.line_attrs.code_block => {
                let mut code_lines: Vec<String> = Vec::new();
                while i < blocks.len() {
                    if let Block::Text(l) = &blocks[i]
                        && l.line_attrs.code_block
                    {
                        code_lines.push(spans_to_plain(&l.spans));
                        i += 1;
                        continue;
                    }
                    break;
                }
                let fence = code_fence_for_lines(&code_lines);
                out.push_str(&fence);
                out.push('\n');
                for line in code_lines {
                    out.push_str(&line);
                    out.push('\n');
                }
                out.push_str(&fence);
                out.push('\n');
            }
            Block::Text(line) => {
                if line.is_blank() {
                    out.push('\n');
                } else {
                    let prefix = line_prefix(&line.line_attrs);
                    let mut content = spans_to_markdown(&line.spans);
                    content = escape_markdown_line_start(&content);
                    out.push_str(&prefix);
                    out.push_str(&content);
                    out.push('\n');
                }
                i += 1;
            }
            Block::Embedded(emb) => {
                match emb {
                    EmbeddedContent::Media { kind, identifier } => {
                        let url = match kind {
                            MediaKind::Photo => format!("dayone-moment://{}", identifier),
                            MediaKind::Video => {
                                format!("dayone-moment:/video/{}", identifier)
                            }
                            MediaKind::Audio => {
                                format!("dayone-moment:/audio/{}", identifier)
                            }
                            MediaKind::Pdf => {
                                format!("dayone-moment:/pdfAttachment/{}", identifier)
                            }
                        };
                        out.push_str(&format!("![]({})\n", url));
                    }
                    EmbeddedContent::ExternalMedia { url, .. } => {
                        let kind_segment = match emb {
                            EmbeddedContent::ExternalMedia {
                                kind: ExternalMediaKind::Video,
                                ..
                            } => "video",
                            EmbeddedContent::ExternalMedia {
                                kind: ExternalMediaKind::Audio,
                                ..
                            } => "audio",
                            _ => unreachable!(),
                        };
                        let encoded_url = URL_SAFE_NO_PAD.encode(url.as_bytes());
                        out.push_str(&format!(
                            "![](dayone-external:/{kind_segment}/{encoded_url})\n"
                        ));
                    }
                    EmbeddedContent::HorizontalRule => {
                        out.push_str("---\n");
                    }
                    EmbeddedContent::Table {
                        markdown_cache,
                        rows,
                    } => {
                        if let Some(md) = markdown_cache {
                            out.push_str(md);
                            out.push('\n');
                        } else {
                            out.push_str(&reconstruct_table_gfm(rows));
                        }
                    }
                    EmbeddedContent::Unknown => {}
                }
                i += 1;
            }
        }
    }
    out
}

fn line_prefix(attrs: &RtjLineAttrs) -> String {
    // For blockquotes, indentation must follow the quote marker.
    let mut prefix = String::new();
    if attrs.quote {
        prefix.push_str("> ");
    }
    if attrs.indent_level > 1 {
        prefix.push_str(&" ".repeat(((attrs.indent_level - 1) as usize) * 2));
    }

    match &attrs.list_style {
        Some(ListStyle::Bulleted) => prefix.push_str("- "),
        Some(ListStyle::Numbered) => {
            prefix.push_str(&format!("{}. ", attrs.list_index.unwrap_or(1)))
        }
        Some(ListStyle::Checkbox) => {
            let mark = if attrs.checked == Some(true) {
                "x"
            } else {
                " "
            };
            prefix.push_str(&format!("- [{}] ", mark));
        }
        None => {
            if let Some(n) = attrs.header {
                prefix.push_str(&"#".repeat(n as usize));
                prefix.push(' ');
            }
        }
    }
    prefix
}

fn spans_to_markdown(spans: &[Span]) -> String {
    spans.iter().map(span_to_markdown).collect()
}

fn span_to_markdown(span: &Span) -> String {
    if span.text.is_empty() {
        return String::new();
    }
    let s = span.text.clone();
    if span.inline_code {
        return inline_code_to_markdown(&s);
    }
    if span.autolink {
        return format!("<{}>", s);
    }
    let formatted_text = apply_span_wrappers(escape_markdown_text(&s), span);
    if let Some(url) = &span.link_url {
        return format!(
            "[{}]({})",
            formatted_text,
            escape_markdown_link_destination(url)
        );
    }
    formatted_text
}

fn apply_span_wrappers(mut text: String, span: &Span) -> String {
    if span.highlight {
        text = format!("=={}==", text);
    }
    if span.strikethrough {
        text = format!("~~{}~~", text);
    }
    if span.bold && span.italic {
        text = format!("**_{}_**", text);
    } else if span.bold {
        text = format!("**{}**", text);
    } else if span.italic {
        text = format!("_{}_", text);
    }
    text
}

fn spans_to_plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

fn code_fence_for_lines(lines: &[String]) -> String {
    "`".repeat(
        (lines
            .iter()
            .map(|line| longest_backtick_run(line))
            .max()
            .unwrap_or(0)
            + 1)
        .max(3),
    )
}

fn longest_backtick_run(text: &str) -> usize {
    let mut longest_run = 0usize;
    let mut current_run = 0usize;
    for ch in text.chars() {
        if ch == '`' {
            current_run += 1;
            if current_run > longest_run {
                longest_run = current_run;
            }
        } else {
            current_run = 0;
        }
    }
    longest_run
}

fn inline_code_to_markdown(text: &str) -> String {
    let fence = "`".repeat(longest_backtick_run(text) + 1);
    if text.starts_with(' ') || text.ends_with(' ') || text.starts_with('`') || text.ends_with('`')
    {
        format!("{fence} {text} {fence}")
    } else {
        format!("{fence}{text}{fence}")
    }
}

fn escape_markdown_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if matches!(ch, '\\' | '*' | '_' | '[' | ']' | '`' | '|' | '~' | '=') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn escape_markdown_link_destination(input: &str) -> String {
    if input.contains(char::is_whitespace) || input.contains('(') || input.contains(')') {
        let escaped = input.replace('<', "%3C").replace('>', "%3E");
        format!("<{escaped}>")
    } else {
        input.to_owned()
    }
}

fn escape_markdown_line_start(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    if input.starts_with("```") || input.starts_with("~~~") || is_thematic_break_like(input) {
        return format!("\\{input}");
    }
    if input.starts_with("# ")
        || input.starts_with("> ")
        || input.starts_with("- ")
        || input.starts_with("* ")
        || input.starts_with("+ ")
    {
        return format!("\\{input}");
    }
    let bytes = input.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && bytes.get(i) == Some(&b'.') && bytes.get(i + 1) == Some(&b' ') {
        return format!("\\{input}");
    }
    input.to_owned()
}

fn is_thematic_break_like(input: &str) -> bool {
    let compact: String = input.chars().filter(|ch| *ch != ' ').collect();
    if compact.len() < 3 {
        return false;
    }
    let mut chars = compact.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !matches!(first, '-' | '*' | '_') {
        return false;
    }
    chars.all(|ch| ch == first)
}

fn reconstruct_table_gfm(rows: &[Vec<TableCell>]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let col_count = rows.iter().map(|row| row.len()).max().unwrap_or(0);
    let header_row_index = rows
        .iter()
        .position(|row| row.iter().any(|cell| cell.header));

    // Header row (use flagged header row, or an empty one)
    out.push('|');
    if let Some(index) = header_row_index {
        for col in 0..col_count {
            out.push(' ');
            if let Some(cell) = rows[index].get(col) {
                out.push_str(&spans_to_markdown(&cell.spans));
            }
            out.push_str(" |");
        }
    } else {
        for _ in 0..col_count {
            out.push_str("  |");
        }
    }
    out.push('\n');

    // Separator
    out.push('|');
    for _ in 0..col_count {
        out.push_str(" --- |");
    }
    out.push('\n');

    // Data rows
    for (row_index, row) in rows.iter().enumerate() {
        if Some(row_index) == header_row_index {
            continue;
        }
        out.push('|');
        for col in 0..col_count {
            out.push(' ');
            if let Some(cell) = row.get(col) {
                out.push_str(&spans_to_markdown(&cell.spans));
            }
            out.push_str(" |");
        }
        out.push('\n');
    }
    out
}

// ── tests ──────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc(contents: serde_json::Value) -> RtjDocument {
        serde_json::from_value(json!({"contents": contents})).unwrap()
    }

    #[test]
    fn plain_text_single_node_no_newline() {
        let d = doc(json!([{"text": "hello"}]));
        let blocks = rtjson_to_blocks(&d);
        assert_eq!(blocks.len(), 1);
        let Block::Text(line) = &blocks[0] else {
            panic!("expected text")
        };
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].text, "hello");
    }

    #[test]
    fn newline_in_text_creates_two_lines() {
        let d = doc(json!([{"text": "line1\nline2"}]));
        let blocks = rtjson_to_blocks(&d);
        assert_eq!(blocks.len(), 2);
        let Block::Text(l1) = &blocks[0] else {
            panic!()
        };
        let Block::Text(l2) = &blocks[1] else {
            panic!()
        };
        assert_eq!(l1.spans[0].text, "line1");
        assert_eq!(l2.spans[0].text, "line2");
    }

    #[test]
    fn double_newline_creates_blank_line() {
        let d = doc(json!([{"text": "para1\n\npara2"}]));
        let blocks = rtjson_to_blocks(&d);
        assert_eq!(blocks.len(), 3);
        let Block::Text(blank) = &blocks[1] else {
            panic!()
        };
        assert!(blank.is_blank());
    }

    #[test]
    fn line_attrs_captured_from_first_node_of_line() {
        let d = doc(json!([
            {"attributes": {"line": {"header": 1}}, "text": "Title\n"}
        ]));
        let blocks = rtjson_to_blocks(&d);
        assert_eq!(blocks.len(), 1);
        let Block::Text(line) = &blocks[0] else {
            panic!()
        };
        assert_eq!(line.line_attrs.header, Some(1));
    }

    #[test]
    fn inline_formatting_becomes_span_attrs() {
        let d = doc(json!([
            {"attributes": {"bold": true}, "text": "bold"},
            {"attributes": {}, "text": " normal"}
        ]));
        let blocks = rtjson_to_blocks(&d);
        assert_eq!(blocks.len(), 1);
        let Block::Text(line) = &blocks[0] else {
            panic!()
        };
        assert_eq!(line.spans.len(), 2);
        assert!(line.spans[0].bold);
        assert!(!line.spans[1].bold);
    }

    #[test]
    fn embedded_node_becomes_embedded_block() {
        let d = doc(json!([
            {"embeddedObjects": [{"type": "photo", "identifier": "ABC"}]}
        ]));
        let blocks = rtjson_to_blocks(&d);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(
            blocks[0],
            Block::Embedded(EmbeddedContent::Media { .. })
        ));
    }

    #[test]
    fn trailing_empty_text_nodes_stripped() {
        let d = doc(json!([
            {"text": "hello\n"},
            {"text": ""}
        ]));
        let blocks = rtjson_to_blocks(&d);
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn horizontal_rule_variants_both_accepted() {
        for type_str in &["horizontalLineRule", "horizontalRuleLine"] {
            let d = doc(json!([
                {"embeddedObjects": [{"type": type_str}]}
            ]));
            let blocks = rtjson_to_blocks(&d);
            assert!(matches!(
                blocks[0],
                Block::Embedded(EmbeddedContent::HorizontalRule)
            ));
        }
    }

    #[test]
    fn plain_text_line_renders_with_newline() {
        let blocks = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs::default(),
            spans: vec![Span {
                text: "hello".into(),
                ..Default::default()
            }],
        })];
        assert_eq!(blocks_to_markdown(&blocks), "hello\n");
    }

    #[test]
    fn plain_text_line_escapes_block_markers_at_start() {
        let blocks = vec![
            Block::Text(TextLine {
                spans: vec![Span {
                    text: "# heading".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            Block::Text(TextLine {
                spans: vec![Span {
                    text: "1. numbered".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            Block::Text(TextLine {
                spans: vec![Span {
                    text: "---".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            Block::Text(TextLine {
                spans: vec![Span {
                    text: "```code".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
        ];
        assert_eq!(
            blocks_to_markdown(&blocks),
            "\\# heading\n\\1. numbered\n\\---\n\\`\\`\\`code\n"
        );
    }

    #[test]
    fn blank_line_renders_as_empty_line() {
        let blocks = vec![
            Block::Text(TextLine {
                spans: vec![Span {
                    text: "a".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            Block::Text(TextLine::default()),
            Block::Text(TextLine {
                spans: vec![Span {
                    text: "b".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
        ];
        assert_eq!(blocks_to_markdown(&blocks), "a\n\nb\n");
    }

    #[test]
    fn heading_renders_with_hashes() {
        for (level, prefix) in [(1u8, "# "), (2, "## "), (3, "### ")] {
            let blocks = vec![Block::Text(TextLine {
                line_attrs: RtjLineAttrs {
                    header: Some(level),
                    ..Default::default()
                },
                spans: vec![Span {
                    text: "Title".into(),
                    ..Default::default()
                }],
            })];
            assert_eq!(blocks_to_markdown(&blocks), format!("{prefix}Title\n"));
        }
    }

    #[test]
    fn bulleted_list_indent_level_1_no_indent() {
        let blocks = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs {
                list_style: Some(ListStyle::Bulleted),
                indent_level: 1,
                ..Default::default()
            },
            spans: vec![Span {
                text: "item".into(),
                ..Default::default()
            }],
        })];
        assert_eq!(blocks_to_markdown(&blocks), "- item\n");
    }

    #[test]
    fn bulleted_list_indent_level_2_two_spaces() {
        let blocks = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs {
                list_style: Some(ListStyle::Bulleted),
                indent_level: 2,
                ..Default::default()
            },
            spans: vec![Span {
                text: "nested".into(),
                ..Default::default()
            }],
        })];
        assert_eq!(blocks_to_markdown(&blocks), "  - nested\n");
    }

    #[test]
    fn numbered_list_renders_1_dot() {
        let blocks = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs {
                list_style: Some(ListStyle::Numbered),
                indent_level: 1,
                ..Default::default()
            },
            spans: vec![Span {
                text: "item".into(),
                ..Default::default()
            }],
        })];
        assert_eq!(blocks_to_markdown(&blocks), "1. item\n");
    }

    #[test]
    fn numbered_list_uses_list_index_when_present() {
        let blocks = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs {
                list_style: Some(ListStyle::Numbered),
                list_index: Some(3),
                ..Default::default()
            },
            spans: vec![Span {
                text: "item".into(),
                ..Default::default()
            }],
        })];
        assert_eq!(blocks_to_markdown(&blocks), "3. item\n");
    }

    #[test]
    fn checkbox_unchecked_and_checked() {
        let mk = |checked: bool| {
            Block::Text(TextLine {
                line_attrs: RtjLineAttrs {
                    list_style: Some(ListStyle::Checkbox),
                    indent_level: 1,
                    checked: Some(checked),
                    ..Default::default()
                },
                spans: vec![Span {
                    text: "task".into(),
                    ..Default::default()
                }],
            })
        };
        assert_eq!(blocks_to_markdown(&[mk(false)]), "- [ ] task\n");
        assert_eq!(blocks_to_markdown(&[mk(true)]), "- [x] task\n");
    }

    #[test]
    fn blockquote_renders_gt_prefix() {
        let blocks = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs {
                quote: true,
                ..Default::default()
            },
            spans: vec![Span {
                text: "quoted".into(),
                ..Default::default()
            }],
        })];
        assert_eq!(blocks_to_markdown(&blocks), "> quoted\n");
    }

    #[test]
    fn code_block_wrapped_in_fences() {
        let blocks = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs {
                code_block: true,
                ..Default::default()
            },
            spans: vec![Span {
                text: "let x = 1;".into(),
                ..Default::default()
            }],
        })];
        assert_eq!(blocks_to_markdown(&blocks), "```\nlet x = 1;\n```\n");
    }

    #[test]
    fn consecutive_code_block_lines_share_fences() {
        let mk = |text: &str| {
            Block::Text(TextLine {
                line_attrs: RtjLineAttrs {
                    code_block: true,
                    ..Default::default()
                },
                spans: vec![Span {
                    text: text.into(),
                    ..Default::default()
                }],
            })
        };
        let blocks = vec![mk("line1"), mk("line2")];
        assert_eq!(blocks_to_markdown(&blocks), "```\nline1\nline2\n```\n");
    }

    #[test]
    fn code_block_uses_longer_fence_when_needed() {
        let blocks = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs {
                code_block: true,
                ..Default::default()
            },
            spans: vec![Span {
                text: "has ``` fence".into(),
                ..Default::default()
            }],
        })];
        assert_eq!(blocks_to_markdown(&blocks), "````\nhas ``` fence\n````\n");
    }

    #[test]
    fn inline_bold_italic_strikethrough_code_highlight() {
        let spans = vec![
            Span {
                text: "a".into(),
                bold: true,
                ..Default::default()
            },
            Span {
                text: "b".into(),
                italic: true,
                ..Default::default()
            },
            Span {
                text: "c".into(),
                bold: true,
                italic: true,
                ..Default::default()
            },
            Span {
                text: "d".into(),
                strikethrough: true,
                ..Default::default()
            },
            Span {
                text: "e".into(),
                inline_code: true,
                ..Default::default()
            },
            Span {
                text: "f".into(),
                highlight: true,
                ..Default::default()
            },
        ];
        let blocks = vec![Block::Text(TextLine {
            spans,
            ..Default::default()
        })];
        assert_eq!(
            blocks_to_markdown(&blocks),
            "**a**_b_**_c_**~~d~~`e`==f==\n"
        );
    }

    #[test]
    fn inline_code_with_backticks_uses_longer_fence() {
        let blocks = vec![Block::Text(TextLine {
            spans: vec![Span {
                text: "has `tick`".into(),
                inline_code: true,
                ..Default::default()
            }],
            ..Default::default()
        })];
        assert_eq!(blocks_to_markdown(&blocks), "`` has `tick` ``\n");
    }

    #[test]
    fn inline_code_with_leading_or_trailing_space_uses_padding_spaces() {
        let blocks = vec![Block::Text(TextLine {
            spans: vec![Span {
                text: " trailing ".into(),
                inline_code: true,
                ..Default::default()
            }],
            ..Default::default()
        })];
        assert_eq!(blocks_to_markdown(&blocks), "`  trailing  `\n");
    }

    #[test]
    fn inline_code_starting_or_ending_with_backtick_uses_padding_spaces() {
        let blocks = vec![Block::Text(TextLine {
            spans: vec![Span {
                text: "`edge`".into(),
                inline_code: true,
                ..Default::default()
            }],
            ..Default::default()
        })];
        assert_eq!(blocks_to_markdown(&blocks), "`` `edge` ``\n");
    }

    #[test]
    fn link_and_autolink() {
        let blocks = vec![Block::Text(TextLine {
            spans: vec![
                Span {
                    text: "click".into(),
                    link_url: Some("https://x.com".into()),
                    ..Default::default()
                },
                Span {
                    text: "https://y.com".into(),
                    autolink: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        })];
        assert_eq!(
            blocks_to_markdown(&blocks),
            "[click](https://x.com)<https://y.com>\n"
        );
    }

    #[test]
    fn markdown_metacharacters_are_escaped_in_text_and_links() {
        let blocks = vec![Block::Text(TextLine {
            spans: vec![
                Span {
                    text: "*_[x]`|~=".into(),
                    ..Default::default()
                },
                Span {
                    text: "*link*".into(),
                    link_url: Some("https://example.com".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        })];
        assert_eq!(
            blocks_to_markdown(&blocks),
            "\\*\\_\\[x\\]\\`\\|\\~\\=[\\*link\\*](https://example.com)\n"
        );
    }

    #[test]
    fn markdown_link_destination_is_wrapped_when_escaping_is_needed() {
        let blocks = vec![Block::Text(TextLine {
            spans: vec![Span {
                text: "click".into(),
                link_url: Some("https://example.com/a path(1)".into()),
                ..Default::default()
            }],
            ..Default::default()
        })];
        assert_eq!(
            blocks_to_markdown(&blocks),
            "[click](<https://example.com/a path(1)>)\n"
        );
    }

    #[test]
    fn link_text_preserves_inline_formatting() {
        let blocks = vec![Block::Text(TextLine {
            spans: vec![Span {
                text: "bold".into(),
                bold: true,
                link_url: Some("https://example.com".into()),
                ..Default::default()
            }],
            ..Default::default()
        })];
        assert_eq!(
            blocks_to_markdown(&blocks),
            "[**bold**](https://example.com)\n"
        );
    }

    #[test]
    fn media_renders_dayone_moment_url() {
        let cases = [
            (MediaKind::Photo, "![](dayone-moment://ABC)\n"),
            (MediaKind::Video, "![](dayone-moment:/video/ABC)\n"),
            (MediaKind::Audio, "![](dayone-moment:/audio/ABC)\n"),
            (MediaKind::Pdf, "![](dayone-moment:/pdfAttachment/ABC)\n"),
        ];
        for (kind, expected) in cases {
            let blocks = vec![Block::Embedded(EmbeddedContent::Media {
                kind,
                identifier: "ABC".into(),
            })];
            assert_eq!(blocks_to_markdown(&blocks), expected);
        }
    }

    #[test]
    fn external_media_renders_typed_placeholder() {
        let blocks = vec![Block::Embedded(EmbeddedContent::ExternalMedia {
            kind: ExternalMediaKind::Video,
            url: "https://youtu.be/x".into(),
        })];
        assert_eq!(
            blocks_to_markdown(&blocks),
            "![](dayone-external:/video/aHR0cHM6Ly95b3V0dS5iZS94)\n"
        );
    }

    #[test]
    fn quote_prefix_composes_with_list_and_header_prefixes() {
        let list = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs {
                quote: true,
                list_style: Some(ListStyle::Bulleted),
                ..Default::default()
            },
            spans: vec![Span {
                text: "item".into(),
                ..Default::default()
            }],
        })];
        assert_eq!(blocks_to_markdown(&list), "> - item\n");

        let heading = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs {
                quote: true,
                header: Some(2),
                ..Default::default()
            },
            spans: vec![Span {
                text: "Title".into(),
                ..Default::default()
            }],
        })];
        assert_eq!(blocks_to_markdown(&heading), "> ## Title\n");

        let nested_list = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs {
                quote: true,
                list_style: Some(ListStyle::Bulleted),
                indent_level: 2,
                ..Default::default()
            },
            spans: vec![Span {
                text: "nested".into(),
                ..Default::default()
            }],
        })];
        assert_eq!(blocks_to_markdown(&nested_list), ">   - nested\n");

        let quoted_list_with_heading_like_content = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs {
                quote: true,
                list_style: Some(ListStyle::Bulleted),
                ..Default::default()
            },
            spans: vec![Span {
                text: "# heading-like text".into(),
                ..Default::default()
            }],
        })];
        assert_eq!(
            blocks_to_markdown(&quoted_list_with_heading_like_content),
            "> - \\# heading-like text\n"
        );
    }

    #[test]
    fn horizontal_rule_renders_dashes() {
        let blocks = vec![Block::Embedded(EmbeddedContent::HorizontalRule)];
        assert_eq!(blocks_to_markdown(&blocks), "---\n");
    }

    #[test]
    fn table_uses_markdown_cache_when_present() {
        let blocks = vec![Block::Embedded(EmbeddedContent::Table {
            markdown_cache: Some("| a | b |\n| --- | --- |\n| 1 | 2 |".into()),
            rows: vec![],
        })];
        assert_eq!(
            blocks_to_markdown(&blocks),
            "| a | b |\n| --- | --- |\n| 1 | 2 |\n"
        );
    }

    #[test]
    fn table_reconstructs_from_rows_when_no_cache() {
        let cell = |text: &str, header: bool| TableCell {
            spans: vec![Span {
                text: text.into(),
                ..Default::default()
            }],
            header,
        };
        let blocks = vec![Block::Embedded(EmbeddedContent::Table {
            markdown_cache: None,
            rows: vec![
                vec![cell("Name", true), cell("Val", true)],
                vec![cell("foo", false), cell("bar", false)],
            ],
        })];
        assert_eq!(
            blocks_to_markdown(&blocks),
            "| Name | Val |\n| --- | --- |\n| foo | bar |\n"
        );
    }

    #[test]
    fn table_reconstruct_uses_flagged_header_row() {
        let cell = |text: &str, header: bool| TableCell {
            spans: vec![Span {
                text: text.into(),
                ..Default::default()
            }],
            header,
        };
        let blocks = vec![Block::Embedded(EmbeddedContent::Table {
            markdown_cache: None,
            rows: vec![
                vec![cell("r1c1", false), cell("r1c2", false)],
                vec![cell("h1", true), cell("h2", true)],
                vec![cell("r3c1", false), cell("r3c2", false)],
            ],
        })];
        assert_eq!(
            blocks_to_markdown(&blocks),
            "| h1 | h2 |\n| --- | --- |\n| r1c1 | r1c2 |\n| r3c1 | r3c2 |\n"
        );
    }

    #[test]
    fn table_reconstruct_without_header_flags_emits_empty_header_row() {
        let cell = |text: &str| TableCell {
            spans: vec![Span {
                text: text.into(),
                ..Default::default()
            }],
            header: false,
        };
        let blocks = vec![Block::Embedded(EmbeddedContent::Table {
            markdown_cache: None,
            rows: vec![vec![cell("a"), cell("b")]],
        })];
        assert_eq!(
            blocks_to_markdown(&blocks),
            "|  |  |\n| --- | --- |\n| a | b |\n"
        );
    }

    #[test]
    fn unknown_embedded_omitted() {
        let blocks = vec![Block::Embedded(EmbeddedContent::Unknown)];
        assert_eq!(blocks_to_markdown(&blocks), "");
    }
}
