use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde_json::Value;

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

fn parse_json(raw: &str) -> Value {
    serde_json::from_str(raw).expect("output should be valid JSON")
}

// ---------------------------------------------------------------------------
// profile list
// ---------------------------------------------------------------------------

#[test]
fn first_run_telemetry_notice_uses_stderr_once() {
    let config_dir = unique_tmp_dir("telemetry-notice");

    let first = run_dayone(&config_dir, &["profile", "list"]);
    assert!(first.status.success());
    parse_json(&String::from_utf8(first.stdout).expect("stdout should be utf8"));
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(stderr.contains("telemetry is enabled by default"));
    assert!(stderr.contains("Usage analytics is anonymous"));
    assert!(stderr.contains("not linked to your Day One account"));

    let second = run_dayone(&config_dir, &["profile", "list"]);
    assert!(second.status.success());
    parse_json(&String::from_utf8(second.stdout).expect("stdout should be utf8"));
    assert!(second.stderr.is_empty());

    let _ = std::fs::remove_dir_all(config_dir);
}

#[test]
fn profile_list_returns_default_profiles() {
    let config_dir = unique_tmp_dir("profile-list-defaults");
    let raw = run_dayone_ok(&config_dir, &["profile", "list"]);
    let output = parse_json(&raw);

    assert_eq!(output["ok"], true);
    let profiles = output["profiles"]
        .as_array()
        .expect("profiles should be array");
    assert_eq!(profiles.len(), 2);

    let names: Vec<&str> = profiles
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"staging"));
    assert!(names.contains(&"production"));

    // Each profile should have base_url and is_active fields.
    for profile in profiles {
        assert!(profile["base_url"].is_string());
        assert!(profile["is_active"].is_boolean());
    }
}

#[test]
fn profile_list_marks_active_profile() {
    let config_dir = unique_tmp_dir("profile-list-active");

    // Default active profile is staging (non-production build).
    let raw = run_dayone_ok(&config_dir, &["profile", "list"]);
    let output = parse_json(&raw);
    let profiles = output["profiles"].as_array().unwrap();

    let active_count = profiles.iter().filter(|p| p["is_active"] == true).count();
    assert_eq!(active_count, 1, "exactly one profile should be active");
}

#[test]
fn profile_list_does_not_require_store() {
    let config_dir = unique_tmp_dir("profile-list-no-store");
    // profile list should succeed without any DB file existing.
    let raw = run_dayone_ok(&config_dir, &["profile", "list"]);
    let output = parse_json(&raw);
    assert_eq!(output["ok"], true);

    // No profiles/ directory should have been created.
    assert!(!config_dir.join("profiles").exists());
}

// ---------------------------------------------------------------------------
// profile set
// ---------------------------------------------------------------------------

#[test]
fn profile_set_switches_active_profile() {
    let config_dir = unique_tmp_dir("profile-set-switch");

    // Initialise config.
    run_dayone_ok(&config_dir, &["profile", "list"]);

    // Switch to production.
    let raw = run_dayone_ok(&config_dir, &["profile", "set", "production"]);
    let output = parse_json(&raw);
    assert_eq!(output["ok"], true);
    assert_eq!(output["active"]["name"], "production");
    assert_eq!(output["active"]["is_active"], true);

    // Verify config.toml was updated.
    let config_contents =
        std::fs::read_to_string(config_dir.join("config.toml")).expect("config.toml should exist");
    assert!(config_contents.contains("active_profile = \"production\""));
}

#[test]
fn profile_set_rejects_invalid_name() {
    let config_dir = unique_tmp_dir("profile-set-invalid");
    run_dayone_ok(&config_dir, &["profile", "list"]);

    let output = run_dayone(&config_dir, &["profile", "set", "../escape"]);
    assert!(
        !output.status.success(),
        "path traversal name should be rejected"
    );

    let output = run_dayone(&config_dir, &["profile", "set", ""]);
    assert!(!output.status.success(), "empty name should be rejected");
}

#[test]
fn profile_set_unknown_name_without_url_fails() {
    let config_dir = unique_tmp_dir("profile-set-unknown");
    run_dayone_ok(&config_dir, &["profile", "list"]);

    let output = run_dayone(&config_dir, &["profile", "set", "nonexistent"]);
    assert!(
        !output.status.success(),
        "setting unknown profile without --api-host should fail"
    );
}

#[test]
fn profile_set_creates_custom_profile_with_url() {
    let config_dir = unique_tmp_dir("profile-set-custom");
    run_dayone_ok(&config_dir, &["profile", "list"]);

    let raw = run_dayone_ok(
        &config_dir,
        &[
            "profile",
            "set",
            "custom-env",
            "--api-host",
            "https://custom.example.com",
        ],
    );
    let output = parse_json(&raw);
    assert_eq!(output["ok"], true);
    assert_eq!(output["active"]["name"], "custom-env");
    assert_eq!(output["active"]["base_url"], "https://custom.example.com");

    // The new profile should appear in profile list.
    let list_raw = run_dayone_ok(&config_dir, &["profile", "list"]);
    let list_output = parse_json(&list_raw);
    let profiles = list_output["profiles"].as_array().unwrap();
    let names: Vec<&str> = profiles
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"custom-env"));
    assert_eq!(profiles.len(), 3); // staging + production + custom-env
}

// ---------------------------------------------------------------------------
// Round-trip persistence: profile set → profile list
// ---------------------------------------------------------------------------

#[test]
fn profile_set_persists_across_commands() {
    let config_dir = unique_tmp_dir("profile-roundtrip");

    // Start with defaults — active is staging.
    run_dayone_ok(&config_dir, &["profile", "list"]);

    // Switch to production.
    run_dayone_ok(&config_dir, &["profile", "set", "production"]);

    // A fresh profile list should show production as active.
    let raw = run_dayone_ok(&config_dir, &["profile", "list"]);
    let output = parse_json(&raw);
    let profiles = output["profiles"].as_array().unwrap();

    let production = profiles.iter().find(|p| p["name"] == "production").unwrap();
    let staging = profiles.iter().find(|p| p["name"] == "staging").unwrap();
    assert_eq!(production["is_active"], true);
    assert_eq!(staging["is_active"], false);
}

// ---------------------------------------------------------------------------
// --profile flag selects per-profile DB
// ---------------------------------------------------------------------------

#[test]
fn profile_flag_selects_correct_db() {
    let config_dir = unique_tmp_dir("profile-flag-db");
    let staging_url = "https://stg.dayone.me";
    let production_url = "https://dayone.me";

    // Trigger DB creation for both profiles by running a Store-opening command.
    run_dayone(
        &config_dir,
        &[
            "--profile",
            "staging",
            "--api-host",
            staging_url,
            "list",
            "journals",
        ],
    );
    run_dayone(
        &config_dir,
        &[
            "--profile",
            "production",
            "--api-host",
            production_url,
            "list",
            "journals",
        ],
    );

    // Each profile should have its own DB.
    let staging_db = config_dir
        .join("profiles")
        .join("staging")
        .join("dayone.db");
    let production_db = config_dir
        .join("profiles")
        .join("production")
        .join("dayone.db");
    assert!(staging_db.exists(), "staging DB should exist");
    assert!(production_db.exists(), "production DB should exist");
}

#[test]
fn omitting_profile_flag_uses_active_profile() {
    let config_dir = unique_tmp_dir("profile-flag-default");
    let staging_url = "https://stg.dayone.me";
    let production_url = "https://dayone.me";

    // Initialise config with default active profile.
    run_dayone_ok(&config_dir, &["profile", "list"]);

    // Switch active to production.
    run_dayone_ok(&config_dir, &["profile", "set", "production"]);

    // Run a Store-opening command without --profile — should use production.
    run_dayone(
        &config_dir,
        &["--api-host", production_url, "list", "journals"],
    );

    let production_db = config_dir
        .join("profiles")
        .join("production")
        .join("dayone.db");
    assert!(
        production_db.exists(),
        "production DB should be created when active"
    );

    // Switch to staging and run again.
    run_dayone_ok(&config_dir, &["profile", "set", "staging"]);
    run_dayone(
        &config_dir,
        &["--api-host", staging_url, "list", "journals"],
    );

    let staging_db = config_dir
        .join("profiles")
        .join("staging")
        .join("dayone.db");
    assert!(
        staging_db.exists(),
        "staging DB should be created when active"
    );
}

#[test]
fn api_host_override_does_not_rewrite_profile_origin() {
    let config_dir = unique_tmp_dir("api-host-profile-identity");
    let production_url = "https://dayone.me";

    run_dayone_ok(
        &config_dir,
        &[
            "--profile",
            "production",
            "--api-host",
            production_url,
            "list",
            "journals",
        ],
    );
    run_dayone_ok(
        &config_dir,
        &[
            "--profile",
            "production",
            "--api-host",
            "https://override.example.com",
            "list",
            "journals",
        ],
    );

    let db_path = config_dir
        .join("profiles")
        .join("production")
        .join("dayone.db");
    let connection = Connection::open(db_path).expect("profile DB should open");
    let stored_url: String = connection
        .query_row(
            "SELECT base_url FROM profiles WHERE name = 'production'",
            [],
            |row| row.get(0),
        )
        .expect("production profile should exist");

    assert_eq!(stored_url, production_url);
}

#[test]
fn api_host_override_uses_selected_profile_auth_session() {
    let config_dir = unique_tmp_dir("api-host-auth-session");

    run_dayone_ok(
        &config_dir,
        &["--profile", "production", "list", "journals"],
    );

    let db_path = config_dir
        .join("profiles")
        .join("production")
        .join("dayone.db");
    let connection = Connection::open(db_path).expect("profile DB should open");
    connection
        .execute_batch(
            r#"
            INSERT INTO auth_sessions (profile_id, token, token_created_at, user_json)
            SELECT id, 'stored-token', '2026-08-04T00:00:00Z', '{"id":"user-1"}'
            FROM profiles WHERE name = 'production';

            -- A leftover override session must not replace the selected profile identity.
            INSERT INTO profiles (name, base_url)
            VALUES ('old-override', 'https://override.example.com');
            INSERT INTO auth_sessions (profile_id, token, token_created_at, user_json)
            SELECT id, 'wrong-token', '2026-08-04T00:00:00Z', '{"id":"wrong-user"}'
            FROM profiles WHERE name = 'old-override';
            "#,
        )
        .expect("auth sessions should save");
    drop(connection);

    let raw = run_dayone_ok(
        &config_dir,
        &[
            "--profile",
            "production",
            "--api-host",
            "https://override.example.com",
            "auth",
            "whoami",
        ],
    );
    let output = parse_json(&raw);

    assert_eq!(output["base_url"], "https://override.example.com");
    assert_eq!(output["user"]["id"], "user-1");
}
