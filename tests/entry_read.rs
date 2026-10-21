use std::process::{Command, Output};

use rusqlite::{Connection, params};
use serde_json::{Value, json};

fn run(config: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dayone"))
        .env("DAYONE_CONFIG_DIR", config)
        .env("DAYONE_TELEMETRY", "0")
        .env("DO_NOT_TRACK", "1")
        .args(["--profile", "staging"])
        .args(args)
        .output()
        .expect("CLI must execute")
}

#[test]
fn single_entry_read_by_id_returns_content_tags_and_journal() {
    let config = tempfile::tempdir().unwrap();
    assert!(run(config.path(), &["list", "journals"]).status.success());
    let connection = Connection::open(config.path().join("profiles/staging/dayone.db")).unwrap();
    let entry_id = "A65AFA22B98A0DEC62EAAD3CD1CCF45D";
    let entry = json!({
        "id": entry_id,
        "body": "Text to inspect before an update",
        "tags": ["travel", "family"],
        "location": {"city": "Makassar"}
    });
    connection
        .execute(
            "INSERT INTO entries (id, journal_id, updated_at, is_deleted, data_json)
         VALUES (?1, 'journal-2', '2026-10-05T19:17:36Z', 0, ?2)",
            params![entry_id, entry.to_string()],
        )
        .unwrap();

    for command in ["read", "get"] {
        for journal in [None, Some("journal-2")] {
            let mut args = vec![
                "entry",
                command,
                "--entry-id",
                "a65afa22b98a0dec62eaad3cd1ccf45d",
            ];
            if let Some(journal) = journal {
                args.extend(["--journal-id", journal]);
            }
            let result = run(config.path(), &args);
            assert!(
                result.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            let output: Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(output["ok"], true);
            assert_eq!(output["entry_id"], entry_id);
            assert_eq!(output["journal_id"], "journal-2");
            assert_eq!(output["entry"]["body"], entry["body"]);
            assert_eq!(output["entry"]["tags"], entry["tags"]);
            assert_eq!(output["entry"]["location"], entry["location"]);
        }
    }

    let mismatch = run(
        config.path(),
        &[
            "entry",
            "read",
            "--entry-id",
            entry_id,
            "--journal-id",
            "journal-1",
        ],
    );
    assert!(!mismatch.status.success());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("belongs to journal"));

    let missing = run(config.path(), &["entry", "read", "--entry-id", "missing"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("not found locally"));
}
