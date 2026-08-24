use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use serde_json::Value;
use time::OffsetDateTime;
use time::UtcOffset;

const STAGING_URL: &str = "https://stg.dayone.me";

fn unique_tmp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    std::env::temp_dir().join(format!("dayone-cli-tests-{label}-{nanos}"))
}

fn run_dayone(config_dir: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_dayone"))
        .env("DAYONE_CONFIG_DIR", config_dir)
        .args(args)
        .output()
        .expect("dayone command should execute");
    if !output.status.success() {
        panic!(
            "dayone command failed: args={args:?}\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).expect("stdout should be utf8")
}

/// Per-profile DB path: `{config_dir}/profiles/staging/dayone.db`
fn staging_db_path(config_dir: &Path) -> PathBuf {
    config_dir
        .join("profiles")
        .join("staging")
        .join("dayone.db")
}

fn seed_local_auth_and_journal(config_dir: &Path, journal_id: &str) {
    fs::create_dir_all(config_dir).expect("config dir should create");
    let _ = run_dayone(
        config_dir,
        &[
            "--profile",
            "staging",
            "--api-host",
            STAGING_URL,
            "list",
            "journals",
        ],
    );

    let db_path = staging_db_path(config_dir);
    let conn = Connection::open(db_path).expect("sqlite should open");
    let profile_id: i64 = conn
        .query_row(
            "SELECT id FROM profiles WHERE base_url = ?1 LIMIT 1",
            params![STAGING_URL],
            |row| row.get(0),
        )
        .expect("staging profile should exist");
    conn.execute(
        r#"
        INSERT INTO auth_sessions (profile_id, token, token_created_at, user_json, created_at, updated_at)
        VALUES (?1, 'test-token', '2026-03-18T00:00:00Z', '{"id":"u1"}', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
        ON CONFLICT(profile_id) DO UPDATE SET
          token = excluded.token,
          token_created_at = excluded.token_created_at,
          user_json = excluded.user_json,
          updated_at = CURRENT_TIMESTAMP
        "#,
        params![profile_id],
    )
    .expect("auth session should insert");
    conn.execute(
        r#"
        INSERT INTO journals (id, user_edit_date, updated_at, deleted_at, is_deleted, data_json)
        VALUES (?1, NULL, CURRENT_TIMESTAMP, NULL, 0, ?2)
        ON CONFLICT(id) DO UPDATE SET
          updated_at = CURRENT_TIMESTAMP,
          data_json = excluded.data_json
        "#,
        params![journal_id, format!(r#"{{"id":"{journal_id}"}}"#)],
    )
    .expect("journal should insert");
}

fn write_temp_attachment(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("attachment should be written");
}

fn tiny_png_bytes() -> &'static [u8] {
    &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0xF0, 0x1F, 0x00, 0x05, 0x00, 0x01, 0xFF, 0x89, 0x99, 0x3D, 0x1D, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}

fn sample_pdf_bytes() -> Vec<u8> {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("e2e/fixtures/sample-pdf.pdf");
    fs::read(&fixture).unwrap_or_else(|err| {
        panic!(
            "failed reading PDF fixture at '{}': {err}",
            fixture.display()
        )
    })
}

#[test]
fn entry_update_refuses_unsupported_feature_flags_without_queuing_changes() {
    let config_dir = unique_tmp_dir("feature-flags");
    let journal_id = "j-feature-flags";
    let entry_id = "e-feature-flags";
    seed_local_auth_and_journal(&config_dir, journal_id);

    let db_path = staging_db_path(&config_dir);
    let conn = Connection::open(&db_path).expect("sqlite should open");
    conn.execute(
        r#"
        INSERT INTO entries (id, journal_id, is_deleted, body, data_json)
        VALUES (?1, ?2, 0, 'before', ?3)
        "#,
        params![
            entry_id,
            journal_id,
            format!(r#"{{"id":"{entry_id}","body":"before","featureFlags":"100"}}"#)
        ],
    )
    .expect("entry should insert");
    drop(conn);

    let output = Command::new(env!("CARGO_BIN_EXE_dayone"))
        .env("DAYONE_CONFIG_DIR", &config_dir)
        .args([
            "--profile",
            "staging",
            "--api-host",
            STAGING_URL,
            "entry",
            "write",
            "--journal-id",
            journal_id,
            "--entry-id",
            entry_id,
            "--body",
            "after",
        ])
        .output()
        .expect("dayone command should execute");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported feature flags (0x100)"));

    let conn = Connection::open(db_path).expect("sqlite should reopen");
    let (body, outbox_count): (String, i64) = (
        conn.query_row(
            "SELECT body FROM entries WHERE id = ?1",
            params![entry_id],
            |row| row.get(0),
        )
        .expect("entry body should query"),
        conn.query_row("SELECT COUNT(*) FROM sync_outbox", [], |row| row.get(0))
            .expect("outbox count should query"),
    );
    assert_eq!(body, "before");
    assert_eq!(outbox_count, 0);
}

#[test]
fn entry_with_attachment_queues_entry_and_original_media() {
    let config_dir = unique_tmp_dir("queue");
    let journal_id = "j-test-1";
    seed_local_auth_and_journal(&config_dir, journal_id);

    let attachment = config_dir.join("sample.jpg");
    write_temp_attachment(&attachment, b"\xff\xd8\xff\xe0fake-jpg");

    let _ = run_dayone(
        &config_dir,
        &[
            "--profile",
            "staging",
            "--api-host",
            STAGING_URL,
            "entry",
            "write",
            "--journal-id",
            journal_id,
            "--body",
            "entry with image attachment",
            "--attach",
            attachment.to_str().expect("path should be utf8"),
        ],
    );

    let conn = Connection::open(staging_db_path(&config_dir)).expect("sqlite should open");
    let outbox_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sync_outbox", [], |row| row.get(0))
        .expect("outbox count should query");
    assert_eq!(outbox_count, 2);

    let mut stmt = conn
        .prepare("SELECT resource FROM sync_outbox ORDER BY next_attempt_epoch_ms ASC, created_at ASC, id ASC")
        .expect("query should prepare");
    let resources = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("rows should query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows should collect");
    assert_eq!(resources[0], "entry");
    assert_eq!(resources[1], "original_media");
}

#[test]
fn sync_processes_entry_before_original_media() {
    let config_dir = unique_tmp_dir("ordering");
    let journal_id = "j-test-2";
    seed_local_auth_and_journal(&config_dir, journal_id);

    let attachment_one = config_dir.join("sample-1.jpg");
    let attachment_two = config_dir.join("sample-2.pdf");
    write_temp_attachment(&attachment_one, b"\xff\xd8\xff\xe0fake-jpg-1");
    write_temp_attachment(&attachment_two, b"%PDF-1.4 fake pdf");

    let _ = run_dayone(
        &config_dir,
        &[
            "--profile",
            "staging",
            "--api-host",
            STAGING_URL,
            "entry",
            "write",
            "--journal-id",
            journal_id,
            "--body",
            "entry with two attachments",
            "--attach",
            attachment_one.to_str().expect("path should be utf8"),
            "--attach",
            attachment_two.to_str().expect("path should be utf8"),
        ],
    );

    let conn = Connection::open(staging_db_path(&config_dir)).expect("sqlite should open");
    let mut stmt = conn
        .prepare(
            r#"
            SELECT resource, id
            FROM sync_outbox
            ORDER BY
              CASE WHEN resource = 'journal' AND operation = 'create' THEN 0 ELSE 1 END,
              next_attempt_epoch_ms ASC,
              created_at ASC,
              id ASC
            "#,
        )
        .expect("query should prepare");
    let ordered = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("rows should query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows should collect");
    assert!(!ordered.is_empty());
    assert_eq!(ordered[0].0, "entry");
    assert!(
        ordered[1..]
            .iter()
            .all(|(resource, _)| resource == "original_media")
    );
}

#[test]
fn retry_original_media_after_entry_success() {
    let config_dir = unique_tmp_dir("retry");
    let journal_id = "j-test-3";
    seed_local_auth_and_journal(&config_dir, journal_id);

    let attachment = config_dir.join("sample.mp4");
    write_temp_attachment(&attachment, b"\x00\x00\x00\x18ftypmp42fake-video");

    let write_output = run_dayone(
        &config_dir,
        &[
            "--profile",
            "staging",
            "--api-host",
            STAGING_URL,
            "entry",
            "write",
            "--journal-id",
            journal_id,
            "--body",
            "entry for retry semantics",
            "--attach",
            attachment.to_str().expect("path should be utf8"),
        ],
    );
    let parsed: Value = serde_json::from_str(&write_output).expect("write output should be json");
    let entry_id = parsed
        .get("entry_id")
        .and_then(Value::as_str)
        .expect("entry_id should exist");

    let conn = Connection::open(staging_db_path(&config_dir)).expect("sqlite should open");
    let original_media_id: String = conn
        .query_row(
            "SELECT id FROM sync_outbox WHERE resource = 'original_media' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("original_media outbox row should exist");
    let payload_json: String = conn
        .query_row(
            "SELECT payload_json FROM sync_outbox WHERE id = ?1",
            params![original_media_id.clone()],
            |row| row.get(0),
        )
        .expect("payload json should exist");
    let payload: Value = serde_json::from_str(&payload_json).expect("payload should parse");
    assert_eq!(
        payload.get("entry_id").and_then(Value::as_str),
        Some(entry_id)
    );

    let attachment_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entry_attachments WHERE journal_id = ?1 AND entry_id = ?2",
            params![journal_id, entry_id],
            |row| row.get(0),
        )
        .expect("attachment rows should count");
    assert_eq!(attachment_count, 1);
}

#[test]
fn entry_write_all_day_flag_coerces_rfc3339_to_all_day_shape() {
    let config_dir = unique_tmp_dir("all-day");
    let journal_id = "j-test-all-day";
    seed_local_auth_and_journal(&config_dir, journal_id);

    let write_output = run_dayone(
        &config_dir,
        &[
            "--profile",
            "staging",
            "--api-host",
            STAGING_URL,
            "entry",
            "write",
            "--journal-id",
            journal_id,
            "--body",
            "daily summary",
            "--date",
            "2026-03-30T18:45:00Z",
            "--all-day",
        ],
    );
    let parsed: Value = serde_json::from_str(&write_output).expect("write output should be json");
    let entry_id = parsed
        .get("entry_id")
        .and_then(Value::as_str)
        .expect("entry_id should exist");

    let conn = Connection::open(staging_db_path(&config_dir)).expect("sqlite should open");
    let entry_json: String = conn
        .query_row(
            "SELECT data_json FROM entries WHERE journal_id = ?1 AND id = ?2",
            params![journal_id, entry_id],
            |r| r.get(0),
        )
        .expect("entry row should exist");
    let entry_value: Value = serde_json::from_str(&entry_json).expect("entry json should parse");
    let payload = entry_value.get("payload").unwrap_or(&entry_value);

    assert_eq!(payload.get("isAllDay").and_then(Value::as_bool), Some(true));
    assert_eq!(payload.get("timeZone").and_then(Value::as_str), Some(""));

    let stored_epoch_ms = payload
        .get("date")
        .and_then(Value::as_f64)
        .map(|value| value as i64)
        .expect("date should exist");

    let date_only_output = run_dayone(
        &config_dir,
        &[
            "--profile",
            "staging",
            "--api-host",
            STAGING_URL,
            "entry",
            "write",
            "--journal-id",
            journal_id,
            "--body",
            "daily summary date-only reference",
            "--date",
            "2026-03-30",
        ],
    );
    let date_only_parsed: Value =
        serde_json::from_str(&date_only_output).expect("date-only output should be json");
    let date_only_entry_id = date_only_parsed
        .get("entry_id")
        .and_then(Value::as_str)
        .expect("date-only entry_id should exist");
    let date_only_json: String = conn
        .query_row(
            "SELECT data_json FROM entries WHERE journal_id = ?1 AND id = ?2",
            params![journal_id, date_only_entry_id],
            |r| r.get(0),
        )
        .expect("date-only entry row should exist");
    let date_only_value: Value =
        serde_json::from_str(&date_only_json).expect("date-only entry json should parse");
    let date_only_payload = date_only_value.get("payload").unwrap_or(&date_only_value);
    let expected_epoch_ms = date_only_payload
        .get("date")
        .and_then(Value::as_f64)
        .map(|value| value as i64)
        .expect("date-only date should exist");

    assert_eq!(
        stored_epoch_ms, expected_epoch_ms,
        "all-day RFC3339 should store the same local-midnight date as YYYY-MM-DD"
    );
}

#[test]
fn entry_write_all_day_without_date_uses_local_midnight_shape() {
    let config_dir = unique_tmp_dir("all-day-no-date");
    let journal_id = "j-test-all-day-no-date";
    seed_local_auth_and_journal(&config_dir, journal_id);

    let write_output = run_dayone(
        &config_dir,
        &[
            "--profile",
            "staging",
            "--api-host",
            STAGING_URL,
            "entry",
            "write",
            "--journal-id",
            journal_id,
            "--body",
            "daily summary no date",
            "--all-day",
        ],
    );
    let parsed: Value = serde_json::from_str(&write_output).expect("write output should be json");
    let entry_id = parsed
        .get("entry_id")
        .and_then(Value::as_str)
        .expect("entry_id should exist");

    let conn = Connection::open(staging_db_path(&config_dir)).expect("sqlite should open");
    let entry_json: String = conn
        .query_row(
            "SELECT data_json FROM entries WHERE journal_id = ?1 AND id = ?2",
            params![journal_id, entry_id],
            |r| r.get(0),
        )
        .expect("entry row should exist");
    let entry_value: Value = serde_json::from_str(&entry_json).expect("entry json should parse");
    let payload = entry_value.get("payload").unwrap_or(&entry_value);

    assert_eq!(payload.get("isAllDay").and_then(Value::as_bool), Some(true));
    assert_eq!(payload.get("timeZone").and_then(Value::as_str), Some(""));

    let stored_epoch_ms = payload
        .get("date")
        .and_then(Value::as_f64)
        .map(|value| value as i64)
        .expect("date should exist");
    let stored_dt =
        OffsetDateTime::from_unix_timestamp_nanos(i128::from(stored_epoch_ms) * 1_000_000)
            .expect("stored date should be a valid instant");
    let local_offset = UtcOffset::local_offset_at(stored_dt)
        .expect("local offset should resolve for stored instant");
    let local_time = stored_dt.to_offset(local_offset).time();
    assert_eq!(local_time.hour(), 0);
    assert_eq!(local_time.minute(), 0);
    assert_eq!(local_time.second(), 0);
}

#[test]
fn image_attachment_persists_thumbnail_metadata_and_bytes() {
    let config_dir = unique_tmp_dir("thumbs");
    let journal_id = "j-test-4";
    seed_local_auth_and_journal(&config_dir, journal_id);

    let attachment = config_dir.join("sample.png");
    write_temp_attachment(&attachment, tiny_png_bytes());

    let write_output = run_dayone(
        &config_dir,
        &[
            "--profile",
            "staging",
            "--api-host",
            STAGING_URL,
            "entry",
            "write",
            "--journal-id",
            journal_id,
            "--body",
            "entry with png attachment",
            "--attach",
            attachment.to_str().expect("path should be utf8"),
            "--attach-type",
            "image",
        ],
    );
    let parsed: Value = serde_json::from_str(&write_output).expect("write output should be json");
    let entry_id = parsed
        .get("entry_id")
        .and_then(Value::as_str)
        .expect("entry_id should exist");

    let conn = Connection::open(staging_db_path(&config_dir)).expect("sqlite should open");
    let row = conn
        .query_row(
            r#"
            SELECT
              width,
              height,
              thumbnail_content_type,
              thumbnail_md5,
              thumbnail_width,
              thumbnail_height,
              thumbnail_file_size_bytes,
              length(thumbnail_bytes)
            FROM entry_attachments
            WHERE journal_id = ?1 AND entry_id = ?2
            LIMIT 1
            "#,
            params![journal_id, entry_id],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, Option<i64>>(7)?,
                ))
            },
        )
        .expect("attachment row should exist");

    assert!(row.0.unwrap_or_default() > 0);
    assert!(row.1.unwrap_or_default() > 0);
    assert_eq!(row.2.as_deref(), Some("image/jpeg"));
    assert!(row.3.is_some());
    assert!(row.4.unwrap_or_default() > 0);
    assert!(row.5.unwrap_or_default() > 0);
    assert!(row.6.unwrap_or_default() > 0);
    assert!(row.7.unwrap_or_default() > 0);

    let entry_json: String = conn
        .query_row(
            "SELECT data_json FROM entries WHERE journal_id = ?1 AND id = ?2",
            params![journal_id, entry_id],
            |r| r.get(0),
        )
        .expect("entry row should exist");
    let entry_value: Value = serde_json::from_str(&entry_json).expect("entry json should parse");
    let moments = entry_value
        .get("payload")
        .and_then(|payload| payload.get("moments"))
        .or_else(|| entry_value.get("moments"))
        .and_then(Value::as_array)
        .expect("moments should exist");
    let first_thumbnail = moments
        .first()
        .and_then(|moment| moment.get("thumbnail"))
        .expect("moment thumbnail should be present");
    assert_eq!(
        first_thumbnail.get("contentType").and_then(Value::as_str),
        Some("image/jpeg")
    );
}

#[test]
fn pdf_attachment_persists_thumbnail_metadata_and_bytes() {
    let config_dir = unique_tmp_dir("pdf-thumbs");
    let journal_id = "j-test-5";
    seed_local_auth_and_journal(&config_dir, journal_id);

    let attachment = config_dir.join("sample.pdf");
    write_temp_attachment(&attachment, &sample_pdf_bytes());

    let write_output = run_dayone(
        &config_dir,
        &[
            "--profile",
            "staging",
            "--api-host",
            STAGING_URL,
            "entry",
            "write",
            "--journal-id",
            journal_id,
            "--body",
            "entry with pdf attachment",
            "--attach",
            attachment.to_str().expect("path should be utf8"),
            "--attach-type",
            "pdfAttachment",
        ],
    );
    let parsed: Value = serde_json::from_str(&write_output).expect("write output should be json");
    let entry_id = parsed
        .get("entry_id")
        .and_then(Value::as_str)
        .expect("entry_id should exist");

    let conn = Connection::open(staging_db_path(&config_dir)).expect("sqlite should open");
    let row = conn
        .query_row(
            r#"
            SELECT
              width,
              height,
              thumbnail_content_type,
              thumbnail_md5,
              thumbnail_width,
              thumbnail_height,
              thumbnail_file_size_bytes,
              length(thumbnail_bytes)
            FROM entry_attachments
            WHERE journal_id = ?1 AND entry_id = ?2
            LIMIT 1
            "#,
            params![journal_id, entry_id],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, Option<i64>>(7)?,
                ))
            },
        )
        .expect("attachment row should exist");

    assert!(row.0.unwrap_or_default() > 0);
    assert!(row.1.unwrap_or_default() > 0);
    assert_eq!(row.2.as_deref(), Some("image/jpeg"));
    assert!(row.3.is_some());
    assert!(row.4.unwrap_or_default() > 0);
    assert!(row.5.unwrap_or_default() > 0);
    assert!(row.6.unwrap_or_default() > 0);
    assert!(row.7.unwrap_or_default() > 0);

    let entry_json: String = conn
        .query_row(
            "SELECT data_json FROM entries WHERE journal_id = ?1 AND id = ?2",
            params![journal_id, entry_id],
            |r| r.get(0),
        )
        .expect("entry row should exist");
    let entry_value: Value = serde_json::from_str(&entry_json).expect("entry json should parse");
    let moments = entry_value
        .get("payload")
        .and_then(|payload| payload.get("moments"))
        .or_else(|| entry_value.get("moments"))
        .and_then(Value::as_array)
        .expect("moments should exist");
    let first_moment = moments.first().expect("moment should exist");
    assert_eq!(
        first_moment.get("type").and_then(Value::as_str),
        Some("pdfAttachment")
    );
    let first_thumbnail = first_moment
        .get("thumbnail")
        .expect("moment thumbnail should be present");
    assert_eq!(
        first_thumbnail.get("contentType").and_then(Value::as_str),
        Some("image/jpeg")
    );
}
