use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU16, Ordering},
};
use std::thread;
use std::time::Duration;

use rusqlite::Connection;
use tempfile::TempDir;

// A real local collector, shared by Tracks and Sentry. Positive controls below
// prove that absence assertions do not pass because the collector is broken.
struct Collector {
    address: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    status: Arc<AtomicU16>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Collector {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let status = Arc::new(AtomicU16::new(200));
        let response_status = status.clone();
        let worker = thread::spawn(move || {
            let mut clients = Vec::new();
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let received = received.clone();
                        let response_status = response_status.clone();
                        clients.push(thread::spawn(move || {
                        stream.set_nonblocking(false).unwrap();
                        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                        let mut request = Vec::new();
                        let mut buffer = [0; 4096];
                        loop {
                            let count = stream.read(&mut buffer).unwrap_or(0);
                            if count == 0 { break; }
                            request.extend_from_slice(&buffer[..count]);
                            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                                let headers = String::from_utf8_lossy(&request[..end]);
                                let length = headers.lines().find_map(|line| {
                                    let (key, value) = line.split_once(':')?;
                                    key.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().unwrap())
                                }).unwrap_or(0);
                                if request.len() >= end + 4 + length { break; }
                            }
                        }
                        if !request.is_empty() {
                            received.lock().unwrap().push(String::from_utf8_lossy(&request).into_owned());
                        }
                        let status = response_status.load(Ordering::Relaxed);
                        let response = format!("HTTP/1.1 {status} Test\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}");
                        let _ = stream.write_all(response.as_bytes());
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("collector accept: {e}"),
                }
            }
            for client in clients {
                client.join().unwrap();
            }
        });
        Self {
            address,
            requests,
            stop,
            status,
            worker: Some(worker),
        }
    }

    fn command(&self, dir: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dayone"));
        command
            .env("DAYONE_CONFIG_DIR", dir)
            // Explicit values prevent secrets-cli from changing these tests.
            .env("DAYONE_TELEMETRY", "1")
            .env("DO_NOT_TRACK", "0")
            .env("DAYONE_API_HOST", "https://dayone.me")
            .env(
                "DAYONE_TRACKS_ENDPOINT",
                format!("http://{}/tracks", self.address),
            )
            .env(
                "DAYONE_SENTRY_DSN",
                format!("http://test@{}/1", self.address),
            )
            .env("HTTP_PROXY", "")
            .env("HTTPS_PROXY", "")
            .env("ALL_PROXY", "")
            .env("NO_PROXY", "127.0.0.1,localhost");
        command
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn run(&self, dir: &Path, args: &[&str]) {
        assert_success(&self.command(dir).args(args).output().unwrap());
    }

    fn tracks_batches(&self) -> Vec<serde_json::Value> {
        self.requests()
            .iter()
            .filter(|request| request.starts_with("POST /tracks "))
            .map(|request| serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap())
            .collect()
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn seed_config(dir: &Path, analytics: &str) {
    std::fs::write(dir.join("config.toml"), format!(
        "version = 1\nactive_profile = 'production'\n[profiles.production]\nbase_url = 'https://dayone.me'\n[analytics]\n{analytics}\n"
    )).unwrap();
}

fn config(dir: &Path) -> toml::Value {
    toml::from_str(&std::fs::read_to_string(dir.join("config.toml")).unwrap()).unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("stdout stays JSON");
}

fn assert_no_collection(dir: &Path, collector: &Collector) {
    assert!(
        collector.requests().is_empty(),
        "telemetry reached the collector"
    );
    assert!(
        config(dir)["analytics"].get("anonymous_id").is_none(),
        "tracking identifier was persisted"
    );
    let db = dir.join("profiles/production/dayone.db");
    if db.exists() {
        let count: i64 = Connection::open(db)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM analytics_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "telemetry was queued without consent");
    }
}

#[test]
fn undecided_unattended_commands_run_without_collection_in_every_region() {
    for region in ["eu", "non-eu", ""] {
        let dir = TempDir::new().unwrap();
        seed_config(dir.path(), "");
        let collector = Collector::new();
        let output = collector
            .command(dir.path())
            .env("DAYONE_TELEMETRY_REGION", region)
            .args(["outbox", "list"])
            .output()
            .unwrap();
        assert_success(&output);
        assert_no_collection(dir.path(), &collector);
    }
}

#[test]
fn refusal_is_respected_regardless_of_region() {
    for region in ["eu", "non-eu"] {
        let dir = TempDir::new().unwrap();
        seed_config(dir.path(), "consent = 'denied'");
        let collector = Collector::new();
        assert_success(
            &collector
                .command(dir.path())
                .env("DAYONE_TELEMETRY_REGION", region)
                .args(["outbox", "list"])
                .output()
                .unwrap(),
        );
        assert_no_collection(dir.path(), &collector);
    }
}

#[test]
fn old_notice_is_not_consent_and_does_not_hide_the_corrected_disclosure() {
    let dir = TempDir::new().unwrap();
    seed_config(dir.path(), "notice_shown = true");
    let collector = Collector::new();
    let first = collector
        .command(dir.path())
        .args(["outbox", "list"])
        .output()
        .unwrap();
    assert_success(&first);
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(stderr.contains("Telemetry is off"), "{stderr}");
    assert!(stderr.contains("installation ID"), "{stderr}");
    assert!(!stderr.contains("never include journal"));
    assert_no_collection(dir.path(), &collector);
    let second = collector
        .command(dir.path())
        .args(["outbox", "list"])
        .output()
        .unwrap();
    assert_success(&second);
    assert!(second.stderr.is_empty());
}

#[test]
fn unversioned_or_stale_grants_are_not_current_consent() {
    for grant in [
        "consent = 'granted'",
        "consent = 'granted'\nconsent_version = 999\nconsent_recorded_at_ms = 1",
    ] {
        let dir = TempDir::new().unwrap();
        seed_config(dir.path(), grant);
        let collector = Collector::new();
        assert_success(
            &collector
                .command(dir.path())
                .args(["outbox", "list"])
                .output()
                .unwrap(),
        );
        assert_no_collection(dir.path(), &collector);
    }
}

#[test]
fn environment_opt_out_does_not_create_an_identifier() {
    for (key, value) in [("DO_NOT_TRACK", "1"), ("DAYONE_TELEMETRY", "0")] {
        let dir = TempDir::new().unwrap();
        seed_config(
            dir.path(),
            "consent = 'granted'\nconsent_version = 1\nconsent_recorded_at_ms = 1",
        );
        let collector = Collector::new();
        assert_success(
            &collector
                .command(dir.path())
                .env(key, value)
                .args(["outbox", "list"])
                .output()
                .unwrap(),
        );
        assert_no_collection(dir.path(), &collector);
    }
}

#[cfg(feature = "telemetry")]
#[test]
fn startup_and_profile_failures_do_not_reach_sentry_without_consent() {
    for failure in ["config", "profile", "store"] {
        let dir = TempDir::new().unwrap();
        seed_config(dir.path(), "");
        let collector = Collector::new();
        let mut command = collector.command(dir.path());
        match failure {
            "config" => {
                std::fs::write(dir.path().join("config.toml"), "[invalid").unwrap();
            }
            "profile" => {
                command.args(["--profile", "missing"]);
            }
            "store" => {
                std::fs::write(dir.path().join("profiles"), "not a directory").unwrap();
            }
            _ => unreachable!(),
        }
        let output = command.args(["outbox", "list"]).output().unwrap();
        assert!(!output.status.success());
        assert!(
            collector.requests().is_empty(),
            "{failure} error reached Sentry"
        );
    }
}

#[test]
fn explicit_commands_persist_consent_and_allow_withdrawal_without_collection() {
    let dir = TempDir::new().unwrap();
    seed_config(dir.path(), "notice_shown = true");
    let collector = Collector::new();
    assert_success(
        &collector
            .command(dir.path())
            .args(["telemetry", "enable"])
            .output()
            .unwrap(),
    );
    let saved = config(dir.path());
    assert_eq!(saved["analytics"]["consent"].as_str(), Some("granted"));
    assert_eq!(saved["analytics"]["consent_version"].as_integer(), Some(1));
    assert!(
        saved["analytics"]["consent_recorded_at_ms"]
            .as_integer()
            .unwrap()
            > 0
    );
    assert_no_collection(dir.path(), &collector);
    assert!(!dir.path().join("profiles").exists());
    assert_success(
        &collector
            .command(dir.path())
            .args(["outbox", "list"])
            .output()
            .unwrap(),
    );
    assert!(
        collector
            .requests()
            .iter()
            .any(|r| r.starts_with("POST /tracks ")),
        "positive Tracks control"
    );
    assert!(config(dir.path())["analytics"]["anonymous_id"].is_str());
    let count = collector.requests().len();
    assert_success(
        &collector
            .command(dir.path())
            .args(["telemetry", "disable"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        config(dir.path())["analytics"]["consent"].as_str(),
        Some("denied")
    );
    assert_success(
        &collector
            .command(dir.path())
            .args(["outbox", "list"])
            .output()
            .unwrap(),
    );
    assert_eq!(collector.requests().len(), count);
    let status = collector
        .command(dir.path())
        .args(["telemetry", "status"])
        .output()
        .unwrap();
    assert_success(&status);
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["consent"], "denied");
    assert_eq!(status["permitted"], false);
}

#[cfg(feature = "telemetry")]
#[test]
fn consented_command_errors_reach_sentry_but_refused_errors_do_not() {
    for (decision, expect_request) in [("granted", true), ("denied", false)] {
        let dir = TempDir::new().unwrap();
        seed_config(
            dir.path(),
            &format!("consent = '{decision}'\nconsent_version = 1\nconsent_recorded_at_ms = 1"),
        );
        let collector = Collector::new();
        // A missing local entry is raised after normal initialization.
        let output = collector
            .command(dir.path())
            .args([
                "entry",
                "read",
                "--journal-id",
                "missing-journal",
                "--entry-id",
                "missing",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(
            collector.requests().iter().any(|r| r.contains("/envelope")),
            expect_request,
            "Sentry collector control: {:?}",
            collector.requests()
        );
    }
}

#[test]
fn enabling_does_not_upload_events_collected_before_consent() {
    let dir = TempDir::new().unwrap();
    seed_config(dir.path(), "");
    let collector = Collector::new();
    assert_success(
        &collector
            .command(dir.path())
            .env("DAYONE_TELEMETRY", "0")
            .args(["outbox", "list"])
            .output()
            .unwrap(),
    );
    let db = Connection::open(dir.path().join("profiles/production/dayone.db")).unwrap();
    let granted_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    db.execute("INSERT INTO analytics_events (event_name, params_json, created_at_ms, attempts) VALUES ('dayone_cli_command_run', ?1, ?2, 0)",
        rusqlite::params![r#"{"_en":"dayone_cli_command_run","is_signed_in":false,"command":"sync"}"#, granted_at - 1000]).unwrap();
    // Seed a current grant so this test also characterizes the old send queue,
    // independently of whether the new consent commands exist yet.
    seed_config(
        dir.path(),
        &format!("consent = 'granted'\nconsent_version = 1\nconsent_recorded_at_ms = {granted_at}"),
    );
    assert_success(
        &collector
            .command(dir.path())
            .args(["outbox", "list"])
            .output()
            .unwrap(),
    );
    let requests = collector.requests();
    assert!(requests.iter().any(|r| r.starts_with("POST /tracks ")));
    assert!(
        !requests.iter().any(|r| r.contains("\"command\":\"sync\"")),
        "pre-consent event was uploaded"
    );
}

#[cfg(unix)]
#[test]
fn interactive_choices_are_disclosed_and_persisted_before_collection() {
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::process::Stdio;
    for (answer, decision) in [("yes\n", "granted"), ("no\n", "denied"), ("\n", "denied")] {
        let dir = TempDir::new().unwrap();
        seed_config(dir.path(), "notice_shown = true");
        let collector = Collector::new();
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: openpty initializes both descriptors on success; each is
        // transferred to exactly one File owner below.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let mut master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        let mut child = collector
            .command(dir.path())
            .args(["outbox", "list"])
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave))
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut prompt = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !prompt.contains("[y/N]") && std::time::Instant::now() < deadline {
            let mut poll = libc::pollfd {
                fd: master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 100) } > 0 {
                let mut buffer = [0; 4096];
                match master.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => prompt.push_str(&String::from_utf8_lossy(&buffer[..n])),
                }
            }
            if child.try_wait().unwrap().is_some() {
                break;
            }
        }
        if !prompt.contains("[y/N]") {
            let _ = child.kill();
            let _ = child.wait();
            panic!("no consent prompt: {prompt}");
        }
        assert!(prompt.contains("installation ID"));
        assert!(prompt.contains("server-provided error text"));
        assert!(!prompt.contains("never include journal"));
        assert_no_collection(dir.path(), &collector);
        master.write_all(answer.as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        assert_success(&output);
        assert_eq!(
            config(dir.path())["analytics"]["consent"].as_str(),
            Some(decision)
        );
        if decision == "denied" {
            assert_no_collection(dir.path(), &collector);
        } else {
            assert!(
                collector
                    .requests()
                    .iter()
                    .any(|r| r.starts_with("POST /tracks "))
            );
        }
    }
}

#[test]
fn withdrawal_stops_collection_in_an_already_running_command() {
    use std::process::Stdio;
    let dir = TempDir::new().unwrap();
    seed_config(
        dir.path(),
        "consent = 'granted'\nconsent_version = 1\nconsent_recorded_at_ms = 1",
    );
    let collector = Collector::new();
    let mut child = collector
        .command(dir.path())
        .args(["entry", "write", "--journal-id", "test", "--body-stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if config(dir.path())["analytics"]
            .get("anonymous_id")
            .is_some()
        {
            break;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("command did not initialize before waiting for body input");
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_success(
        &collector
            .command(dir.path())
            .args(["telemetry", "disable"])
            .output()
            .unwrap(),
    );
    // Empty content raises a command error after initialization. Both the
    // Tracks lifecycle event and the Sentry error must respect withdrawal.
    child.stdin.take().unwrap().write_all(b"\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("entry body cannot be empty"));
    assert_no_collection(dir.path(), &collector);
}

#[test]
fn inactive_collectors_do_not_show_a_consent_notice_or_create_an_id() {
    for reason in ["staging", "account-opt-out"] {
        let dir = TempDir::new().unwrap();
        seed_config(dir.path(), "");
        let collector = Collector::new();
        assert_success(
            &collector
                .command(dir.path())
                .env("DAYONE_TELEMETRY", "0")
                .args(["outbox", "list"])
                .output()
                .unwrap(),
        );
        let mut command = collector.command(dir.path());
        // An invalid runtime DSN prevents any compile-time DSN from being
        // selected, and cannot create a Sentry collector itself.
        command.env("DAYONE_SENTRY_DSN", "not-a-dsn");
        if reason == "staging" {
            command
                .env("DAYONE_API_HOST", "https://stg.dayone.me")
                .env("DAYONE_TRACKS_ENDPOINT", "");
        } else {
            let db = Connection::open(dir.path().join("profiles/production/dayone.db")).unwrap();
            db.execute(
                "INSERT INTO user_settings (id, data_json) VALUES (1, ?1)",
                [r#"{"track_usage_statistics":false}"#],
            )
            .unwrap();
        }
        let output = command.args(["outbox", "list"]).output().unwrap();
        assert_success(&output);
        assert!(
            output.stderr.is_empty(),
            "{reason}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_no_collection(dir.path(), &collector);
    }
}

#[test]
fn help_profile_and_doctor_do_not_collect_even_with_consent() {
    let dir = TempDir::new().unwrap();
    seed_config(
        dir.path(),
        "consent = 'granted'\nconsent_version = 1\nconsent_recorded_at_ms = 1",
    );
    let collector = Collector::new();
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["profile", "list"],
        vec!["doctor"],
    ] {
        let output = collector.command(dir.path()).args(args).output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert_no_collection(dir.path(), &collector);
    }
    assert!(!dir.path().join("profiles").exists());
}

#[test]
fn consent_reset_excludes_future_dated_activity_in_other_profiles() {
    let dir = TempDir::new().unwrap();
    seed_config(dir.path(), "");
    // Both profiles share the consent configuration, but have separate queues.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join("config.toml"))
        .unwrap();
    writeln!(file, "[profiles.secondary]\nbase_url = 'https://dayone.me'").unwrap();
    drop(file);
    let collector = Collector::new();
    collector.run(dir.path(), &["telemetry", "enable"]);
    collector.status.store(503, Ordering::Relaxed);
    let future = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        + 60_000;
    for profile in ["production", "secondary"] {
        // Real CLI events remain queued after the collector rejects delivery.
        collector.run(dir.path(), &["--profile", profile, "outbox", "list"]);
        let db =
            Connection::open(dir.path().join(format!("profiles/{profile}/dayone.db"))).unwrap();
        // Model a clock correction before withdrawal without changing the
        // system clock, event properties, or the original identity.
        assert_eq!(
            db.execute("UPDATE analytics_events SET created_at_ms = ?1", [future])
                .unwrap(),
            1
        );
    }
    let old_id = config(dir.path())["analytics"]["anonymous_id"]
        .as_str()
        .unwrap()
        .to_owned();
    collector.requests.lock().unwrap().clear();
    collector.status.store(200, Ordering::Relaxed);
    collector.run(dir.path(), &["telemetry", "disable"]);
    collector.run(dir.path(), &["outbox", "list"]);
    assert!(
        collector.requests().is_empty(),
        "withdrawn consent must stop reporting"
    );
    collector.run(dir.path(), &["telemetry", "enable"]);
    assert!(
        config(dir.path())["analytics"]["consent_recorded_at_ms"]
            .as_integer()
            .unwrap()
            < future,
        "the fixture must leave old events dated after the new consent"
    );
    for profile in ["secondary", "production"] {
        collector.run(dir.path(), &["--profile", profile, "list", "journals"]);
    }
    let batches = collector.tracks_batches();
    let commands: Vec<_> = batches
        .iter()
        .flat_map(|batch| batch["events"].as_array().unwrap())
        .map(|event| event["command"].as_str().unwrap())
        .collect();
    // Positive controls must arrive from both profiles; old outbox events
    // must neither retain their old identity nor be reassigned to the new one.
    assert_eq!(commands, ["list", "list"]);
    assert!(
        batches
            .iter()
            .all(|batch| batch["commonProps"]["_ui"] != old_id)
    );
}

#[test]
fn clock_correction_preserves_retries_under_unchanged_consent() {
    let dir = TempDir::new().unwrap();
    seed_config(dir.path(), "");
    let collector = Collector::new();
    collector.run(dir.path(), &["telemetry", "enable"]);
    let granted_at = config(dir.path())["analytics"]["consent_recorded_at_ms"]
        .as_integer()
        .unwrap();
    collector.status.store(503, Ordering::Relaxed);
    collector.run(dir.path(), &["outbox", "list"]);
    collector.run(dir.path(), &["outbox", "list"]);
    let old_id = config(dir.path())["analytics"]["anonymous_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let db = Connection::open(dir.path().join("profiles/production/dayone.db")).unwrap();
    // Both events were actually recorded after consent. Model a clock that
    // places one at the consent timestamp and the other just before it.
    assert_eq!(
        db.execute(
            "UPDATE analytics_events SET created_at_ms = ?1",
            [granted_at]
        )
        .unwrap(),
        2
    );
    db.execute("UPDATE analytics_events SET created_at_ms = ?1 WHERE id = (SELECT MIN(id) FROM analytics_events)", [granted_at - 1_000]).unwrap();
    collector.requests.lock().unwrap().clear();
    assert_success(
        &collector
            .command(dir.path())
            .env("DAYONE_TELEMETRY", "0")
            .args(["outbox", "list"])
            .output()
            .unwrap(),
    );
    assert!(
        collector.requests().is_empty(),
        "a temporary override must stop delivery"
    );
    collector.status.store(200, Ordering::Relaxed);
    collector.run(dir.path(), &["list", "journals"]);
    let batches = collector.tracks_batches();
    let commands: Vec<_> = batches
        .iter()
        .flat_map(|batch| batch["events"].as_array().unwrap())
        .map(|event| event["command"].as_str().unwrap())
        .collect();
    assert_eq!(commands, ["outbox", "outbox", "list"]);
    assert!(
        batches
            .iter()
            .all(|batch| batch["commonProps"]["_ui"] == old_id)
    );
    // A subsequent flush must not duplicate successfully delivered retries.
    collector.requests.lock().unwrap().clear();
    collector.run(dir.path(), &["list", "journals"]);
    let batches = collector.tracks_batches();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0]["events"].as_array().unwrap().len(), 1);
    assert_eq!(batches[0]["events"][0]["command"], "list");
}
