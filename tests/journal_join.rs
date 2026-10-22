use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use rusqlite::{Connection, params};
use serde_json::{Value, json};

fn run(config: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    // Explicit host arguments take precedence over both inherited env and secrets-cli.
    let mut child = Command::new(env!("CARGO_BIN_EXE_dayone"))
        .env("DAYONE_CONFIG_DIR", config)
        .env("DO_NOT_TRACK", "1")
        .env("DAYONE_API_HOST", "https://stg.dayone.me")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    } else {
        drop(child.stdin.take());
    }
    child.wait_with_output().unwrap()
}

#[test]
fn invitation_stdin_and_positional_inputs_dispatch_and_return_json() {
    let config = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{address}");
    let link = format!(
        "https://{address}/accept-journal-invite?token=cli-test-invite#BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc="
    );
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/shared-journal-invitation.json")).unwrap();
    let info = json!({
        "journal_id": "shared-journal-id",
        "expiration_date": "2099-01-01T00:00:00Z",
        "owner_public_key": fixture["owner_public_key"],
        "owner_public_key_hmac": fixture["owner_public_key_hmac"],
        "participants": [{"id":"participant", "role":"member"}]
    })
    .to_string();
    assert!(
        run(
            config.path(),
            &["profile", "set", "fixture", "--api-host", &base],
            None
        )
        .status
        .success()
    );
    assert!(
        run(
            config.path(),
            &[
                "--profile",
                "fixture",
                "--api-host",
                &base,
                "list",
                "journals"
            ],
            None
        )
        .status
        .success()
    );
    let conn = Connection::open(config.path().join("profiles/fixture/dayone.db")).unwrap();
    let profile_id: i64 = conn
        .query_row(
            "SELECT id FROM profiles WHERE base_url = ?1",
            [&base],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO auth_sessions (profile_id, token, token_created_at, user_json, created_at, updated_at) VALUES (?1, 'cli-test-bearer', '2026-01-01T00:00:00Z', '{\"id\":\"participant\"}', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        params![profile_id],
    ).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        for _ in 0..2 {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "CLI did not fetch the invitation"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(err) => panic!("{err}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let mut bytes = [0; 2048];
                let count = stream.read(&mut bytes).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&bytes[..count]);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("GET /api/shares/invite/cli-test-invite HTTP/1.1"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer cli-test-bearer")
            );
            assert!(!request.contains("BwcHBwcH"));
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", info.len(), info).unwrap();
        }
    });
    for (mut args, stdin) in [
        (
            vec!["--profile", "fixture", "journal", "join", link.as_str()],
            None,
        ),
        (
            vec!["--profile", "fixture", "journal", "join", "--link-stdin"],
            Some(format!("{link}\n")),
        ),
    ] {
        args.extend(["--api-host", base.as_str()]);
        let output = run(config.path(), &args, stdin.as_deref());
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["journal_id"], "shared-journal-id");
        assert_eq!(value["status"], "already_member");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("cli-test-invite"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("cli-test-invite"));
    }
    server.join().unwrap();
    for input in ["\n", "invalid-private-invitation-token#private-key\n"] {
        let output = run(
            config.path(),
            &["--profile", "fixture", "journal", "join", "--link-stdin"],
            Some(input),
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-invitation-token"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-key"));
    }
}
