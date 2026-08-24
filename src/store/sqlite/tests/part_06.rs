use std::collections::HashMap;

use super::super::*;
use super::common::{roundtrip, roundtrip_with_user_edit_date, test_store_path};
use crate::store::entries::json::{
    collect_merged_extra_fields, parse_iso_to_epoch_ms, rebuild_entry_content,
};
use proptest::collection::{hash_map, vec};
use proptest::prelude::*;
use rusqlite::params;
use serde_json::{Map, Value, json};

const FIXED_USER_EDIT_DATE: &str = "2026-03-01T12:00:00.000Z";

#[derive(Debug, PartialEq)]
struct EntryRowSnapshot {
    date: Option<f64>,
    user_edit_date: Option<String>,
    title: Option<String>,
    body: Option<String>,
    rich_text_json: Option<String>,
    time_zone: Option<String>,
    tags_json: Option<String>,
    moments_json: Option<String>,
    client_meta_json: Option<String>,
    extra_fields_json: Option<String>,
}

fn ascii_string(max_len: usize) -> BoxedStrategy<String> {
    vec(32u8..=126u8, 0..=max_len)
        .prop_map(|bytes| bytes.into_iter().map(char::from).collect())
        .boxed()
}

fn identifier(prefix: &'static str, max_suffix_len: usize) -> BoxedStrategy<String> {
    vec(
        prop_oneof![
            Just('_'),
            Just('-'),
            Just('x'),
            Just('y'),
            Just('z'),
            (b'a'..=b'z').prop_map(char::from),
            (b'0'..=b'9').prop_map(char::from)
        ],
        1..=max_suffix_len,
    )
    .prop_map(move |suffix| {
        let suffix: String = suffix.into_iter().collect();
        format!("{prefix}{suffix}")
    })
    .boxed()
}

fn nested_json_strategy() -> BoxedStrategy<Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-10_000i64..10_000i64).prop_map(|n| json!(n)),
        ascii_string(16).prop_map(|s| Value::String(s)),
    ];

    leaf.prop_recursive(3, 32, 4, |inner| {
        prop_oneof![
            vec(inner.clone(), 0..=3).prop_map(Value::Array),
            hash_map(identifier("k", 8), inner, 0..=3).prop_map(|map| {
                let mut obj = Map::new();
                for (k, v) in map {
                    obj.insert(k, v);
                }
                Value::Object(obj)
            }),
        ]
    })
    .boxed()
}

fn rich_text_json_strategy() -> BoxedStrategy<Value> {
    let inline_text = ascii_string(18).prop_map(|text| json!({ "text": text }));
    let inline_bold = ascii_string(18).prop_map(|text| {
        json!({
            "text": text,
            "attributes": { "bold": true }
        })
    });
    let inline_italic = ascii_string(18).prop_map(|text| {
        json!({
            "text": text,
            "attributes": { "italic": true }
        })
    });
    let line_attrs = prop_oneof![
        Just(json!({})),
        Just(json!({ "heading": { "level": 1 } })),
        Just(json!({ "heading": { "level": 2 } })),
        Just(json!({ "blockquote": {} })),
    ];
    let paragraph = (
        line_attrs,
        vec(prop_oneof![inline_text, inline_bold, inline_italic], 1..=3),
    )
        .prop_map(|(line, children)| {
            json!({
                "attributes": { "line": line },
                "children": children
            })
        });

    prop_oneof![
        Just(json!(r#"{"meta":{"version":1}}"#)),
        ascii_string(24).prop_map(Value::String),
        Just(json!({
            "meta": { "version": 1 },
            "nodes": [{ "text": "hello", "attributes": { "line": {} } }]
        })),
        Just(json!([
            { "text": "hello" },
            { "text": "world" }
        ])),
        vec(paragraph, 1..=4).prop_map(|nodes| {
            json!({
                "meta": {
                    "version": 1,
                    "created": {
                        "platform": "dayone-cli-test",
                        "version": 1
                    }
                },
                "nodes": nodes
            })
        }),
    ]
    .boxed()
}

fn date_value_strategy() -> BoxedStrategy<Value> {
    prop_oneof![
        (0i64..4_102_444_800_000i64).prop_map(|ms| json!(ms)),
        (0i64..4_102_444_800_000i64).prop_map(|ms| json!(ms.to_string())),
        prop::sample::select(vec![
            "2026-03-01T00:00:00Z".to_owned(),
            "2024-01-01T12:34:56Z".to_owned(),
            "2025-11-30T23:59:59Z".to_owned(),
            "2026-03-01T12:34:56-08:00".to_owned(),
            "2026-07-15T09:10:11+05:30".to_owned(),
        ])
        .prop_map(|s| json!(s)),
    ]
    .boxed()
}

fn tags_strategy() -> BoxedStrategy<Value> {
    vec(ascii_string(12), 0..=3)
        .prop_map(|tags| Value::Array(tags.into_iter().map(Value::String).collect()))
        .boxed()
}

fn moments_strategy() -> BoxedStrategy<Value> {
    vec(
        (
            identifier("m", 6),
            prop::sample::select(vec!["photo", "video"]),
        ),
        0..=2,
    )
    .prop_map(|moments| {
        let values = moments
            .into_iter()
            .map(|(id, kind)| json!({ "id": id, "type": kind }))
            .collect();
        Value::Array(values)
    })
    .boxed()
}

fn client_meta_strategy() -> BoxedStrategy<Value> {
    (
        ascii_string(12),
        ascii_string(12),
        prop::sample::select(vec!["macOS", "iOS", "Android", "Windows"]),
    )
        .prop_map(|(device_id, device_name, os_name)| {
            json!({
                "deviceId": device_id,
                "deviceName": device_name,
                "creationOSName": os_name
            })
        })
        .boxed()
}

fn known_field_entries_strategy() -> BoxedStrategy<Vec<(String, Value)>> {
    (
        proptest::option::of(date_value_strategy()),
        proptest::option::of(ascii_string(20).prop_map(|s| json!(s))),
        proptest::option::of(ascii_string(40).prop_map(|s| json!(s))),
        proptest::option::of(rich_text_json_strategy()),
        proptest::option::of(prop_oneof![
            Just(json!(null)),
            prop::sample::select(vec![
                "UTC".to_owned(),
                "America/Los_Angeles".to_owned(),
                "Europe/Berlin".to_owned(),
            ])
            .prop_map(|s| json!(s))
        ]),
        proptest::option::of(prop_oneof![Just(json!(null)), tags_strategy()]),
        proptest::option::of(prop_oneof![Just(json!(null)), moments_strategy()]),
        proptest::option::of(prop_oneof![Just(json!(null)), client_meta_strategy()]),
    )
        .prop_map(
            |(date, title, body, rich_text_json, time_zone, tags, moments, client_meta)| {
                let mut fields = Vec::new();
                if let Some(value) = date {
                    fields.push(("date".to_owned(), value));
                }
                if let Some(value) = title {
                    fields.push(("title".to_owned(), value));
                }
                if let Some(value) = body {
                    fields.push(("body".to_owned(), value));
                }
                if let Some(value) = rich_text_json {
                    fields.push(("richTextJSON".to_owned(), value));
                }
                if let Some(value) = time_zone {
                    fields.push(("timeZone".to_owned(), value));
                }
                if let Some(value) = tags {
                    fields.push(("tags".to_owned(), value));
                }
                if let Some(value) = moments {
                    fields.push(("moments".to_owned(), value));
                }
                if let Some(value) = client_meta {
                    fields.push(("clientMeta".to_owned(), value));
                }
                fields
            },
        )
        .boxed()
}

fn documented_unknown_field_entries_strategy() -> BoxedStrategy<Vec<(String, Value)>> {
    let candidate = prop_oneof![
        (-10_000i64..10_000i64).prop_map(|v| ("userEditDate".to_owned(), json!(v))),
        (0u32..=1_000_000).prop_map(|v| ("editingTime".to_owned(), json!(v))),
        (0u32..=86_400).prop_map(|v| ("duration".to_owned(), json!(v))),
        any::<bool>().prop_map(|v| ("isAllDay".to_owned(), json!(v))),
        any::<bool>().prop_map(|v| ("isPinned".to_owned(), json!(v))),
        any::<bool>().prop_map(|v| ("starred".to_owned(), json!(v))),
        ascii_string(8).prop_map(|s| ("featureFlags".to_owned(), json!(s))),
        ascii_string(12).prop_map(|s| ("activity".to_owned(), json!(s))),
        nested_json_strategy().prop_map(|v| ("location".to_owned(), v)),
        nested_json_strategy().prop_map(|v| ("weather".to_owned(), v)),
        nested_json_strategy().prop_map(|v| ("steps".to_owned(), v)),
        nested_json_strategy().prop_map(|v| ("music".to_owned(), v)),
        ascii_string(16).prop_map(|s| ("ownerUserId".to_owned(), json!(s))),
        ascii_string(16).prop_map(|s| ("creatorUserId".to_owned(), json!(s))),
        ascii_string(16).prop_map(|s| ("editorUserId".to_owned(), json!(s))),
        ascii_string(16).prop_map(|s| ("lastEditingDeviceID".to_owned(), json!(s))),
        ascii_string(16).prop_map(|s| ("lastEditingDeviceName".to_owned(), json!(s))),
        prop_oneof![Just(json!(null)), ascii_string(16).prop_map(|s| json!(s))]
            .prop_map(|v| ("templateID".to_owned(), v)),
        prop_oneof![Just(json!(null)), ascii_string(16).prop_map(|s| json!(s))]
            .prop_map(|v| ("promptID".to_owned(), v)),
        prop_oneof![Just(json!(null)), ascii_string(16).prop_map(|s| json!(s))]
            .prop_map(|v| ("entryType".to_owned(), v)),
        prop_oneof![Just(json!(null)), ascii_string(16).prop_map(|s| json!(s))]
            .prop_map(|v| ("unread_marker_id".to_owned(), v)),
        any::<bool>().prop_map(|v| ("is_shared".to_owned(), json!(v))),
    ];

    vec(candidate, 0..=8)
        .prop_map(|pairs| {
            let mut fields = Map::new();
            for (key, value) in pairs {
                fields.insert(key, value);
            }
            fields.into_iter().collect()
        })
        .boxed()
}

fn unknown_fields_strategy(prefix: &'static str) -> BoxedStrategy<HashMap<String, Value>> {
    hash_map(identifier(prefix, 10), nested_json_strategy(), 0..=3).boxed()
}

fn reserved_fields_strategy() -> BoxedStrategy<Vec<(String, Value)>> {
    (
        proptest::option::of(ascii_string(12).prop_map(|s| json!(s))),
        proptest::option::of(
            prop::sample::select(vec![
                "2026-03-01T00:00:00Z".to_owned(),
                "2026-03-02T10:30:00Z".to_owned(),
            ])
            .prop_map(|s| json!(s)),
        ),
        proptest::option::of(
            prop::sample::select(vec![
                "2026-03-01T00:00:00Z".to_owned(),
                "2026-03-02T10:30:00Z".to_owned(),
            ])
            .prop_map(|s| json!(s)),
        ),
        proptest::option::of(any::<bool>().prop_map(|v| json!(v))),
        proptest::option::of(any::<bool>().prop_map(|v| json!(v))),
        proptest::option::of((0u32..=10).prop_map(|v| json!(v))),
        proptest::option::of(
            prop::sample::select(vec![
                "2026-03-27T15:00:00Z".to_owned(),
                "2026-03-28T16:45:00Z".to_owned(),
            ])
            .prop_map(|s| json!(s)),
        ),
    )
        .prop_map(
            |(
                journal_id,
                updated_at,
                user_edit_date,
                is_deleted,
                needs_sync,
                revision,
                deleted_at,
            )| {
                let mut fields = Vec::new();
                if let Some(value) = journal_id {
                    fields.push(("journal_id".to_owned(), value));
                }
                if let Some(value) = updated_at {
                    fields.push(("updated_at".to_owned(), value));
                }
                if let Some(value) = user_edit_date {
                    fields.push(("user_edit_date".to_owned(), value));
                }
                if let Some(value) = is_deleted {
                    fields.push(("is_deleted".to_owned(), value));
                }
                if let Some(value) = needs_sync {
                    fields.push(("needs_sync".to_owned(), value));
                }
                if let Some(value) = revision {
                    fields.push(("revision".to_owned(), value));
                }
                if let Some(value) = deleted_at {
                    fields.push(("deleted_at".to_owned(), value));
                }
                fields
            },
        )
        .boxed()
}

fn entry_strategy() -> BoxedStrategy<Value> {
    (
        identifier("entry-", 10),
        known_field_entries_strategy(),
        documented_unknown_field_entries_strategy(),
        unknown_fields_strategy("futureRoot"),
        proptest::option::of(known_field_entries_strategy()),
        proptest::option::of(unknown_fields_strategy("futurePayload")),
        reserved_fields_strategy(),
    )
        .prop_map(
            |(
                id,
                known_fields,
                documented_unknown_fields,
                unknown_root_fields,
                payload_known_fields,
                payload_unknown_fields,
                reserved_fields,
            )| {
                let mut root = Map::new();
                root.insert("id".to_owned(), Value::String(id));

                for (key, value) in known_fields {
                    root.insert(key, value);
                }
                for (key, value) in documented_unknown_fields {
                    root.insert(key, value);
                }
                for (key, value) in unknown_root_fields {
                    root.insert(key, value);
                }
                for (key, value) in reserved_fields {
                    root.insert(key, value);
                }

                let mut payload = Map::new();
                if let Some(fields) = payload_known_fields {
                    for (key, value) in fields {
                        payload.insert(key, value);
                    }
                }
                if let Some(fields) = payload_unknown_fields {
                    for (key, value) in fields {
                        payload.insert(key, value);
                    }
                }
                if !payload.is_empty() {
                    root.insert("payload".to_owned(), Value::Object(payload));
                }

                Value::Object(root)
            },
        )
        .boxed()
}

/// Independent oracle for SQL column extraction tests.
/// Reimplements the null-filtering fallback from `extract_entry_fields` so that
/// `expected_row_snapshot` can predict column values without calling production code.
/// This is intentionally NOT calling production functions — the point is to verify
/// extraction correctness from a separate implementation.
fn field_value_non_null<'a>(entry: &'a Value, key: &str) -> Option<&'a Value> {
    entry.get(key).filter(|v| !v.is_null()).or_else(|| {
        entry
            .get("payload")
            .and_then(Value::as_object)
            .and_then(|p| p.get(key))
    })
}

/// Build the expected rebuilt entry using production `rebuild_entry_content`.
/// This ensures the roundtrip test validates the actual upsert→read→rebuild pipeline
/// rather than comparing against a reimplemented oracle.
fn expected_rebuilt_entry(entry: &Value) -> Value {
    let entry_id = entry
        .get("id")
        .and_then(Value::as_str)
        .expect("generated entry should have an id");
    rebuild_entry_content(entry, None, entry_id)
}

fn expected_rich_text_json(value: &Value) -> Option<String> {
    let has_empty_contents = |value: &Value| {
        value
            .get("contents")
            .or_else(|| value.get("nodes"))
            .and_then(Value::as_array)
            .map(|contents| contents.is_empty())
            .unwrap_or(false)
    };

    match value {
        Value::Null => None,
        Value::String(text) if text.trim().is_empty() => None,
        Value::String(text) => {
            if serde_json::from_str::<Value>(text.trim())
                .ok()
                .map(|parsed| has_empty_contents(&parsed))
                .unwrap_or(false)
            {
                None
            } else {
                Some(text.to_owned())
            }
        }
        Value::Object(obj) if obj.is_empty() => None,
        Value::Object(_) if has_empty_contents(value) => None,
        Value::Array(items) if items.is_empty() => None,
        Value::Object(_) | Value::Array(_) => Some(value.to_string()),
        _ => None,
    }
}

fn expected_row_snapshot(entry: &Value, user_edit_date: Option<&str>) -> EntryRowSnapshot {
    EntryRowSnapshot {
        date: field_value_non_null(entry, "date").and_then(|value| {
            if let Some(num) = value.as_f64() {
                Some(num)
            } else {
                value.as_str().and_then(|s| {
                    s.parse::<f64>()
                        .ok()
                        .or_else(|| parse_iso_to_epoch_ms(s).ok())
                })
            }
        }),
        user_edit_date: user_edit_date.map(str::to_owned),
        title: field_value_non_null(entry, "title")
            .and_then(Value::as_str)
            .map(str::to_owned),
        body: field_value_non_null(entry, "body")
            .and_then(Value::as_str)
            .map(str::to_owned),
        rich_text_json: field_value_non_null(entry, "richTextJSON")
            .and_then(expected_rich_text_json),
        time_zone: field_value_non_null(entry, "timeZone").and_then(|value| {
            if value.is_null() {
                None
            } else {
                value.as_str().map(str::to_owned)
            }
        }),
        tags_json: field_value_non_null(entry, "tags").and_then(|v| {
            if v.is_null() {
                None
            } else {
                Some(v.to_string())
            }
        }),
        moments_json: field_value_non_null(entry, "moments").and_then(|v| {
            if v.is_null() {
                None
            } else {
                Some(v.to_string())
            }
        }),
        client_meta_json: field_value_non_null(entry, "clientMeta").and_then(|v| {
            if v.is_null() {
                None
            } else {
                Some(v.to_string())
            }
        }),
        extra_fields_json: {
            let merged = collect_merged_extra_fields(entry);
            if merged.is_empty() {
                None
            } else {
                Some(Value::Object(merged).to_string())
            }
        },
    }
}

fn read_row_snapshot(store: &Store, id: &str) -> EntryRowSnapshot {
    let conn = store.connect().expect("store should connect");
    conn.query_row(
        r#"
        SELECT
          date,
          user_edit_date,
          title,
          body,
          rich_text_json,
          time_zone,
          tags_json,
          moments_json,
          client_meta_json,
          extra_fields_json
        FROM entries
        WHERE id = ?1
        "#,
        params![id],
        |row| {
            Ok(EntryRowSnapshot {
                date: row.get(0)?,
                user_edit_date: row.get(1)?,
                title: row.get(2)?,
                body: row.get(3)?,
                rich_text_json: row.get(4)?,
                time_zone: row.get(5)?,
                tags_json: row.get(6)?,
                moments_json: row.get(7)?,
                client_meta_json: row.get(8)?,
                extra_fields_json: row.get(9)?,
            })
        },
    )
    .expect("entry row should exist")
}

fn generated_entry_id(entry: &Value) -> &str {
    entry
        .get("id")
        .and_then(Value::as_str)
        .expect("generated entry should have an id")
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        .. ProptestConfig::default()
    })]

    #[test]
    fn fuzz_roundtrip_preserves_normalized_content(entry in entry_strategy()) {
        let path = test_store_path("fuzz-roundtrip");
        let store = Store::open_at(&path).expect("store should open");
        let id = generated_entry_id(&entry).to_owned();

        let rebuilt = roundtrip(&store, &id, "journal-fuzz", &entry.to_string());
        let expected = expected_rebuilt_entry(&entry);

        prop_assert_eq!(rebuilt, expected);
    }

    #[test]
    fn fuzz_storage_extraction_matches_expected(entry in entry_strategy()) {
        let path = test_store_path("fuzz-storage");
        let store = Store::open_at(&path).expect("store should open");
        let id = generated_entry_id(&entry).to_owned();

        store
            .upsert_entry_json_row(&id, "journal-fuzz", Some("2026-03-01T00:00:00Z"), None, &entry.to_string())
            .expect("upsert should succeed");

        let snapshot = read_row_snapshot(&store, &id);
        let expected = expected_row_snapshot(&entry, None);

        prop_assert_eq!(snapshot, expected);
    }

    #[test]
    fn fuzz_upsert_with_edit_date_matches_expected(entry in entry_strategy()) {
        let path = test_store_path("fuzz-edit-date");
        let store = Store::open_at(&path).expect("store should open");
        let id = generated_entry_id(&entry).to_owned();

        let rebuilt = roundtrip_with_user_edit_date(&store, &id, "journal-fuzz", &entry.to_string());
        let expected_content = expected_rebuilt_entry(&entry);
        prop_assert_eq!(rebuilt, expected_content);

        let snapshot = read_row_snapshot(&store, &id);
        let expected_row = expected_row_snapshot(&entry, Some(FIXED_USER_EDIT_DATE));
        prop_assert_eq!(snapshot, expected_row);
    }

    #[test]
    fn fuzz_rich_text_json_shapes_populate_column(rich_text_json in rich_text_json_strategy()) {
        let path = test_store_path("fuzz-rtj");
        let store = Store::open_at(&path).expect("store should open");
        let id = "entry-richtext-fuzz";
        let entry = json!({
            "id": id,
            "richTextJSON": rich_text_json,
        });

        let rebuilt = roundtrip(&store, id, "journal-fuzz", &entry.to_string());
        prop_assert_eq!(rebuilt, expected_rebuilt_entry(&entry));

        let snapshot = read_row_snapshot(&store, id);
        let expected = expected_row_snapshot(&entry, None);
        prop_assert_eq!(snapshot.rich_text_json, expected.rich_text_json);
    }
}
