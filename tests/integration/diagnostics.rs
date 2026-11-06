use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

fn unique_tmp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be valid")
        .as_nanos();
    std::env::temp_dir().join(format!("dayone-cli-diagnostics-{label}-{nanos}"))
}

fn run_dayone(config_dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_dayone"))
        .env("DAYONE_CONFIG_DIR", config_dir)
        .env("DAYONE_TELEMETRY", "0")
        .args(args)
        .output()
        .expect("dayone command should execute")
}

fn trace_records(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .expect("trace should be readable")
        .lines()
        .map(|line| serde_json::from_str(line).expect("each trace line should be JSON"))
        .collect()
}

#[test]
fn traced_command_preserves_stdout_and_writes_single_invocation() {
    let root = unique_tmp_dir("stdout");
    let plain_config = root.join("plain");
    let traced_config = root.join("traced");
    let trace = root.join("trace.jsonl");
    std::fs::create_dir_all(&root).expect("root");

    let plain = run_dayone(&plain_config, &["profile", "list"]);
    let traced = run_dayone(
        &traced_config,
        &[
            "--trace-file",
            trace.to_str().expect("utf8 path"),
            "profile",
            "list",
        ],
    );
    assert!(plain.status.success());
    assert!(traced.status.success());
    assert_eq!(plain.stdout, traced.stdout, "tracing must not alter stdout");

    assert_eq!(plain.stderr, traced.stderr, "tracing must not alter stderr");

    let records = trace_records(&trace);
    assert_eq!(records.first().unwrap()["event"], "invocation_started");
    assert_eq!(records.last().unwrap()["event"], "invocation_finished");
    let invocation = records[0]["invocation_id"].as_str().expect("invocation id");
    assert!(
        records
            .iter()
            .all(|record| record["invocation_id"] == invocation)
    );
    assert!(records.iter().any(|record| record["event"] == "config"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&trace)
            .expect("trace metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn existing_trace_file_is_never_overwritten_and_command_does_not_run() {
    let root = unique_tmp_dir("existing");
    let config = root.join("config");
    let trace = root.join("trace.jsonl");
    std::fs::create_dir_all(&root).expect("root");
    std::fs::write(&trace, "do-not-overwrite").expect("seed trace");

    let output = run_dayone(
        &config,
        &[
            "--trace-file",
            trace.to_str().expect("utf8 path"),
            "profile",
            "list",
        ],
    );
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr").trim(),
        "Diagnostic trace file already exists; choose a new --trace-file path or remove the existing file"
    );
    assert_eq!(
        std::fs::read_to_string(&trace).expect("trace contents"),
        "do-not-overwrite"
    );
    assert!(
        !config.exists(),
        "command must not initialize config after trace failure"
    );
}

#[test]
fn runtime_config_failure_is_typed_without_leaking_its_path() {
    let root = unique_tmp_dir("config-failure-private-path");
    let config = root.join("config-is-a-file");
    let trace = root.join("trace.jsonl");
    std::fs::create_dir_all(&root).expect("root");
    std::fs::write(&config, b"not a directory").expect("config blocker");

    let output = run_dayone(
        &config,
        &[
            "--trace-file",
            trace.to_str().expect("utf8 path"),
            "profile",
            "list",
        ],
    );
    assert!(!output.status.success());
    let raw = std::fs::read_to_string(&trace).expect("trace");
    assert!(!raw.contains("config-failure-private-path"));
    assert!(!raw.contains("config-is-a-file"));
    let records = trace_records(&trace);
    assert!(records.iter().any(|record| {
        record["event"] == "config" && record["data"]["error"]["category"] == "io"
    }));
    assert_eq!(records.last().unwrap()["data"]["outcome"], "error");
}

#[test]
fn profile_resolution_failure_is_not_traced_as_config_success() {
    let root = unique_tmp_dir("profile-failure");
    let trace = root.join("trace.jsonl");
    let init = run_dayone(&root, &["profile", "list"]);
    assert!(init.status.success());

    let output = run_dayone(
        &root,
        &[
            "--trace-file",
            trace.to_str().expect("utf8 path"),
            "--profile",
            "missing-private-profile",
            "outbox",
            "list",
        ],
    );
    assert!(!output.status.success());
    let config_records = trace_records(&trace)
        .into_iter()
        .filter(|record| record["event"] == "config")
        .collect::<Vec<_>>();
    assert_eq!(config_records.len(), 1);
    assert_eq!(config_records[0]["data"]["result"], "error");
    assert!(
        !std::fs::read_to_string(&trace)
            .expect("trace")
            .contains("missing-private-profile")
    );
}

#[test]
fn malformed_profile_origin_is_a_typed_config_failure() {
    let root = unique_tmp_dir("invalid-origin");
    let trace = root.join("trace.jsonl");
    std::fs::create_dir_all(&root).expect("root");
    std::fs::write(
        root.join("config.toml"),
        "version = 1\nactive_profile = \"production\"\n\n[profiles.production]\nbase_url = \"https://\"\n",
    )
    .expect("config");

    let output = run_dayone(
        &root,
        &[
            "--trace-file",
            trace.to_str().expect("utf8 path"),
            "outbox",
            "list",
        ],
    );
    assert!(!output.status.success());
    let records = trace_records(&trace);
    let config = records
        .iter()
        .find(|record| record["event"] == "config")
        .expect("config record");
    assert_eq!(config["data"]["result"], "error");
    assert_eq!(config["data"]["endpoint"], "unknown");
    assert_eq!(config["data"]["error"]["category"], "config");
}

#[test]
fn trace_omits_raw_arguments_paths_and_custom_hosts() {
    let root = unique_tmp_dir("privacy-canary");
    let config = root.join("private-user-path");
    let trace = root.join("trace.jsonl");
    std::fs::create_dir_all(&root).expect("root");

    let output = run_dayone(
        &config,
        &[
            "--trace-file",
            trace.to_str().expect("utf8 path"),
            "profile",
            "set",
            "private-profile-name",
            "--api-host",
            "https://private-host.example.test",
        ],
    );
    assert!(output.status.success());
    let raw = std::fs::read_to_string(&trace).expect("trace");
    for forbidden in [
        "private-profile-name",
        "private-host.example.test",
        "private-user-path",
        trace.to_str().expect("utf8 path"),
    ] {
        assert!(!raw.contains(forbidden), "trace leaked {forbidden}");
    }
    assert!(
        trace_records(&trace).iter().any(|record| {
            record["event"] == "config" && record["data"]["endpoint"] == "custom"
        })
    );
}

#[test]
fn doctor_is_partial_success_and_does_not_create_config_or_database() {
    let root = unique_tmp_dir("doctor-missing");
    let output = run_dayone(&root, &["doctor"]);
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    assert_eq!(report["status"], "attention");
    assert!(report["check_counts"]["finding"].as_u64().unwrap() > 0);
    assert_eq!(report["check_counts"]["fail"], 0);
    assert!(report["check_counts"]["not_run"].as_u64().unwrap() > 0);
    assert_eq!(report["privacy_mode"], "shareable");
    assert_eq!(report["collection_complete"], false);
    let config = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "config")
        .expect("config check");
    assert_eq!(
        config["summary"],
        "Config file is missing; built-in defaults are active"
    );
    assert_eq!(config["details"]["source"], "built_in_defaults");
    assert!(config["details"]["file_parseable"].is_null());
    assert!(
        config["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| {
                finding["code"] == "config_missing" && finding["action"].is_string()
            })
    );
    assert!(
        report["invocation_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert!(
        !root.exists(),
        "doctor must not create a missing config directory"
    );
}

#[test]
fn doctor_reports_unknown_database_state_when_config_blocks_path_resolution() {
    let root = unique_tmp_dir("doctor-config-blocker");
    std::fs::write(&root, b"not a directory").expect("config blocker");

    let output = run_dayone(&root, &["doctor"]);
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    let checks = report["checks"].as_array().expect("checks");
    let filesystem = checks
        .iter()
        .find(|check| check["check"] == "filesystem")
        .expect("filesystem check");
    assert_eq!(filesystem["status"], "fail");
    assert_eq!(
        filesystem["summary"],
        "Config path is not a directory; profile storage could not be inspected"
    );
    assert_eq!(
        filesystem["details"].get("profile_directory_exists"),
        Some(&Value::Null)
    );
    assert_eq!(
        filesystem["details"].get("database_exists"),
        Some(&Value::Null)
    );

    let sqlite = checks
        .iter()
        .find(|check| check["check"] == "sqlite")
        .expect("sqlite check");
    assert_eq!(sqlite["status"], "not_run");
    assert_eq!(
        sqlite["summary"],
        "SQLite could not be inspected because no profile database path was available"
    );
    assert_eq!(sqlite["details"]["reason"], "database_path_unavailable");
}

#[test]
fn doctor_reports_corrupt_sqlite_without_echoing_its_path() {
    let root = unique_tmp_dir("doctor-corrupt-private-path");
    let init = run_dayone(&root, &["profile", "list"]);
    assert!(init.status.success());
    let profile_dir = root.join("profiles").join("production");
    std::fs::create_dir_all(&profile_dir).expect("profile directory");
    std::fs::write(profile_dir.join("dayone.db"), b"not a sqlite database")
        .expect("corrupt database");

    let output = run_dayone(&root, &["--profile", "production", "doctor"]);
    assert!(output.status.success());
    let raw = String::from_utf8(output.stdout).expect("doctor utf8");
    let report: Value = serde_json::from_str(&raw).expect("doctor JSON");
    assert_eq!(report["status"], "failed");
    assert!(report["check_counts"]["fail"].as_u64().unwrap() > 0);
    assert_eq!(report["collection_complete"], false);
    assert!(
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["check"] == "sqlite" && check["status"] == "fail")
    );
    assert!(!raw.contains("doctor-corrupt-private-path"));
    assert!(!raw.contains("dayone.db"));
}
