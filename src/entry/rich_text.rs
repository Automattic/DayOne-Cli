use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::convert::model::{RtjDocument, RtjNode};
use crate::entry::attachments::{MediaType, NewMoment};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMomentPlaceholder {
    pub start: usize,
    pub end: usize,
    pub moment_type: MediaType,
    pub identifier: Option<String>,
}

pub fn build_rich_text_json_with_attachments(
    base_rich_text_json: Option<String>,
    body: &str,
    new_moments: &[NewMoment],
    now_ms: i64,
    allow_inline_placeholder_rebuild: bool,
) -> Option<String> {
    let placeholders = parse_dayone_moment_placeholders(body);
    if base_rich_text_json.is_none() {
        if allow_inline_placeholder_rebuild && !placeholders.is_empty() {
            return build_rich_text_json_from_body_with_placeholders(
                body,
                &placeholders,
                new_moments,
                now_ms,
            );
        }
        return None;
    }

    let should_use_inline_placeholder_composition = allow_inline_placeholder_rebuild
        && !placeholders.is_empty()
        && placeholders
            .iter()
            .any(|placeholder| placeholder.identifier.is_none());
    if should_use_inline_placeholder_composition {
        return build_rich_text_json_from_body_with_placeholders(
            body,
            &placeholders,
            new_moments,
            now_ms,
        );
    }

    if new_moments.is_empty() {
        return base_rich_text_json;
    }
    let embedded_node = json!({
        "embeddedObjects": new_moments
            .iter()
            .map(|moment| {
                json!({
                    "type": moment.moment_type.as_rich_text_embed_type(),
                    "identifier": moment.id.as_str()
                })
            })
            .collect::<Vec<Value>>()
    });

    if let Some(raw) = base_rich_text_json {
        let Ok(mut doc) = serde_json::from_str::<Value>(&raw) else {
            return Some(raw);
        };
        let Some(contents) = doc.get_mut("contents").and_then(Value::as_array_mut) else {
            return Some(raw);
        };
        contents.push(embedded_node);
        return serde_json::to_string(&doc).ok().or(Some(raw));
    }
    None
}

pub fn existing_entry_has_meaningful_rich_text(existing_entry: Option<&Value>) -> bool {
    let Some(raw) = existing_entry.and_then(extract_existing_rich_text_json) else {
        return false;
    };
    let Ok(doc) = serde_json::from_str::<Value>(raw) else {
        return true;
    };
    let created_platform = doc
        .get("meta")
        .and_then(|meta| meta.get("created"))
        .and_then(|created| created.get("platform"))
        .and_then(Value::as_str);
    if created_platform != Some("dayone-cli") {
        return true;
    }
    let Some(contents) = doc.get("contents").and_then(Value::as_array) else {
        return true;
    };
    for node in contents {
        if let Some(text_node) = node.as_object().and_then(|obj| {
            let attrs = obj.get("attributes")?;
            let text = obj.get("text")?;
            Some((attrs, text))
        }) {
            let (attributes, _text) = text_node;
            let is_empty_attributes = attributes
                .as_object()
                .map(|obj| obj.is_empty())
                .unwrap_or(false);
            if !is_empty_attributes {
                return true;
            }
            continue;
        }
        if node
            .as_object()
            .and_then(|obj| obj.get("embeddedObjects"))
            .and_then(Value::as_array)
            .is_some()
        {
            continue;
        }
        return true;
    }
    false
}

pub fn parse_dayone_moment_placeholders(body: &str) -> Vec<ParsedMomentPlaceholder> {
    const PREFIX: &str = "![](dayone-moment:";
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel_start) = body[cursor..].find(PREFIX) {
        let start = cursor + rel_start;
        let after_prefix = start + PREFIX.len();
        let Some(rel_end) = body[after_prefix..].find(')') else {
            break;
        };
        let end = after_prefix + rel_end + 1;
        let marker = &body[after_prefix..after_prefix + rel_end];
        if let Some((moment_type, identifier)) = parse_dayone_moment_marker(marker) {
            out.push(ParsedMomentPlaceholder {
                start,
                end,
                moment_type,
                identifier,
            });
        }
        cursor = end;
    }
    out
}

pub fn build_updated_rich_text_json(
    existing_entry: Option<&Value>,
    new_body: &str,
    now_ms: i64,
) -> Option<String> {
    let entry = existing_entry?;
    let existing_raw = extract_existing_rich_text_json(entry)?;
    let existing_body = extract_existing_body(entry).unwrap_or_default();
    let existing_body_normalized = normalize_line_endings(existing_body);
    let new_body_normalized = normalize_line_endings(new_body);

    if new_body_normalized == existing_body_normalized {
        return Some(existing_raw.to_owned());
    }

    if let Some(suffix) = new_body_normalized.strip_prefix(&existing_body_normalized) {
        if suffix.is_empty() {
            return Some(existing_raw.to_owned());
        }
        if let Some(updated) = append_text_to_rich_text(existing_raw, suffix) {
            return Some(updated);
        }
        return rebuild_rich_text_json_from_body(new_body, now_ms);
    }

    if rich_text_json_has_embedded_objects(existing_raw) {
        return Some(existing_raw.to_owned());
    }

    rebuild_rich_text_json_from_body(new_body, now_ms)
}

fn rebuild_rich_text_json_from_body(new_body: &str, now_ms: i64) -> Option<String> {
    let placeholders = parse_dayone_moment_placeholders(new_body);
    build_rich_text_json_from_body_with_placeholders(new_body, &placeholders, &[], now_ms)
}

fn parse_dayone_moment_marker(marker: &str) -> Option<(MediaType, Option<String>)> {
    if let Some(image_id) = marker.strip_prefix("//") {
        return Some((MediaType::Image, normalized_placeholder_id(image_id)));
    }
    let typed = marker.strip_prefix('/')?;
    let slash = typed.find('/')?;
    let (raw_type, raw_id_with_slash) = typed.split_at(slash);
    let raw_id = raw_id_with_slash.strip_prefix('/').unwrap_or_default();
    let moment_type = match raw_type {
        "video" => MediaType::Video,
        "audio" => MediaType::Audio,
        "pdfAttachment" => MediaType::PdfAttachment,
        "image" | "photo" => MediaType::Image,
        _ => return None,
    };
    Some((moment_type, normalized_placeholder_id(raw_id)))
}

fn normalized_placeholder_id(raw_id: &str) -> Option<String> {
    let trimmed = raw_id.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_owned())
}

fn build_rich_text_json_from_body_with_placeholders(
    body: &str,
    placeholders: &[ParsedMomentPlaceholder],
    new_moments: &[NewMoment],
    now_ms: i64,
) -> Option<String> {
    let mut contents: Vec<Value> = Vec::new();
    let mut markdown_fragment_cache: HashMap<String, Vec<Value>> = HashMap::new();
    let mut cursor = 0usize;
    let mut next_attachment_idx = 0usize;
    let mut inline_new_moment_ids: HashSet<&str> = HashSet::new();

    for placeholder in placeholders {
        let mut embedded_identifier: Option<String> = None;
        if let Some(identifier) = &placeholder.identifier {
            if new_moments.iter().any(|moment| moment.id == *identifier) {
                inline_new_moment_ids.insert(identifier.as_str());
            }
            embedded_identifier = Some(identifier.clone());
        } else if let Some(candidate) = new_moments.get(next_attachment_idx)
            && placeholder.moment_type == candidate.moment_type
        {
            next_attachment_idx += 1;
            inline_new_moment_ids.insert(candidate.id.as_str());
            embedded_identifier = Some(candidate.id.clone());
        }

        if placeholder.start > cursor {
            let text = &body[cursor..placeholder.start];
            // A blank line above an embedded object is markdown layout, not entry
            // content. The body separates an appended attachment from the block above
            // it with a blank line, and the conversion mirrors fragment trailing
            // newlines into the last text node, which clients render as a blank line
            // above the attachment. Drop the separator before converting the
            // fragment.
            let text = if embedded_identifier.is_some() {
                text.trim_end_matches(['\r', '\n'])
            } else {
                text
            };
            append_rtjson_nodes_from_markdown_cached(
                &mut contents,
                text,
                &mut markdown_fragment_cache,
            );
        }

        if let Some(identifier) = embedded_identifier {
            contents.push(json!({
                "embeddedObjects": [{
                    "type": placeholder.moment_type.as_rich_text_embed_type(),
                    "identifier": identifier
                }]
            }));
        } else {
            let raw_placeholder = &body[placeholder.start..placeholder.end];
            append_rtjson_nodes_from_markdown_cached(
                &mut contents,
                raw_placeholder,
                &mut markdown_fragment_cache,
            );
        }

        cursor = placeholder.end;
    }

    if cursor < body.len() {
        let trailing_text = &body[cursor..];
        append_rtjson_nodes_from_markdown_cached(
            &mut contents,
            trailing_text,
            &mut markdown_fragment_cache,
        );
    }

    for moment in new_moments {
        if inline_new_moment_ids.contains(moment.id.as_str()) {
            continue;
        }
        contents.push(json!({
            "embeddedObjects": [{
                "type": moment.moment_type.as_rich_text_embed_type(),
                "identifier": moment.id.as_str()
            }]
        }));
    }

    if contents.is_empty() {
        append_rtjson_nodes_from_markdown_cached(&mut contents, body, &mut markdown_fragment_cache);
    }

    serde_json::to_string(&json!({
        "contents": contents,
        "meta": {
            "version": 1,
            "created": {
                "version": now_ms / 1000,
                "platform": "dayone-cli"
            },
            "small-lines-removed": true
        }
    }))
    .ok()
}

#[cfg(test)]
fn append_rtjson_nodes_from_markdown(contents: &mut Vec<Value>, text: &str) {
    let mut cache: HashMap<String, Vec<Value>> = HashMap::new();
    append_rtjson_nodes_from_markdown_cached(contents, text, &mut cache);
}

fn append_rtjson_nodes_from_markdown_cached(
    contents: &mut Vec<Value>,
    text: &str,
    cache: &mut HashMap<String, Vec<Value>>,
) {
    if text.is_empty() {
        return;
    }

    if let Some(cached_nodes) = cache.get(text) {
        contents.extend(cached_nodes.iter().cloned());
        return;
    }

    let mut placeholder_rt = crate::convert::markdown_to_rtjson(text);
    align_fragment_trailing_newlines(&mut placeholder_rt.document.contents, text);
    let produced_nodes: Vec<Value> = if placeholder_rt.document.contents.is_empty() {
        vec![json!({ "text": text })]
    } else {
        placeholder_rt
            .document
            .contents
            .into_iter()
            .map(|node| serde_json::to_value(node).expect("rtjson node serialization"))
            .collect()
    };
    cache.insert(text.to_owned(), produced_nodes.clone());
    contents.extend(produced_nodes);
}

fn align_fragment_trailing_newlines(nodes: &mut Vec<crate::convert::model::RtjNode>, source: &str) {
    use crate::convert::model::RtjNode;

    let expected = normalize_line_endings(source)
        .chars()
        .rev()
        .take_while(|c| *c == '\n')
        .count();

    if let Some(RtjNode::Text(last_text_node)) = nodes.last_mut() {
        align_text_node_trailing_newlines(last_text_node, expected);
        return;
    }

    if expected > 0 {
        nodes.push(RtjNode::Text(crate::convert::model::RtjTextNode {
            text: "\n".repeat(expected),
            attributes: None,
        }));
    }
}

fn align_text_node_trailing_newlines(
    node: &mut crate::convert::model::RtjTextNode,
    expected: usize,
) {
    let actual = node.text.chars().rev().take_while(|c| *c == '\n').count();
    if actual == expected {
        return;
    }
    if actual > expected {
        let keep_len = node.text.len() - (actual - expected);
        node.text.truncate(keep_len);
    } else {
        node.text.push_str(&"\n".repeat(expected - actual));
    }
}

#[cfg(test)]
fn parse_markdown_heading_line(line: &str) -> Option<(u8, String)> {
    let newline = if line.ends_with('\n') { "\n" } else { "" };
    let core = line.strip_suffix('\n').unwrap_or(line);
    let core = core.strip_suffix('\r').unwrap_or(core);
    let mut hashes = 0usize;
    for ch in core.chars() {
        if ch == '#' {
            hashes += 1;
        } else {
            break;
        }
    }
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = core.get(hashes..)?;
    let body = rest.strip_prefix(' ')?;
    Some((hashes as u8, format!("{body}{newline}")))
}

fn extract_existing_rich_text_json(entry: &Value) -> Option<&str> {
    entry
        .get("payload")
        .and_then(|payload| payload.get("richTextJSON"))
        .or_else(|| entry.get("richTextJSON"))
        .and_then(Value::as_str)
}

fn extract_existing_body(entry: &Value) -> Option<&str> {
    entry
        .get("payload")
        .and_then(|payload| payload.get("body"))
        .or_else(|| entry.get("body"))
        .and_then(Value::as_str)
}

fn normalize_line_endings(input: &str) -> String {
    input.replace("\r\n", "\n").replace('\r', "\n")
}

fn rich_text_json_has_embedded_objects(raw: &str) -> bool {
    serde_json::from_str::<RtjDocument>(raw)
        .ok()
        .map(|doc| {
            doc.contents.iter().any(|node| {
                matches!(node, RtjNode::Embedded(embedded) if !embedded.embedded_objects.is_empty())
            })
        })
        .unwrap_or(false)
}

fn append_text_to_rich_text(existing_raw: &str, suffix: &str) -> Option<String> {
    let mut doc: Value = serde_json::from_str(existing_raw).ok()?;
    let contents = doc.get_mut("contents")?.as_array_mut()?;
    contents.push(json!({
        "attributes": {},
        "text": suffix
    }));
    serde_json::to_string(&doc).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::attachments::{MediaType, NewMoment};

    fn new_moment(id: &str, moment_type: MediaType) -> NewMoment {
        NewMoment {
            id: id.to_owned(),
            moment_type,
            content_type: "application/octet-stream".to_owned(),
            md5_body: "abc".to_owned(),
            file_size_bytes: 1,
            pdf_name: if moment_type == MediaType::PdfAttachment {
                Some("sample-pdf".to_owned())
            } else {
                None
            },
            thumbnail: None,
        }
    }

    #[test]
    fn rich_text_extractor_works_for_payload_and_root() {
        let payload_entry = json!({
            "payload": {
                "richTextJSON": "{\"contents\":[]}"
            }
        });
        let root_entry = json!({
            "richTextJSON": "{\"contents\":[]}"
        });
        assert_eq!(
            extract_existing_rich_text_json(&payload_entry),
            Some("{\"contents\":[]}")
        );
        assert_eq!(
            extract_existing_rich_text_json(&root_entry),
            Some("{\"contents\":[]}")
        );
    }

    #[test]
    fn build_rich_text_json_with_attachments_returns_none_without_existing_rich_text() {
        let out = build_rich_text_json_with_attachments(
            None,
            "hello",
            &[NewMoment {
                id: "M1".to_owned(),
                moment_type: MediaType::PdfAttachment,
                content_type: "application/pdf".to_owned(),
                md5_body: "abc".to_owned(),
                file_size_bytes: 123,
                pdf_name: Some("sample-pdf".to_owned()),
                thumbnail: None,
            }],
            1_700_000_000_000,
            true,
        );
        assert!(out.is_none());
    }

    #[test]
    fn build_rich_text_json_with_attachments_builds_inline_pdf_embed_without_existing_rich_text() {
        let out = build_rich_text_json_with_attachments(
            None,
            "Before\n\n![](dayone-moment:/pdfAttachment/PDF-1)\n\nAfter",
            &[new_moment("PDF-1", MediaType::PdfAttachment)],
            1_700_000_000_000,
            true,
        )
        .expect("rich text should be created");
        let parsed: Value = serde_json::from_str(&out).expect("rich text should parse");
        let contents = parsed
            .get("contents")
            .and_then(Value::as_array)
            .expect("contents should exist");
        assert!(contents.iter().any(|node| {
            node.get("embeddedObjects")
                .and_then(Value::as_array)
                .map(|objects| {
                    objects.iter().any(|obj| {
                        obj.get("type").and_then(Value::as_str) == Some("pdfAttachment")
                            && obj.get("identifier").and_then(Value::as_str) == Some("PDF-1")
                    })
                })
                .unwrap_or(false)
        }));
    }

    #[test]
    fn build_rich_text_json_from_body_with_bound_placeholder_does_not_duplicate_embed() {
        let body = "Before\n\n![](dayone-moment:/pdfAttachment/PDF-1)\n\nAfter";
        let placeholders = parse_dayone_moment_placeholders(body);
        let raw = build_rich_text_json_from_body_with_placeholders(
            body,
            &placeholders,
            &[new_moment("PDF-1", MediaType::PdfAttachment)],
            1_700_000_000_000,
        )
        .expect("rich text should be created");
        let parsed: Value = serde_json::from_str(&raw).expect("rich text should parse");
        let embed_count = parsed
            .get("contents")
            .and_then(Value::as_array)
            .map(|contents| {
                contents
                    .iter()
                    .filter(|node| node.get("embeddedObjects").is_some())
                    .count()
            })
            .unwrap_or_default();
        assert_eq!(embed_count, 1);
    }

    #[test]
    fn parse_markdown_heading_line_parses_hash_headers() {
        let parsed = parse_markdown_heading_line("# Title\n").expect("heading should parse");
        assert_eq!(parsed.0, 1);
        assert_eq!(parsed.1, "Title\n");

        let parsed_crlf =
            parse_markdown_heading_line("# Title\r\n").expect("CRLF heading should parse");
        assert_eq!(parsed_crlf.0, 1);
        assert_eq!(parsed_crlf.1, "Title\n");

        let parsed_h2 = parse_markdown_heading_line("## Subtitle").expect("h2 should parse");
        assert_eq!(parsed_h2.0, 2);
        assert_eq!(parsed_h2.1, "Subtitle");

        assert!(parse_markdown_heading_line("Not a heading").is_none());
        assert!(parse_markdown_heading_line("#NoSpace").is_none());
    }

    #[test]
    fn build_rich_text_json_from_body_with_placeholders_converts_heading_lines() {
        let body = "# Heading\n\nBefore\n\n![](dayone-moment:/pdfAttachment/PDF-1)\n";
        let placeholders = parse_dayone_moment_placeholders(body);
        let raw = build_rich_text_json_from_body_with_placeholders(
            body,
            &placeholders,
            &[new_moment("PDF-1", MediaType::PdfAttachment)],
            1_700_000_000_000,
        )
        .expect("rich text should be created");
        let parsed: Value = serde_json::from_str(&raw).expect("rich text should parse");
        let contents = parsed
            .get("contents")
            .and_then(Value::as_array)
            .expect("contents should exist");
        let first = contents.first().expect("first node should exist");
        assert_eq!(
            first
                .get("attributes")
                .and_then(|v| v.get("line"))
                .and_then(|v| v.get("header"))
                .and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(first.get("text").and_then(Value::as_str), Some("Heading\n"));
    }

    #[test]
    fn append_rtjson_nodes_from_markdown_emits_nodes_per_paragraph() {
        let mut contents = Vec::new();
        append_rtjson_nodes_from_markdown(&mut contents, "Line one\n\nLine two\n");
        assert_eq!(contents.len(), 2);
        let t0 = contents[0]
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("");
        let t1 = contents[1]
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("");
        assert!(t0.contains("Line one"), "first node text: {t0:?}");
        assert!(t1.contains("Line two"), "second node text: {t1:?}");
    }

    #[test]
    fn append_rtjson_nodes_from_markdown_preserves_missing_trailing_newline() {
        let mut contents = Vec::new();
        append_rtjson_nodes_from_markdown(&mut contents, "Before placeholder");
        let last_text = contents
            .last()
            .and_then(|node| node.get("text"))
            .and_then(Value::as_str)
            .expect("expected trailing text node");
        assert_eq!(last_text, "Before placeholder");
    }

    #[test]
    fn append_rtjson_nodes_from_markdown_preserves_multiple_trailing_newlines() {
        let mut contents = Vec::new();
        append_rtjson_nodes_from_markdown(&mut contents, "Line one\n\n");
        let last_text = contents
            .last()
            .and_then(|node| node.get("text"))
            .and_then(Value::as_str)
            .expect("expected trailing text node");
        assert!(last_text.ends_with("\n\n"), "got {last_text:?}");
    }

    #[test]
    fn append_rtjson_nodes_from_markdown_adds_trailing_newline_after_embedded_node() {
        let mut contents = Vec::new();
        append_rtjson_nodes_from_markdown(&mut contents, "![](dayone-moment://IMG)\n");
        assert!(contents.len() >= 2, "expected embedded + trailing text");
        let last_text = contents
            .last()
            .and_then(|node| node.get("text"))
            .and_then(Value::as_str)
            .expect("expected trailing text node");
        assert_eq!(last_text, "\n");
    }

    #[test]
    fn append_rtjson_nodes_from_markdown_preserves_crlf_trailing_blank_lines() {
        let mut contents = Vec::new();
        append_rtjson_nodes_from_markdown(&mut contents, "Line one\r\n\r\n");
        let last_text = contents
            .last()
            .and_then(|node| node.get("text"))
            .and_then(Value::as_str)
            .expect("expected trailing text node");
        assert!(last_text.ends_with("\n\n"), "got {last_text:?}");
    }

    #[test]
    fn parse_dayone_moment_placeholders_supports_image_and_typed_forms() {
        let body = "a\n\n![](dayone-moment://IMG1)\n\n![](dayone-moment:/video/VID1)\n\n![](dayone-moment:/audio/)";
        let parsed = parse_dayone_moment_placeholders(body);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].moment_type, MediaType::Image);
        assert_eq!(parsed[0].identifier.as_deref(), Some("IMG1"));
        assert_eq!(parsed[1].moment_type, MediaType::Video);
        assert_eq!(parsed[1].identifier.as_deref(), Some("VID1"));
        assert_eq!(parsed[2].moment_type, MediaType::Audio);
        assert_eq!(parsed[2].identifier, None);
    }

    #[test]
    fn existing_rich_text_is_preserved_when_body_contains_only_bound_placeholders() {
        let existing_raw = "{\"contents\":[{\"attributes\":{\"line\":{\"header\":1}},\"text\":\"Title\\n\"},{\"embeddedObjects\":[{\"type\":\"photo\",\"identifier\":\"IMG-1\"}]}],\"meta\":{\"version\":1,\"created\":{\"version\":1746805027,\"platform\":\"webapp\"},\"small-lines-removed\":true}}".to_owned();
        let body = "# Title\n\n![](dayone-moment://IMG-1)\n";
        let out = build_rich_text_json_with_attachments(
            Some(existing_raw.clone()),
            body,
            &[],
            1_700_000_000_000,
            false,
        )
        .expect("rich text should exist");
        assert_eq!(out, existing_raw);
    }

    #[test]
    fn existing_rich_text_with_new_attachment_appends_embed_instead_of_rebuilding_placeholders() {
        let existing_raw = "{\"contents\":[{\"attributes\":{\"line\":{\"header\":1}},\"text\":\"Title\\n\"},{\"embeddedObjects\":[{\"type\":\"photo\",\"identifier\":\"IMG-1\"}]}],\"meta\":{\"version\":1,\"created\":{\"version\":1746805027,\"platform\":\"webapp\"},\"small-lines-removed\":true}}".to_owned();
        let body = "# Title\n\n![](dayone-moment://IMG-1)\n";
        let out = build_rich_text_json_with_attachments(
            Some(existing_raw),
            body,
            &[new_moment("IMG-2", MediaType::Image)],
            1_700_000_000_000,
            false,
        )
        .expect("rich text should exist");
        let doc: Value = serde_json::from_str(&out).expect("doc should parse");
        let contents = doc
            .get("contents")
            .and_then(Value::as_array)
            .expect("contents should exist");
        assert_eq!(contents.len(), 3);
        assert_eq!(
            contents[2]
                .get("embeddedObjects")
                .and_then(Value::as_array)
                .and_then(|arr| arr.first())
                .and_then(|it| it.get("identifier"))
                .and_then(Value::as_str),
            Some("IMG-2")
        );
    }

    #[test]
    fn placeholder_rebuild_mismatch_does_not_consume_attachment_for_later_match() {
        let existing_raw =
            "{\"contents\":[{\"attributes\":{},\"text\":\"before\"}],\"meta\":{\"version\":1,\"created\":{\"version\":1746805027,\"platform\":\"dayone-cli\"},\"small-lines-removed\":true}}"
                .to_owned();
        let body = "![](dayone-moment:/video/)\n\n![](dayone-moment://)";
        let out = build_rich_text_json_with_attachments(
            Some(existing_raw),
            body,
            &[new_moment("IMG-1", MediaType::Image)],
            1_700_000_000_000,
            true,
        )
        .expect("rich text should exist");
        let doc: Value = serde_json::from_str(&out).expect("doc should parse");
        let contents = doc
            .get("contents")
            .and_then(Value::as_array)
            .expect("contents should exist");
        assert!(contents.iter().any(|node| {
            node.get("embeddedObjects")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .any(|it| it.get("identifier").and_then(Value::as_str) == Some("IMG-1"))
                })
                .unwrap_or(false)
        }));
    }

    #[test]
    fn build_updated_rich_text_json_rebuilds_wholesale_rewrite_from_new_body() {
        let existing = json!({
            "payload": {
                "body": "# Old title\n\nOld paragraph.\n\nOld tail.",
                "richTextJSON": "{\"contents\":[{\"attributes\":{\"line\":{\"header\":1}},\"text\":\"Old title\\n\"},{\"text\":\"Old paragraph.\\n\\nOld tail.\"}],\"meta\":{\"version\":1,\"created\":{\"version\":1746805027,\"platform\":\"webapp\"},\"small-lines-removed\":true}}"
            }
        });

        let updated = build_updated_rich_text_json(
            Some(&existing),
            "New title\n\nCompletely different content.",
            1_700_000_000_000,
        )
        .expect("rich text should rebuild");

        assert!(!updated.contains("Old paragraph"), "got {updated}");
        assert!(updated.contains("New title"), "got {updated}");
        assert!(updated.contains("dayone-cli"), "got {updated}");
    }

    #[test]
    fn build_updated_rich_text_json_preserves_non_append_edit_when_embeds_exist() {
        let existing_raw = "{\"contents\":[{\"attributes\":{\"line\":{\"header\":1}},\"text\":\"Old title\\n\"},{\"text\":\"Old paragraph.\\n\\n\"},{\"embeddedObjects\":[{\"type\":\"photo\",\"identifier\":\"IMG-1\"},{\"type\":\"map\",\"identifier\":\"MAP-1\"}]},{\"text\":\"Old tail.\"}],\"meta\":{\"version\":1,\"created\":{\"version\":1746805027,\"platform\":\"webapp\"},\"small-lines-removed\":true}}";
        let existing = json!({
            "payload": {
                "body": "# Old title\n\nOld paragraph.\n\nOld tail.",
                "richTextJSON": existing_raw
            }
        });

        let updated = build_updated_rich_text_json(
            Some(&existing),
            "# Old title\n\nChanged middle paragraph.\n\nOld tail.",
            1_700_000_000_000,
        )
        .expect("rich text should be preserved");

        assert_eq!(updated, existing_raw);
    }

    #[test]
    fn build_updated_rich_text_json_preserves_non_append_edit_when_embeds_exist_in_nodes_alias() {
        let existing_raw = "{\"nodes\":[{\"text\":\"Old title\\n\"},{\"embeddedObjects\":[{\"type\":\"photo\",\"identifier\":\"IMG-1\"}]}],\"meta\":{\"version\":1}}";
        let existing = json!({
            "payload": {
                "body": "Old title",
                "richTextJSON": existing_raw
            }
        });

        let updated =
            build_updated_rich_text_json(Some(&existing), "Changed title", 1_700_000_000_000)
                .expect("rich text should be preserved");

        assert_eq!(updated, existing_raw);
    }

    #[test]
    fn build_updated_rich_text_json_rebuilds_append_when_existing_json_invalid() {
        let existing = json!({
            "payload": {
                "body": "before",
                "richTextJSON": "{not-valid-json"
            }
        });

        let updated =
            build_updated_rich_text_json(Some(&existing), "before\n\nappended", 1_700_000_000_000)
                .expect("rich text should rebuild");

        assert!(updated.contains("before"), "got {updated}");
        assert!(updated.contains("appended"), "got {updated}");
        assert!(!updated.contains("{not-valid-json"));
        assert!(updated.contains("dayone-cli"), "got {updated}");
    }

    #[test]
    fn meaningful_rich_text_detection_distinguishes_formatted_vs_plain_nodes() {
        let formatted = json!({
            "payload": {
                "richTextJSON": "{\"contents\":[{\"attributes\":{\"line\":{\"header\":1}},\"text\":\"Title\\n\"}],\"meta\":{\"version\":1}}"
            }
        });
        let plain_cli = json!({
            "payload": {
                "richTextJSON": "{\"contents\":[{\"attributes\":{},\"text\":\"# Title\"},{\"embeddedObjects\":[{\"type\":\"photo\",\"identifier\":\"M1\"}]}],\"meta\":{\"version\":1,\"created\":{\"platform\":\"dayone-cli\"}}}"
            }
        });
        assert!(existing_entry_has_meaningful_rich_text(Some(&formatted)));
        assert!(!existing_entry_has_meaningful_rich_text(Some(&plain_cli)));
    }

    #[test]
    fn build_rich_text_json_from_body_with_placeholders_drops_the_body_separator_blank_line() {
        let body = "# Entry Title\n\n![](dayone-moment://IMG-1)";
        let placeholders = parse_dayone_moment_placeholders(body);
        let raw = build_rich_text_json_from_body_with_placeholders(
            body,
            &placeholders,
            &[new_moment("IMG-1", MediaType::Image)],
            1_000_000,
        )
        .expect("rich text should be created");
        let parsed: Value = serde_json::from_str(&raw).expect("rich text should parse");
        assert_eq!(
            parsed["contents"],
            json!([
                {"attributes": {"line": {"header": 1}}, "text": "Entry Title"},
                {"embeddedObjects": [{"type": "photo", "identifier": "IMG-1"}]}
            ])
        );
    }

    #[test]
    fn build_rich_text_json_from_body_with_placeholders_drops_the_body_separator_for_crlf() {
        let body = "# Entry Title\r\n\r\n![](dayone-moment://IMG-1)";
        let placeholders = parse_dayone_moment_placeholders(body);
        let raw = build_rich_text_json_from_body_with_placeholders(
            body,
            &placeholders,
            &[new_moment("IMG-1", MediaType::Image)],
            1_000_000,
        )
        .expect("rich text should be created");
        let parsed: Value = serde_json::from_str(&raw).expect("rich text should parse");
        assert_eq!(
            parsed["contents"],
            json!([
                {"attributes": {"line": {"header": 1}}, "text": "Entry Title"},
                {"embeddedObjects": [{"type": "photo", "identifier": "IMG-1"}]}
            ])
        );
    }

    #[test]
    fn build_rich_text_json_from_body_with_unresolved_placeholder_keeps_the_separator() {
        // Only a placeholder that becomes an embedded object drops the separator. An
        // unbound placeholder with no matching attachment stays markdown text, so the
        // fragment above it keeps whatever the user wrote.
        let body = "# Entry Title\n\n![](dayone-moment:/video/)";
        let placeholders = parse_dayone_moment_placeholders(body);
        let raw = build_rich_text_json_from_body_with_placeholders(
            body,
            &placeholders,
            &[new_moment("IMG-1", MediaType::Image)],
            1_000_000,
        )
        .expect("rich text should be created");
        let parsed: Value = serde_json::from_str(&raw).expect("rich text should parse");
        assert_eq!(parsed["contents"][0]["text"], json!("Entry Title\n\n"));
        assert!(
            parsed["contents"].as_array().is_some_and(|contents| {
                contents
                    .iter()
                    .any(|node| node["embeddedObjects"][0]["identifier"].as_str() == Some("IMG-1"))
            }),
            "the unmatched attachment should still be appended: {raw}"
        );
    }
}
