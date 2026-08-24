use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{Connection, params};
use serde_json::Value;
use tempfile::TempDir;

const STAGING_URL: &str = "https://stg.dayone.me";

fn run_dayone(config_dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_dayone"))
        .env("DAYONE_CONFIG_DIR", config_dir)
        .args(args)
        .output()
        .expect("dayone command should execute")
}

fn run_dayone_ok(config_dir: &Path, args: &[&str]) -> String {
    let output = run_dayone(config_dir, args);
    if !output.status.success() {
        panic!(
            "dayone command failed: args={args:?}\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).expect("stdout should be utf8")
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/memory-process")
        .join(name)
}

/// Per-profile DB path: `{config_dir}/profiles/staging/dayone.db`
fn staging_db_path(config_dir: &Path) -> PathBuf {
    config_dir
        .join("profiles")
        .join("staging")
        .join("dayone.db")
}

fn seed_local_auth(config_dir: &Path) {
    fs::create_dir_all(config_dir).expect("config dir should create");
    // Run any Store-opening command to trigger per-profile DB creation.
    // Profile commands no longer open the Store, so we use `list journals`.
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
    let conn = Connection::open(&db_path).expect("sqlite should open");
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
}

#[test]
fn memory_process_skips_when_no_new_user_messages() {
    let temp_dir = TempDir::new().expect("temp dir should create");
    let config_dir = temp_dir.path();
    seed_local_auth(config_dir);

    let payload_file = fixture_path("conversation-skip-no-user.json");
    let raw = run_dayone_ok(
        config_dir,
        &[
            "--profile",
            "staging",
            "--api-host",
            STAGING_URL,
            "memory",
            "process",
            "--json-file",
            payload_file.to_str().expect("fixture path should be utf8"),
        ],
    );
    let output: Value = serde_json::from_str(&raw).expect("output should be json");
    let expected: Value = serde_json::from_str(
        &fs::read_to_string(fixture_path("expected-skip-no-user.json"))
            .expect("expected fixture should read"),
    )
    .expect("expected fixture should parse");
    // Keep `profile_id` structural (it depends on local DB init), and assert stable fields via fixture.

    assert_eq!(output["ok"], expected["ok"]);
    assert_eq!(output["base_url"], expected["base_url"]);
    assert!(
        output["profile_id"]
            .as_i64()
            .is_some_and(|profile_id| profile_id > 0),
        "profile_id should be present and positive"
    );
    assert_eq!(output["processed"], expected["processed"]);
    assert_eq!(output["skipped_reason"], expected["skipped_reason"]);
    assert_eq!(output["api_loops"], expected["api_loops"]);
    assert_eq!(output["new_message_count"], expected["new_message_count"]);
    assert_eq!(
        output["context_message_count"],
        expected["context_message_count"]
    );
    assert_eq!(
        output["last_memory_check_message_id"],
        expected["last_memory_check_message_id"]
    );
    assert_eq!(output["updated_summary"], expected["updated_summary"]);
    assert_eq!(output["memory_changes"], expected["memory_changes"]);
}
