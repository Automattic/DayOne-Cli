use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use comrak::nodes::{AstNode, ListType, NodeValue};
use comrak::{Arena, Options, parse_document};

use super::model::*;

/// Parses `![](dayone-moment:...)` image URLs into embedded media. Supports legacy
/// `dayone-moment://<id>` (photo) and typed `dayone-moment:/<type>/<id>` placeholders.
fn parse_dayone_moment_embed(url: &str) -> Option<EmbeddedContent> {
    if let Some(rest) = url.strip_prefix("dayone-moment://") {
        if rest.is_empty() {
            return None;
        }
        return Some(EmbeddedContent::Media {
            kind: MediaKind::Photo,
            identifier: rest.to_owned(),
        });
    }
    let rest = url.strip_prefix("dayone-moment:/")?;
    let (kind_str, identifier) = rest.split_once('/')?;
    if identifier.is_empty() {
        return None;
    }
    let kind = match kind_str {
        "photo" => MediaKind::Photo,
        "video" => MediaKind::Video,
        "audio" => MediaKind::Audio,
        "pdfAttachment" => MediaKind::Pdf,
        _ => return None,
    };
    Some(EmbeddedContent::Media {
        kind,
        identifier: identifier.to_owned(),
    })
}

fn parse_dayone_external_embed(url: &str) -> Option<EmbeddedContent> {
    let rest = url.strip_prefix("dayone-external:/")?;
    let (kind_str, encoded_url) = rest.split_once('/')?;
    if encoded_url.is_empty() {
        return None;
    }
    let decoded_bytes = URL_SAFE_NO_PAD.decode(encoded_url).ok()?;
    let decoded_url = String::from_utf8(decoded_bytes).ok()?;
    let kind = match kind_str {
        "video" => ExternalMediaKind::Video,
        "audio" => ExternalMediaKind::Audio,
        _ => return None,
    };
    Some(EmbeddedContent::ExternalMedia {
        kind,
        url: decoded_url,
    })
}

pub fn markdown_to_rtjson(markdown: &str) -> ConversionResult {
    let blocks = markdown_to_blocks(markdown);
    let (document, has_tables) = blocks_to_rtjson(blocks);
    ConversionResult {
        document,
        has_tables,
    }
}

pub(crate) fn markdown_to_blocks(markdown: &str) -> Vec<Block> {
    let arena = Arena::new();
    let mut opts = Options::default();
    opts.extension.strikethrough = true;
    opts.extension.table = true;
    opts.extension.tasklist = true;
    opts.extension.autolink = true;
    opts.extension.highlight = true;
    let root = parse_document(&arena, markdown, &opts);
    let mut blocks: Vec<Block> = Vec::new();
    walk_node(root, &mut blocks, &WalkContext::default());
    blocks
}

#[derive(Default, Clone)]
struct WalkContext {
    list_style: Option<ListStyle>,
    list_index: Option<u32>,
    indent_level: u32,
    quote: bool,
    heading: Option<u8>,
}

fn walk_node<'a>(node: &'a AstNode<'a>, blocks: &mut Vec<Block>, ctx: &WalkContext) {
    match &node.data.borrow().value {
        NodeValue::Document => {
            let mut prev_was_paragraph = false;
            for child in node.children() {
                let is_paragraph = matches!(child.data.borrow().value, NodeValue::Paragraph);
                let before = blocks.len();
                walk_node(child, blocks, ctx);
                let after = blocks.len();
                // Insert a blank line between consecutive paragraphs that both produced output,
                // including embedded-only paragraphs.
                if prev_was_paragraph && is_paragraph && after > before {
                    blocks.insert(before, Block::Text(TextLine::default()));
                }
                if after > before {
                    prev_was_paragraph = is_paragraph;
                } else {
                    prev_was_paragraph = false;
                }
            }
        }
        NodeValue::Paragraph => {
            // Single-image paragraph: dayone-moment embeds become media blocks; other images
            // fall through to inline collection or literal fallback below.
            let children: Vec<_> = node.children().collect();
            if children.len() == 1
                && let NodeValue::Image(link) = &children[0].data.borrow().value
            {
                if let Some(emb) = parse_dayone_moment_embed(&link.url) {
                    blocks.push(Block::Embedded(emb));
                    return;
                }
                if let Some(emb) = parse_dayone_external_embed(&link.url) {
                    blocks.push(Block::Embedded(emb));
                    return;
                }
            }
            let mut spans: Vec<Span> = Vec::new();
            collect_inline_spans(node, &mut spans);
            if !spans.is_empty() {
                blocks.push(Block::Text(TextLine {
                    line_attrs: RtjLineAttrs {
                        list_style: ctx.list_style.clone(),
                        list_index: ctx.list_index,
                        indent_level: ctx.indent_level,
                        quote: ctx.quote,
                        header: ctx.heading,
                        ..Default::default()
                    },
                    spans,
                }));
            } else if children.len() == 1
                && let NodeValue::Image(link) = &children[0].data.borrow().value
            {
                blocks.push(Block::Text(TextLine {
                    line_attrs: RtjLineAttrs {
                        list_style: ctx.list_style.clone(),
                        list_index: ctx.list_index,
                        indent_level: ctx.indent_level,
                        quote: ctx.quote,
                        header: ctx.heading,
                        ..Default::default()
                    },
                    spans: vec![Span {
                        text: format!("![]({})", link.url),
                        ..Default::default()
                    }],
                }));
            }
        }
        NodeValue::Heading(h) => {
            let mut spans: Vec<Span> = Vec::new();
            collect_inline_spans(node, &mut spans);
            if !spans.is_empty() {
                blocks.push(Block::Text(TextLine {
                    line_attrs: RtjLineAttrs {
                        list_style: ctx.list_style.clone(),
                        list_index: ctx.list_index,
                        indent_level: ctx.indent_level,
                        quote: ctx.quote,
                        header: Some(h.level),
                        ..Default::default()
                    },
                    spans,
                }));
            }
        }
        NodeValue::BlockQuote => {
            let mut child_ctx = ctx.clone();
            child_ctx.quote = true;
            for child in node.children() {
                walk_node(child, blocks, &child_ctx);
            }
        }
        NodeValue::List(list) => {
            let style = match list.list_type {
                ListType::Bullet => ListStyle::Bulleted,
                ListType::Ordered => ListStyle::Numbered,
            };
            let mut child_ctx = ctx.clone();
            child_ctx.list_style = Some(style);
            child_ctx.list_index = None;
            child_ctx.indent_level = ctx.indent_level + 1;
            for (index, child) in node.children().enumerate() {
                let mut item_ctx = child_ctx.clone();
                if list.list_type == ListType::Ordered {
                    item_ctx.list_index = Some((list.start + index) as u32);
                }
                walk_node(child, blocks, &item_ctx);
            }
        }
        NodeValue::Item(_) => {
            for child in node.children() {
                walk_node(child, blocks, ctx);
            }
        }
        NodeValue::TaskItem(task) => {
            let checked = task.symbol.is_some();
            // Collect inline content from children (the paragraph inside the task item)
            let mut spans: Vec<Span> = Vec::new();
            for child in node.children() {
                collect_inline_spans(child, &mut spans);
            }
            if !spans.is_empty() {
                blocks.push(Block::Text(TextLine {
                    line_attrs: RtjLineAttrs {
                        list_style: Some(ListStyle::Checkbox),
                        list_index: ctx.list_index,
                        indent_level: ctx.indent_level,
                        checked: Some(checked),
                        quote: ctx.quote,
                        header: ctx.heading,
                        ..Default::default()
                    },
                    spans,
                }));
            }
        }
        NodeValue::CodeBlock(cb) => {
            if cb.literal.is_empty() {
                blocks.push(Block::Text(TextLine {
                    line_attrs: RtjLineAttrs {
                        code_block: true,
                        ..Default::default()
                    },
                    spans: vec![Span {
                        text: String::new(),
                        ..Default::default()
                    }],
                }));
            } else {
                for line in cb.literal.lines() {
                    blocks.push(Block::Text(TextLine {
                        line_attrs: RtjLineAttrs {
                            code_block: true,
                            ..Default::default()
                        },
                        spans: vec![Span {
                            text: line.to_owned(),
                            ..Default::default()
                        }],
                    }));
                }
            }
        }
        NodeValue::ThematicBreak => {
            blocks.push(Block::Embedded(EmbeddedContent::HorizontalRule));
        }
        NodeValue::Table(_) => {
            // Reconstruct markdown source for the cache field
            let md_cache = render_table_to_markdown(node);
            let rows = collect_table_rows(node);
            blocks.push(Block::Embedded(EmbeddedContent::Table {
                markdown_cache: Some(md_cache),
                rows,
            }));
        }
        NodeValue::HtmlBlock(html) => {
            let text = html.literal.clone();
            if !text.is_empty() {
                for html_line in text.lines() {
                    blocks.push(Block::Text(TextLine {
                        line_attrs: RtjLineAttrs {
                            list_style: ctx.list_style.clone(),
                            indent_level: ctx.indent_level,
                            quote: ctx.quote,
                            header: ctx.heading,
                            ..Default::default()
                        },
                        spans: vec![Span {
                            text: html_line.to_owned(),
                            ..Default::default()
                        }],
                    }));
                }
            }
        }
        _ => {
            for child in node.children() {
                walk_node(child, blocks, ctx);
            }
        }
    }
}

/// Collect inline spans from inline nodes (Text, Strong, Emph, etc.)
fn collect_inline_spans<'a>(node: &'a AstNode<'a>, spans: &mut Vec<Span>) {
    collect_inline_spans_with_attrs(node, spans, &InlineAttrs::default());
}

#[derive(Default, Clone)]
struct InlineAttrs {
    bold: bool,
    italic: bool,
    strikethrough: bool,
    inline_code: bool,
    highlight: bool,
    link_url: Option<String>,
    autolink: bool,
}

fn collect_inline_spans_with_attrs<'a>(
    node: &'a AstNode<'a>,
    spans: &mut Vec<Span>,
    attrs: &InlineAttrs,
) {
    match &node.data.borrow().value {
        NodeValue::Text(t) => {
            if !t.is_empty() {
                spans.push(Span {
                    text: t.to_string(),
                    bold: attrs.bold,
                    italic: attrs.italic,
                    strikethrough: attrs.strikethrough,
                    inline_code: attrs.inline_code,
                    link_url: attrs.link_url.clone(),
                    autolink: attrs.autolink,
                    highlight: attrs.highlight,
                });
            }
        }
        NodeValue::Code(c) => {
            spans.push(Span {
                text: c.literal.clone(),
                bold: attrs.bold,
                italic: attrs.italic,
                strikethrough: attrs.strikethrough,
                inline_code: true,
                link_url: attrs.link_url.clone(),
                autolink: attrs.autolink,
                highlight: attrs.highlight,
            });
        }
        NodeValue::Strong => {
            let mut child_attrs = attrs.clone();
            child_attrs.bold = true;
            for child in node.children() {
                collect_inline_spans_with_attrs(child, spans, &child_attrs);
            }
        }
        NodeValue::Emph => {
            let mut child_attrs = attrs.clone();
            child_attrs.italic = true;
            for child in node.children() {
                collect_inline_spans_with_attrs(child, spans, &child_attrs);
            }
        }
        NodeValue::Strikethrough => {
            let mut child_attrs = attrs.clone();
            child_attrs.strikethrough = true;
            for child in node.children() {
                collect_inline_spans_with_attrs(child, spans, &child_attrs);
            }
        }
        NodeValue::Highlight => {
            let mut child_attrs = attrs.clone();
            child_attrs.highlight = true;
            for child in node.children() {
                collect_inline_spans_with_attrs(child, spans, &child_attrs);
            }
        }
        NodeValue::Link(link) => {
            let url = link.url.clone();
            // Collect child text to detect autolinks (<URL>)
            let mut child_spans: Vec<Span> = Vec::new();
            let mut plain_attrs = attrs.clone();
            plain_attrs.link_url = None;
            for child in node.children() {
                collect_inline_spans_with_attrs(child, &mut child_spans, &plain_attrs);
            }
            // An autolink is a link where the link text equals the URL
            let child_text: String = child_spans.iter().map(|s| s.text.as_str()).collect();
            if child_text == url {
                // Emit as autolink
                spans.push(Span {
                    text: url,
                    autolink: true,
                    bold: attrs.bold,
                    italic: attrs.italic,
                    strikethrough: attrs.strikethrough,
                    inline_code: attrs.inline_code,
                    highlight: attrs.highlight,
                    link_url: None,
                });
            } else {
                // Regular link — re-emit child spans with link_url
                let mut child_attrs = attrs.clone();
                child_attrs.link_url = Some(url);
                for child in node.children() {
                    collect_inline_spans_with_attrs(child, spans, &child_attrs);
                }
            }
        }
        NodeValue::Image(_link) => {
            // Inline images are preserved as literal markdown spans so mixed-content
            // paragraphs don't silently drop image content.
            if let NodeValue::Image(link) = &node.data.borrow().value {
                spans.push(Span {
                    text: format!("![]({})", link.url),
                    bold: attrs.bold,
                    italic: attrs.italic,
                    strikethrough: attrs.strikethrough,
                    inline_code: attrs.inline_code,
                    link_url: attrs.link_url.clone(),
                    autolink: attrs.autolink,
                    highlight: attrs.highlight,
                });
            }
        }
        NodeValue::HtmlInline(html) => {
            if !html.is_empty() {
                spans.push(Span {
                    text: html.to_string(),
                    bold: attrs.bold,
                    italic: attrs.italic,
                    strikethrough: attrs.strikethrough,
                    inline_code: attrs.inline_code,
                    link_url: attrs.link_url.clone(),
                    autolink: attrs.autolink,
                    highlight: attrs.highlight,
                });
            }
        }
        NodeValue::SoftBreak => {
            if !matches!(spans.last(), Some(Span { text, .. }) if text.ends_with(' ')) {
                spans.push(Span {
                    text: " ".into(),
                    bold: attrs.bold,
                    italic: attrs.italic,
                    strikethrough: attrs.strikethrough,
                    inline_code: attrs.inline_code,
                    link_url: attrs.link_url.clone(),
                    autolink: attrs.autolink,
                    highlight: attrs.highlight,
                });
            }
        }
        NodeValue::LineBreak => {
            spans.push(Span {
                text: "\n".into(),
                bold: attrs.bold,
                italic: attrs.italic,
                strikethrough: attrs.strikethrough,
                inline_code: attrs.inline_code,
                link_url: attrs.link_url.clone(),
                autolink: attrs.autolink,
                highlight: attrs.highlight,
            });
        }
        _ => {
            for child in node.children() {
                collect_inline_spans_with_attrs(child, spans, attrs);
            }
        }
    }
}

fn collect_table_rows<'a>(table_node: &'a AstNode<'a>) -> Vec<Vec<TableCell>> {
    table_node
        .children()
        .map(|row_node| {
            let header = match &row_node.data.borrow().value {
                NodeValue::TableRow(h) => *h,
                _ => false,
            };
            row_node
                .children()
                .map(|cell_node| {
                    let mut spans = Vec::new();
                    collect_inline_spans(cell_node, &mut spans);
                    TableCell { spans, header }
                })
                .collect()
        })
        .collect()
}

fn render_table_to_markdown<'a>(table_node: &'a AstNode<'a>) -> String {
    let rows = collect_table_rows(table_node);
    use super::to_markdown::blocks_to_markdown;
    let block = Block::Embedded(EmbeddedContent::Table {
        markdown_cache: None,
        rows,
    });
    let md = blocks_to_markdown(&[block]);
    md.trim_end_matches('\n').to_owned()
}

pub(crate) fn blocks_to_rtjson(blocks: Vec<Block>) -> (RtjDocument, bool) {
    let mut contents: Vec<RtjNode> = Vec::new();
    let mut has_tables = false;
    let mut i = 0;

    while i < blocks.len() {
        match &blocks[i] {
            Block::Text(line) if line.is_blank() => {
                // Blank line: append \n to the text of the previous text node
                if let Some(RtjNode::Text(prev)) = contents.last_mut() {
                    prev.text.push('\n');
                } else {
                    contents.push(RtjNode::Text(RtjTextNode {
                        text: "\n".to_owned(),
                        attributes: None,
                    }));
                }
                i += 1;
            }
            Block::Text(line) => {
                let nodes = text_line_to_rtjson_nodes(line);
                contents.extend(nodes);
                i += 1;
            }
            Block::Embedded(emb) => {
                let obj = embedded_to_rtjson_value(emb, &mut has_tables);
                if let Some(obj) = obj {
                    contents.push(RtjNode::Embedded(RtjEmbeddedNode {
                        embedded_objects: vec![obj],
                        attributes: None,
                    }));
                }
                i += 1;
            }
        }
    }

    let meta = serde_json::json!({
        "version": 1,
        "created": {
            "platform": "dayone-cli",
            "version": current_epoch_seconds()
        },
        "small-lines-removed": true
    });

    (
        RtjDocument {
            meta: Some(meta),
            contents,
        },
        has_tables,
    )
}

fn current_epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn text_line_to_rtjson_nodes(line: &TextLine) -> Vec<RtjNode> {
    let mut nodes: Vec<RtjNode> = Vec::new();
    let span_count = line.spans.len();

    for (idx, span) in line.spans.iter().enumerate() {
        let is_last = idx == span_count - 1;
        let text = if is_last {
            format!("{}\n", span.text)
        } else {
            span.text.clone()
        };

        let mut attrs = RtjTextAttrs {
            bold: span.bold,
            italic: span.italic,
            strikethrough: span.strikethrough,
            inline_code: span.inline_code,
            autolink: span.autolink,
            link_url: span.link_url.clone(),
            ..Default::default()
        };
        if span.highlight {
            attrs.highlighted_color = Some(DEFAULT_HIGHLIGHT_COLOR.to_owned());
        }

        // Propagate structural line attrs to every node in the line.
        let la = &line.line_attrs;
        if la.header.is_some() || la.list_style.is_some() || la.quote || la.code_block {
            attrs.line = Some(la.clone());
        }

        let has_any_attr = attrs.bold
            || attrs.italic
            || attrs.strikethrough
            || attrs.inline_code
            || attrs.autolink
            || attrs.link_url.is_some()
            || attrs.highlighted_color.is_some()
            || attrs.line.is_some();

        nodes.push(RtjNode::Text(RtjTextNode {
            text,
            attributes: if has_any_attr { Some(attrs) } else { None },
        }));
    }

    // Empty line (no spans) produces a single empty text node
    if nodes.is_empty() {
        nodes.push(RtjNode::Text(RtjTextNode {
            text: "\n".into(),
            attributes: None,
        }));
    }

    nodes
}

fn embedded_to_rtjson_value(
    emb: &EmbeddedContent,
    has_tables: &mut bool,
) -> Option<serde_json::Value> {
    use serde_json::json;
    match emb {
        EmbeddedContent::Media { kind, identifier } => {
            let type_str = match kind {
                MediaKind::Photo => "photo",
                MediaKind::Video => "video",
                MediaKind::Audio => "audio",
                MediaKind::Pdf => "pdfAttachment",
            };
            Some(json!({ "type": type_str, "identifier": identifier }))
        }
        EmbeddedContent::ExternalMedia { kind, url } => {
            let type_str = match kind {
                ExternalMediaKind::Video => "externalVideo",
                ExternalMediaKind::Audio => "externalAudio",
            };
            Some(json!({ "type": type_str, "url": url }))
        }
        EmbeddedContent::HorizontalRule => Some(json!({ "type": "horizontalLineRule" })),
        EmbeddedContent::Table {
            markdown_cache,
            rows,
        } => {
            *has_tables = true;
            let rows_val: Vec<Vec<serde_json::Value>> = rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| {
                            let content: Vec<serde_json::Value> = cell
                                .spans
                                .iter()
                                .map(|s| {
                                    let mut attrs = serde_json::Map::new();
                                    if s.bold {
                                        attrs.insert("bold".into(), json!(true));
                                    }
                                    if s.italic {
                                        attrs.insert("italic".into(), json!(true));
                                    }
                                    if s.strikethrough {
                                        attrs.insert("strikethrough".into(), json!(true));
                                    }
                                    if s.inline_code {
                                        attrs.insert("inlineCode".into(), json!(true));
                                    }
                                    if s.highlight {
                                        attrs.insert(
                                            "highlightedColor".into(),
                                            json!(DEFAULT_HIGHLIGHT_COLOR),
                                        );
                                    }
                                    if let Some(url) = &s.link_url {
                                        attrs.insert("linkURL".into(), json!(url));
                                    }
                                    if s.autolink {
                                        attrs.insert("autolink".into(), json!(true));
                                    }
                                    if attrs.is_empty() {
                                        json!({ "text": s.text })
                                    } else {
                                        json!({ "text": s.text, "attributes": attrs })
                                    }
                                })
                                .collect();
                            let mut cell_val = json!({ "content": content });
                            if cell.header {
                                cell_val["header"] = json!(true);
                            }
                            cell_val
                        })
                        .collect()
                })
                .collect();
            let mut table = json!({ "type": "table", "rows": rows_val });
            if let Some(md) = markdown_cache {
                table["markdown"] = json!(md);
            }
            Some(table)
        }
        EmbeddedContent::Unknown => None,
    }
}

// ── tests ──────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    fn first_text(blocks: &[Block]) -> &TextLine {
        match &blocks[0] {
            Block::Text(l) => l,
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn plain_paragraph() {
        let blocks = markdown_to_blocks("hello world");
        let line = first_text(&blocks);
        assert_eq!(line.spans[0].text, "hello world");
        assert!(!line.spans[0].bold);
    }

    #[test]
    fn heading_level_2() {
        let blocks = markdown_to_blocks("## Section");
        let line = first_text(&blocks);
        assert_eq!(line.line_attrs.header, Some(2));
        assert_eq!(line.spans[0].text, "Section");
    }

    #[test]
    fn heading_inside_blockquote_preserves_quote_context() {
        let blocks = markdown_to_blocks("> ## Section");
        let line = first_text(&blocks);
        assert_eq!(line.line_attrs.header, Some(2));
        assert!(line.line_attrs.quote);
    }

    #[test]
    fn bold_inline() {
        let blocks = markdown_to_blocks("**bold** text");
        let line = first_text(&blocks);
        assert!(line.spans[0].bold);
        assert_eq!(line.spans[0].text, "bold");
        assert!(!line.spans[1].bold);
    }

    #[test]
    fn italic_inline() {
        let blocks = markdown_to_blocks("_italic_");
        let line = first_text(&blocks);
        assert!(line.spans[0].italic);
    }

    #[test]
    fn strikethrough_inline() {
        let blocks = markdown_to_blocks("~~strike~~");
        let line = first_text(&blocks);
        assert!(line.spans[0].strikethrough);
    }

    #[test]
    fn inline_code() {
        let blocks = markdown_to_blocks("`code`");
        let line = first_text(&blocks);
        assert!(line.spans[0].inline_code);
        assert_eq!(line.spans[0].text, "code");
    }

    #[test]
    fn inline_code_preserves_parent_inline_attrs() {
        let blocks = markdown_to_blocks("**`code`**");
        let line = first_text(&blocks);
        assert_eq!(line.spans.len(), 1);
        assert!(line.spans[0].inline_code);
        assert!(line.spans[0].bold);
    }

    #[test]
    fn highlight_detected_in_text() {
        let blocks = markdown_to_blocks("before ==hi== after");
        let line = first_text(&blocks);
        let hi_span = line
            .spans
            .iter()
            .find(|s| s.highlight)
            .expect("no highlight span");
        assert_eq!(hi_span.text, "hi");
    }

    #[test]
    fn link() {
        let blocks = markdown_to_blocks("[click](https://x.com)");
        let line = first_text(&blocks);
        assert_eq!(line.spans[0].link_url.as_deref(), Some("https://x.com"));
        assert_eq!(line.spans[0].text, "click");
    }

    #[test]
    fn consecutive_paragraphs_preserve_blank_line_when_first_is_embed() {
        let blocks = markdown_to_blocks("![](dayone-moment://ABC)\n\nafter");
        assert_eq!(blocks.len(), 3);
        assert!(matches!(
            &blocks[0],
            Block::Embedded(EmbeddedContent::Media { identifier, .. }) if identifier == "ABC"
        ));
        let Block::Text(blank) = &blocks[1] else {
            panic!("expected blank line");
        };
        assert!(blank.is_blank());
        let line = first_text(&blocks[2..]);
        assert_eq!(line.spans[0].text, "after");
    }

    #[test]
    fn bulleted_list_item() {
        let blocks = markdown_to_blocks("- item");
        let line = first_text(&blocks);
        assert_eq!(line.line_attrs.list_style, Some(ListStyle::Bulleted));
        assert_eq!(line.line_attrs.indent_level, 1);
    }

    #[test]
    fn numbered_list_item() {
        let blocks = markdown_to_blocks("1. item");
        let line = first_text(&blocks);
        assert_eq!(line.line_attrs.list_style, Some(ListStyle::Numbered));
        assert_eq!(line.line_attrs.list_index, Some(1));
    }

    #[test]
    fn ordered_list_preserves_start_index() {
        let blocks = markdown_to_blocks("3. three\n4. four");
        let first = first_text(&blocks);
        assert_eq!(first.line_attrs.list_style, Some(ListStyle::Numbered));
        assert_eq!(first.line_attrs.list_index, Some(3));
        let second = first_text(&blocks[1..]);
        assert_eq!(second.line_attrs.list_index, Some(4));
    }

    #[test]
    fn checkbox_checked_and_unchecked() {
        let blocks = markdown_to_blocks("- [x] done\n- [ ] todo");
        let checked = first_text(&blocks);
        assert_eq!(checked.line_attrs.list_style, Some(ListStyle::Checkbox));
        assert_eq!(checked.line_attrs.checked, Some(true));
    }

    #[test]
    fn task_item_inside_blockquote_preserves_quote_context() {
        let blocks = markdown_to_blocks("> - [x] done");
        let line = first_text(&blocks);
        assert_eq!(line.line_attrs.list_style, Some(ListStyle::Checkbox));
        assert_eq!(line.line_attrs.checked, Some(true));
        assert!(line.line_attrs.quote);
    }

    #[test]
    fn blockquote() {
        let blocks = markdown_to_blocks("> quoted");
        let line = first_text(&blocks);
        assert!(line.line_attrs.quote);
    }

    #[test]
    fn soft_break_is_rendered_as_space_in_span_text() {
        let blocks = markdown_to_blocks("alpha\nbeta");
        let line = first_text(&blocks);
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].text, "alpha");
        assert_eq!(line.spans[1].text, " ");
        assert_eq!(line.spans[2].text, "beta");
    }

    #[test]
    fn soft_break_space_preserves_active_inline_attrs() {
        let blocks = markdown_to_blocks("**alpha\nbeta**");
        let line = first_text(&blocks);
        assert_eq!(line.spans.len(), 3);
        assert!(line.spans[0].bold);
        assert_eq!(line.spans[1].text, " ");
        assert!(line.spans[1].bold);
        assert!(line.spans[2].bold);
    }

    #[test]
    fn hard_line_break_is_preserved_as_newline_span() {
        let blocks = markdown_to_blocks("alpha\\\nbeta");
        let line = first_text(&blocks);
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].text, "alpha");
        assert_eq!(line.spans[1].text, "\n");
        assert_eq!(line.spans[2].text, "beta");
    }

    #[test]
    fn inline_html_is_preserved_as_text() {
        let blocks = markdown_to_blocks("before <span>x</span> after");
        let line = first_text(&blocks);
        let text: String = line.spans.iter().map(|s| s.text.as_str()).collect();
        assert!(text.contains("<span>x</span>"));
    }

    #[test]
    fn html_block_is_preserved_as_text() {
        let blocks = markdown_to_blocks("<div>hello</div>");
        let line = first_text(&blocks);
        assert_eq!(line.spans[0].text, "<div>hello</div>");
    }

    #[test]
    fn fenced_code_block() {
        let blocks = markdown_to_blocks("```\ncode here\n```");
        let line = first_text(&blocks);
        assert!(line.line_attrs.code_block);
        assert_eq!(line.spans[0].text, "code here");
    }

    #[test]
    fn empty_fenced_code_block_preserved_as_code_line() {
        let blocks = markdown_to_blocks("```\n```");
        assert_eq!(blocks.len(), 1);
        let line = first_text(&blocks);
        assert!(line.line_attrs.code_block);
        assert_eq!(line.spans.len(), 1);
        assert!(line.spans[0].text.is_empty());
    }

    #[test]
    fn thematic_break_becomes_horizontal_rule() {
        let blocks = markdown_to_blocks("---");
        assert!(matches!(
            blocks[0],
            Block::Embedded(EmbeddedContent::HorizontalRule)
        ));
    }

    #[test]
    fn dayone_moment_image_becomes_media_block() {
        let blocks = markdown_to_blocks("![](dayone-moment://ABC123)");
        assert!(matches!(
            &blocks[0],
            Block::Embedded(EmbeddedContent::Media {
                kind: MediaKind::Photo,
                identifier,
            }) if identifier == "ABC123"
        ));
    }

    #[test]
    fn dayone_moment_typed_path_sets_media_kind() {
        let blocks = markdown_to_blocks("![](dayone-moment:/video/V1)");
        assert!(matches!(
            &blocks[0],
            Block::Embedded(EmbeddedContent::Media {
                kind: MediaKind::Video,
                identifier,
            }) if identifier == "V1"
        ));
        let blocks = markdown_to_blocks("![](dayone-moment:/audio/A1)");
        assert!(matches!(
            &blocks[0],
            Block::Embedded(EmbeddedContent::Media {
                kind: MediaKind::Audio,
                identifier,
            }) if identifier == "A1"
        ));
    }

    #[test]
    fn dayone_moment_unknown_typed_path_stays_literal_text() {
        let blocks = markdown_to_blocks("![](dayone-moment:/mystery/X1)");
        let line = first_text(&blocks);
        assert_eq!(line.spans[0].text, "![](dayone-moment:/mystery/X1)");
    }

    #[test]
    fn unknown_image_paragraph_emits_literal_markdown_text() {
        let blocks = markdown_to_blocks("![](https://example.com/x.png)");
        let line = first_text(&blocks);
        assert_eq!(line.spans[0].text, "![](https://example.com/x.png)");
    }

    #[test]
    fn inline_image_with_surrounding_text_is_preserved() {
        let blocks = markdown_to_blocks("before ![](https://example.com/x.png) after");
        let line = first_text(&blocks);
        let text: String = line.spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "before ![](https://example.com/x.png) after");
    }

    #[test]
    fn external_audio_round_trips_rtjson_type() {
        let d: RtjDocument = serde_json::from_value(serde_json::json!({
            "contents": [{
                "embeddedObjects": [{ "type": "externalAudio", "url": "https://x.test/a" }]
            }]
        }))
        .unwrap();
        let blocks = crate::convert::to_markdown::rtjson_to_blocks(&d);
        let (out, _) = blocks_to_rtjson(blocks);
        let obj = match &out.contents[0] {
            RtjNode::Embedded(n) => &n.embedded_objects[0],
            _ => panic!("expected embedded"),
        };
        assert_eq!(
            obj.get("type").and_then(|v| v.as_str()),
            Some("externalAudio")
        );
        assert_eq!(
            obj.get("url").and_then(|v| v.as_str()),
            Some("https://x.test/a")
        );
    }

    #[test]
    fn dayone_external_typed_path_becomes_external_media_block() {
        let blocks = markdown_to_blocks("![](dayone-external:/audio/aHR0cHM6Ly94LnRlc3QvYQ)");
        assert!(matches!(
            &blocks[0],
            Block::Embedded(EmbeddedContent::ExternalMedia {
                kind: ExternalMediaKind::Audio,
                url,
            }) if url == "https://x.test/a"
        ));
    }

    #[test]
    fn gfm_table_becomes_table_block() {
        let md = "| a | b |\n| --- | --- |\n| 1 | 2 |";
        let blocks = markdown_to_blocks(md);
        let Block::Embedded(EmbeddedContent::Table {
            markdown_cache,
            rows,
        }) = &blocks[0]
        else {
            panic!("expected table block")
        };
        assert!(markdown_cache.is_some());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][0].spans[0].text, "1");
    }

    #[test]
    fn has_tables_flag_set_when_table_present() {
        let result = markdown_to_rtjson("| a |\n|---|\n| 1 |");
        assert!(result.has_tables);
    }

    #[test]
    fn has_tables_flag_false_when_no_table() {
        let result = markdown_to_rtjson("hello");
        assert!(!result.has_tables);
    }

    #[test]
    fn plain_text_block_to_rtjson_node() {
        let (doc, _) = blocks_to_rtjson(vec![Block::Text(TextLine {
            spans: vec![Span {
                text: "hello".into(),
                ..Default::default()
            }],
            ..Default::default()
        })]);
        assert_eq!(doc.contents.len(), 1);
        let RtjNode::Text(tn) = &doc.contents[0] else {
            panic!()
        };
        assert_eq!(tn.text, "hello\n");
    }

    #[test]
    fn bold_span_sets_bold_attr() {
        let (doc, _) = blocks_to_rtjson(vec![Block::Text(TextLine {
            spans: vec![Span {
                text: "hi".into(),
                bold: true,
                ..Default::default()
            }],
            ..Default::default()
        })]);
        let RtjNode::Text(tn) = &doc.contents[0] else {
            panic!()
        };
        assert!(tn.attributes.as_ref().unwrap().bold);
    }

    #[test]
    fn highlight_span_sets_highlighted_color() {
        let (doc, _) = blocks_to_rtjson(vec![Block::Text(TextLine {
            spans: vec![Span {
                text: "hi".into(),
                highlight: true,
                ..Default::default()
            }],
            ..Default::default()
        })]);
        let RtjNode::Text(tn) = &doc.contents[0] else {
            panic!()
        };
        let color = tn.attributes.as_ref().unwrap().highlighted_color.as_deref();
        assert_eq!(color, Some(DEFAULT_HIGHLIGHT_COLOR));
    }

    #[test]
    fn line_with_multiple_spans_produces_multiple_nodes() {
        let (doc, _) = blocks_to_rtjson(vec![Block::Text(TextLine {
            spans: vec![
                Span {
                    text: "a".into(),
                    bold: true,
                    ..Default::default()
                },
                Span {
                    text: "b".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        })]);
        // "a" gets its own node, "b\n" gets its own node
        assert_eq!(doc.contents.len(), 2);
    }

    #[test]
    fn line_attrs_applied_to_all_nodes_in_line() {
        let (doc, _) = blocks_to_rtjson(vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs {
                header: Some(1),
                ..Default::default()
            },
            spans: vec![
                Span {
                    text: "a".into(),
                    ..Default::default()
                },
                Span {
                    text: "b".into(),
                    ..Default::default()
                },
            ],
        })]);
        let RtjNode::Text(first) = &doc.contents[0] else {
            panic!()
        };
        assert_eq!(
            first
                .attributes
                .as_ref()
                .unwrap()
                .line
                .as_ref()
                .unwrap()
                .header,
            Some(1)
        );
        let RtjNode::Text(second) = &doc.contents[1] else {
            panic!()
        };
        // Second node gets \n and the same line attrs.
        assert!(
            second
                .attributes
                .as_ref()
                .and_then(|a| a.line.as_ref())
                .is_some()
        );
    }

    #[test]
    fn blank_line_produces_double_newline_on_previous_node() {
        let (doc, _) = blocks_to_rtjson(vec![
            Block::Text(TextLine {
                spans: vec![Span {
                    text: "para".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            Block::Text(TextLine::default()), // blank line
            Block::Text(TextLine {
                spans: vec![Span {
                    text: "next".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
        ]);
        // The blank line should produce \n\n after "para"
        let RtjNode::Text(first) = &doc.contents[0] else {
            panic!()
        };
        assert!(
            first.text.ends_with("\n\n"),
            "expected \\n\\n, got {:?}",
            first.text
        );
    }

    #[test]
    fn blank_line_after_embedded_produces_newline_text_node() {
        let (doc, _) = blocks_to_rtjson(vec![
            Block::Embedded(EmbeddedContent::Media {
                kind: MediaKind::Photo,
                identifier: "XYZ".into(),
            }),
            Block::Text(TextLine::default()),
        ]);
        assert_eq!(doc.contents.len(), 2);
        let RtjNode::Text(newline) = &doc.contents[1] else {
            panic!("expected text node after embedded");
        };
        assert_eq!(newline.text, "\n");
    }

    #[test]
    fn photo_produces_embedded_node() {
        let (doc, _) = blocks_to_rtjson(vec![Block::Embedded(EmbeddedContent::Media {
            kind: MediaKind::Photo,
            identifier: "XYZ".into(),
        })]);
        let RtjNode::Embedded(en) = &doc.contents[0] else {
            panic!()
        };
        let obj = &en.embedded_objects[0];
        assert_eq!(obj.get("type").and_then(|v| v.as_str()), Some("photo"));
        assert_eq!(obj.get("identifier").and_then(|v| v.as_str()), Some("XYZ"));
    }

    #[test]
    fn table_produces_embedded_table_node() {
        let (doc, has_tables) = blocks_to_rtjson(vec![Block::Embedded(EmbeddedContent::Table {
            markdown_cache: Some("| a |\n| --- |\n| b |".into()),
            rows: vec![],
        })]);
        assert!(has_tables);
        let RtjNode::Embedded(en) = &doc.contents[0] else {
            panic!()
        };
        assert_eq!(
            en.embedded_objects[0].get("type").and_then(|v| v.as_str()),
            Some("table")
        );
        assert_eq!(
            en.embedded_objects[0]
                .get("markdown")
                .and_then(|v| v.as_str()),
            Some("| a |\n| --- |\n| b |")
        );
    }

    #[test]
    fn horizontal_rule_produces_embedded_node() {
        let (doc, _) = blocks_to_rtjson(vec![Block::Embedded(EmbeddedContent::HorizontalRule)]);
        let RtjNode::Embedded(en) = &doc.contents[0] else {
            panic!()
        };
        assert_eq!(
            en.embedded_objects[0].get("type").and_then(|v| v.as_str()),
            Some("horizontalLineRule")
        );
    }

    #[test]
    fn meta_created_version_is_epoch_seconds_like() {
        let (doc, _) = blocks_to_rtjson(vec![Block::Text(TextLine {
            spans: vec![Span {
                text: "x".into(),
                ..Default::default()
            }],
            ..Default::default()
        })]);
        let created_version = doc
            .meta
            .as_ref()
            .and_then(|m| m.get("created"))
            .and_then(|c| c.get("version"))
            .and_then(|v| v.as_u64())
            .expect("created.version should exist");
        assert!(created_version > 1_500_000_000);
    }
}
