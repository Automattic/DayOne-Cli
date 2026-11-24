use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn command(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dayone"));
    command
        .env("DAYONE_CONFIG_DIR", dir)
        .env("HOME", dir)
        .env("USERPROFILE", dir)
        .env("DO_NOT_TRACK", "1")
        .env("DAYONE_TELEMETRY", "0")
        .env_remove("DAYONE_INSTALL_METHOD")
        .env_remove("DAYONE_NO_UPDATE_NOTIFIER")
        .env_remove("NO_UPDATE_NOTIFIER")
        .env_remove("CI")
        .env_remove("npm_config_update_notifier");
    command
}

fn output_json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout must contain one JSON value")
}

fn cache_path(dir: &Path, installation: &str) -> std::path::PathBuf {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        os => os,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        arch => arch,
    };
    let variant = if cfg!(target_os = "linux") && !cfg!(feature = "embeddings") {
        "-no-embeddings"
    } else {
        ""
    };
    let extension = if cfg!(windows) { ".exe" } else { "" };
    dir.join(format!(
        "updates-{installation}-stable-dayone-{os}-{arch}{variant}{extension}.json"
    ))
}

fn cache(dir: &Path, installation: &str, checked_at: u64) {
    let path = cache_path(dir, installation);
    fs::write(&path, json!({
            "checked_at": checked_at, "latest_version": "99.10.0", "notes_complete": true, "notes_error": null,
            "releases": [
                {"version":"99.10.0", "url":"https://github.com/Automattic/DayOne-Cli/releases/tag/v99.10.0", "notes":"Adds a new export format."},
                {"version":"99.9.0", "url":"https://github.com/Automattic/DayOne-Cli/releases/tag/v99.9.0", "notes":"Adds an import command."}
            ]
    }).to_string()).unwrap();
    fs::write(
        path.with_extension("notice.json"),
        json!({
            "last_attempt": checked_at,
            "snapshot": {"checked_at": checked_at, "latest_version": "99.10.0"}
        })
        .to_string(),
    )
    .unwrap();
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn updates_are_account_free_and_offline_cache_misses_are_unknown() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("config.toml"), "invalid = [").unwrap();
    let output = command(dir.path())
        .args(["updates", "--offline"])
        .output()
        .unwrap();
    let report = output_json(&output);
    assert_eq!(report["status"], "unknown");
    assert_eq!(report["ok"], false);
    assert_eq!(report["update_available"], Value::Null);
    assert_eq!(report["installation"], "standalone");
    assert!(output.stderr.is_empty());
    assert_eq!(
        fs::read_to_string(dir.path().join("config.toml")).unwrap(),
        "invalid = ["
    );
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);

    cache(dir.path(), "standalone", now());
    let output = command(dir.path())
        .args(["updates", "--offline", "--since", "99.9.0"])
        .output()
        .unwrap();
    let report = output_json(&output);
    assert_eq!(report["status"], "update_available");
    assert_eq!(report["releases"].as_array().unwrap().len(), 1);
    assert_eq!(report["releases"][0]["notes"], "Adds a new export format.");
    assert_eq!(report["since"], "99.9.0");
    assert!(output.stderr.is_empty());
}

#[test]
fn non_tty_notice_preserves_json_and_opt_out_without_disabling_explicit_discovery() {
    let dir = tempfile::tempdir().unwrap();
    cache(dir.path(), "npm", now());
    let output = command(dir.path())
        .env("DAYONE_INSTALL_METHOD", "npm")
        .args(["profile", "list"])
        .output()
        .unwrap();
    let profiles = output_json(&output);
    assert_eq!(profiles["ok"], true);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Update available:"));
    assert!(stderr.contains("dayone updates"));
    assert!(stderr.contains("https://github.com/Automattic/DayOne-Cli/releases/tag/v99.10.0"));
    assert!(stderr.contains("npm install -g @dayone/cli@99.10.0"));

    for (key, value) in [
        ("NO_UPDATE_NOTIFIER", "1"),
        ("DAYONE_NO_UPDATE_NOTIFIER", "1"),
        ("npm_config_update_notifier", "false"),
        ("CI", "true"),
    ] {
        let output = command(dir.path())
            .env("DAYONE_INSTALL_METHOD", "npm")
            .env(key, value)
            .args(["profile", "list"])
            .output()
            .unwrap();
        assert_eq!(output_json(&output), profiles);
        assert!(output.stderr.is_empty(), "opt-out {key}");
    }
    let output = command(dir.path())
        .env("DAYONE_INSTALL_METHOD", "npm")
        .env("NO_UPDATE_NOTIFIER", "1")
        .args(["updates", "--offline"])
        .output()
        .unwrap();
    let report = output_json(&output);
    assert_eq!(report["update_available"], true);
    assert_eq!(
        report["update_instructions"],
        "npm install -g @dayone/cli@99.10.0"
    );

    // Ordinary commands do not need to read release-note history.
    fs::remove_file(cache_path(dir.path(), "npm")).unwrap();
    let output = command(dir.path())
        .env("DAYONE_INSTALL_METHOD", "npm")
        .args(["profile", "list"])
        .output()
        .unwrap();
    assert_eq!(output_json(&output), profiles);
    assert_eq!(String::from_utf8(output.stderr).unwrap(), stderr);
}

#[test]
fn stale_or_corrupt_cache_is_not_a_current_update_assessment() {
    let dir = tempfile::tempdir().unwrap();
    cache(dir.path(), "standalone", now() - 86_400);
    let report = output_json(
        &command(dir.path())
            .args(["updates", "--offline"])
            .output()
            .unwrap(),
    );
    assert_eq!(report["status"], "unknown");
    assert_eq!(report["stale"], true);
    assert_eq!(report["latest_version"], "99.10.0");
    let profile = command(dir.path())
        .args(["profile", "list"])
        .output()
        .unwrap();
    assert_eq!(output_json(&profile)["ok"], true);
    assert!(profile.stderr.is_empty());

    fs::write(cache_path(dir.path(), "standalone"), "broken").unwrap();
    let report = output_json(
        &command(dir.path())
            .args(["updates", "--offline"])
            .output()
            .unwrap(),
    );
    assert_eq!(report["status"], "unknown");
    assert_eq!(report["latest_version"], Value::Null);
}

#[test]
fn updates_are_discoverable_and_help_does_not_initialize_storage() {
    let dir = tempfile::tempdir().unwrap();
    let output = command(dir.path()).arg("--help").output().unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("updates")
    );
    let output = command(dir.path())
        .args(["updates", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--offline"));
    assert!(help.contains("--since"));
    assert!(help.contains("--channel"));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}
