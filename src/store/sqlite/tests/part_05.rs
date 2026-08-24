//! Round-trip tests: JSON blob → SQL schema → rebuild → JSON blob.
//!
//! Verifies three properties of the `upsert_entry_json_row` + `rebuild_entry_content` cycle:
//! 1. **No property loss** — every field in the original JSON appears in the rebuilt JSON
//! 2. **No property mutation** — field values are identical after round-trip
//! 3. **No property addition** — rebuilt JSON has no fields that weren't in the original

use super::super::*;
use super::common::{
    assert_roundtrip_eq, read_rebuilt_entry, roundtrip, roundtrip_with_user_edit_date,
    test_store_path,
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

#[test]
fn roundtrip_full_entry_all_documented_fields() {
    let path = test_store_path("rt-full");
    let store = Store::open_at(&path).expect("store should open");

    let entry = json!({
        "id": "A1B2C3D4E5F67890A1B2C3D4E5F67890",
        "date": 1700000000000_i64,
        "userEditDate": 1700000060000_i64,
        "editingTime": 42000,
        "duration": 0,
        "body": "Had a great hike today.",
        "richTextJSON": {
            "meta": { "version": 1, "created": { "platform": "com.bloombuilt.dayone-ios", "version": 305 } },
            "nodes": [{ "text": "Had a great hike today.", "attributes": { "line": {} } }]
        },
        "timeZone": "America/Los_Angeles",
        "isAllDay": false,
        "isPinned": false,
        "starred": false,
        "featureFlags": "11",
        "tags": ["hiking", "outdoors"],
        "activity": "Walking",
        "location": {
            "latitude": 37.8651,
            "longitude": -119.5383,
            "altitude": 1200.0,
            "heading": 90.0,
            "speed": 1.2,
            "placeName": "Yosemite Valley",
            "localityName": "Yosemite Village",
            "administrativeArea": "California",
            "country": "United States",
            "streetAddress": "",
            "timeZoneName": "America/Los_Angeles",
            "fullAddress": "Yosemite Village, CA, United States",
            "userLabel": "Hiking Spot",
            "region": {
                "identifier": "yosemite-region",
                "latitude": 37.8651,
                "longitude": -119.5383,
                "radius": 500.0
            },
            "route": {
                "summary_polyline": ["encodedPolylineString"],
                "start_latlng": [37.8651, -119.5383],
                "end_latlng": [37.7580, -119.4855],
                "source": "Strava"
            }
        },
        "weather": {
            "description": "Sunny",
            "tempCelsius": 22.0,
            "code": "clear-day",
            "moonPhase": 0.42,
            "moonPhaseCode": "waxing-gibbous",
            "service": "WeatherKit",
            "windBearing": 225,
            "windSpeedKph": 14.8,
            "windChillCelsius": 20.1,
            "pressureMb": 1013.25,
            "visibilityKm": 16.0,
            "relativeHumidity": 45,
            "sunriseDate": 1700000000000_i64,
            "sunsetDate": 1700043600000_i64
        },
        "steps": { "stepCount": 8432, "ignore": false },
        "music": { "artist": "Radiohead", "track": "Karma Police", "album": "OK Computer", "albumYear": 1997 },
        "moments": [{
            "id": "MOMENT-UUID-1234",
            "type": "photo",
            "contentType": "image/jpeg",
            "md5": "d41d8cd98f00b204e9800998ecf8427e",
            "date": 1700000000000_i64,
            "createdAt": 1700000000000_i64,
            "height": 3024,
            "width": 4032,
            "isSketch": false,
            "favorite": false,
            "creationDevice": "iPhone 15 Pro",
            "creationDeviceIdentifier": "DEVICE-UUID",
            "thumbnail": {
                "md5": "abc123",
                "contentType": "image/jpeg",
                "fileSize": 8192,
                "height": 256,
                "width": 341
            },
            "thumbnailContentType": "image/jpeg"
        }],
        "clientMeta": {
            "deviceId": "DEVICE-UUID",
            "deviceName": "Murphy's iPhone",
            "creationDevice": "iPhone 15 Pro",
            "creationDeviceModel": "iPhone16,2",
            "creationDeviceType": "iPhone",
            "creationOSName": "iOS",
            "creationOSVersion": "17.2",
            "browserName": json!(null),
            "browserVersion": json!(null),
            "creationAIHost": json!(null),
            "creationAIModel": json!(null),
            "source": json!(null)
        },
        "ownerUserId": "USER-UUID",
        "creatorUserId": "USER-UUID",
        "editorUserId": "USER-UUID",
        "lastEditingDeviceID": "DEVICE-UUID",
        "lastEditingDeviceName": "iPhone 15 Pro",
        "templateID": json!(null),
        "promptID": json!(null),
        "entryType": json!(null),
        "unread_marker_id": json!(null),
        "is_shared": false
    });

    let data_json = serde_json::to_string(&entry).expect("fixture should serialize");
    let rebuilt = roundtrip(
        &store,
        "A1B2C3D4E5F67890A1B2C3D4E5F67890",
        "journal-1",
        &data_json,
    );

    assert_roundtrip_eq(&entry, &rebuilt, &[]);

    // Spot-check deeply nested values
    assert_eq!(
        rebuilt["location"]["region"]["radius"], 500.0,
        "round-trip: location.region.radius"
    );
    assert_eq!(
        rebuilt["weather"]["tempCelsius"], 22.0,
        "round-trip: weather.tempCelsius"
    );
    assert_eq!(
        rebuilt["moments"][0]["thumbnail"]["fileSize"], 8192,
        "round-trip: moments[0].thumbnail.fileSize"
    );
    assert_eq!(
        rebuilt["clientMeta"]["creationDeviceModel"], "iPhone16,2",
        "round-trip: clientMeta.creationDeviceModel"
    );
    assert_eq!(
        rebuilt["music"]["albumYear"], 1997,
        "round-trip: music.albumYear"
    );
}

#[test]
fn roundtrip_known_fields_individually() {
    let path = test_store_path("rt-known");
    let store = Store::open_at(&path).expect("store should open");

    let cases: Vec<(&str, Value)> = vec![
        ("date", json!(1700000000000_i64)),
        ("title", json!("My Title")),
        ("body", json!("Hello world\nLine two")),
        ("richTextJSON", json!("{\"meta\":{\"version\":1}}")),
        ("timeZone", json!("America/New_York")),
        ("tags", json!(["travel", "personal"])),
        ("moments", json!([{"id": "m1", "type": "photo"}])),
        (
            "clientMeta",
            json!({"deviceId": "d1", "deviceName": "iPhone"}),
        ),
    ];

    for (i, (key, value)) in cases.iter().enumerate() {
        let id = format!("known-{i}");
        let mut entry = serde_json::Map::new();
        entry.insert("id".to_owned(), json!(id));
        entry.insert((*key).to_owned(), value.clone());

        let data_json = Value::Object(entry).to_string();
        let rebuilt = roundtrip(&store, &id, "journal-1", &data_json);

        assert_eq!(
            rebuilt.get(*key),
            Some(value),
            "known field `{key}` should round-trip"
        );
    }
}

#[test]
fn upsert_preserves_data_json_bytes_when_no_dropped_fields_are_present() {
    let path = test_store_path("rt-preserve-data-json-without-drops");
    let store = Store::open_at(&path).expect("store should open");
    let data_json = r#"{
  "id": "preserve-json-1",
  "body": "real body",
  "weather": { "conditionCode": "clear" }
}"#;

    store
        .upsert_entry_json_row(
            "preserve-json-1",
            "journal-1",
            Some("2026-03-01T00:00:00Z"),
            None,
            data_json,
        )
        .expect("entry should save");

    let conn = store.connect().expect("store should connect");
    let stored: String = conn
        .query_row(
            "SELECT data_json FROM entries WHERE id = ?1",
            params!["preserve-json-1"],
            |row| row.get(0),
        )
        .expect("entry row should exist");

    assert_eq!(stored, data_json);
}

#[test]
fn dropped_decrypted_text_field_is_not_preserved() {
    let path = test_store_path("rt-dropped-decrypted-text");
    let store = Store::open_at(&path).expect("store should open");

    let entry = json!({
        "id": "dropped-decrypted-text-1",
        "body": "real body",
        "decrypted_text": "legacy synthetic fallback",
        "payload": {
            "body": "payload body",
            "decrypted_text": "legacy payload fallback"
        }
    });

    let rebuilt = roundtrip(
        &store,
        "dropped-decrypted-text-1",
        "journal-1",
        &entry.to_string(),
    );

    assert_eq!(rebuilt.get("body"), Some(&json!("real body")));
    assert_eq!(rebuilt.get("decrypted_text"), None);

    let conn = store.connect().expect("store should connect");
    let (decrypted_text, extra_fields_json, data_json): (Option<String>, Option<String>, String) =
        conn.query_row(
            "SELECT decrypted_text, extra_fields_json, data_json FROM entries WHERE id = ?1",
            params!["dropped-decrypted-text-1"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("entry row should exist");
    let stored: Value = serde_json::from_str(&data_json).expect("stored data_json should parse");

    assert_eq!(decrypted_text, None);
    assert_eq!(extra_fields_json, None);
    assert_eq!(stored.get("decrypted_text"), None);
    assert_eq!(stored.pointer("/payload/decrypted_text"), None);
    assert_eq!(
        stored.pointer("/payload/body"),
        Some(&json!("payload body"))
    );
}

#[test]
fn roundtrip_documented_unknown_fields_individually() {
    let path = test_store_path("rt-unknown-doc");
    let store = Store::open_at(&path).expect("store should open");

    let cases: Vec<(&str, Value)> = vec![
        ("userEditDate", json!(1700000001000_i64)),
        ("editingTime", json!(42000)),
        ("duration", json!(0)),
        ("isAllDay", json!(false)),
        ("isPinned", json!(true)),
        ("starred", json!(true)),
        ("featureFlags", json!("11")),
        ("activity", json!("Walking")),
        (
            "location",
            json!({"latitude": 40.7128, "longitude": -74.0060}),
        ),
        (
            "weather",
            json!({"description": "Sunny", "tempCelsius": 22.0}),
        ),
        ("steps", json!({"stepCount": 8432, "ignore": false})),
        (
            "music",
            json!({"artist": "Radiohead", "track": "Karma Police"}),
        ),
        ("ownerUserId", json!("user-uuid")),
        ("creatorUserId", json!("user-uuid")),
        ("editorUserId", json!("user-uuid")),
        ("lastEditingDeviceID", json!("device-uuid")),
        ("lastEditingDeviceName", json!("iPhone 15")),
        ("templateID", json!("template-uuid")),
        ("promptID", json!("prompt-uuid")),
        ("entryType", json!("welcome-entry")),
        ("unread_marker_id", json!("marker-uuid")),
        ("is_shared", json!(true)),
    ];

    for (i, (key, value)) in cases.iter().enumerate() {
        let id = format!("unknown-doc-{i}");
        let mut entry = serde_json::Map::new();
        entry.insert("id".to_owned(), json!(id));
        entry.insert("body".to_owned(), json!("anchor"));
        entry.insert((*key).to_owned(), value.clone());

        let data_json = Value::Object(entry).to_string();
        let rebuilt = roundtrip(&store, &id, "journal-1", &data_json);

        assert_eq!(
            rebuilt.get(*key),
            Some(value),
            "documented field `{key}` should survive via extra_fields_json"
        );
    }
}

#[test]
fn roundtrip_future_unknown_fields_survive() {
    let path = test_store_path("rt-future");
    let store = Store::open_at(&path).expect("store should open");

    let entry = json!({
        "id": "future-1",
        "body": "hello",
        "futureApiField2027": {
            "nested": true,
            "deeply": { "level2": { "level3": [1, "two", null, { "level4": true }] } }
        },
        "anotherFutureField": 42
    });

    let data_json = entry.to_string();
    let rebuilt = roundtrip(&store, "future-1", "journal-1", &data_json);

    assert_roundtrip_eq(&entry, &rebuilt, &[]);
    assert_eq!(
        rebuilt["futureApiField2027"]["deeply"]["level2"]["level3"][3]["level4"],
        true
    );
    assert_eq!(rebuilt["anotherFutureField"], 42);
}

#[test]
fn roundtrip_payload_wrapper_flattens() {
    let path = test_store_path("rt-payload");
    let store = Store::open_at(&path).expect("store should open");

    let entry = json!({
        "id": "payload-1",
        "payload": {
            "body": "hello from payload",
            "date": 1700000000000_i64,
            "weather": { "tempCelsius": 20.0 },
            "tags": ["a", "b"]
        }
    });

    let data_json = entry.to_string();
    let rebuilt = roundtrip(&store, "payload-1", "journal-1", &data_json);

    // Fields should be at root level
    assert_eq!(rebuilt["body"], "hello from payload");
    assert_eq!(rebuilt["date"], 1700000000000_i64);
    assert_eq!(rebuilt["tags"], json!(["a", "b"]));
    assert_eq!(rebuilt["weather"]["tempCelsius"], 20.0);
    // No payload wrapper in rebuilt
    assert!(
        rebuilt.get("payload").is_none(),
        "payload wrapper should be stripped"
    );
}

#[test]
fn roundtrip_mixed_root_and_payload_fields() {
    let path = test_store_path("rt-mixed");
    let store = Store::open_at(&path).expect("store should open");

    let entry = json!({
        "id": "mixed-1",
        "body": "root body",
        "futureRootField": "from-root",
        "payload": {
            "body": "payload body",
            "date": 123,
            "futurePayloadField": "from-payload"
        }
    });

    let data_json = entry.to_string();
    let rebuilt = roundtrip(&store, "mixed-1", "journal-1", &data_json);

    // Root body wins over payload body
    assert_eq!(rebuilt["body"], "root body");
    // date comes from payload since not at root
    assert_eq!(rebuilt["date"], 123);
    assert_eq!(
        rebuilt["futureRootField"], "from-root",
        "unknown field at root must survive when payload is present"
    );
    assert_eq!(
        rebuilt["futurePayloadField"], "from-payload",
        "unknown field inside payload must survive"
    );
}

#[test]
fn roundtrip_null_fields_preserved() {
    let path = test_store_path("rt-nulls");
    let store = Store::open_at(&path).expect("store should open");

    let entry = json!({
        "id": "nulls-1",
        "body": "anchor",
        "templateID": null,
        "promptID": null,
        "entryType": null,
        "unread_marker_id": null
    });

    let data_json = entry.to_string();
    let rebuilt = roundtrip(&store, "nulls-1", "journal-1", &data_json);

    for key in &["templateID", "promptID", "entryType", "unread_marker_id"] {
        assert!(
            rebuilt.get(*key).is_some(),
            "null field `{key}` should be present"
        );
        assert!(
            rebuilt[*key].is_null(),
            "null field `{key}` should remain null"
        );
    }
}

#[test]
fn roundtrip_empty_collections() {
    let path = test_store_path("rt-empty-coll");
    let store = Store::open_at(&path).expect("store should open");

    let entry = json!({
        "id": "empty-coll-1",
        "tags": [],
        "moments": [],
        "clientMeta": {},
        "richTextJSON": {}
    });

    let data_json = entry.to_string();
    let rebuilt = roundtrip(&store, "empty-coll-1", "journal-1", &data_json);

    assert_eq!(rebuilt["tags"], json!([]));
    assert_eq!(rebuilt["moments"], json!([]));
    assert_eq!(rebuilt["clientMeta"], json!({}));
    assert_eq!(rebuilt["richTextJSON"], json!({}));
}

#[test]
fn markdown_only_entry_with_empty_rich_text_json_stores_null_rich_text_column() {
    let path = test_store_path("rt-empty-rich-text-null-column");
    let store = Store::open_at(&path).expect("store should open");

    for (id, rich_text_json) in [
        ("empty-rtj-object", json!({})),
        (
            "empty-rtj-contents",
            json!({"contents": [], "meta": {"version": 1}}),
        ),
    ] {
        let entry = json!({
            "id": id,
            "body": "markdown body",
            "richTextJSON": rich_text_json
        });

        store
            .upsert_entry_json_row(id, "journal-1", None, None, &entry.to_string())
            .expect("entry should save");

        let conn = store.connect().expect("store should connect");
        let rich_text_json: Option<String> = conn
            .query_row(
                "SELECT rich_text_json FROM entries WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .expect("rich_text_json query should succeed")
            .flatten();

        assert_eq!(rich_text_json, None);
    }
}

#[test]
fn roundtrip_missing_optional_fields_stay_absent() {
    let path = test_store_path("rt-minimal");
    let store = Store::open_at(&path).expect("store should open");

    let entry = json!({
        "id": "minimal-1",
        "body": "just a body"
    });

    let data_json = entry.to_string();
    let rebuilt = roundtrip(&store, "minimal-1", "journal-1", &data_json);

    assert_eq!(rebuilt["id"], "minimal-1");
    assert_eq!(rebuilt["body"], "just a body");

    // None of these should appear
    for absent_key in &[
        "tags",
        "moments",
        "clientMeta",
        "location",
        "weather",
        "timeZone",
        "date",
        "title",
        "richTextJSON",
        "starred",
        "isPinned",
        "activity",
    ] {
        assert!(
            rebuilt.get(*absent_key).is_none(),
            "absent field `{absent_key}` should not appear in rebuilt output"
        );
    }
}

#[test]
fn roundtrip_reserved_fields_intentionally_stripped() {
    let path = test_store_path("rt-reserved");
    let store = Store::open_at(&path).expect("store should open");

    let entry = json!({
        "id": "reserved-1",
        "body": "hello",
        "journal_id": "j-1",
        "updated_at": "2026-03-01T00:00:00Z",
        "user_edit_date": "2026-03-01T00:00:00Z",
        "is_deleted": false,
        "needs_sync": true,
        "revision": 5,
        "deleted_at": "2026-03-27T15:00:00Z"
    });

    let data_json = entry.to_string();

    // Use upsert with deleted_at to test that path too
    store
        .upsert_entry_json_row(
            "reserved-1",
            "journal-1",
            Some("2026-03-01T00:00:00Z"),
            Some("2026-03-27T15:00:00Z"),
            &data_json,
        )
        .expect("upsert should succeed");
    let rebuilt = read_rebuilt_entry(&store, "reserved-1");

    // `id` and `deleted_at` are re-injected
    assert_eq!(rebuilt["id"], "reserved-1");
    assert_eq!(rebuilt["deleted_at"], "2026-03-27T15:00:00Z");

    // Known content field preserved
    assert_eq!(rebuilt["body"], "hello");

    // These reserved fields should be stripped
    for key in &[
        "journal_id",
        "updated_at",
        "user_edit_date",
        "is_deleted",
        "needs_sync",
        "revision",
    ] {
        assert!(
            rebuilt.get(*key).is_none(),
            "reserved field `{key}` should be stripped from rebuilt output"
        );
    }
}

#[test]
fn roundtrip_camelcase_user_edit_date_survives() {
    let path = test_store_path("rt-camel-ued");
    let store = Store::open_at(&path).expect("store should open");

    let entry = json!({
        "id": "camel-1",
        "body": "hi",
        "userEditDate": 1700000001000_i64
    });

    let data_json = entry.to_string();
    let rebuilt = roundtrip(&store, "camel-1", "journal-1", &data_json);

    assert_eq!(
        rebuilt["userEditDate"], 1700000001000_i64,
        "camelCase userEditDate should survive (only snake_case user_edit_date is reserved)"
    );
}

#[test]
fn roundtrip_date_integer_preserved() {
    let path = test_store_path("rt-date-int");
    let store = Store::open_at(&path).expect("store should open");

    let entry = json!({
        "id": "date-int-1",
        "date": 1700000000000_i64
    });

    let data_json = entry.to_string();
    let rebuilt = roundtrip(&store, "date-int-1", "journal-1", &data_json);

    // The value should be numerically identical
    assert_eq!(rebuilt["date"].as_i64(), Some(1700000000000_i64));
}

#[test]
fn roundtrip_date_rfc3339_offset_preserved_with_timezone() {
    let path = test_store_path("rt-date-offset");
    let store = Store::open_at(&path).expect("store should open");

    let entry = json!({
        "id": "date-offset-1",
        "date": "2026-03-01T12:34:56-08:00",
        "timeZone": "America/Los_Angeles"
    });

    let data_json = entry.to_string();
    let rebuilt = roundtrip(&store, "date-offset-1", "journal-1", &data_json);

    assert_eq!(
        rebuilt["date"].as_str(),
        Some("2026-03-01T12:34:56-08:00"),
        "RFC3339 date string should round-trip exactly, including offset"
    );
    assert_eq!(
        rebuilt["timeZone"].as_str(),
        Some("America/Los_Angeles"),
        "timeZone should round-trip alongside offset-bearing date strings"
    );
}

#[test]
fn roundtrip_richtext_as_object_preserved() {
    let path = test_store_path("rt-rtj-obj");
    let store = Store::open_at(&path).expect("store should open");

    let rtj = json!({
        "meta": { "version": 1 },
        "nodes": [{ "text": "hello", "attributes": { "line": {} } }]
    });
    let entry = json!({
        "id": "rtj-obj-1",
        "richTextJSON": rtj
    });

    let data_json = entry.to_string();
    let rebuilt = roundtrip(&store, "rtj-obj-1", "journal-1", &data_json);

    assert_eq!(rebuilt["richTextJSON"], rtj);

    let conn = store.connect().expect("store should connect");
    let col: Option<String> = conn
        .query_row(
            "SELECT rich_text_json FROM entries WHERE id = ?1",
            params!["rtj-obj-1"],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .expect("rich_text_json query should succeed")
        .flatten();
    let expected_rtj = rtj.to_string();
    assert_eq!(
        col.as_deref(),
        Some(expected_rtj.as_str()),
        "object richTextJSON should populate rich_text_json column (not only data_json)"
    );
}

#[test]
fn roundtrip_richtext_as_string_preserved() {
    let path = test_store_path("rt-rtj-str");
    let store = Store::open_at(&path).expect("store should open");

    let rtj_string = r#"{"meta":{"version":1}}"#;
    let entry = json!({
        "id": "rtj-str-1",
        "richTextJSON": rtj_string
    });

    let data_json = entry.to_string();
    let rebuilt = roundtrip(&store, "rtj-str-1", "journal-1", &data_json);

    assert_eq!(rebuilt["richTextJSON"].as_str(), Some(rtj_string));
}

#[test]
fn roundtrip_deeply_nested_unknown_fields() {
    let path = test_store_path("rt-deep-nest");
    let store = Store::open_at(&path).expect("store should open");

    let entry = json!({
        "id": "deep-1",
        "body": "anchor",
        "futureApi": {
            "level1": {
                "level2": {
                    "level3": [1, "two", null, { "level4": true }]
                }
            }
        }
    });

    let data_json = entry.to_string();
    let rebuilt = roundtrip(&store, "deep-1", "journal-1", &data_json);

    assert_eq!(rebuilt["futureApi"]["level1"]["level2"]["level3"][0], 1);
    assert_eq!(rebuilt["futureApi"]["level1"]["level2"]["level3"][1], "two");
    assert!(rebuilt["futureApi"]["level1"]["level2"]["level3"][2].is_null());
    assert_eq!(
        rebuilt["futureApi"]["level1"]["level2"]["level3"][3]["level4"],
        true
    );
}

#[test]
fn roundtrip_unicode_and_special_characters() {
    let path = test_store_path("rt-unicode");
    let store = Store::open_at(&path).expect("store should open");

    let body_text = "Hello \u{1F600} 世界 مرحبا\n\ttab indented";
    let entry = json!({
        "id": "unicode-1",
        "body": body_text,
        "tags": ["café", "日本語", "emoji-\u{1F680}"]
    });

    let data_json = entry.to_string();
    let rebuilt = roundtrip(&store, "unicode-1", "journal-1", &data_json);

    assert_eq!(rebuilt["body"], body_text);
    assert_eq!(rebuilt["tags"][0], "café");
    assert_eq!(rebuilt["tags"][1], "日本語");
    assert_eq!(rebuilt["tags"][2], "emoji-\u{1F680}");
}

#[test]
fn roundtrip_upsert_twice_preserves_extra_fields() {
    let path = test_store_path("rt-upsert-twice");
    let store = Store::open_at(&path).expect("store should open");

    let v1 = json!({
        "id": "twice-1",
        "body": "version one",
        "futureField": "keep-me",
        "starred": true
    });
    store
        .upsert_entry_json_row(
            "twice-1",
            "journal-1",
            Some("2026-03-01T00:00:00Z"),
            None,
            &v1.to_string(),
        )
        .expect("first upsert should succeed");

    let v2 = json!({
        "id": "twice-1",
        "body": "version two",
        "futureField": "keep-me",
        "starred": true
    });
    store
        .upsert_entry_json_row(
            "twice-1",
            "journal-1",
            Some("2026-03-02T00:00:00Z"),
            None,
            &v2.to_string(),
        )
        .expect("second upsert should succeed");

    let rebuilt = read_rebuilt_entry(&store, "twice-1");

    assert_eq!(rebuilt["body"], "version two");
    assert_eq!(rebuilt["futureField"], "keep-me");
    assert_eq!(rebuilt["starred"], true);
}

#[test]
fn roundtrip_upsert_with_edit_date_matches_storage_and_rebuild() {
    let path = test_store_path("rt-upsert-edit-date");
    let store = Store::open_at(&path).expect("store should open");

    let entry = json!({
        "id": "ued-rt-1",
        "body": "sync-path upsert",
        "starred": true,
        "futureExtra": { "n": [1, 2] }
    });

    let data_json = entry.to_string();
    let rebuilt = roundtrip_with_user_edit_date(&store, "ued-rt-1", "journal-1", &data_json);

    assert_roundtrip_eq(&entry, &rebuilt, &[]);

    let conn = store.connect().expect("store should connect");
    let ued: Option<String> = conn
        .query_row(
            "SELECT user_edit_date FROM entries WHERE id = ?1",
            params!["ued-rt-1"],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .expect("user_edit_date query should succeed")
        .flatten();
    assert_eq!(
        ued.as_deref(),
        Some("2026-03-01T12:00:00.000Z"),
        "upsert_entry_json_row_with_edit_date should persist user_edit_date"
    );
}

#[test]
fn roundtrip_deleted_entry_preserves_content() {
    let path = test_store_path("rt-deleted");
    let store = Store::open_at(&path).expect("store should open");

    let entry = json!({
        "id": "deleted-1",
        "body": "deleted content",
        "deleted_at": "2026-03-27T15:00:00.000Z",
        "starred": true,
        "futureField": "preserved"
    });

    store
        .upsert_entry_json_row(
            "deleted-1",
            "journal-1",
            Some("2026-03-01T00:00:00Z"),
            Some("2026-03-27T15:00:00.000Z"),
            &entry.to_string(),
        )
        .expect("upsert should succeed");

    let rebuilt = read_rebuilt_entry(&store, "deleted-1");

    assert_eq!(rebuilt["body"], "deleted content");
    assert_eq!(rebuilt["deleted_at"], "2026-03-27T15:00:00.000Z");
    assert_eq!(rebuilt["starred"], true);
    assert_eq!(rebuilt["futureField"], "preserved");
}
