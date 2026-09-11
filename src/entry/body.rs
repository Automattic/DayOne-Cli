use std::collections::HashSet;
use std::io::Read;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime, PrimitiveDateTime, UtcOffset};

use crate::convert::model::{RtjDocument, RtjNode};
use crate::entry::attachments::{MediaType, NewMoment};
use crate::entry::rich_text::{
    build_rich_text_json_with_attachments, build_updated_rich_text_json,
    existing_entry_has_meaningful_rich_text, parse_dayone_moment_placeholders,
};
use crate::store::entries::json::rebuild_entry_content;

const TABLE_FEATURE_FLAG_BIT: u64 = 0x20000;
const SUPPORTED_ENTRY_FEATURE_FLAGS: u64 =
    0x0001 | 0x0002 | 0x0004 | 0x0008 | 0x0010 | 0x0020 | 0x0080 | TABLE_FEATURE_FLAG_BIT;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestedEntryDate {
    pub epoch_ms: i64,
    pub is_all_day: bool,
}

pub fn read_body(body: Option<String>, body_stdin: bool) -> Result<String> {
    if body_stdin && body.is_some() {
        bail!("use either --body or --body-stdin, not both");
    }
    if let Some(body) = body {
        return Ok(body);
    }
    if body_stdin {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("failed reading body from stdin")?;
        return Ok(buf);
    }
    bail!("missing body; provide --body or --body-stdin")
}

pub fn build_entry_content(
    existing_entry: Option<&Value>,
    existing_extra_fields_json: Option<&str>,
    entry_id: &str,
    body: String,
    requested_entry_date: Option<RequestedEntryDate>,
    now_ms: i64,
    new_moments: &[NewMoment],
) -> Result<Value> {
    let mut body = body;
    let preserve_existing_rich_text = existing_entry_has_meaningful_rich_text(existing_entry);
    if !preserve_existing_rich_text && !new_moments.is_empty() {
        body = resolve_body_dayone_moment_placeholders(&body, new_moments);
    }

    let existing_content =
        existing_entry_content(existing_entry, existing_extra_fields_json, entry_id);
    if let Some(content) = existing_content.as_ref() {
        ensure_entry_features_supported(content)?;
    }
    let mut content = existing_content.unwrap_or_else(|| {
        json!({
            "id": entry_id,
            "date": requested_entry_date.map(|it| it.epoch_ms).unwrap_or(now_ms) as f64,
            "timeZone": Value::Null,
            "moments": [],
            "tags": [],
            "clientMeta": {
                "creationDevice": "dayone-cli",
                "creationOSName": std::env::consts::OS,
                "creationDeviceType": "desktop",
                "creationOSVersion": "unknown",
                "source": "dayone-cli"
            }
        })
    });

    if let Some(obj) = content.as_object_mut() {
        let rich_text_json = build_rich_text_json_with_attachments(
            if preserve_existing_rich_text {
                build_updated_rich_text_json(existing_entry, &body, now_ms)
            } else {
                None
            },
            &body,
            new_moments,
            now_ms,
            !preserve_existing_rich_text,
        );
        obj.insert("id".to_owned(), Value::String(entry_id.to_owned()));
        obj.insert("body".to_owned(), Value::String(body));
        match rich_text_json {
            Some(raw) => {
                if rich_text_json_contains_table(&raw) {
                    set_feature_flag_bit(obj, TABLE_FEATURE_FLAG_BIT);
                }
                obj.insert("richTextJSON".to_owned(), Value::String(raw));
            }
            None => {
                obj.remove("richTextJSON");
            }
        }
        if let Some(requested_date) = requested_entry_date {
            obj.insert("date".to_owned(), json!(requested_date.epoch_ms as f64));
            if requested_date.is_all_day {
                obj.insert("isAllDay".to_owned(), Value::Bool(true));
                obj.insert("timeZone".to_owned(), Value::String(String::new()));
            } else {
                obj.remove("isAllDay");
                obj.insert("timeZone".to_owned(), Value::Null);
            }
        } else {
            obj.entry("date".to_owned())
                .or_insert_with(|| json!(now_ms as f64));
            obj.entry("timeZone".to_owned()).or_insert(Value::Null);
        }
        obj.entry("moments".to_owned())
            .or_insert_with(|| Value::Array(Vec::new()));
        append_new_moments(obj, new_moments);
        obj.entry("tags".to_owned())
            .or_insert_with(|| Value::Array(Vec::new()));

        let client_meta = obj
            .entry("clientMeta".to_owned())
            .or_insert_with(|| json!({}));
        if let Some(meta_obj) = client_meta.as_object_mut() {
            meta_obj
                .entry("creationDevice".to_owned())
                .or_insert_with(|| Value::String("dayone-cli".to_owned()));
            meta_obj
                .entry("creationOSName".to_owned())
                .or_insert_with(|| Value::String(std::env::consts::OS.to_owned()));
            meta_obj
                .entry("creationDeviceType".to_owned())
                .or_insert_with(|| Value::String("desktop".to_owned()));
            meta_obj
                .entry("creationOSVersion".to_owned())
                .or_insert_with(|| Value::String("unknown".to_owned()));
            let preserve_existing_source = meta_obj
                .get("source")
                .and_then(Value::as_str)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            if !preserve_existing_source {
                meta_obj.insert("source".to_owned(), Value::String("dayone-cli".to_owned()));
            }
        } else {
            obj.insert(
                "clientMeta".to_owned(),
                json!({
                    "creationDevice": "dayone-cli",
                    "creationOSName": std::env::consts::OS,
                    "creationDeviceType": "desktop",
                    "creationOSVersion": "unknown",
                    "source": "dayone-cli"
                }),
            );
        }
    }

    Ok(content)
}

pub fn parse_requested_entry_date(
    input: Option<&str>,
    all_day: bool,
) -> Result<Option<RequestedEntryDate>> {
    let Some(raw) = input else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("date cannot be empty");
    }

    if let Ok(dt) = OffsetDateTime::parse(trimmed, &Rfc3339) {
        let millis = if all_day {
            date_to_local_midnight_epoch_ms(dt.date(), trimmed)?
        } else {
            offset_datetime_to_epoch_ms(dt)?
        };
        return Ok(Some(RequestedEntryDate {
            epoch_ms: millis,
            is_all_day: all_day,
        }));
    }

    let date_format =
        time::format_description::parse("[year]-[month]-[day]").expect("date format must be valid");
    if let Ok(date) = Date::parse(trimmed, &date_format) {
        let millis = date_to_local_midnight_epoch_ms(date, trimmed)?;
        return Ok(Some(RequestedEntryDate {
            epoch_ms: millis,
            is_all_day: true,
        }));
    }

    bail!("date must be YYYY-MM-DD or RFC3339 (for example 2026-03-31T09:30:00Z)")
}

pub fn local_midnight_epoch_ms_for_epoch_ms(epoch_ms: i64) -> Result<i64> {
    let epoch_nanos = i128::from(epoch_ms) * 1_000_000;
    let instant = OffsetDateTime::from_unix_timestamp_nanos(epoch_nanos)
        .context("date value is out of supported range")?;
    let local_offset = UtcOffset::local_offset_at(instant).map_err(|_| {
        anyhow::anyhow!(
            "failed to determine local time offset for --all-day without --date; provide --date YYYY-MM-DD or RFC3339"
        )
    })?;
    let local_date = instant.to_offset(local_offset).date();
    date_to_local_midnight_epoch_ms(local_date, "--all-day without --date")
}

fn date_to_local_midnight_epoch_ms(date: Date, date_literal: &str) -> Result<i64> {
    let local_midnight_components = date.with_hms(0, 0, 0).with_context(|| {
        format!("failed to build local midnight from date value {date_literal:?}")
    })?;
    let local_offset = resolve_local_midnight_offset(local_midnight_components, date_literal)?;
    let local_midnight = local_midnight_components.assume_offset(local_offset);
    offset_datetime_to_epoch_ms(local_midnight)
}

fn offset_datetime_to_epoch_ms(dt: OffsetDateTime) -> Result<i64> {
    let millis = dt.unix_timestamp_nanos().div_euclid(1_000_000);
    i64::try_from(millis).context("date value is out of supported range")
}

fn resolve_local_midnight_offset(
    local_midnight: PrimitiveDateTime,
    date_literal: &str,
) -> Result<UtcOffset> {
    const MAX_ITERATIONS: usize = 8;

    let mut candidate = if let Ok(offset) = UtcOffset::current_local_offset() {
        offset
    } else {
        UtcOffset::local_offset_at(local_midnight.assume_utc()).map_err(|_| {
            anyhow::anyhow!(
                "failed to determine local time offset for --date value {date_literal:?}; provide RFC3339 to set an explicit time"
            )
        })?
    };

    for _ in 0..MAX_ITERATIONS {
        let utc_guess = local_midnight
            .assume_offset(candidate)
            .to_offset(UtcOffset::UTC);
        let resolved = UtcOffset::local_offset_at(utc_guess).map_err(|_| {
            anyhow::anyhow!(
                "failed to determine local time offset for --date value {date_literal:?}; provide RFC3339 to set an explicit time"
            )
        })?;
        if resolved == candidate {
            return Ok(resolved);
        }
        candidate = resolved;
    }

    bail!(
        "failed to resolve a stable local offset for --date value {date_literal:?}; provide RFC3339 to set an explicit time"
    )
}

fn append_new_moments(obj: &mut serde_json::Map<String, Value>, new_moments: &[NewMoment]) {
    if new_moments.is_empty() {
        return;
    }
    let Some(existing) = obj.get_mut("moments").and_then(Value::as_array_mut) else {
        return;
    };
    for moment in new_moments {
        let already_exists = existing
            .iter()
            .any(|item| item.get("id").and_then(Value::as_str) == Some(moment.id.as_str()));
        if already_exists {
            continue;
        }
        let mut payload = json!({
            "id": moment.id.as_str(),
            "type": moment.moment_type.api_type_str(),
            "contentType": moment.content_type.as_str(),
            "md5": moment.md5_body.as_str(),
            "fileSize": moment.file_size_bytes
        });
        if let Some(obj) = payload.as_object_mut()
            && let Some(thumbnail) = moment.thumbnail.as_ref()
        {
            obj.insert(
                "thumbnail".to_owned(),
                json!({
                    "contentType": thumbnail.content_type.as_str(),
                    "md5": thumbnail.md5.as_str(),
                    "width": thumbnail.width,
                    "height": thumbnail.height,
                    "fileSize": thumbnail.file_size_bytes
                }),
            );
        }
        if moment.moment_type == MediaType::PdfAttachment
            && let Some(obj) = payload.as_object_mut()
        {
            obj.insert("width".to_owned(), json!(0));
            obj.insert("height".to_owned(), json!(0));
            if let Some(pdf_name) = &moment.pdf_name {
                obj.insert("pdfName".to_owned(), Value::String(pdf_name.clone()));
            }
        }
        existing.push(payload);
    }
}

fn rich_text_json_contains_table(raw: &str) -> bool {
    let Ok(doc) = serde_json::from_str::<RtjDocument>(raw) else {
        return false;
    };
    doc.contents.iter().any(|node| {
        if let RtjNode::Embedded(embedded) = node {
            return embedded
                .embedded_objects
                .iter()
                .any(|obj| obj.get("type").and_then(Value::as_str) == Some("table"));
        }
        false
    })
}

fn ensure_entry_features_supported(entry: &Value) -> Result<()> {
    let Some(value) = entry.get("featureFlags").filter(|value| !value.is_null()) else {
        return Ok(());
    };
    let flags = match value {
        Value::String(raw) if raw.trim().is_empty() => 0,
        Value::String(raw) => {
            u64::from_str_radix(raw.trim(), 16).map_err(|_| invalid_feature_flags_error())?
        }
        Value::Number(number) => number.as_u64().ok_or_else(invalid_feature_flags_error)?,
        _ => return Err(invalid_feature_flags_error().into()),
    };
    let unsupported = flags & !SUPPORTED_ENTRY_FEATURE_FLAGS;
    if unsupported != 0 {
        return Err(crate::telemetry::UserError::new(format!(
            "entry uses unsupported feature flags (0x{unsupported:x}); refusing to edit because it could cause data loss"
        ))
        .into());
    }
    Ok(())
}

fn invalid_feature_flags_error() -> crate::telemetry::UserError {
    crate::telemetry::UserError::new(
        "entry has invalid featureFlags; refusing to edit because it could cause data loss",
    )
}

fn set_feature_flag_bit(obj: &mut serde_json::Map<String, Value>, bit: u64) {
    let existing = obj.get("featureFlags");
    let current = match existing {
        Some(Value::String(s)) => u64::from_str_radix(s.trim(), 16).unwrap_or(0),
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        _ => 0,
    };
    let updated = current | bit;
    let new_value = match existing {
        Some(Value::Number(_)) => json!(updated),
        _ => Value::String(format!("{updated:x}")),
    };
    obj.insert("featureFlags".to_owned(), new_value);
}

fn existing_entry_content(
    existing_entry: Option<&Value>,
    existing_extra_fields_json: Option<&str>,
    entry_id: &str,
) -> Option<Value> {
    let entry = existing_entry?;
    Some(rebuild_entry_content(
        entry,
        existing_extra_fields_json,
        entry_id,
    ))
}

fn resolve_body_dayone_moment_placeholders(body: &str, new_moments: &[NewMoment]) -> String {
    let placeholders = parse_dayone_moment_placeholders(body);
    if placeholders.is_empty() {
        return append_unmatched_moment_placeholders(body.to_owned(), new_moments, &HashSet::new());
    }

    let mut out = String::new();
    let mut cursor = 0usize;
    let mut next_attachment_idx = 0usize;
    let mut used_new_moment_ids: HashSet<&str> = HashSet::new();

    for placeholder in &placeholders {
        if placeholder.start > cursor {
            out.push_str(&body[cursor..placeholder.start]);
        }

        if let Some(identifier) = &placeholder.identifier {
            out.push_str(&render_moment_placeholder(
                placeholder.moment_type,
                identifier,
            ));
        } else if let Some(candidate) = new_moments.get(next_attachment_idx)
            && placeholder.moment_type == candidate.moment_type
        {
            next_attachment_idx += 1;
            used_new_moment_ids.insert(candidate.id.as_str());
            out.push_str(&render_moment_placeholder(
                placeholder.moment_type,
                &candidate.id,
            ));
        } else {
            out.push_str(&body[placeholder.start..placeholder.end]);
        }

        cursor = placeholder.end;
    }
    if cursor < body.len() {
        out.push_str(&body[cursor..]);
    }

    append_unmatched_moment_placeholders(out, new_moments, &used_new_moment_ids)
}

fn append_unmatched_moment_placeholders(
    mut body: String,
    new_moments: &[NewMoment],
    used_new_moment_ids: &HashSet<&str>,
) -> String {
    for moment in new_moments {
        if used_new_moment_ids.contains(moment.id.as_str()) {
            continue;
        }
        if !body.is_empty() && !body.ends_with("\n\n") {
            if body.ends_with('\n') {
                body.push('\n');
            } else {
                body.push_str("\n\n");
            }
        }
        body.push_str(&render_moment_placeholder(moment.moment_type, &moment.id));
    }
    body
}

fn render_moment_placeholder(media_type: MediaType, identifier: &str) -> String {
    match media_type.placeholder_path_segment() {
        Some(segment) => format!("![](dayone-moment:/{segment}/{identifier})"),
        None => format!("![](dayone-moment://{identifier})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::attachments::{MediaType, NewMoment, NewMomentThumbnail};

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
    fn rich_text_table_detection_supports_nodes_alias() {
        let raw = serde_json::json!({
            "nodes": [
                {
                    "embeddedObjects": [
                        { "type": "table", "rows": [] }
                    ]
                }
            ]
        })
        .to_string();
        assert!(rich_text_json_contains_table(&raw));
    }

    #[test]
    fn build_entry_content_refuses_unsupported_or_invalid_feature_flags() {
        for feature_flags in [json!("100"), json!("400"), json!("nope")] {
            let existing = json!({
                "id": "protected-entry",
                "body": "before",
                "featureFlags": feature_flags
            });
            let error = build_entry_content(
                Some(&existing),
                None,
                "protected-entry",
                "after".to_owned(),
                None,
                1_000,
                &[],
            )
            .expect_err("unsafe edit should be refused");
            assert!(error.to_string().contains("refusing to edit"));
            assert!(crate::telemetry::is_user_error(&error));
        }
    }

    #[test]
    fn build_entry_content_allows_supported_feature_flags() {
        let existing = json!({
            "id": "supported-entry",
            "body": "before",
            "featureFlags": "b3"
        });
        let content = build_entry_content(
            Some(&existing),
            None,
            "supported-entry",
            "after".to_owned(),
            None,
            1_000,
            &[],
        )
        .expect("supported edit should succeed");
        assert_eq!(content.get("body").and_then(Value::as_str), Some("after"));
    }

    #[test]
    fn build_entry_content_preserves_existing_moments_and_tags_on_rewrite() {
        let existing = json!({
            "payload": {
                "id": "strava-1",
                "date": 1234.0,
                "moments": [{"identifier":"m1"}],
                "tags": ["run"],
                "body": "before",
                "richTextJSON": "{\"contents\":[{\"attributes\":{},\"text\":\"before\"},{\"embeddedObjects\":[{\"type\":\"photo\",\"identifier\":\"m1\"}]}],\"meta\":{\"version\":1,\"created\":{\"version\":1746805027,\"platform\":\"webapp\"},\"small-lines-removed\":true}}"
            }
        });
        let content = build_entry_content(
            Some(&existing),
            None,
            "strava-1",
            "after".to_owned(),
            None,
            9999,
            &[],
        )
        .expect("content build should succeed");
        assert_eq!(content.get("body").and_then(Value::as_str), Some("after"));
        assert_eq!(content.get("date").and_then(Value::as_f64), Some(1234.0));
        assert_eq!(content["moments"][0]["identifier"], "m1");
        assert_eq!(content["tags"][0], "run");
        assert_eq!(
            content.get("richTextJSON").and_then(Value::as_str),
            Some(
                "{\"contents\":[{\"attributes\":{},\"text\":\"before\"},{\"embeddedObjects\":[{\"type\":\"photo\",\"identifier\":\"m1\"}]}],\"meta\":{\"version\":1,\"created\":{\"version\":1746805027,\"platform\":\"webapp\"},\"small-lines-removed\":true}}"
            )
        );
    }

    #[test]
    fn build_entry_content_defaults_when_existing_missing() {
        let content =
            build_entry_content(None, None, "entry-1", "hello".to_owned(), None, 123, &[])
                .expect("content build should succeed");
        assert_eq!(content.get("id").and_then(Value::as_str), Some("entry-1"));
        assert_eq!(content.get("body").and_then(Value::as_str), Some("hello"));
        assert!(content.get("moments").and_then(Value::as_array).is_some());
        assert!(content.get("tags").and_then(Value::as_array).is_some());
        assert!(content.get("richTextJSON").is_none());
    }

    #[test]
    fn build_entry_content_preserves_unknown_extra_fields() {
        let existing = json!({
            "id": "future-1",
            "body": "before",
            "clientMeta": {
                "source": "mobile"
            }
        });
        let content = build_entry_content(
            Some(&existing),
            Some(r#"{"futureClientField":{"nested":true},"body":"stale"}"#),
            "future-1",
            "after".to_owned(),
            Some(RequestedEntryDate {
                epoch_ms: 123,
                is_all_day: false,
            }),
            123,
            &[],
        )
        .expect("content build should succeed");
        assert_eq!(content.get("body").and_then(Value::as_str), Some("after"));
        assert_eq!(content["futureClientField"]["nested"], Value::Bool(true));
        assert_eq!(content["clientMeta"]["source"], "mobile");
        assert_eq!(content.get("date").and_then(Value::as_f64), Some(123.0));
    }

    #[test]
    fn build_entry_content_uses_requested_date_when_provided() {
        let content = build_entry_content(
            None,
            None,
            "entry-2",
            "hello".to_owned(),
            Some(RequestedEntryDate {
                epoch_ms: 1_234_567,
                is_all_day: false,
            }),
            999,
            &[],
        )
        .expect("content build should succeed");
        assert_eq!(
            content.get("date").and_then(Value::as_f64),
            Some(1_234_567.0)
        );
    }

    #[test]
    fn build_entry_content_overrides_existing_date_when_requested() {
        let existing = json!({
            "payload": {
                "id": "entry-3",
                "date": 42.0,
                "body": "before"
            }
        });
        let content = build_entry_content(
            Some(&existing),
            None,
            "entry-3",
            "after".to_owned(),
            Some(RequestedEntryDate {
                epoch_ms: 77_000,
                is_all_day: false,
            }),
            1_000,
            &[],
        )
        .expect("content build should succeed");
        assert_eq!(content.get("date").and_then(Value::as_f64), Some(77_000.0));
    }

    #[test]
    fn parse_requested_entry_date_accepts_rfc3339() {
        let parsed = parse_requested_entry_date(Some("2026-03-30T18:45:00Z"), false)
            .expect("rfc3339 should parse");
        assert!(parsed.is_some_and(|it| !it.is_all_day));
    }

    #[test]
    fn parse_requested_entry_date_accepts_yyyy_mm_dd() {
        let parsed =
            parse_requested_entry_date(Some("2026-03-30"), false).expect("date-only should parse");
        assert!(parsed.is_some_and(|it| it.is_all_day));
    }

    #[test]
    fn parse_requested_entry_date_rejects_invalid_format() {
        let err = parse_requested_entry_date(Some("03/30/2026"), false)
            .expect_err("invalid format should fail");
        assert!(err.to_string().contains("YYYY-MM-DD or RFC3339"));
    }

    #[test]
    fn parse_requested_entry_date_coerces_rfc3339_to_all_day_when_requested() {
        let coerced = parse_requested_entry_date(Some("2026-03-30T18:45:00Z"), true)
            .expect("rfc3339 should parse")
            .expect("date should be present");
        let date_only = parse_requested_entry_date(Some("2026-03-30"), false)
            .expect("date-only should parse")
            .expect("date should be present");
        assert!(coerced.is_all_day);
        assert_eq!(coerced.epoch_ms, date_only.epoch_ms);
    }

    #[test]
    fn local_midnight_epoch_ms_for_epoch_ms_matches_local_midnight_of_instant_date() {
        let instant_epoch_ms = OffsetDateTime::parse("2026-03-30T18:45:00Z", &Rfc3339)
            .expect("instant should parse")
            .unix_timestamp_nanos()
            .div_euclid(1_000_000) as i64;
        let local_midnight_epoch_ms = local_midnight_epoch_ms_for_epoch_ms(instant_epoch_ms)
            .expect("local midnight should resolve");
        let local_midnight_dt = OffsetDateTime::from_unix_timestamp_nanos(
            i128::from(local_midnight_epoch_ms) * 1_000_000,
        )
        .expect("local midnight epoch should convert to instant");
        let local_offset = UtcOffset::local_offset_at(local_midnight_dt)
            .expect("local offset should resolve for midnight instant");
        let local_midnight_time = local_midnight_dt.to_offset(local_offset).time();
        assert_eq!(local_midnight_time.hour(), 0);
        assert_eq!(local_midnight_time.minute(), 0);
        assert_eq!(local_midnight_time.second(), 0);
    }

    #[test]
    fn offset_datetime_to_epoch_ms_uses_floor_division_for_pre_epoch_instants() {
        let dt =
            OffsetDateTime::from_unix_timestamp_nanos(-1).expect("pre-epoch instant should parse");
        let millis = offset_datetime_to_epoch_ms(dt).expect("epoch millis conversion should work");
        assert_eq!(millis, -1);
    }

    #[test]
    fn build_entry_content_sets_all_day_shape_for_date_only_input() {
        let content = build_entry_content(
            None,
            None,
            "entry-4",
            "hello".to_owned(),
            Some(RequestedEntryDate {
                epoch_ms: 1_234_567,
                is_all_day: true,
            }),
            999,
            &[],
        )
        .expect("content build should succeed");
        assert_eq!(content.get("isAllDay").and_then(Value::as_bool), Some(true));
        assert_eq!(content.get("timeZone").and_then(Value::as_str), Some(""));
    }

    #[test]
    fn rich_text_rebuilds_when_existing_json_is_invalid_and_body_changes() {
        let existing = json!({
            "payload": {
                "id": "strava-2",
                "body": "before",
                "richTextJSON": "{not-valid-json"
            }
        });
        let content = build_entry_content(
            Some(&existing),
            None,
            "strava-2",
            "after".to_owned(),
            None,
            42_000,
            &[],
        )
        .expect("content build should succeed");
        let rich_text_raw = content
            .get("richTextJSON")
            .and_then(Value::as_str)
            .expect("richTextJSON should be rebuilt");
        assert!(rich_text_raw.contains("after"), "got {rich_text_raw}");
        assert!(!rich_text_raw.contains("{not-valid-json"));
    }

    #[test]
    fn rich_text_append_preserves_embedded_nodes_for_append_only_update() {
        let existing = json!({
            "payload": {
                "id": "strava-3",
                "body": "old body",
                "richTextJSON": "{\"contents\":[{\"attributes\":{\"line\":{\"header\":1}},\"text\":\"Morning Run\\n\"},{\"attributes\":{},\"text\":\"Old content\"},{\"embeddedObjects\":[{\"type\":\"photo\",\"identifier\":\"IMG-1\"},{\"type\":\"map\",\"identifier\":\"MAP-1\"}]}],\"meta\":{\"version\":1,\"created\":{\"version\":1746805027,\"platform\":\"webapp\"},\"small-lines-removed\":true}}"
            }
        });
        let content = build_entry_content(
            Some(&existing),
            None,
            "strava-3",
            "old body\n\nFelt great".to_owned(),
            None,
            1_000,
            &[],
        )
        .expect("content build should succeed");
        let rich_text_raw = content
            .get("richTextJSON")
            .and_then(Value::as_str)
            .expect("richTextJSON should exist");
        let rich_text: RtjDocument =
            serde_json::from_str(rich_text_raw).expect("rich text should parse");

        assert_eq!(rich_text.contents.len(), 4);
        let embedded = match &rich_text.contents[2] {
            RtjNode::Embedded(node) => node,
            _ => panic!("third node should be embeddedObjects"),
        };
        assert_eq!(embedded.embedded_objects.len(), 2);
        assert_eq!(
            embedded.embedded_objects[0]
                .get("identifier")
                .and_then(Value::as_str),
            Some("IMG-1")
        );
        let appended = match &rich_text.contents[3] {
            RtjNode::Text(node) => node.text.as_str(),
            _ => panic!("fourth node should be appended text"),
        };
        assert_eq!(appended, "\n\nFelt great");
    }

    #[test]
    fn rich_text_append_detects_append_with_crlf_new_body() {
        let existing = json!({
            "payload": {
                "id": "strava-4",
                "body": "Morning Run\nDistance: 5km",
                "richTextJSON": "{\"contents\":[{\"attributes\":{},\"text\":\"Morning Run\\nDistance: 5km\"},{\"embeddedObjects\":[{\"type\":\"photo\",\"identifier\":\"IMG-9\"}]}],\"meta\":{\"version\":1,\"created\":{\"version\":1746805027,\"platform\":\"webapp\"},\"small-lines-removed\":true}}"
            }
        });
        let content = build_entry_content(
            Some(&existing),
            None,
            "strava-4",
            "Morning Run\r\nDistance: 5km\r\n\r\nFelt great".to_owned(),
            None,
            1_000,
            &[],
        )
        .expect("content build should succeed");

        let rich_text_raw = content
            .get("richTextJSON")
            .and_then(Value::as_str)
            .expect("richTextJSON should exist");
        let rich_text: RtjDocument =
            serde_json::from_str(rich_text_raw).expect("rich text should parse");

        assert_eq!(rich_text.contents.len(), 3);
        assert!(matches!(&rich_text.contents[1], RtjNode::Embedded(_)));
        let appended = match &rich_text.contents[2] {
            RtjNode::Text(node) => node.text.as_str(),
            _ => panic!("third node should be appended text"),
        };
        assert_eq!(appended, "\n\nFelt great");
    }

    #[test]
    fn rich_text_wholesale_rewrite_rebuilds_content_instead_of_preserving_stale_rtj() {
        let existing = json!({
            "payload": {
                "id": "rewrite-1",
                "body": "# Old title\n\nOld paragraph.\n\nOld tail.",
                "richTextJSON": "{\"contents\":[{\"attributes\":{\"line\":{\"header\":1}},\"text\":\"Old title\\n\"},{\"text\":\"Old paragraph.\\n\\nOld tail.\"}],\"meta\":{\"version\":1,\"created\":{\"version\":1746805027,\"platform\":\"webapp\"},\"small-lines-removed\":true}}"
            }
        });
        let content = build_entry_content(
            Some(&existing),
            None,
            "rewrite-1",
            "New title\n\nCompletely different content.".to_owned(),
            None,
            1_000,
            &[],
        )
        .expect("content build should succeed");

        assert_eq!(
            content.get("body").and_then(Value::as_str),
            Some("New title\n\nCompletely different content.")
        );

        let rich_text_raw = content
            .get("richTextJSON")
            .and_then(Value::as_str)
            .expect("richTextJSON should exist");
        assert!(
            !rich_text_raw.contains("Old paragraph"),
            "wholesale rewrite should not preserve stale paragraph RTJ: {rich_text_raw}"
        );
        assert!(
            rich_text_raw.contains("New title"),
            "wholesale rewrite should rebuild RTJ from new body: {rich_text_raw}"
        );
    }

    #[test]
    fn build_entry_content_sets_table_feature_flag_for_generated_rich_text() {
        let content = build_entry_content(
            None,
            None,
            "entry-table-1",
            "| h1 |\n| --- |\n| v1 |\n\n![](dayone-moment://)\n".to_owned(),
            None,
            1_000,
            &[new_moment("IMG-T1", MediaType::Image)],
        )
        .expect("content build should succeed");
        assert_eq!(
            content.get("featureFlags").and_then(Value::as_str),
            Some("20000")
        );
    }

    #[test]
    fn build_entry_content_ors_table_feature_flag_with_existing_string_flags() {
        let existing = json!({
            "payload": {
                "id": "entry-table-2",
                "body": "before",
                "featureFlags": "11"
            }
        });
        let content = build_entry_content(
            Some(&existing),
            None,
            "entry-table-2",
            "| h1 |\n| --- |\n| v1 |\n\n![](dayone-moment://)\n".to_owned(),
            None,
            1_000,
            &[new_moment("IMG-T2", MediaType::Image)],
        )
        .expect("content build should succeed");
        assert_eq!(
            content.get("featureFlags").and_then(Value::as_str),
            Some("20011")
        );
    }

    #[test]
    fn append_new_moments_adds_unique_ids_only() {
        let mut obj = json!({
            "moments": [{
                "id": "M1",
                "type": "image",
                "contentType": "image/jpeg"
            }]
        })
        .as_object()
        .expect("json object should exist")
        .clone();
        append_new_moments(
            &mut obj,
            &[
                NewMoment {
                    id: "M1".to_owned(),
                    moment_type: MediaType::Image,
                    content_type: "image/jpeg".to_owned(),
                    md5_body: "abc".to_owned(),
                    file_size_bytes: 42,
                    pdf_name: None,
                    thumbnail: None,
                },
                NewMoment {
                    id: "M2".to_owned(),
                    moment_type: MediaType::PdfAttachment,
                    content_type: "application/pdf".to_owned(),
                    md5_body: "def".to_owned(),
                    file_size_bytes: 55,
                    pdf_name: Some("sample-pdf".to_owned()),
                    thumbnail: Some(NewMomentThumbnail {
                        content_type: "image/jpeg".to_owned(),
                        md5: "thumb-md5".to_owned(),
                        width: 100,
                        height: 50,
                        file_size_bytes: 12,
                    }),
                },
            ],
        );
        let moments = obj
            .get("moments")
            .and_then(Value::as_array)
            .expect("moments array should exist");
        assert_eq!(moments.len(), 2);
        assert_eq!(moments[1].get("id").and_then(Value::as_str), Some("M2"));
        assert_eq!(
            moments[1].get("type").and_then(Value::as_str),
            Some("pdfAttachment")
        );
        assert_eq!(
            moments[1].get("pdfName").and_then(Value::as_str),
            Some("sample-pdf")
        );
        assert_eq!(moments[1].get("width").and_then(Value::as_i64), Some(0));
        assert_eq!(moments[1].get("height").and_then(Value::as_i64), Some(0));
        assert_eq!(
            moments[1]
                .get("thumbnail")
                .and_then(|thumb| thumb.get("md5"))
                .and_then(Value::as_str),
            Some("thumb-md5")
        );
    }

    #[test]
    fn append_new_moments_omits_thumbnail_when_missing() {
        let mut obj = json!({
            "moments": []
        })
        .as_object()
        .expect("json object should exist")
        .clone();
        append_new_moments(
            &mut obj,
            &[NewMoment {
                id: "M3".to_owned(),
                moment_type: MediaType::Image,
                content_type: "image/jpeg".to_owned(),
                md5_body: "abc".to_owned(),
                file_size_bytes: 42,
                pdf_name: None,
                thumbnail: None,
            }],
        );
        let moments = obj
            .get("moments")
            .and_then(Value::as_array)
            .expect("moments array should exist");
        assert_eq!(moments.len(), 1);
        assert!(moments[0].get("thumbnail").is_none());
    }

    #[test]
    fn render_moment_placeholder_uses_dayone_web_forms() {
        assert_eq!(
            render_moment_placeholder(MediaType::Image, "IMG-1"),
            "![](dayone-moment://IMG-1)"
        );
        assert_eq!(
            render_moment_placeholder(MediaType::Video, "VID-1"),
            "![](dayone-moment:/video/VID-1)"
        );
        assert_eq!(
            render_moment_placeholder(MediaType::Audio, "AUD-1"),
            "![](dayone-moment:/audio/AUD-1)"
        );
        assert_eq!(
            render_moment_placeholder(MediaType::PdfAttachment, "PDF-1"),
            "![](dayone-moment:/pdfAttachment/PDF-1)"
        );
    }

    #[test]
    fn resolve_body_placeholders_preserves_existing_identifier_markers() {
        let body = "Before\n\n![](dayone-moment://IMG1)\n\nAfter";
        let resolved = resolve_body_dayone_moment_placeholders(body, &[]);
        assert_eq!(resolved, body);
    }

    #[test]
    fn resolve_body_placeholders_maps_unbound_markers_in_order() {
        let body = "One\n\n![](dayone-moment://)\n\nTwo\n\n![](dayone-moment:/pdfAttachment/)";
        let resolved = resolve_body_dayone_moment_placeholders(
            body,
            &[
                new_moment("IMG-1", MediaType::Image),
                new_moment("PDF-1", MediaType::PdfAttachment),
            ],
        );
        assert_eq!(
            resolved,
            "One\n\n![](dayone-moment://IMG-1)\n\nTwo\n\n![](dayone-moment:/pdfAttachment/PDF-1)"
        );
    }

    #[test]
    fn resolve_body_unmatched_attachments_append_as_placeholders() {
        let body = "Intro\n\n![](dayone-moment://)\n\nTail";
        let resolved = resolve_body_dayone_moment_placeholders(
            body,
            &[
                new_moment("IMG-1", MediaType::Image),
                new_moment("IMG-2", MediaType::Image),
            ],
        );
        assert_eq!(
            resolved,
            "Intro\n\n![](dayone-moment://IMG-1)\n\nTail\n\n![](dayone-moment://IMG-2)"
        );
    }

    #[test]
    fn resolve_body_type_mismatch_keeps_original_placeholder_and_appends_attachment() {
        let body = "Intro\n\n![](dayone-moment:/video/)\n\nTail";
        let resolved =
            resolve_body_dayone_moment_placeholders(body, &[new_moment("IMG-1", MediaType::Image)]);
        assert!(resolved.contains("![](dayone-moment:/video/)"));
        assert!(resolved.ends_with("![](dayone-moment://IMG-1)"));
    }

    #[test]
    fn resolve_body_mismatch_does_not_consume_attachment_for_later_matching_placeholder() {
        let body = "![](dayone-moment:/video/)\n\n![](dayone-moment://)";
        let resolved =
            resolve_body_dayone_moment_placeholders(body, &[new_moment("IMG-1", MediaType::Image)]);
        assert_eq!(
            resolved,
            "![](dayone-moment:/video/)\n\n![](dayone-moment://IMG-1)"
        );
    }

    #[test]
    fn build_entry_content_appends_attachment_without_a_blank_line_gap() {
        // Regression for DAYONE-1185: the blank line separator added to the body
        // markdown was copied into the preceding rich text line, so clients showed
        // an extra blank line between the text and the attached image.
        let content = build_entry_content(
            None,
            None,
            "entry-attach-1",
            "# Entry Title".to_owned(),
            None,
            1_000_000,
            &[new_moment("IMG-A1", MediaType::Image)],
        )
        .expect("content build should succeed");

        assert_eq!(
            content.get("body").and_then(Value::as_str),
            Some("# Entry Title\n\n![](dayone-moment://IMG-A1)")
        );

        let raw = content
            .get("richTextJSON")
            .and_then(Value::as_str)
            .expect("richTextJSON should exist");
        let parsed: Value = serde_json::from_str(raw).expect("rich text should parse");
        assert_eq!(
            parsed["contents"],
            json!([
                {"attributes": {"line": {"header": 1}}, "text": "Entry Title\n"},
                {"embeddedObjects": [{"type": "photo", "identifier": "IMG-A1"}]}
            ])
        );
    }

    #[test]
    fn build_entry_content_appends_paragraph_and_multiple_attachments_without_blank_lines() {
        let content = build_entry_content(
            None,
            None,
            "entry-attach-2",
            "Morning run.".to_owned(),
            None,
            1_000_000,
            &[
                new_moment("IMG-A2", MediaType::Image),
                new_moment("PDF-A2", MediaType::PdfAttachment),
            ],
        )
        .expect("content build should succeed");

        let raw = content
            .get("richTextJSON")
            .and_then(Value::as_str)
            .expect("richTextJSON should exist");
        let parsed: Value = serde_json::from_str(raw).expect("rich text should parse");
        assert_eq!(
            parsed["contents"],
            json!([
                {"text": "Morning run.\n"},
                {"embeddedObjects": [{"type": "photo", "identifier": "IMG-A2"}]},
                {
                    "embeddedObjects": [{
                        "type": "pdfAttachment",
                        "identifier": "PDF-A2"
                    }]
                }
            ])
        );
    }
}
