use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;

use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

struct SyncServer {
    url: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl SyncServer {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(err) => panic!("accept: {err}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 4096];
                while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                    let count = stream.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                }
                let request = String::from_utf8_lossy(&request);
                let path = request.split_whitespace().nth(1).unwrap();
                let path = path.strip_prefix("/api").unwrap();
                let body = if path.starts_with("/v2/sync/entries/") {
                    if path.contains("cursor=") {
                        String::new()
                    } else {
                        let entries = if path.contains("/journal-a/") {
                            vec![
                                json!({"id":"entry-a", "body":"Coffee after a walk", "cursor":"page-done"}),
                                json!({"id":"entry-empty", "body":"  ", "cursor":"page-done"}),
                                json!({"id":"entry-current", "body":"Already indexed", "cursor":"page-done"}),
                            ]
                        } else {
                            vec![
                                json!({"id":"entry-b", "body":"Dinner with friends", "cursor":"page-done"}),
                                json!({"id":"entry-c", "body":"A walk by the sea", "cursor":"page-done"}),
                            ]
                        };
                        entries
                            .into_iter()
                            .map(|entry| format!("{entry}\n"))
                            .collect()
                    }
                } else if path.starts_with("/user-settings") {
                    json!({"sync_marker":"after-entries"}).to_string()
                } else {
                    json!({"items":[]}).to_string()
                };
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        Self {
            url,
            stop,
            worker: Some(worker),
        }
    }

    fn run(&self, root: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_dayone"))
            .env_clear()
            .env("HOME", root)
            .env("XDG_CONFIG_HOME", root.join("xdg"))
            .env("DAYONE_CONFIG_DIR", root.join("config"))
            .env("DAYONE_TELEMETRY", "0")
            .env("DO_NOT_TRACK", "1")
            .env("FASTEMBED_CACHE_DIR", root.join("cache"))
            .env("HF_HOME", root.join("cache"))
            .env("HF_ENDPOINT", "http://127.0.0.1:0")
            .args(["--profile", "production", "--api-host", &self.url])
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for SyncServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}

#[test]
fn sync_reports_embedding_failure_after_preserving_data_and_finishing_other_work() {
    let server = SyncServer::new();
    let root = tempfile::tempdir().unwrap();
    assert!(
        server
            .run(root.path(), &["list", "journals"])
            .status
            .success()
    );
    let db = Connection::open(root.path().join("config/profiles/production/dayone.db")).unwrap();
    db.execute(
        "UPDATE profiles SET base_url = ?1 WHERE name = 'production'",
        [&server.url],
    )
    .unwrap();
    db.execute("INSERT INTO auth_sessions (profile_id, token, token_created_at, user_json) SELECT id, 'synthetic-token', CURRENT_TIMESTAMP, '{}' FROM profiles WHERE name = 'production'", []).unwrap();

    let empty = server.run(root.path(), &["sync"]);
    assert!(
        empty.status.success(),
        "{}",
        String::from_utf8_lossy(&empty.stderr)
    );
    for journal in ["journal-a", "journal-b"] {
        db.execute(
            "INSERT INTO journal_sync_info (journal_id, status) VALUES (?1, 'idle')",
            [journal],
        )
        .unwrap();
    }
    db.execute("INSERT INTO entry_embeddings (entry_id, journal_id, source_hash, embedding_json, dimension) VALUES ('entry-current', 'journal-a', ?1, ?2, 384)",
        params![format!("{:x}", Sha256::digest(b"Already indexed")), json!(vec![0.0; 384]).to_string()]).unwrap();

    let output = server.run(root.path(), &["sync"]);
    let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
    if cfg!(feature = "embeddings") {
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("entry embedding generation failed"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        output.status.success(),
        !cfg!(feature = "embeddings"),
        "{summary}"
    );
    assert_eq!(summary["ok"], !cfg!(feature = "embeddings"));
    assert!(
        summary["fatal_error"].is_null(),
        "data sync must not be reported as aborted"
    );
    if cfg!(feature = "embeddings") {
        assert_eq!(summary["embeddings"]["status"], "failed");
        assert_eq!(summary["embeddings"]["error"], "generation_failed");
        assert_eq!(summary["embeddings"]["failed"], 1);
        assert_eq!(summary["embeddings"]["skipped"], 2);
        assert_eq!(summary["embeddings"]["unchanged"], 1);
        assert_eq!(summary["embeddings"]["generated"], 0);
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("dayone embeddings recalculate-entries")
        );
    } else {
        assert_eq!(summary["embeddings"]["status"], "disabled");
        assert_eq!(summary["embeddings"]["failed"], 0);
    }
    let resources = summary["resources"].as_array().unwrap();
    assert!(
        resources
            .iter()
            .all(|resource| resource["status"] == "success"),
        "{resources:?}"
    );
    assert!(
        resources
            .iter()
            .any(|resource| resource["resource"] == "outbox:post")
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM entries", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        5
    );
    for journal in ["journal-a", "journal-b"] {
        assert_eq!(
            db.query_row(
                "SELECT cursor FROM sync_cursors WHERE resource_key = ?1",
                [format!("entries:{journal}")],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "page-done"
        );
    }
    assert!(
        db.query_row("SELECT data_json FROM user_settings", [], |row| row
            .get::<_, String>(0))
            .unwrap()
            .contains("after-entries")
    );
    assert_eq!(
        db.query_row(
            "SELECT status FROM sync_runs ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        if cfg!(feature = "embeddings") {
            "failed"
        } else {
            "success"
        }
    );

    let later_sync = server.run(root.path(), &["sync"]);
    assert!(later_sync.status.success());
    let later_summary: Value = serde_json::from_slice(&later_sync.stdout).unwrap();
    assert_eq!(later_summary["embeddings"]["generated"], 0);
    assert_eq!(later_summary["embeddings"]["failed"], 0);
    if cfg!(feature = "embeddings") {
        let doctor = server.run(root.path(), &["doctor"]);
        assert!(doctor.status.success());
        let report: Value = serde_json::from_slice(&doctor.stdout).unwrap();
        let check = report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["check"] == "embedding_index")
            .unwrap();
        assert_eq!(check["status"], "finding");
        assert_eq!(check["details"]["missing_embeddings"], 3);
        assert_eq!(check["details"]["current_embeddings"], 1);

        db.execute_batch(
            "DELETE FROM sync_cursors;
             INSERT INTO entry_embeddings (entry_id, journal_id, source_hash, embedding_json, dimension)
             VALUES ('entry-a', 'journal-a', x'80', '[]', 384);",
        ).unwrap();
        let storage = server.run(root.path(), &["sync"]);
        assert_eq!(storage.status.code(), Some(1));
        let summary: Value = serde_json::from_slice(&storage.stdout).unwrap();
        assert_eq!(summary["ok"], false);
        assert!(summary["fatal_error"].is_null());
        assert_eq!(
            summary["embeddings"],
            json!({
                "status":"failed", "error":"storage_failed", "generated":0,
                "unchanged":1, "failed":1, "skipped":2
            })
        );
    }
}
