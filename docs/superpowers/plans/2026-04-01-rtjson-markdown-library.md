# RTJson ↔ Markdown Conversion Library Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a TDD'd `src/convert/` module with `rtjson_to_markdown` and `markdown_to_rtjson` functions backed by 21 real RTJson fixture files, replacing the ad hoc Markdown handling in `entry_write.rs`.

**Architecture:** RTJson nodes are flattened into a `Vec<Block>` intermediate (text lines + embedded objects), then rendered to Markdown or serialized back to RTJson nodes. Comrak parses Markdown for the reverse direction; `==highlight==` is handled via comrak's native `highlight` extension (`opts.extension.highlight = true`), which emits `NodeValue::Highlight` nodes in the AST.

**Tech Stack:** Rust, comrak 0.51 (GFM tables + tasklist + strikethrough + autolink + highlight), serde_json

---

## File Map

| Action | Path | Responsibility |
|--------|------|----------------|
| Create | `src/convert/mod.rs` | Public API: `rtjson_to_markdown`, `markdown_to_rtjson`, `ConversionResult` |
| Create | `src/convert/model.rs` | `RtjDocument`, `RtjNode`, `RtjTextAttrs`, `RtjLineAttrs`, `Block`, `TextLine`, `EmbeddedContent`, `Span` |
| Create | `src/convert/to_markdown.rs` | `rtjson_to_blocks`, `blocks_to_markdown` |
| Create | `src/convert/from_markdown.rs` | `markdown_to_blocks`, `blocks_to_rtjson` |
| Create | `tests/fixtures/markdown/01.md` … `21.md` | Hand-authored expected Markdown for each RTJson fixture |
| Create | `tests/convert.rs` | Integration tests: fixture pairs + round-trips |
| Modify | `Cargo.toml` | Add `comrak = "0.51"` |
| Modify | `src/main.rs` | Add `mod convert;` |
| Modify | `src/commands/entry_write.rs` | Replace `append_markdown_text_nodes` / `parse_markdown_heading_line` with `markdown_to_rtjson` |
| Delete | `src/rich_text.rs` | Replaced by production types in `src/convert/model.rs` |

---

## Task 1: Scaffold — dependency, module files, mod declaration

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/main.rs`
- Create: `src/convert/mod.rs`
- Create: `src/convert/model.rs`
- Create: `src/convert/to_markdown.rs`
- Create: `src/convert/from_markdown.rs`

- [ ] **Step 1.1: Add comrak to Cargo.toml**

In `Cargo.toml`, add after the existing `clap` or similar dependency line:
```toml
comrak = "0.51"
```

- [ ] **Step 1.2: Create `src/convert/mod.rs`**

```rust
pub mod model;
mod to_markdown;
mod from_markdown;

pub use model::{ConversionResult, RtjDocument};
pub use to_markdown::rtjson_to_markdown;
pub use from_markdown::markdown_to_rtjson;
```

- [ ] **Step 1.3: Create `src/convert/model.rs`** (stubs only for now)

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Default highlight color applied when `==text==` is converted to RTJson.
/// Confirmed from fixture 08: highlightedColor = "ffc107cc" (amber).
pub const DEFAULT_HIGHLIGHT_COLOR: &str = "ffc107cc";

/// A parsed RTJson document.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RtjDocument {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    #[serde(alias = "nodes", default)]
    pub contents: Vec<RtjNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RtjNode {
    Embedded(RtjEmbeddedNode),
    Text(RtjTextNode),
    Unknown(Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RtjTextNode {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<RtjTextAttrs>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RtjTextAttrs {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strikethrough: bool,
    #[serde(default, rename = "inlineCode", skip_serializing_if = "std::ops::Not::not")]
    pub inline_code: bool,
    #[serde(default, rename = "highlightedColor", skip_serializing_if = "Option::is_none")]
    pub highlighted_color: Option<String>,
    #[serde(default, rename = "linkURL", skip_serializing_if = "Option::is_none")]
    pub link_url: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub autolink: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cursor_placement: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<RtjLineAttrs>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RtjLineAttrs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<u8>,
    #[serde(default, rename = "listStyle", skip_serializing_if = "Option::is_none")]
    pub list_style: Option<ListStyle>,
    #[serde(default, rename = "indentLevel", skip_serializing_if = "is_zero_u32")]
    pub indent_level: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(default, rename = "listIndex", skip_serializing_if = "Option::is_none")]
    pub list_index: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quote: bool,
    #[serde(default, rename = "codeBlock", skip_serializing_if = "std::ops::Not::not")]
    pub code_block: bool,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

fn is_zero_u32(v: &u32) -> bool { *v == 0 }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ListStyle {
    Bulleted,
    Numbered,
    Checkbox,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RtjEmbeddedNode {
    #[serde(rename = "embeddedObjects")]
    pub embedded_objects: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Value>,
}

// ── Intermediate Block model ──────────────────────────────────────────────────

/// A flattened block — the seam between RTJson and Markdown.
#[derive(Debug, Clone)]
pub enum Block {
    /// One logical line of text (may be empty = blank line).
    Text(TextLine),
    /// An embedded object (photo, table, horizontal rule, etc.).
    Embedded(EmbeddedContent),
}

#[derive(Debug, Clone, Default)]
pub struct TextLine {
    pub line_attrs: RtjLineAttrs,
    pub spans: Vec<Span>,
}

impl TextLine {
    pub fn is_blank(&self) -> bool {
        self.spans.is_empty()
            && self.line_attrs == RtjLineAttrs::default()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub inline_code: bool,
    pub highlight: bool,
    pub link_url: Option<String>,
    pub autolink: bool,
}

#[derive(Debug, Clone)]
pub enum EmbeddedContent {
    /// photo, video, audio, pdfAttachment
    Media { kind: MediaKind, identifier: String },
    /// externalVideo, externalAudio
    ExternalMedia { url: String },
    /// horizontalLineRule / horizontalRuleLine (both accepted)
    HorizontalRule,
    /// table with optional pre-rendered markdown cache
    Table { markdown_cache: Option<String>, rows: Vec<Vec<TableCell>> },
    /// Any other type — omitted from Markdown output
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MediaKind { Photo, Video, Audio, Pdf }

#[derive(Debug, Clone)]
pub struct TableCell {
    pub spans: Vec<Span>,
    pub header: bool,
}

/// Return value of `markdown_to_rtjson`.
pub struct ConversionResult {
    pub document: RtjDocument,
    /// `true` when the document contains at least one table node.
    /// Caller should set feature flag `0x20000` on the entry when this is true.
    pub has_tables: bool,
}
```

- [ ] **Step 1.4: Create `src/convert/to_markdown.rs`** (stub)

```rust
use super::model::{Block, RtjDocument};

pub fn rtjson_to_markdown(doc: &RtjDocument) -> String {
    let blocks = rtjson_to_blocks(doc);
    blocks_to_markdown(&blocks)
}

pub(crate) fn rtjson_to_blocks(_doc: &RtjDocument) -> Vec<Block> {
    vec![]
}

pub(crate) fn blocks_to_markdown(_blocks: &[Block]) -> String {
    String::new()
}
```

- [ ] **Step 1.5: Create `src/convert/from_markdown.rs`** (stub)

```rust
use super::model::{Block, ConversionResult, RtjDocument};

pub fn markdown_to_rtjson(markdown: &str) -> ConversionResult {
    let blocks = markdown_to_blocks(markdown);
    let (document, has_tables) = blocks_to_rtjson(blocks);
    ConversionResult { document, has_tables }
}

pub(crate) fn markdown_to_blocks(_markdown: &str) -> Vec<Block> {
    vec![]
}

pub(crate) fn blocks_to_rtjson(_blocks: Vec<Block>) -> (RtjDocument, bool) {
    (RtjDocument::default(), false)
}
```

- [ ] **Step 1.6: Add `mod convert` to `src/main.rs`**

Find the existing `mod` declarations near the top of `src/main.rs` and add:
```rust
mod convert;
```

- [ ] **Step 1.7: Verify it compiles**

```bash
cargo build 2>&1 | grep "^error"
```
Expected: no output (clean build).

- [ ] **Step 1.8: Commit**

```bash
git add Cargo.toml Cargo.lock src/convert/ src/main.rs
git commit -m "feat(convert): scaffold rtjson↔markdown conversion module"
```

---

## Task 2: Core types — deserialization tests

**Files:**
- Modify: `src/convert/model.rs` (add tests)

- [ ] **Step 2.1: Write deserialization tests in `src/convert/model.rs`**

Add at the bottom of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rtj_document_parses_contents_field() {
        let doc: RtjDocument = serde_json::from_value(json!({
            "contents": [{"text": "hello"}],
            "meta": {"version": 1}
        })).unwrap();
        assert_eq!(doc.contents.len(), 1);
        assert!(matches!(doc.contents[0], RtjNode::Text(_)));
    }

    #[test]
    fn rtj_document_accepts_nodes_alias() {
        let doc: RtjDocument = serde_json::from_value(json!({
            "nodes": [{"text": "hello"}]
        })).unwrap();
        assert_eq!(doc.contents.len(), 1);
    }

    #[test]
    fn rtj_text_attrs_parses_inline_formatting() {
        let node: RtjTextNode = serde_json::from_value(json!({
            "text": "hi",
            "attributes": {
                "bold": true,
                "italic": true,
                "strikethrough": true,
                "inlineCode": true,
                "highlightedColor": "ffc107cc",
                "linkURL": "https://example.com",
                "autolink": false
            }
        })).unwrap();
        let attrs = node.attributes.unwrap();
        assert!(attrs.bold);
        assert!(attrs.italic);
        assert!(attrs.strikethrough);
        assert!(attrs.inline_code);
        assert_eq!(attrs.highlighted_color.as_deref(), Some("ffc107cc"));
        assert_eq!(attrs.link_url.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn rtj_text_attrs_preserves_unknown_fields() {
        let node: RtjTextNode = serde_json::from_value(json!({
            "text": "hi",
            "attributes": {"bold": true, "pageLink": true, "unknownFuture": 42}
        })).unwrap();
        let attrs = node.attributes.unwrap();
        assert!(attrs.bold);
        assert_eq!(attrs.extra.get("pageLink"), Some(&json!(true)));
        assert_eq!(attrs.extra.get("unknownFuture"), Some(&json!(42)));
    }

    #[test]
    fn rtj_line_attrs_parses_all_types() {
        let attrs: RtjLineAttrs = serde_json::from_value(json!({
            "header": 2,
            "listStyle": "bulleted",
            "indentLevel": 1,
            "checked": false,
            "quote": true,
            "codeBlock": false
        })).unwrap();
        assert_eq!(attrs.header, Some(2));
        assert_eq!(attrs.list_style, Some(ListStyle::Bulleted));
        assert_eq!(attrs.indent_level, 1);
        assert_eq!(attrs.checked, Some(false));
        assert!(attrs.quote);
    }

    #[test]
    fn rtj_embedded_node_parsed() {
        let doc: RtjDocument = serde_json::from_value(json!({
            "contents": [
                {"embeddedObjects": [{"type": "photo", "identifier": "ABC123"}]}
            ]
        })).unwrap();
        assert!(matches!(doc.contents[0], RtjNode::Embedded(_)));
    }

    #[test]
    fn rtj_unknown_node_preserved() {
        let doc: RtjDocument = serde_json::from_value(json!({
            "contents": [{"unknownField": true}]
        })).unwrap();
        assert!(matches!(doc.contents[0], RtjNode::Unknown(_)));
    }
}
```

- [ ] **Step 2.2: Run tests**

```bash
cargo test convert::model 2>&1 | tail -5
```
Expected:
```
test convert::model::tests::rtj_document_parses_contents_field ... ok
test convert::model::tests::rtj_document_accepts_nodes_alias ... ok
...
test result: ok. 6 passed; 0 failed
```

- [ ] **Step 2.3: Commit**

```bash
git add src/convert/model.rs
git commit -m "feat(convert): core RTJson types with serde + tests"
```

---

## Task 3: RTJson → Blocks

**Files:**
- Modify: `src/convert/to_markdown.rs`

- [ ] **Step 3.1: Write failing unit tests for `rtjson_to_blocks`**

Add at the bottom of `src/convert/to_markdown.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::model::*;
    use serde_json::json;

    fn doc(contents: serde_json::Value) -> RtjDocument {
        serde_json::from_value(json!({"contents": contents})).unwrap()
    }

    #[test]
    fn plain_text_single_node_no_newline() {
        let d = doc(json!([{"text": "hello"}]));
        let blocks = rtjson_to_blocks(&d);
        assert_eq!(blocks.len(), 1);
        let Block::Text(line) = &blocks[0] else { panic!("expected text") };
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].text, "hello");
    }

    #[test]
    fn newline_in_text_creates_two_lines() {
        let d = doc(json!([{"text": "line1\nline2"}]));
        let blocks = rtjson_to_blocks(&d);
        assert_eq!(blocks.len(), 2);
        let Block::Text(l1) = &blocks[0] else { panic!() };
        let Block::Text(l2) = &blocks[1] else { panic!() };
        assert_eq!(l1.spans[0].text, "line1");
        assert_eq!(l2.spans[0].text, "line2");
    }

    #[test]
    fn double_newline_creates_blank_line() {
        let d = doc(json!([{"text": "para1\n\npara2"}]));
        let blocks = rtjson_to_blocks(&d);
        assert_eq!(blocks.len(), 3);
        let Block::Text(blank) = &blocks[1] else { panic!() };
        assert!(blank.is_blank());
    }

    #[test]
    fn line_attrs_captured_from_first_node_of_line() {
        let d = doc(json!([
            {"attributes": {"line": {"header": 1}}, "text": "Title\n"}
        ]));
        let blocks = rtjson_to_blocks(&d);
        assert_eq!(blocks.len(), 1);
        let Block::Text(line) = &blocks[0] else { panic!() };
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
        let Block::Text(line) = &blocks[0] else { panic!() };
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
        assert!(matches!(blocks[0], Block::Embedded(EmbeddedContent::Media { .. })));
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
            assert!(matches!(blocks[0], Block::Embedded(EmbeddedContent::HorizontalRule)));
        }
    }
}
```

- [ ] **Step 3.2: Run tests to confirm they fail**

```bash
cargo test convert::to_markdown::tests 2>&1 | grep "FAILED\|panicked" | head -5
```
Expected: failures (functions return empty vec).

- [ ] **Step 3.3: Implement `rtjson_to_blocks` and helpers**

Replace the body of `src/convert/to_markdown.rs` with:

```rust
use super::model::*;

pub fn rtjson_to_markdown(doc: &RtjDocument) -> String {
    let blocks = rtjson_to_blocks(doc);
    blocks_to_markdown(&blocks)
}

pub(crate) fn rtjson_to_blocks(doc: &RtjDocument) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut pending_spans: Vec<Span> = Vec::new();
    let mut pending_line_attrs: Option<RtjLineAttrs> = None;

    let flush = |blocks: &mut Vec<Block>, spans: &mut Vec<Span>, attrs: &mut Option<RtjLineAttrs>| {
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
                if pending_line_attrs.is_none() {
                    if let Some(a) = attrs {
                        if let Some(la) = &a.line {
                            pending_line_attrs = Some(la.clone());
                        }
                    }
                }
                let span_base = attrs_to_span_base(attrs);
                let mut remaining = tn.text.as_str();
                while !remaining.is_empty() {
                    match remaining.find('\n') {
                        Some(pos) => {
                            let segment = &remaining[..pos];
                            if !segment.is_empty() {
                                pending_spans.push(Span { text: segment.to_owned(), ..span_base.clone() });
                            }
                            flush(&mut blocks, &mut pending_spans, &mut pending_line_attrs);
                            remaining = &remaining[pos + 1..];
                        }
                        None => {
                            if !remaining.is_empty() {
                                pending_spans.push(Span { text: remaining.to_owned(), ..span_base.clone() });
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
    let Some(a) = attrs else { return Span::default() };
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
        "externalVideo" | "externalAudio" => {
            let url = obj.get("url")?.as_str()?.to_owned();
            Some(EmbeddedContent::ExternalMedia { url })
        }
        "horizontalLineRule" | "horizontalRuleLine" => Some(EmbeddedContent::HorizontalRule),
        "table" => {
            let markdown_cache = obj.get("markdown").and_then(|v| v.as_str()).map(|s| s.to_owned());
            let rows = parse_table_rows(obj.get("rows")?);
            Some(EmbeddedContent::Table { markdown_cache, rows })
        }
        _ => Some(EmbeddedContent::Unknown),
    }
}

fn parse_table_rows(rows_val: &serde_json::Value) -> Vec<Vec<TableCell>> {
    let Some(rows) = rows_val.as_array() else { return vec![] };
    rows.iter().map(|row| {
        let Some(cells) = row.as_array() else { return vec![] };
        cells.iter().map(|cell| {
            let header = cell.get("header").and_then(|v| v.as_bool()).unwrap_or(false);
            let spans = cell.get("content")
                .and_then(|c| c.as_array())
                .map(|content| {
                    content.iter().filter_map(|seg| {
                        let text = seg.get("text")?.as_str()?.to_owned();
                        if text.is_empty() { return None; }
                        let a = seg.get("attributes");
                        Some(Span {
                            text,
                            bold: a.and_then(|a| a.get("bold")).and_then(|v| v.as_bool()).unwrap_or(false),
                            italic: a.and_then(|a| a.get("italic")).and_then(|v| v.as_bool()).unwrap_or(false),
                            strikethrough: a.and_then(|a| a.get("strikethrough")).and_then(|v| v.as_bool()).unwrap_or(false),
                            inline_code: a.and_then(|a| a.get("inlineCode")).and_then(|v| v.as_bool()).unwrap_or(false),
                            highlight: a.and_then(|a| a.get("highlightedColor")).is_some(),
                            link_url: a.and_then(|a| a.get("linkURL")).and_then(|v| v.as_str()).map(|s| s.to_owned()),
                            autolink: a.and_then(|a| a.get("autolink")).and_then(|v| v.as_bool()).unwrap_or(false),
                        })
                    }).collect()
                })
                .unwrap_or_default();
            TableCell { spans, header }
        }).collect()
    }).collect()
}

pub(crate) fn blocks_to_markdown(_blocks: &[Block]) -> String {
    String::new() // implemented in Task 4
}

// ── tests ──────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    // (tests from Step 3.1 go here)
}
```

- [ ] **Step 3.4: Run tests**

```bash
cargo test convert::to_markdown::tests 2>&1 | tail -8
```
Expected: all 8 tests pass.

- [ ] **Step 3.5: Commit**

```bash
git add src/convert/to_markdown.rs
git commit -m "feat(convert): implement rtjson_to_blocks with unit tests"
```

---

## Task 4: Blocks → Markdown

**Files:**
- Modify: `src/convert/to_markdown.rs`

- [ ] **Step 4.1: Write failing unit tests for `blocks_to_markdown`**

Add to the `tests` module in `src/convert/to_markdown.rs`:

```rust
    #[test]
    fn plain_text_line_renders_with_newline() {
        let blocks = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs::default(),
            spans: vec![Span { text: "hello".into(), ..Default::default() }],
        })];
        assert_eq!(blocks_to_markdown(&blocks), "hello\n");
    }

    #[test]
    fn blank_line_renders_as_empty_line() {
        let blocks = vec![
            Block::Text(TextLine { spans: vec![Span { text: "a".into(), ..Default::default() }], ..Default::default() }),
            Block::Text(TextLine::default()),
            Block::Text(TextLine { spans: vec![Span { text: "b".into(), ..Default::default() }], ..Default::default() }),
        ];
        assert_eq!(blocks_to_markdown(&blocks), "a\n\nb\n");
    }

    #[test]
    fn heading_renders_with_hashes() {
        for (level, prefix) in [(1u8,"# "),(2,"## "),(3,"### ")] {
            let blocks = vec![Block::Text(TextLine {
                line_attrs: RtjLineAttrs { header: Some(level), ..Default::default() },
                spans: vec![Span { text: "Title".into(), ..Default::default() }],
            })];
            assert_eq!(blocks_to_markdown(&blocks), format!("{prefix}Title\n"));
        }
    }

    #[test]
    fn bulleted_list_indent_level_1_no_indent() {
        let blocks = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs { list_style: Some(ListStyle::Bulleted), indent_level: 1, ..Default::default() },
            spans: vec![Span { text: "item".into(), ..Default::default() }],
        })];
        assert_eq!(blocks_to_markdown(&blocks), "- item\n");
    }

    #[test]
    fn bulleted_list_indent_level_2_two_spaces() {
        let blocks = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs { list_style: Some(ListStyle::Bulleted), indent_level: 2, ..Default::default() },
            spans: vec![Span { text: "nested".into(), ..Default::default() }],
        })];
        assert_eq!(blocks_to_markdown(&blocks), "  - nested\n");
    }

    #[test]
    fn numbered_list_renders_1_dot() {
        let blocks = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs { list_style: Some(ListStyle::Numbered), indent_level: 1, ..Default::default() },
            spans: vec![Span { text: "item".into(), ..Default::default() }],
        })];
        assert_eq!(blocks_to_markdown(&blocks), "1. item\n");
    }

    #[test]
    fn checkbox_unchecked_and_checked() {
        let mk = |checked: bool| Block::Text(TextLine {
            line_attrs: RtjLineAttrs {
                list_style: Some(ListStyle::Checkbox),
                indent_level: 1,
                checked: Some(checked),
                ..Default::default()
            },
            spans: vec![Span { text: "task".into(), ..Default::default() }],
        });
        assert_eq!(blocks_to_markdown(&[mk(false)]), "- [ ] task\n");
        assert_eq!(blocks_to_markdown(&[mk(true)]),  "- [x] task\n");
    }

    #[test]
    fn blockquote_renders_gt_prefix() {
        let blocks = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs { quote: true, ..Default::default() },
            spans: vec![Span { text: "quoted".into(), ..Default::default() }],
        })];
        assert_eq!(blocks_to_markdown(&blocks), "> quoted\n");
    }

    #[test]
    fn code_block_wrapped_in_fences() {
        let blocks = vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs { code_block: true, ..Default::default() },
            spans: vec![Span { text: "let x = 1;".into(), ..Default::default() }],
        })];
        assert_eq!(blocks_to_markdown(&blocks), "```\nlet x = 1;\n```\n");
    }

    #[test]
    fn consecutive_code_block_lines_share_fences() {
        let mk = |text: &str| Block::Text(TextLine {
            line_attrs: RtjLineAttrs { code_block: true, ..Default::default() },
            spans: vec![Span { text: text.into(), ..Default::default() }],
        });
        let blocks = vec![mk("line1"), mk("line2")];
        assert_eq!(blocks_to_markdown(&blocks), "```\nline1\nline2\n```\n");
    }

    #[test]
    fn inline_bold_italic_strikethrough_code_highlight() {
        let spans = vec![
            Span { text: "a".into(), bold: true, ..Default::default() },
            Span { text: "b".into(), italic: true, ..Default::default() },
            Span { text: "c".into(), bold: true, italic: true, ..Default::default() },
            Span { text: "d".into(), strikethrough: true, ..Default::default() },
            Span { text: "e".into(), inline_code: true, ..Default::default() },
            Span { text: "f".into(), highlight: true, ..Default::default() },
        ];
        let blocks = vec![Block::Text(TextLine { spans, ..Default::default() })];
        assert_eq!(blocks_to_markdown(&blocks), "**a**_b_**_c_**~~d~~`e`==f==\n");
    }

    #[test]
    fn link_and_autolink() {
        let blocks = vec![Block::Text(TextLine {
            spans: vec![
                Span { text: "click".into(), link_url: Some("https://x.com".into()), ..Default::default() },
                Span { text: "https://y.com".into(), autolink: true, ..Default::default() },
            ],
            ..Default::default()
        })];
        assert_eq!(blocks_to_markdown(&blocks), "[click](https://x.com)<https://y.com>\n");
    }

    #[test]
    fn media_renders_dayone_moment_url() {
        for kind in [MediaKind::Photo, MediaKind::Video, MediaKind::Audio, MediaKind::Pdf] {
            let blocks = vec![Block::Embedded(EmbeddedContent::Media { kind, identifier: "ABC".into() })];
            assert_eq!(blocks_to_markdown(&blocks), "![](dayone-moment://ABC)\n");
        }
    }

    #[test]
    fn external_media_renders_bare_url() {
        let blocks = vec![Block::Embedded(EmbeddedContent::ExternalMedia { url: "https://youtu.be/x".into() })];
        assert_eq!(blocks_to_markdown(&blocks), "https://youtu.be/x\n");
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
        assert_eq!(blocks_to_markdown(&blocks), "| a | b |\n| --- | --- |\n| 1 | 2 |\n");
    }

    #[test]
    fn table_reconstructs_from_rows_when_no_cache() {
        let cell = |text: &str, header: bool| TableCell {
            spans: vec![Span { text: text.into(), ..Default::default() }],
            header,
        };
        let blocks = vec![Block::Embedded(EmbeddedContent::Table {
            markdown_cache: None,
            rows: vec![
                vec![cell("Name", true), cell("Val", true)],
                vec![cell("foo", false), cell("bar", false)],
            ],
        })];
        assert_eq!(blocks_to_markdown(&blocks), "| Name | Val |\n| --- | --- |\n| foo | bar |\n");
    }

    #[test]
    fn unknown_embedded_omitted() {
        let blocks = vec![Block::Embedded(EmbeddedContent::Unknown)];
        assert_eq!(blocks_to_markdown(&blocks), "");
    }
```

- [ ] **Step 4.2: Run tests to confirm they fail**

```bash
cargo test convert::to_markdown::tests::plain_text_line 2>&1 | grep "FAILED\|error"
```
Expected: FAILED (returns empty string).

- [ ] **Step 4.3: Implement `blocks_to_markdown`**

Replace the `blocks_to_markdown` stub in `src/convert/to_markdown.rs`:

```rust
pub(crate) fn blocks_to_markdown(blocks: &[Block]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < blocks.len() {
        match &blocks[i] {
            Block::Text(line) if line.line_attrs.code_block => {
                out.push_str("```\n");
                while i < blocks.len() {
                    if let Block::Text(l) = &blocks[i] {
                        if l.line_attrs.code_block {
                            out.push_str(&spans_to_plain(&l.spans));
                            out.push('\n');
                            i += 1;
                            continue;
                        }
                    }
                    break;
                }
                out.push_str("```\n");
            }
            Block::Text(line) => {
                if line.is_blank() {
                    out.push('\n');
                } else {
                    out.push_str(&line_prefix(&line.line_attrs));
                    out.push_str(&spans_to_markdown(&line.spans));
                    out.push('\n');
                }
                i += 1;
            }
            Block::Embedded(emb) => {
                match emb {
                    EmbeddedContent::Media { identifier, .. } => {
                        out.push_str(&format!("![](dayone-moment://{})\n", identifier));
                    }
                    EmbeddedContent::ExternalMedia { url } => {
                        out.push_str(url);
                        out.push('\n');
                    }
                    EmbeddedContent::HorizontalRule => {
                        out.push_str("---\n");
                    }
                    EmbeddedContent::Table { markdown_cache, rows } => {
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
    // indent = (indent_level - 1) * 2 spaces; level 0 and 1 both get no indent
    let indent = if attrs.indent_level > 1 {
        " ".repeat(((attrs.indent_level - 1) as usize) * 2)
    } else {
        String::new()
    };
    match &attrs.list_style {
        Some(ListStyle::Bulleted) => format!("{}- ", indent),
        Some(ListStyle::Numbered) => format!("{}1. ", indent),
        Some(ListStyle::Checkbox) => {
            let mark = if attrs.checked == Some(true) { "x" } else { " " };
            format!("{}- [{}] ", indent, mark)
        }
        None if attrs.quote => "> ".to_owned(),
        None => match attrs.header {
            Some(n) => "#".repeat(n as usize) + " ",
            None => String::new(),
        },
    }
}

fn spans_to_markdown(spans: &[Span]) -> String {
    spans.iter().map(span_to_markdown).collect()
}

fn span_to_markdown(span: &Span) -> String {
    if span.text.is_empty() { return String::new(); }
    let mut s = span.text.clone();
    if span.inline_code { return format!("`{}`", s); }
    if span.autolink    { return format!("<{}>", s); }
    if let Some(url) = &span.link_url { return format!("[{}]({})", s, url); }
    if span.highlight    { s = format!("=={}==", s); }
    if span.strikethrough { s = format!("~~{}~~", s); }
    if span.bold && span.italic { s = format!("**_{}_{}", s) + "**"; }
    else if span.bold    { s = format!("**{}**", s); }
    else if span.italic  { s = format!("_{}_", s); }
    s
}

fn spans_to_plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

fn reconstruct_table_gfm(rows: &[Vec<TableCell>]) -> String {
    if rows.is_empty() { return String::new(); }
    let mut out = String::new();
    let col_count = rows[0].len();
    // Header row
    out.push('|');
    for cell in &rows[0] {
        out.push(' ');
        out.push_str(&spans_to_markdown(&cell.spans));
        out.push_str(" |");
    }
    out.push('\n');
    // Separator
    out.push('|');
    for _ in 0..col_count { out.push_str(" --- |"); }
    out.push('\n');
    // Data rows
    for row in rows.iter().skip(1) {
        out.push('|');
        for cell in row {
            out.push(' ');
            out.push_str(&spans_to_markdown(&cell.spans));
            out.push_str(" |");
        }
        out.push('\n');
    }
    out
}
```

- [ ] **Step 4.4: Run all to_markdown tests**

```bash
cargo test convert::to_markdown::tests 2>&1 | tail -8
```
Expected: all tests pass.

- [ ] **Step 4.5: Commit**

```bash
git add src/convert/to_markdown.rs
git commit -m "feat(convert): implement blocks_to_markdown with unit tests"
```

---

## Task 5: Author Markdown fixture files

**Files:**
- Create: `tests/fixtures/markdown/01.md` … `21.md`

These are the expected outputs of `rtjson_to_markdown` for each fixture. Author them based on the RTJson content, then verify against the implementation in Task 6.

- [ ] **Step 5.1: Create `tests/fixtures/markdown/` directory and write all 21 files**

`tests/fixtures/markdown/01.md`:
```
01 - This is plain text with a single paragraph
```

`tests/fixtures/markdown/02.md`:
```
02 - This is multiple paragraphs. The first one containing some words.

And the second paragraph containing yet more words.
```

`tests/fixtures/markdown/03.md`:
```
# 03 - Header one
```

`tests/fixtures/markdown/04.md`:
```
## 04 - Header two
```

`tests/fixtures/markdown/05.md`:
```
### 05 - Header three
```

`tests/fixtures/markdown/06.md`:
```
06 - **Bold**, _italic,_ and ~~strikethrough~~
```

`tests/fixtures/markdown/07.md`:
```
07 - `this is inline code`
```
And this is a codeblock
```
```

`tests/fixtures/markdown/08.md`:
```
08 - ==This text is highlighted==
```

`tests/fixtures/markdown/09.md`:
```
09 - An auto-link, which is also a normal link, to <https://swapi.dev/>
```

`tests/fixtures/markdown/10.md`:
```
10 -
- A
- Single
- Level
- Bulleted
- List
```

`tests/fixtures/markdown/11.md`:
```
11 -
- A
  - Bulleted
    - List
  - With
- indents
```

`tests/fixtures/markdown/12.md`:
```
12 -
1. A
1. Numbered
  - (With indent)
1. List
```

`tests/fixtures/markdown/13.md`:
```
13 -
- [ ] An unchecked item
- [x] A checked item
```

`tests/fixtures/markdown/14.md`:
```
14 -
> A block quote
```

`tests/fixtures/markdown/15.md`:
```
15 - A tiny embedded photo
![](dayone-moment://260A4B6206A74F67B93EB90DE9640DC2)
```

`tests/fixtures/markdown/16.md`:
```
16 - Video and audio
![](dayone-moment://716218DB71014D9E8297904B4118E277)
![](dayone-moment://36E1FA2368E343ADA09946ABDB5216D5)
```

`tests/fixtures/markdown/17.md`:
```
17 - PDF
![](dayone-moment://7ABB86054D994E939ACAA3828B57A270)
```

`tests/fixtures/markdown/18.md`:
```
# 18 - Embedded video
<https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=RDdQw4w9WgXcQ&start_radio=1>
```

`tests/fixtures/markdown/19.md`:
```
# 19 - Horizontal Rule
---
```

`tests/fixtures/markdown/20.md`:
```
# 20 - Table
| *Food* | Place |
| --- | --- |
| Pizza | **NYC** |
```

`tests/fixtures/markdown/21.md`:
```
# 21 - Mixed
- **bold** inside a list
```

**Important:** Every file must end with exactly one newline character (no trailing blank line). Check with:
```bash
for f in tests/fixtures/markdown/*.md; do
  last=$(tail -c1 "$f" | xxd | head -1)
  echo "$f: $last"
done
```
All files should end with `0a` (newline).

- [ ] **Step 5.2: Commit fixtures**

```bash
git add tests/fixtures/markdown/
git commit -m "feat(convert): add 21 hand-authored Markdown fixture files"
```

---

## Task 6: RTJson → Markdown integration tests

**Files:**
- Create: `tests/convert.rs`

- [ ] **Step 6.1: Create integration test file**

```rust
// tests/convert.rs
use dayone::convert::{rtjson_to_markdown, RtjDocument};

fn load_rtjson(name: &str) -> RtjDocument {
    let path = format!("tests/fixtures/rtjson/{}", name);
    let json = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path, e));
    serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("failed to parse {}: {}", path, e))
}

fn load_md(name: &str) -> String {
    let path = format!("tests/fixtures/markdown/{}", name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path, e))
}

macro_rules! rtjson_to_md_test {
    ($name:ident, $num:literal) => {
        #[test]
        fn $name() {
            let doc = load_rtjson(concat!($num, ".json"));
            let expected = load_md(concat!($num, ".md"));
            assert_eq!(rtjson_to_markdown(&doc), expected,
                "fixture {} Markdown mismatch", $num);
        }
    };
}

rtjson_to_md_test!(fixture_01_plain_text, "01");
rtjson_to_md_test!(fixture_02_multiple_paragraphs, "02");
rtjson_to_md_test!(fixture_03_header_one, "03");
rtjson_to_md_test!(fixture_04_header_two, "04");
rtjson_to_md_test!(fixture_05_header_three, "05");
rtjson_to_md_test!(fixture_06_inline_formatting, "06");
rtjson_to_md_test!(fixture_07_code, "07");
rtjson_to_md_test!(fixture_08_highlight, "08");
rtjson_to_md_test!(fixture_09_links, "09");
rtjson_to_md_test!(fixture_10_bulleted_list, "10");
rtjson_to_md_test!(fixture_11_nested_list, "11");
rtjson_to_md_test!(fixture_12_numbered_list, "12");
rtjson_to_md_test!(fixture_13_checkbox, "13");
rtjson_to_md_test!(fixture_14_blockquote, "14");
rtjson_to_md_test!(fixture_15_photo, "15");
rtjson_to_md_test!(fixture_16_video_audio, "16");
rtjson_to_md_test!(fixture_17_pdf, "17");
rtjson_to_md_test!(fixture_18_external_video, "18");
rtjson_to_md_test!(fixture_19_horizontal_rule, "19");
rtjson_to_md_test!(fixture_20_table, "20");
rtjson_to_md_test!(fixture_21_mixed, "21");
```

Also add `pub mod convert;` to `src/lib.rs` if it exists, or ensure the `dayone` crate exposes `convert`. Check with:
```bash
grep "^pub mod\|^mod" src/main.rs | head -10
```
If the crate is a binary (no `src/lib.rs`), the integration tests need access via `use dayone::...`. In that case, re-export from `src/main.rs` or move to `src/lib.rs`. Check the existing test pattern in `tests/` to see how other integration tests import — copy that pattern.

- [ ] **Step 6.2: Run all fixture tests**

```bash
cargo test --test convert 2>&1 | tail -15
```
Expected: all 21 pass. If any fail, check the diff between actual and expected output and fix either the fixture `.md` file or the conversion logic.

- [ ] **Step 6.3: Commit**

```bash
git add tests/convert.rs
git commit -m "feat(convert): RTJson→Markdown integration tests pass for all 21 fixtures"
```

---

## Task 7: Markdown → Blocks (comrak AST walker)

**Files:**
- Modify: `src/convert/from_markdown.rs`

- [ ] **Step 7.1: Write failing unit tests for `markdown_to_blocks`**

Add to `src/convert/from_markdown.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::model::*;

    fn first_text(blocks: &[Block]) -> &TextLine {
        match &blocks[0] { Block::Text(l) => l, _ => panic!("expected text") }
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
    fn highlight_detected_in_text() {
        let blocks = markdown_to_blocks("before ==hi== after");
        let line = first_text(&blocks);
        let hi_span = line.spans.iter().find(|s| s.highlight).expect("no highlight span");
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
    }

    #[test]
    fn checkbox_checked_and_unchecked() {
        let blocks = markdown_to_blocks("- [x] done\n- [ ] todo");
        let checked = first_text(&blocks);
        assert_eq!(checked.line_attrs.list_style, Some(ListStyle::Checkbox));
        assert_eq!(checked.line_attrs.checked, Some(true));
    }

    #[test]
    fn blockquote() {
        let blocks = markdown_to_blocks("> quoted");
        let line = first_text(&blocks);
        assert!(line.line_attrs.quote);
    }

    #[test]
    fn fenced_code_block() {
        let blocks = markdown_to_blocks("```\ncode here\n```");
        let line = first_text(&blocks);
        assert!(line.line_attrs.code_block);
        assert_eq!(line.spans[0].text, "code here");
    }

    #[test]
    fn thematic_break_becomes_horizontal_rule() {
        let blocks = markdown_to_blocks("---");
        assert!(matches!(blocks[0], Block::Embedded(EmbeddedContent::HorizontalRule)));
    }

    #[test]
    fn dayone_moment_image_becomes_media_block() {
        let blocks = markdown_to_blocks("![](dayone-moment://ABC123)");
        assert!(matches!(
            &blocks[0],
            Block::Embedded(EmbeddedContent::Media { identifier, .. }) if identifier == "ABC123"
        ));
    }

    #[test]
    fn gfm_table_becomes_table_block() {
        let md = "| a | b |\n| --- | --- |\n| 1 | 2 |";
        let blocks = markdown_to_blocks(md);
        let Block::Embedded(EmbeddedContent::Table { markdown_cache, rows }) = &blocks[0] else {
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
}
```

- [ ] **Step 7.2: Run tests to confirm they fail**

```bash
cargo test convert::from_markdown::tests 2>&1 | grep "FAILED" | head -5
```
Expected: failures.

- [ ] **Step 7.3: Implement `markdown_to_blocks` in `src/convert/from_markdown.rs`**

```rust
use comrak::{parse_document, Arena, Options};
use comrak::nodes::{AstNode, NodeValue, ListType};
use super::model::*;

pub fn markdown_to_rtjson(markdown: &str) -> ConversionResult {
    let blocks = markdown_to_blocks(markdown);
    let has_tables = blocks.iter().any(|b| matches!(b, Block::Embedded(EmbeddedContent::Table { .. })));
    let (document, _) = blocks_to_rtjson(blocks);
    ConversionResult { document, has_tables }
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
    indent_level: u32,
    quote: bool,
    heading: Option<u8>,
    code_block: bool,
}

fn walk_node<'a>(node: &'a AstNode<'a>, blocks: &mut Vec<Block>, ctx: &WalkContext) {
    match &node.data.borrow().value {
        NodeValue::Document => {
            for child in node.children() { walk_node(child, blocks, ctx); }
        }
        NodeValue::Paragraph => {
            let mut spans: Vec<Span> = Vec::new();
            collect_inline_spans(node, &mut spans);
            if !spans.is_empty() {
                blocks.push(Block::Text(TextLine {
                    line_attrs: RtjLineAttrs {
                        list_style: ctx.list_style.clone(),
                        indent_level: ctx.indent_level,
                        quote: ctx.quote,
                        header: ctx.heading,
                        ..Default::default()
                    },
                    spans,
                }));
            }
        }
        NodeValue::Heading(h) => {
            let mut child_ctx = ctx.clone();
            child_ctx.heading = Some(h.level);
            for child in node.children() { walk_node(child, blocks, &child_ctx); }
        }
        NodeValue::BlockQuote => {
            let mut child_ctx = ctx.clone();
            child_ctx.quote = true;
            for child in node.children() { walk_node(child, blocks, &child_ctx); }
        }
        NodeValue::List(list) => {
            let style = match list.list_type {
                ListType::Bullet => ListStyle::Bulleted,
                ListType::Ordered => ListStyle::Numbered,
            };
            let mut child_ctx = ctx.clone();
            child_ctx.list_style = Some(style);
            child_ctx.indent_level = ctx.indent_level + 1;
            for child in node.children() { walk_node(child, blocks, &child_ctx); }
        }
        NodeValue::Item(_) => {
            for child in node.children() { walk_node(child, blocks, ctx); }
        }
        NodeValue::TaskItem(task) => {
            let checked = task.symbol.is_some();
            // Collect inline content from children (the paragraph inside the task item)
            let mut spans: Vec<Span> = Vec::new();
            for child in node.children() { collect_inline_spans(child, &mut spans); }
            if !spans.is_empty() {
                blocks.push(Block::Text(TextLine {
                    line_attrs: RtjLineAttrs {
                        list_style: Some(ListStyle::Checkbox),
                        indent_level: ctx.indent_level,
                        checked: Some(checked),
                        ..Default::default()
                    },
                    spans,
                }));
            }
        }
        NodeValue::CodeBlock(cb) => {
            for line in cb.literal.lines() {
                blocks.push(Block::Text(TextLine {
                    line_attrs: RtjLineAttrs { code_block: true, ..Default::default() },
                    spans: vec![Span { text: line.to_owned(), ..Default::default() }],
                }));
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
        NodeValue::HtmlBlock(_) => {} // skip
        _ => {
            for child in node.children() { walk_node(child, blocks, ctx); }
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

fn collect_inline_spans_with_attrs<'a>(node: &'a AstNode<'a>, spans: &mut Vec<Span>, attrs: &InlineAttrs) {
    match &node.data.borrow().value {
        NodeValue::Text(t) => {
            if !t.is_empty() {
                spans.push(Span {
                    text: t.clone(),
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
                inline_code: true,
                ..Default::default()
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
            // Check if it's a dayone-moment URL — handled separately in walk_node
            let mut child_attrs = attrs.clone();
            child_attrs.link_url = Some(url);
            for child in node.children() {
                collect_inline_spans_with_attrs(child, spans, &child_attrs);
            }
        }
        NodeValue::Image(link) => {
            // dayone-moment:// images become EmbeddedContent — skip here,
            // handled in the Image branch of walk_node at paragraph level
        }
        NodeValue::SoftBreak | NodeValue::LineBreak => {
            spans.push(Span { text: "\n".into(), ..Default::default() });
        }
        _ => {
            for child in node.children() {
                collect_inline_spans_with_attrs(child, spans, attrs);
            }
        }
    }
}

/// Handle Image nodes that represent dayone-moment:// URLs at the paragraph level.
/// Called from `walk_node` for Paragraph children.
fn maybe_dayone_moment<'a>(node: &'a AstNode<'a>) -> Option<EmbeddedContent> {
    // Walk all children looking for a single Image node
    let children: Vec<_> = node.children().collect();
    if children.len() == 1 {
        if let NodeValue::Image(link) = &children[0].data.borrow().value {
            if link.url.starts_with("dayone-moment://") {
                let identifier = link.url.trim_start_matches("dayone-moment://").to_owned();
                return Some(EmbeddedContent::Media { kind: MediaKind::Photo, identifier });
            }
        }
    }
    None
}

fn collect_table_rows<'a>(table_node: &'a AstNode<'a>) -> Vec<Vec<TableCell>> {
    table_node.children().map(|row_node| {
        let header = match &row_node.data.borrow().value {
            NodeValue::TableRow(h) => *h,
            _ => false,
        };
        row_node.children().map(|cell_node| {
            let mut spans = Vec::new();
            collect_inline_spans(cell_node, &mut spans);
            TableCell { spans, header }
        }).collect()
    }).collect()
}

fn render_table_to_markdown<'a>(table_node: &'a AstNode<'a>) -> String {
    let rows = collect_table_rows(table_node);
    use super::to_markdown::blocks_to_markdown;
    use super::model::Block;
    let block = Block::Embedded(EmbeddedContent::Table { markdown_cache: None, rows });
    let md = blocks_to_markdown(&[block]);
    md.trim_end_matches('\n').to_owned()
}

pub(crate) fn blocks_to_rtjson(_blocks: Vec<Block>) -> (RtjDocument, bool) {
    (RtjDocument::default(), false) // implemented in Task 8
}
```

**Note:** The `Paragraph` walk in `walk_node` above needs updating to handle Image nodes (dayone-moment URLs). Replace the `NodeValue::Paragraph` arm with:

```rust
NodeValue::Paragraph => {
    // Check if this paragraph is a single dayone-moment image
    let children: Vec<_> = node.children().collect();
    if children.len() == 1 {
        if let NodeValue::Image(link) = &children[0].data.borrow().value {
            if link.url.starts_with("dayone-moment://") {
                let identifier = link.url.trim_start_matches("dayone-moment://").to_owned();
                blocks.push(Block::Embedded(EmbeddedContent::Media {
                    kind: MediaKind::Photo,
                    identifier,
                }));
                return;
            }
        }
    }
    let mut spans: Vec<Span> = Vec::new();
    collect_inline_spans(node, &mut spans);
    if !spans.is_empty() {
        blocks.push(Block::Text(TextLine {
            line_attrs: RtjLineAttrs {
                list_style: ctx.list_style.clone(),
                indent_level: ctx.indent_level,
                quote: ctx.quote,
                header: ctx.heading,
                ..Default::default()
            },
            spans,
        }));
    }
}
```

- [ ] **Step 7.4: Run from_markdown unit tests**

```bash
cargo test convert::from_markdown::tests 2>&1 | tail -15
```
Expected: all tests pass. Fix any failures by adjusting the implementation — common issues are softbreak handling, nested list indent levels, or table row collection.

- [ ] **Step 7.5: Commit**

```bash
git add src/convert/from_markdown.rs
git commit -m "feat(convert): implement markdown_to_blocks via comrak AST walker"
```

---

## Task 8: Blocks → RTJson

**Files:**
- Modify: `src/convert/from_markdown.rs`

- [ ] **Step 8.1: Write failing unit tests for `blocks_to_rtjson`**

Add to the test module in `src/convert/from_markdown.rs`:

```rust
    #[test]
    fn plain_text_block_to_rtjson_node() {
        let (doc, _) = blocks_to_rtjson(vec![Block::Text(TextLine {
            spans: vec![Span { text: "hello".into(), ..Default::default() }],
            ..Default::default()
        })]);
        assert_eq!(doc.contents.len(), 1);
        let RtjNode::Text(tn) = &doc.contents[0] else { panic!() };
        assert_eq!(tn.text, "hello\n");
    }

    #[test]
    fn bold_span_sets_bold_attr() {
        let (doc, _) = blocks_to_rtjson(vec![Block::Text(TextLine {
            spans: vec![Span { text: "hi".into(), bold: true, ..Default::default() }],
            ..Default::default()
        })]);
        let RtjNode::Text(tn) = &doc.contents[0] else { panic!() };
        assert!(tn.attributes.as_ref().unwrap().bold);
    }

    #[test]
    fn highlight_span_sets_highlighted_color() {
        let (doc, _) = blocks_to_rtjson(vec![Block::Text(TextLine {
            spans: vec![Span { text: "hi".into(), highlight: true, ..Default::default() }],
            ..Default::default()
        })]);
        let RtjNode::Text(tn) = &doc.contents[0] else { panic!() };
        let color = tn.attributes.as_ref().unwrap().highlighted_color.as_deref();
        assert_eq!(color, Some(DEFAULT_HIGHLIGHT_COLOR));
    }

    #[test]
    fn line_with_multiple_spans_produces_multiple_nodes() {
        let (doc, _) = blocks_to_rtjson(vec![Block::Text(TextLine {
            spans: vec![
                Span { text: "a".into(), bold: true, ..Default::default() },
                Span { text: "b".into(), ..Default::default() },
            ],
            ..Default::default()
        })]);
        // "a" gets its own node, "b\n" gets its own node
        assert_eq!(doc.contents.len(), 2);
    }

    #[test]
    fn line_attrs_applied_to_first_node_only() {
        let (doc, _) = blocks_to_rtjson(vec![Block::Text(TextLine {
            line_attrs: RtjLineAttrs { header: Some(1), ..Default::default() },
            spans: vec![
                Span { text: "a".into(), ..Default::default() },
                Span { text: "b".into(), ..Default::default() },
            ],
        })]);
        let RtjNode::Text(first) = &doc.contents[0] else { panic!() };
        assert_eq!(first.attributes.as_ref().unwrap().line.as_ref().unwrap().header, Some(1));
        let RtjNode::Text(second) = &doc.contents[1] else { panic!() };
        // Second node gets \n and no line attrs
        assert!(second.attributes.as_ref().map(|a| a.line.is_none()).unwrap_or(true));
    }

    #[test]
    fn blank_line_produces_double_newline_on_previous_node() {
        let (doc, _) = blocks_to_rtjson(vec![
            Block::Text(TextLine {
                spans: vec![Span { text: "para".into(), ..Default::default() }],
                ..Default::default()
            }),
            Block::Text(TextLine::default()), // blank line
            Block::Text(TextLine {
                spans: vec![Span { text: "next".into(), ..Default::default() }],
                ..Default::default()
            }),
        ]);
        // The blank line should produce \n\n after "para"
        let RtjNode::Text(first) = &doc.contents[0] else { panic!() };
        assert!(first.text.ends_with("\n\n"), "expected \\n\\n, got {:?}", first.text);
    }

    #[test]
    fn photo_produces_embedded_node() {
        let (doc, _) = blocks_to_rtjson(vec![
            Block::Embedded(EmbeddedContent::Media { kind: MediaKind::Photo, identifier: "XYZ".into() }),
        ]);
        let RtjNode::Embedded(en) = &doc.contents[0] else { panic!() };
        let obj = &en.embedded_objects[0];
        assert_eq!(obj.get("type").and_then(|v| v.as_str()), Some("photo"));
        assert_eq!(obj.get("identifier").and_then(|v| v.as_str()), Some("XYZ"));
    }

    #[test]
    fn table_produces_embedded_table_node() {
        let (doc, has_tables) = blocks_to_rtjson(vec![
            Block::Embedded(EmbeddedContent::Table {
                markdown_cache: Some("| a |\n| --- |\n| b |".into()),
                rows: vec![],
            }),
        ]);
        assert!(has_tables);
        let RtjNode::Embedded(en) = &doc.contents[0] else { panic!() };
        assert_eq!(en.embedded_objects[0].get("type").and_then(|v| v.as_str()), Some("table"));
        assert_eq!(
            en.embedded_objects[0].get("markdown").and_then(|v| v.as_str()),
            Some("| a |\n| --- |\n| b |")
        );
    }

    #[test]
    fn horizontal_rule_produces_embedded_node() {
        let (doc, _) = blocks_to_rtjson(vec![Block::Embedded(EmbeddedContent::HorizontalRule)]);
        let RtjNode::Embedded(en) = &doc.contents[0] else { panic!() };
        assert_eq!(en.embedded_objects[0].get("type").and_then(|v| v.as_str()), Some("horizontalLineRule"));
    }
```

- [ ] **Step 8.2: Run tests to confirm they fail**

```bash
cargo test convert::from_markdown::tests::plain_text_block_to_rtjson 2>&1 | grep "FAILED\|panicked"
```
Expected: FAILED.

- [ ] **Step 8.3: Implement `blocks_to_rtjson`**

Replace the stub in `src/convert/from_markdown.rs`:

```rust
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
            "version": 1
        },
        "small-lines-removed": true
    });

    (RtjDocument { meta: Some(meta), contents }, has_tables)
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

        let mut attrs = RtjTextAttrs::default();
        attrs.bold = span.bold;
        attrs.italic = span.italic;
        attrs.strikethrough = span.strikethrough;
        attrs.inline_code = span.inline_code;
        attrs.autolink = span.autolink;
        attrs.link_url = span.link_url.clone();
        if span.highlight {
            attrs.highlighted_color = Some(DEFAULT_HIGHLIGHT_COLOR.to_owned());
        }

        // Line attrs only on the first node of the line
        if idx == 0 {
            let la = &line.line_attrs;
            if la.header.is_some() || la.list_style.is_some() || la.quote || la.code_block {
                attrs.line = Some(la.clone());
            }
        }

        let has_any_attr = attrs.bold || attrs.italic || attrs.strikethrough
            || attrs.inline_code || attrs.autolink || attrs.link_url.is_some()
            || attrs.highlighted_color.is_some() || attrs.line.is_some();

        nodes.push(RtjNode::Text(RtjTextNode {
            text,
            attributes: if has_any_attr { Some(attrs) } else { None },
        }));
    }

    // Empty line (no spans) produces a single empty text node
    if nodes.is_empty() {
        nodes.push(RtjNode::Text(RtjTextNode { text: "\n".into(), attributes: None }));
    }

    nodes
}

fn embedded_to_rtjson_value(emb: &EmbeddedContent, has_tables: &mut bool) -> Option<serde_json::Value> {
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
        EmbeddedContent::ExternalMedia { url } => {
            Some(json!({ "type": "externalVideo", "url": url }))
        }
        EmbeddedContent::HorizontalRule => {
            Some(json!({ "type": "horizontalLineRule" }))
        }
        EmbeddedContent::Table { markdown_cache, rows } => {
            *has_tables = true;
            let rows_val: Vec<Vec<serde_json::Value>> = rows.iter().map(|row| {
                row.iter().map(|cell| {
                    let content: Vec<serde_json::Value> = cell.spans.iter().map(|s| {
                        let mut attrs = serde_json::Map::new();
                        if s.bold { attrs.insert("bold".into(), json!(true)); }
                        if s.italic { attrs.insert("italic".into(), json!(true)); }
                        if s.strikethrough { attrs.insert("strikethrough".into(), json!(true)); }
                        if s.inline_code { attrs.insert("inlineCode".into(), json!(true)); }
                        if s.highlight { attrs.insert("highlightedColor".into(), json!(DEFAULT_HIGHLIGHT_COLOR)); }
                        if let Some(url) = &s.link_url { attrs.insert("linkURL".into(), json!(url)); }
                        if attrs.is_empty() {
                            json!({ "text": s.text })
                        } else {
                            json!({ "text": s.text, "attributes": attrs })
                        }
                    }).collect();
                    let mut cell_val = json!({ "content": content });
                    if cell.header {
                        cell_val["header"] = json!(true);
                    }
                    cell_val
                }).collect()
            }).collect();
            let mut table = json!({ "type": "table", "rows": rows_val });
            if let Some(md) = markdown_cache {
                table["markdown"] = json!(md);
            }
            Some(table)
        }
        EmbeddedContent::Unknown => None,
    }
}
```

- [ ] **Step 8.4: Run all from_markdown tests**

```bash
cargo test convert::from_markdown::tests 2>&1 | tail -15
```
Expected: all tests pass.

- [ ] **Step 8.5: Commit**

```bash
git add src/convert/from_markdown.rs
git commit -m "feat(convert): implement blocks_to_rtjson with unit tests"
```

---

## Task 9: Round-trip integration tests

**Files:**
- Modify: `tests/convert.rs`

- [ ] **Step 9.1: Add round-trip tests to `tests/convert.rs`**

```rust
use dayone::convert::markdown_to_rtjson;

// Markdown → RTJson → Markdown must equal original
macro_rules! md_roundtrip_test {
    ($name:ident, $num:literal) => {
        #[test]
        fn $name() {
            let md = load_md(concat!($num, ".md"));
            let result = markdown_to_rtjson(&md);
            let back = rtjson_to_markdown(&result.document);
            assert_eq!(back, md, "fixture {} Markdown round-trip failed", $num);
        }
    };
}

md_roundtrip_test!(roundtrip_md_01, "01");
md_roundtrip_test!(roundtrip_md_02, "02");
md_roundtrip_test!(roundtrip_md_03, "03");
md_roundtrip_test!(roundtrip_md_04, "04");
md_roundtrip_test!(roundtrip_md_05, "05");
md_roundtrip_test!(roundtrip_md_06, "06");
md_roundtrip_test!(roundtrip_md_07, "07");
md_roundtrip_test!(roundtrip_md_08, "08");
md_roundtrip_test!(roundtrip_md_09, "09");
md_roundtrip_test!(roundtrip_md_10, "10");
md_roundtrip_test!(roundtrip_md_11, "11");
md_roundtrip_test!(roundtrip_md_12, "12");
md_roundtrip_test!(roundtrip_md_13, "13");
md_roundtrip_test!(roundtrip_md_14, "14");
md_roundtrip_test!(roundtrip_md_20, "20");
md_roundtrip_test!(roundtrip_md_21, "21");

#[test]
fn has_tables_set_for_fixture_20() {
    let md = load_md("20.md");
    let result = markdown_to_rtjson(&md);
    assert!(result.has_tables, "fixture 20 (table) should set has_tables");
}

#[test]
fn has_tables_false_for_fixture_01() {
    let md = load_md("01.md");
    let result = markdown_to_rtjson(&md);
    assert!(!result.has_tables);
}
```

Note: fixtures 15–19 are intentionally omitted from the round-trip tests. Photo/video/PDF/external-video/horizontal-rule all go through embedded nodes; the round-trip `md → rtjson → md` works by definition for those since we just produce the same `dayone-moment://` URL or `---`. Add them if desired.

- [ ] **Step 9.2: Run round-trip tests**

```bash
cargo test --test convert 2>&1 | tail -20
```
Expected: all pass. For any failures, examine what `markdown_to_rtjson` produces vs. what `rtjson_to_markdown` emits — the most common issues are whitespace at paragraph boundaries or bold/italic nesting order.

- [ ] **Step 9.3: Run full test suite**

```bash
cargo test --locked 2>&1 | tail -5
```
Expected: ≥ 213 tests pass (existing), plus all new convert tests. The pre-existing `env_util::tests::secrets_cli_sets_unset_var` failure is a known issue unrelated to this work.

- [ ] **Step 9.4: Commit**

```bash
git add tests/convert.rs
git commit -m "feat(convert): add round-trip integration tests for Markdown↔RTJson"
```

---

## Task 10: Integrate into `entry_write.rs`, delete `rich_text.rs`

**Files:**
- Modify: `src/commands/entry_write.rs`
- Delete: `src/rich_text.rs`
- Modify: `src/main.rs` (remove `mod rich_text`)

- [ ] **Step 10.1: Replace `append_markdown_text_nodes` call site in `entry_write.rs`**

Find `build_rich_text_json_from_body_with_placeholders` (around line 871). Currently it calls `append_markdown_text_nodes(&mut contents, text)` to convert Markdown text segments to RTJson nodes.

Replace those calls with:

```rust
use crate::convert::markdown_to_rtjson;
// ...
// Instead of: append_markdown_text_nodes(&mut contents, text);
let result = markdown_to_rtjson(text);
for node in result.document.contents {
    contents.push(serde_json::to_value(node).expect("rtjson node serialization"));
}
```

There are typically 2–3 call sites to `append_markdown_text_nodes` in this function. Replace all of them.

- [ ] **Step 10.2: Delete the now-unused functions**

Remove `append_markdown_text_nodes` (around line 952) and `parse_markdown_heading_line` (around line 984) from `entry_write.rs`. These are dead code once the above replacement is made.

- [ ] **Step 10.3: Delete `src/rich_text.rs` and remove its `mod` declaration**

```bash
rm src/rich_text.rs
```

In `src/main.rs`, remove the line:
```rust
mod rich_text;
```

- [ ] **Step 10.4: Fix tests in `entry_write.rs` that used `rich_text` types**

The tests in `entry_write.rs` that previously used `build_from_body` from `rich_text.rs` now need updating. Search for usages:

```bash
grep -n "build_from_body\|rich_text\|RichTextDocument\|RichTextNode" src/commands/entry_write.rs | head -20
```

For each test that constructed an `RichTextDocument` to set up fixture data, replace with equivalent JSON construction directly using `serde_json::json!({})` — the tests were testing serialization behavior, not the type system.

- [ ] **Step 10.5: Run the full test suite**

```bash
cargo test --locked 2>&1 | tail -10
```
Expected: all existing tests pass (minus the pre-existing `secrets_cli_sets_unset_var` failure) plus all new convert tests.

- [ ] **Step 10.6: Commit**

```bash
git add src/commands/entry_write.rs src/main.rs
git rm src/rich_text.rs
git commit -m "feat(convert): wire markdown_to_rtjson into entry_write, delete rich_text.rs"
```

---

## Task 11: Final verification and push

- [ ] **Step 11.1: Run full test suite**

```bash
cargo test --locked 2>&1 | tail -5
```
Expected: all convert tests pass, no regressions in existing tests.

- [ ] **Step 11.2: Run clippy**

```bash
cargo clippy 2>&1 | grep "^error" | head -10
```
Expected: no errors (warnings are advisory per AGENTS.md).

- [ ] **Step 11.3: Push feature branch**

```bash
git push -u origin feat/DAYONE-704-rtjson-markdown-library
```

- [ ] **Step 11.4: Open PR**

```bash
gh pr create \
  --title "feat(convert): RTJson ↔ Markdown conversion library (DAYONE-704)" \
  --body "$(cat docs/superpowers/specs/2026-04-01-rtjson-markdown-library-design.md | head -30)"
```
