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
    #[serde(
        default,
        rename = "inlineCode",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub inline_code: bool,
    #[serde(
        default,
        rename = "highlightedColor",
        skip_serializing_if = "Option::is_none"
    )]
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
    #[serde(
        default,
        rename = "codeBlock",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub code_block: bool,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

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
        self.spans.is_empty() && self.line_attrs == RtjLineAttrs::default()
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
    /// externalVideo, externalAudio (kind preserved for RTJson round-trips)
    ExternalMedia {
        kind: ExternalMediaKind,
        url: String,
    },
    /// horizontalLineRule / horizontalRuleLine (both accepted)
    HorizontalRule,
    /// table with optional pre-rendered markdown cache
    Table {
        markdown_cache: Option<String>,
        rows: Vec<Vec<TableCell>>,
    },
    /// Any other type — omitted from Markdown output
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MediaKind {
    Photo,
    Video,
    Audio,
    Pdf,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExternalMediaKind {
    Video,
    Audio,
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rtj_document_parses_contents_field() {
        let doc: RtjDocument = serde_json::from_value(json!({
            "contents": [{"text": "hello"}],
            "meta": {"version": 1}
        }))
        .unwrap();
        assert_eq!(doc.contents.len(), 1);
        assert!(matches!(doc.contents[0], RtjNode::Text(_)));
    }

    #[test]
    fn rtj_document_accepts_nodes_alias() {
        let doc: RtjDocument = serde_json::from_value(json!({
            "nodes": [{"text": "hello"}]
        }))
        .unwrap();
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
        }))
        .unwrap();
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
        }))
        .unwrap();
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
        }))
        .unwrap();
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
        }))
        .unwrap();
        assert!(matches!(doc.contents[0], RtjNode::Embedded(_)));
    }

    #[test]
    fn rtj_unknown_node_preserved() {
        let doc: RtjDocument = serde_json::from_value(json!({
            "contents": [{"unknownField": true}]
        }))
        .unwrap();
        assert!(matches!(doc.contents[0], RtjNode::Unknown(_)));
    }
}
