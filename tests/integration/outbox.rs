use std::process::{Command, Output};

use rusqlite::Connection;
use serde_json::{Value, json};

fn run(config: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dayone"))
        .env("DAYONE_CONFIG_DIR", config)
        .env("DO_NOT_TRACK", "1")
        .env("DAYONE_TELEMETRY", "0")
        .args(["--profile", "staging"])
        .args(args)
        .output()
        .expect("CLI should run")
}

#[test]
fn retry_command_requeues_one_failed_payload_without_syncing() {
    let config = tempfile::tempdir().unwrap();
    assert!(run(config.path(), &["outbox", "list"]).status.success());
    let connection = Connection::open(config.path().join("profiles/staging/dayone.db")).unwrap();
    let payload = json!({"entry_json":{"id":"e","body":"retained text"},"queued_at_epoch_ms":123})
        .to_string();
    connection.execute(
        "INSERT INTO sync_outbox (id, resource, operation, object_id, payload_json, status, attempt_count, next_attempt_epoch_ms) VALUES ('entry:j:e', 'entry', 'update', 'j:e', ?1, 'failed', 8, 0)",
        [&payload],
    ).unwrap();

    let result = run(config.path(), &["outbox", "retry", "--id", "entry:j:e"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(output["id"], "entry:j:e");
    assert_eq!(output["queued"], true);
    assert_eq!(output["synced"], false);
    let (status, saved_payload): (String, String) = connection
        .query_row(
            "SELECT status, payload_json FROM sync_outbox WHERE id = 'entry:j:e'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "pending");
    assert_eq!(saved_payload, payload);
    assert!(
        !run(config.path(), &["outbox", "retry", "--id", "entry:j:e"])
            .status
            .success()
    );
}
