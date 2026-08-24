use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use serde_json::Value;

const STAGING_URL: &str = "https://stg.dayone.me";

fn unique_tmp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    std::env::temp_dir().join(format!("dayone-cli-tests-{label}-{nanos}"))
}

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
fn logout_without_force_warns_and_leaves_db_intact() {
    let config_dir = unique_tmp_dir("logout-warn");
    seed_local_auth(&config_dir);

    let db_path = staging_db_path(&config_dir);
    assert!(db_path.exists(), "db should exist before logout");

    let raw = run_dayone_ok(
        &config_dir,
        &[
            "--profile",
            "staging",
            "--api-host",
            STAGING_URL,
            "auth",
            "logout",
        ],
    );
    let output: Value = serde_json::from_str(&raw).expect("output should be json");

    assert_eq!(output["ok"], false);
    assert!(
        output["warning"].as_str().is_some(),
        "warning field should be present"
    );
    assert!(
        output["hint"].as_str().is_some(),
        "hint field should be present"
    );
    assert!(
        db_path.exists(),
        "db should still exist after warning-only logout"
    );
}

#[test]
fn logout_without_force_exits_zero() {
    let config_dir = unique_tmp_dir("logout-exit");
    seed_local_auth(&config_dir);

    let output = run_dayone(
        &config_dir,
        &[
            "--profile",
            "staging",
            "--api-host",
            STAGING_URL,
            "auth",
            "logout",
        ],
    );
    assert!(
        output.status.success(),
        "logout without --force should exit 0"
    );
}

#[test]
fn logout_with_force_removes_db_and_returns_ok() {
    let config_dir = unique_tmp_dir("logout-force");
    seed_local_auth(&config_dir);

    let db_path = staging_db_path(&config_dir);
    assert!(db_path.exists(), "db should exist before --force logout");

    let raw = run_dayone_ok(
        &config_dir,
        &[
            "--profile",
            "staging",
            "--api-host",
            STAGING_URL,
            "auth",
            "logout",
            "--force",
        ],
    );
    let output: Value = serde_json::from_str(&raw).expect("output should be json");

    assert_eq!(output["ok"], true);
    assert!(
        output.get("warning").is_none(),
        "warning should be absent on success"
    );
    assert!(
        !db_path.exists(),
        "db should be removed after --force logout"
    );
}

#[test]
fn logout_with_force_on_empty_store_still_succeeds() {
    let config_dir = unique_tmp_dir("logout-force-empty");
    fs::create_dir_all(&config_dir).expect("config dir should create");
    // Trigger staging profile DB creation with a Store-opening command.
    let _ = run_dayone(
        config_dir.as_path(),
        &[
            "--profile",
            "staging",
            "--api-host",
            STAGING_URL,
            "list",
            "journals",
        ],
    );

    let raw = run_dayone_ok(
        &config_dir,
        &[
            "--profile",
            "staging",
            "--api-host",
            STAGING_URL,
            "auth",
            "logout",
            "--force",
        ],
    );
    let output: Value = serde_json::from_str(&raw).expect("output should be json");
    assert_eq!(output["ok"], true);
}
