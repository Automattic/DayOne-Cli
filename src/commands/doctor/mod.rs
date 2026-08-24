//! `dayone doctor` — emit a non-destructive, privacy-safe local diagnostic report.

mod journal_key_encoding;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use anyhow::Result;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;

use crate::config::AppConfig;
use crate::diagnostics::{Environment, SafeError};
use crate::store::sqlite::MIGRATIONS;

pub struct DoctorArgs {
    pub config_dir: Option<PathBuf>,
    pub requested_profile: Option<String>,
    pub api_host_override: Option<String>,
    pub include_private_identifiers: bool,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    invocation_id: String,
    generated_at: String,
    status: &'static str,
    check_counts: CheckCounts,
    privacy_mode: &'static str,
    collection_complete: bool,
    environment: Environment,
    checks: Vec<CheckOutcome>,
}

#[derive(Debug, Default, Serialize)]
struct CheckCounts {
    pass: usize,
    finding: usize,
    fail: usize,
    not_run: usize,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum CheckStatus {
    Pass,
    Finding,
    Fail,
    NotRun,
}

#[derive(Debug, Serialize)]
pub(super) struct CheckOutcome {
    check: &'static str,
    status: CheckStatus,
    summary: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    findings: Vec<Value>,
}

impl CheckOutcome {
    fn pass(check: &'static str, summary: &'static str, details: impl Serialize) -> Self {
        Self::new(check, CheckStatus::Pass, summary, Some(details))
    }

    fn finding(check: &'static str, summary: &'static str, details: impl Serialize) -> Self {
        Self::new(check, CheckStatus::Finding, summary, Some(details))
    }

    fn fail(check: &'static str, summary: &'static str, details: impl Serialize) -> Self {
        Self::new(check, CheckStatus::Fail, summary, Some(details))
    }

    fn not_run(check: &'static str, summary: &'static str, reason: &'static str) -> Self {
        Self::new(
            check,
            CheckStatus::NotRun,
            summary,
            Some(serde_json::json!({ "reason": reason })),
        )
    }

    fn with_finding(
        mut self,
        code: &'static str,
        impact: &'static str,
        action: &'static str,
    ) -> Self {
        self.findings.push(serde_json::json!({
            "code": code,
            "impact": impact,
            "action": action,
        }));
        self
    }

    fn new(
        check: &'static str,
        status: CheckStatus,
        summary: &'static str,
        details: Option<impl Serialize>,
    ) -> Self {
        Self {
            check,
            status,
            summary,
            details: details.and_then(|value| serde_json::to_value(value).ok()),
            findings: Vec::new(),
        }
    }
}

pub fn execute(args: DoctorArgs) -> Result<()> {
    if args.include_private_identifiers {
        eprintln!(
            "Warning: doctor report includes private journal and entry identifiers; review it before sharing"
        );
    }
    let report = collect(&args);
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn collect(args: &DoctorArgs) -> DoctorReport {
    let mut checks = Vec::new();
    let config = inspect_config(args);
    checks.push(config.outcome);

    let filesystem = inspect_filesystem(config.config_dir.as_deref(), config.db_path.as_deref());
    checks.push(filesystem.outcome);

    let mut connection = None;
    if let Some(db_path) = config.db_path.as_deref() {
        let sqlite = inspect_sqlite(db_path);
        connection = sqlite.connection;
        checks.push(sqlite.outcome);
    } else {
        checks.push(CheckOutcome::not_run(
            "sqlite",
            "SQLite could not be inspected because no profile database path was available",
            "database_path_unavailable",
        ));
    }

    if let Some(conn) = connection.as_ref() {
        checks.push(inspect_scale(conn));
        checks.push(inspect_session_and_encryption(
            conn,
            config.profile_name.as_deref(),
        ));
        checks.push(inspect_sync(conn));
        checks.push(inspect_outbox(conn));
        checks.push(journal_key_encoding::run(
            conn,
            args.include_private_identifiers,
        ));
    } else {
        for (check, summary) in [
            ("local_scale", "Local data scale could not be inspected"),
            (
                "session_encryption",
                "Session and encryption state could not be inspected",
            ),
            ("sync", "Sync state could not be inspected"),
            ("outbox", "Outbox state could not be inspected"),
            (
                "journal_key_encoding",
                "Journal key encoding could not be inspected",
            ),
        ] {
            checks.push(CheckOutcome::not_run(check, summary, "sqlite_unavailable"));
        }
    }

    let mut check_counts = CheckCounts::default();
    for check in &checks {
        match check.status {
            CheckStatus::Pass => check_counts.pass += 1,
            CheckStatus::Finding => check_counts.finding += 1,
            CheckStatus::Fail => check_counts.fail += 1,
            CheckStatus::NotRun => check_counts.not_run += 1,
        }
    }
    let collection_complete = check_counts.not_run == 0;
    let status = if check_counts.fail > 0 {
        "failed"
    } else if check_counts.finding > 0 || check_counts.not_run > 0 {
        "attention"
    } else {
        "ok"
    };
    DoctorReport {
        invocation_id: crate::diagnostics::invocation_id()
            .unwrap_or_else(|| "unavailable".to_owned()),
        generated_at: crate::diagnostics::utc_timestamp(),
        status,
        check_counts,
        privacy_mode: if args.include_private_identifiers {
            "private_identifiers"
        } else {
            "shareable"
        },
        collection_complete,
        environment: crate::diagnostics::environment(),
        checks,
    }
}

struct ConfigInspection {
    outcome: CheckOutcome,
    config_dir: Option<PathBuf>,
    profile_name: Option<String>,
    db_path: Option<PathBuf>,
}

#[derive(Serialize)]
struct ConfigDetails {
    present: bool,
    source: &'static str,
    file_parseable: Option<bool>,
    effective_config_version: Option<u32>,
    effective_profile_count: Option<usize>,
    selected_profile_resolved: bool,
    profile_origin_valid: bool,
    endpoint: &'static str,
}

fn inspect_config(args: &DoctorArgs) -> ConfigInspection {
    let Some(config_dir) = args.config_dir.clone() else {
        return ConfigInspection {
            outcome: CheckOutcome::not_run(
                "config",
                "Config location could not be resolved",
                "config_directory_unavailable",
            ),
            config_dir: None,
            profile_name: None,
            db_path: None,
        };
    };
    let config_path = config_dir.join("config.toml");
    let present = config_path.is_file();
    let config = match AppConfig::load(&config_dir) {
        Ok(config) => config,
        Err(error) => {
            let (summary, code, impact, action, file_parseable) = match error {
                crate::config::ConfigError::Parse(_, _) => (
                    "Config file is invalid and could not be parsed",
                    "config_invalid",
                    "No effective profile configuration could be loaded",
                    "Repair or replace config.toml",
                    Some(false),
                ),
                _ => (
                    "Config file could not be read",
                    "config_unreadable",
                    "No effective profile configuration could be loaded",
                    "Check the config path and file permissions",
                    None,
                ),
            };
            return ConfigInspection {
                outcome: CheckOutcome::fail(
                    "config",
                    summary,
                    ConfigDetails {
                        present,
                        source: "unavailable",
                        file_parseable,
                        effective_config_version: None,
                        effective_profile_count: None,
                        selected_profile_resolved: false,
                        profile_origin_valid: false,
                        endpoint: "unknown",
                    },
                )
                .with_finding(code, impact, action),
                config_dir: Some(config_dir),
                profile_name: None,
                db_path: None,
            };
        }
    };

    let profile_name = args
        .requested_profile
        .clone()
        .unwrap_or_else(|| config.active_profile.clone());
    let profile = config.get_profile(&profile_name).ok();
    let profile_url =
        profile.and_then(|profile| crate::http::normalize_base_url(&profile.base_url).ok());
    let override_url = args
        .api_host_override
        .as_deref()
        .and_then(|value| crate::http::normalize_base_url(value).ok());
    let endpoint = override_url
        .as_deref()
        .or(profile_url.as_deref())
        .map(crate::diagnostics::endpoint_class)
        .unwrap_or("unknown");
    let db_path = profile.and_then(|_| AppConfig::profile_db_path(&config_dir, &profile_name).ok());
    let details = ConfigDetails {
        present,
        source: if present {
            "config_file"
        } else {
            "built_in_defaults"
        },
        file_parseable: present.then_some(true),
        effective_config_version: Some(config.version),
        effective_profile_count: Some(config.profiles.len()),
        selected_profile_resolved: profile.is_some(),
        profile_origin_valid: profile_url.is_some(),
        endpoint,
    };
    let invalid_override = args.api_host_override.is_some() && override_url.is_none();
    let has_failure = profile.is_none() || profile_url.is_none() || invalid_override;
    let outcome = if !present || has_failure {
        let summary = if profile.is_none() {
            "Selected profile is missing from config"
        } else if profile_url.is_none() {
            "Selected profile API host is invalid"
        } else if invalid_override {
            "API host override is invalid"
        } else {
            "Config file is missing; built-in defaults are active"
        };
        let mut outcome = if has_failure {
            CheckOutcome::fail("config", summary, details)
        } else {
            CheckOutcome::finding("config", summary, details)
        };
        if !present {
            outcome = outcome.with_finding(
                "config_missing",
                "Built-in profiles are active; saved profile selection is unavailable",
                "Run `dayone setup` if this was not expected",
            );
        }
        if profile.is_none() {
            outcome = outcome.with_finding(
                "selected_profile_missing",
                "The requested or active profile could not be resolved",
                "Choose an existing profile or repair config.toml",
            );
        } else if profile_url.is_none() {
            outcome = outcome.with_finding(
                "profile_origin_invalid",
                "The selected profile cannot provide a usable API endpoint",
                "Repair or recreate the selected profile configuration",
            );
        }
        if invalid_override {
            outcome = outcome.with_finding(
                "api_host_invalid",
                "The runtime API host override cannot be used",
                "Pass a valid HTTP(S) origin to --api-host",
            );
        }
        outcome
    } else {
        CheckOutcome::pass(
            "config",
            "Config file is readable and selected profile resolves",
            details,
        )
    };
    ConfigInspection {
        outcome,
        config_dir: Some(config_dir),
        profile_name: profile.map(|_| profile_name),
        db_path,
    }
}

struct FilesystemInspection {
    outcome: CheckOutcome,
}

#[derive(Serialize)]
struct FilesystemDetails {
    config_directory_exists: bool,
    profile_directory_exists: Option<bool>,
    temporary_write_probe: &'static str,
    temporary_wal_probe: &'static str,
    probe_cleanup: &'static str,
    database_exists: Option<bool>,
    database_size_bytes: Option<u64>,
    database_write_openable: Option<bool>,
    #[cfg(unix)]
    config_directory_permissions: Option<String>,
    #[cfg(unix)]
    config_file_permissions: Option<String>,
    #[cfg(unix)]
    profile_directory_permissions: Option<String>,
    #[cfg(unix)]
    database_permissions: Option<String>,
    #[cfg(unix)]
    database_wal_permissions: Option<String>,
    #[cfg(unix)]
    database_shm_permissions: Option<String>,
    #[cfg(unix)]
    paths_owned_by_current_user: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<SafeError>,
}

fn inspect_filesystem(config_dir: Option<&Path>, db_path: Option<&Path>) -> FilesystemInspection {
    let config_path_exists = config_dir.is_some_and(Path::exists);
    let config_exists = config_dir.is_some_and(Path::is_dir);
    let profile_dir = db_path.and_then(Path::parent);
    let profile_exists = profile_dir.map(Path::is_dir);
    let database_exists = db_path.map(Path::is_file);
    let database_size_bytes = db_path
        .and_then(|path| fs::metadata(path).ok())
        .map(|metadata| metadata.len());
    let database_write_openable = db_path
        .filter(|path| path.is_file())
        .map(|path| OpenOptions::new().read(true).write(true).open(path).is_ok());
    #[cfg(unix)]
    let config_file = config_dir.map(|path| path.join("config.toml"));
    #[cfg(unix)]
    let database_wal = db_path.map(|path| sqlite_sidecar_path(path, "-wal"));
    #[cfg(unix)]
    let database_shm = db_path.map(|path| sqlite_sidecar_path(path, "-shm"));
    #[cfg(unix)]
    let current_uid = crate::util::current_uid_string()
        .ok()
        .and_then(|uid| uid.parse::<u32>().ok());
    #[cfg(unix)]
    let inspected_paths = [
        config_dir.filter(|path| path.exists()),
        config_file.as_deref().filter(|path| path.exists()),
        profile_dir.filter(|path| path.exists()),
        db_path.filter(|path| path.exists()),
        database_wal.as_deref().filter(|path| path.exists()),
        database_shm.as_deref().filter(|path| path.exists()),
    ];
    #[cfg(unix)]
    let paths_owned_by_current_user = current_uid.and_then(|uid| {
        let owners = inspected_paths
            .into_iter()
            .flatten()
            .filter_map(|path| {
                fs::symlink_metadata(path)
                    .ok()
                    .map(|metadata| metadata.uid())
            })
            .collect::<Vec<_>>();
        (!owners.is_empty()).then(|| owners.into_iter().all(|owner| owner == uid))
    });

    let mut write_probe = "not_run";
    let mut wal_probe = "not_run";
    let mut cleanup = "not_run";
    let mut safe_error = None;
    if let Some(dir) = profile_dir.filter(|path| path.is_dir()) {
        cleanup = if remove_stale_doctor_probes(dir) {
            "pass"
        } else {
            "fail"
        };
        let suffix = crate::diagnostics::invocation_id().unwrap_or_else(|| "doctor".to_owned());
        let probe_file = dir.join(format!(".doctor-write-{suffix}.tmp"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe_file)
        {
            Ok(mut file) => {
                let write_result = file
                    .write_all(b"dayone-doctor-probe")
                    .and_then(|()| file.flush());
                drop(file);
                write_probe = if write_result.is_ok() { "pass" } else { "fail" };
                if let Err(error) = write_result {
                    safe_error = Some(crate::diagnostics::classify_error(&error));
                }
                if fs::remove_file(&probe_file).is_err() {
                    cleanup = "fail";
                }
                if write_probe == "pass" {
                    let wal_path = dir.join(format!(".doctor-wal-{suffix}.db"));
                    let result = wal_probe_at(&wal_path);
                    wal_probe = if result.is_ok() { "pass" } else { "fail" };
                    if let Err(error) = result {
                        safe_error = Some(crate::diagnostics::classify_error(&error));
                    }
                    let cleaned = remove_sqlite_probe(&wal_path);
                    if !cleaned {
                        cleanup = "fail";
                    }
                }
            }
            Err(error) => {
                write_probe = "fail";
                safe_error = Some(crate::diagnostics::classify_error(&error));
            }
        }
    }

    #[cfg(unix)]
    let config_directory_permissions = config_dir.and_then(unix_permissions);
    #[cfg(unix)]
    let config_file_permissions = config_file.as_deref().and_then(unix_permissions);
    #[cfg(unix)]
    let profile_directory_permissions = profile_dir.and_then(unix_permissions);
    #[cfg(unix)]
    let database_permissions = db_path.and_then(unix_permissions);
    #[cfg(unix)]
    let database_wal_permissions = database_wal.as_deref().and_then(unix_permissions);
    #[cfg(unix)]
    let database_shm_permissions = database_shm.as_deref().and_then(unix_permissions);
    #[cfg(unix)]
    let permissions_need_repair = [
        (config_directory_permissions.as_deref(), "0700"),
        (config_file_permissions.as_deref(), "0600"),
        (profile_directory_permissions.as_deref(), "0700"),
        (database_permissions.as_deref(), "0600"),
        (database_wal_permissions.as_deref(), "0600"),
        (database_shm_permissions.as_deref(), "0600"),
    ]
    .into_iter()
    .any(|(actual, expected)| actual.is_some_and(|actual| actual != expected));

    let details = FilesystemDetails {
        config_directory_exists: config_exists,
        profile_directory_exists: profile_exists,
        temporary_write_probe: write_probe,
        temporary_wal_probe: wal_probe,
        probe_cleanup: cleanup,
        database_exists,
        database_size_bytes,
        database_write_openable,
        #[cfg(unix)]
        config_directory_permissions,
        #[cfg(unix)]
        config_file_permissions,
        #[cfg(unix)]
        profile_directory_permissions,
        #[cfg(unix)]
        database_permissions,
        #[cfg(unix)]
        database_wal_permissions,
        #[cfg(unix)]
        database_shm_permissions,
        #[cfg(unix)]
        paths_owned_by_current_user,
        error: safe_error,
    };
    let config_path_invalid = config_path_exists && !config_exists;
    let failure_summary = if config_path_invalid {
        Some("Config path is not a directory; profile storage could not be inspected")
    } else if write_probe == "fail" {
        Some("Profile directory is not writable")
    } else if wal_probe == "fail" {
        Some("SQLite WAL writes failed in the profile directory")
    } else if cleanup == "fail" {
        Some("Diagnostic probe files could not be removed")
    } else if database_write_openable == Some(false) {
        Some("SQLite database cannot be opened for writing")
    } else {
        None
    };
    let outcome = if let Some(summary) = failure_summary {
        CheckOutcome::fail("filesystem", summary, details)
    } else if db_path.is_none() {
        CheckOutcome::new(
            "filesystem",
            CheckStatus::NotRun,
            "Profile storage could not be inspected because no database path was available",
            Some(details),
        )
    } else if !config_exists || profile_exists == Some(false) || database_exists == Some(false) {
        let summary = match (config_exists, profile_exists, database_exists) {
            (false, Some(false), Some(false)) => {
                "Config directory, profile directory, and SQLite database are missing"
            }
            (true, Some(false), Some(false)) => "Profile directory and SQLite database are missing",
            (_, Some(true), Some(false)) => "SQLite database is missing",
            (false, Some(true), Some(true)) => "Config directory is missing",
            _ => "Required profile storage is missing",
        };
        CheckOutcome::finding("filesystem", summary, details)
    } else {
        #[cfg(unix)]
        if permissions_need_repair || paths_owned_by_current_user == Some(false) {
            CheckOutcome::finding(
                "filesystem",
                "Profile storage ownership or permissions need attention",
                details,
            )
        } else {
            CheckOutcome::pass("filesystem", "Filesystem probes passed", details)
        }
        #[cfg(not(unix))]
        CheckOutcome::pass("filesystem", "Filesystem probes passed", details)
    };
    #[cfg(unix)]
    let outcome = {
        let mut outcome = outcome;
        if permissions_need_repair {
            outcome = outcome.with_finding(
                "storage_permissions_insecure",
                "Other local users may be able to inspect CLI configuration or profile data",
                "Set config/profile directories to 0700 and config/database files to 0600",
            );
        }
        if paths_owned_by_current_user == Some(false) {
            outcome = outcome.with_finding(
                "storage_owner_unexpected",
                "The CLI may be unable to repair permissions or write profile data",
                "Inspect ownership with `ls -ld`; if these Day One paths should belong to you, use `sudo chown \"$(id -un)\" <path>`, then rerun `dayone doctor`",
            );
        }
        outcome
    };
    FilesystemInspection { outcome }
}

#[cfg(unix)]
fn unix_permissions(path: &Path) -> Option<String> {
    fs::symlink_metadata(path)
        .ok()
        .map(|metadata| format!("{:04o}", metadata.permissions().mode() & 0o777))
}

#[cfg(unix)]
fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

const STALE_PROBE_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

fn remove_stale_doctor_probes(directory: &Path) -> bool {
    let Ok(entries) = fs::read_dir(directory) else {
        return false;
    };
    let now = std::time::SystemTime::now();
    let mut cleaned = true;
    for entry in entries {
        let Ok(entry) = entry else {
            cleaned = false;
            continue;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_doctor_probe_name(name) {
            continue;
        }
        let old_enough = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= STALE_PROBE_AGE);
        if old_enough && fs::remove_file(entry.path()).is_err() {
            cleaned = false;
        }
    }
    cleaned
}

fn is_doctor_probe_name(name: &str) -> bool {
    let write_id = name
        .strip_prefix(".doctor-write-")
        .and_then(|name| name.strip_suffix(".tmp"));
    let wal_id = [".db", ".db-wal", ".db-shm", ".db-journal"]
        .into_iter()
        .find_map(|suffix| {
            name.strip_prefix(".doctor-wal-")
                .and_then(|name| name.strip_suffix(suffix))
        });
    write_id.or(wal_id).is_some_and(is_invocation_id)
}

fn is_invocation_id(value: &str) -> bool {
    value.len() == 36
        && value.chars().enumerate().all(|(index, character)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                character == '-'
            } else {
                character.is_ascii_hexdigit()
            }
        })
}

fn wal_probe_at(path: &Path) -> rusqlite::Result<()> {
    let connection = Connection::open(path)?;
    let _: String = connection.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    connection.execute_batch("CREATE TABLE probe (id INTEGER); INSERT INTO probe VALUES (1);")?;
    Ok(())
}

fn remove_sqlite_probe(path: &Path) -> bool {
    let sidecar = |suffix: &str| {
        let mut value = path.as_os_str().to_owned();
        value.push(suffix);
        PathBuf::from(value)
    };
    let mut cleaned = true;
    for candidate in [
        path.to_path_buf(),
        sidecar("-wal"),
        sidecar("-shm"),
        sidecar("-journal"),
    ] {
        if let Err(error) = fs::remove_file(candidate)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            cleaned = false;
        }
    }
    cleaned
}

struct SqliteInspection {
    outcome: CheckOutcome,
    connection: Option<Connection>,
}

#[derive(Serialize)]
struct SqliteDetails {
    database_exists: bool,
    read_only_open: bool,
    sqlite_version: &'static str,
    journal_mode: &'static str,
    applied_migration: Option<i64>,
    applied_migration_count: i64,
    expected_migration: i64,
    schema_current: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<SafeError>,
}

fn inspect_sqlite(db_path: &Path) -> SqliteInspection {
    let expected_migration = MIGRATIONS.last().map(|(version, _)| *version).unwrap_or(0);
    if !db_path.is_file() {
        return SqliteInspection {
            outcome: CheckOutcome::finding(
                "sqlite",
                "SQLite database file is missing",
                SqliteDetails {
                    database_exists: false,
                    read_only_open: false,
                    sqlite_version: rusqlite::version(),
                    journal_mode: "unknown",
                    applied_migration: None,
                    applied_migration_count: 0,
                    expected_migration,
                    schema_current: false,
                    error: None,
                },
            ),
            connection: None,
        };
    }

    let started = std::time::Instant::now();
    let connection = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    );
    let Ok(connection) = connection else {
        let error = connection.expect_err("checked error");
        crate::diagnostics::record_sqlite_failure("open", "database", started.elapsed(), &error);
        return SqliteInspection {
            outcome: CheckOutcome::fail(
                "sqlite",
                "SQLite database could not be opened read-only",
                SqliteDetails {
                    database_exists: true,
                    read_only_open: false,
                    sqlite_version: rusqlite::version(),
                    journal_mode: "unknown",
                    applied_migration: None,
                    applied_migration_count: 0,
                    expected_migration,
                    schema_current: false,
                    error: Some(crate::diagnostics::classify_error(&error)),
                },
            ),
            connection: None,
        };
    };
    crate::diagnostics::record_sqlite_success("open", "database", 1, started.elapsed());

    let journal_mode =
        match connection.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0)) {
            Ok(mode) => safe_journal_mode(&mode),
            Err(error) => {
                crate::diagnostics::record_sqlite_failure(
                    "read",
                    "database",
                    started.elapsed(),
                    &error,
                );
                return SqliteInspection {
                    outcome: CheckOutcome::fail(
                        "sqlite",
                        "SQLite metadata could not be read",
                        SqliteDetails {
                            database_exists: true,
                            read_only_open: true,
                            sqlite_version: rusqlite::version(),
                            journal_mode: "unknown",
                            applied_migration: None,
                            applied_migration_count: 0,
                            expected_migration,
                            schema_current: false,
                            error: Some(crate::diagnostics::classify_error(&error)),
                        },
                    ),
                    connection: None,
                };
            }
        };
    let (applied_migration, applied_migration_count) =
        if table_exists(&connection, "schema_migrations") {
            match connection.query_row(
                "SELECT MAX(version), COUNT(*) FROM schema_migrations",
                [],
                |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, i64>(1)?)),
            ) {
                Ok(versions) => versions,
                Err(_) => {
                    return SqliteInspection {
                        outcome: CheckOutcome::not_run(
                            "sqlite",
                            "SQLite migration metadata could not be inspected",
                            "sqlite_query_failed",
                        ),
                        connection: Some(connection),
                    };
                }
            }
        } else {
            (None, 0)
        };
    let schema_current = applied_migration == Some(expected_migration)
        && applied_migration_count == i64::try_from(MIGRATIONS.len()).unwrap_or(i64::MAX);
    let details = SqliteDetails {
        database_exists: true,
        read_only_open: true,
        sqlite_version: rusqlite::version(),
        journal_mode,
        applied_migration,
        applied_migration_count,
        expected_migration,
        schema_current,
        error: None,
    };
    SqliteInspection {
        outcome: if schema_current && journal_mode == "wal" {
            CheckOutcome::pass(
                "sqlite",
                "SQLite opens read-only and schema is current",
                details,
            )
        } else {
            let summary = if !schema_current && journal_mode != "wal" {
                "SQLite schema is incomplete and journal mode is not WAL"
            } else if !schema_current {
                "SQLite schema is incomplete or outdated"
            } else {
                "SQLite journal mode is not WAL"
            };
            CheckOutcome::fail("sqlite", summary, details)
        },
        connection: Some(connection),
    }
}

fn safe_journal_mode(mode: &str) -> &'static str {
    match mode.to_ascii_lowercase().as_str() {
        "wal" => "wal",
        "delete" => "delete",
        "truncate" => "truncate",
        "persist" => "persist",
        "memory" => "memory",
        "off" => "off",
        _ => "unknown",
    }
}

#[derive(Serialize)]
struct ScaleDetails {
    journals: Option<i64>,
    entries: Option<i64>,
    attachments: Option<i64>,
    media_bytes: Option<i64>,
    cursors: Option<i64>,
    outbox: Option<i64>,
}

fn inspect_scale(connection: &Connection) -> CheckOutcome {
    if !table_exists(connection, "journals") || !table_exists(connection, "entries") {
        return CheckOutcome::not_run(
            "local_scale",
            "Local aggregate counts could not be collected",
            "schema_unavailable",
        );
    }
    let details = ScaleDetails {
        journals: table_count(connection, "journals"),
        entries: table_count(connection, "entries"),
        attachments: table_count(connection, "entry_attachments"),
        media_bytes: if table_exists(connection, "entry_attachments") {
            connection
                .query_row(
                    "SELECT COALESCE(SUM(file_size_bytes), 0) FROM entry_attachments",
                    [],
                    |row| row.get(0),
                )
                .ok()
        } else {
            None
        },
        cursors: table_count(connection, "sync_cursors"),
        outbox: table_count(connection, "sync_outbox"),
    };
    if details.journals.is_none()
        || details.entries.is_none()
        || details.attachments.is_none()
        || details.media_bytes.is_none()
        || details.cursors.is_none()
        || details.outbox.is_none()
    {
        CheckOutcome::not_run(
            "local_scale",
            "Local aggregate counts could not be collected",
            "sqlite_query_failed",
        )
    } else {
        CheckOutcome::pass("local_scale", "Local aggregate counts collected", details)
    }
}

#[derive(Serialize)]
struct SessionEncryptionDetails {
    session: &'static str,
    token_age_seconds: Option<i64>,
    key_source: &'static str,
    key_availability: &'static str,
}

fn inspect_session_and_encryption(
    connection: &Connection,
    profile_name: Option<&str>,
) -> CheckOutcome {
    let Some(profile_name) = profile_name else {
        return CheckOutcome::not_run(
            "session_encryption",
            "Session and encryption state could not be inspected",
            "profile_unavailable",
        );
    };
    if !table_exists(connection, "profiles") || !table_exists(connection, "auth_sessions") {
        return CheckOutcome::not_run(
            "session_encryption",
            "Session and encryption tables are unavailable",
            "schema_unavailable",
        );
    }
    let profile_id = match connection
        .query_row(
            "SELECT id FROM profiles WHERE name = ?1 LIMIT 1",
            params![profile_name],
            |row| row.get::<_, i64>(0),
        )
        .optional()
    {
        Ok(value) => value,
        Err(_) => {
            return CheckOutcome::not_run(
                "session_encryption",
                "Session and encryption state could not be inspected",
                "sqlite_query_failed",
            );
        }
    };
    let Some(profile_id) = profile_id else {
        return CheckOutcome::fail(
            "session_encryption",
            "Selected profile is absent from SQLite",
            SessionEncryptionDetails {
                session: "missing",
                token_age_seconds: None,
                key_source: "missing",
                key_availability: "not_configured",
            },
        );
    };
    let session_count = match connection.query_row(
        "SELECT COUNT(*) FROM auth_sessions WHERE profile_id = ?1",
        params![profile_id],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(value) => value,
        Err(_) => {
            return CheckOutcome::not_run(
                "session_encryption",
                "Session and encryption state could not be inspected",
                "sqlite_query_failed",
            );
        }
    };
    let token_age_seconds = if session_count > 0 {
        match connection.query_row(
            "SELECT CAST(strftime('%s', 'now') AS INTEGER) - CAST(strftime('%s', MAX(token_created_at)) AS INTEGER) FROM auth_sessions WHERE profile_id = ?1",
            params![profile_id],
            |row| row.get::<_, Option<i64>>(0),
        ) {
            Ok(value) => value,
            Err(_) => {
                return CheckOutcome::not_run(
                    "session_encryption",
                    "Session and encryption state could not be inspected",
                    "sqlite_query_failed",
                );
            }
        }
    } else {
        None
    };
    if !table_exists(connection, "encryption_keys") {
        return CheckOutcome::not_run(
            "session_encryption",
            "Session and encryption tables are unavailable",
            "schema_unavailable",
        );
    }
    let key_source = match connection
        .query_row(
            "SELECT CASE WHEN master_key = ?2 THEN 'keychain' ELSE 'sqlite' END FROM encryption_keys WHERE profile_id = ?1",
            params![profile_id, crate::store::ENCRYPTION_KEY_KEYCHAIN_SENTINEL],
            |row| row.get::<_, String>(0),
        )
        .optional()
    {
        Ok(value) => value.as_deref().map(safe_key_source).unwrap_or("missing"),
        Err(_) => {
            return CheckOutcome::not_run(
                "session_encryption",
                "Session and encryption state could not be inspected",
                "sqlite_query_failed",
            );
        }
    };
    let session = if session_count == 0 {
        "missing"
    } else if token_age_seconds.is_some() {
        "present"
    } else {
        "invalid"
    };
    let details = SessionEncryptionDetails {
        session,
        token_age_seconds,
        key_source,
        key_availability: if key_source == "missing" {
            "not_configured"
        } else if key_source == "keychain" {
            "configured_not_verified"
        } else {
            "configured"
        },
    };
    if session == "present" && token_age_seconds.is_some_and(|age| age >= 0) {
        CheckOutcome::pass("session_encryption", "Session metadata is present", details)
    } else if session == "missing" {
        CheckOutcome::finding(
            "session_encryption",
            "Authentication session is missing",
            details,
        )
    } else {
        CheckOutcome::fail(
            "session_encryption",
            "Authentication session timestamp is invalid",
            details,
        )
    }
}

fn safe_key_source(value: &str) -> &'static str {
    match value {
        "keychain" => "keychain",
        "sqlite" => "sqlite",
        _ => "missing",
    }
}

#[derive(Serialize)]
struct SyncDetails {
    successful_runs: i64,
    invalid_success_timestamps: i64,
    last_success_age_seconds: Option<i64>,
    lock_state: &'static str,
    lock_age_seconds: Option<i64>,
    cursor_count: Option<i64>,
    resource_state_count: Option<i64>,
}

fn inspect_sync(connection: &Connection) -> CheckOutcome {
    if !table_exists(connection, "sync_runs") {
        return CheckOutcome::not_run(
            "sync",
            "Sync state tables are unavailable",
            "schema_unavailable",
        );
    }
    let (successful_runs, invalid_success_timestamps, last_success_age_seconds) = match connection
        .query_row(
            "WITH successful AS (
             SELECT CASE
                        WHEN finished_at IS NOT NULL
                         AND strftime('%Y-%m-%d %H:%M:%S', finished_at) = finished_at
                        THEN CAST(strftime('%s', finished_at) AS INTEGER)
                    END AS finished_epoch
             FROM sync_runs
             WHERE status = 'success'
         )
         SELECT COUNT(*),
                COALESCE(SUM(finished_epoch IS NULL), 0),
                CAST(strftime('%s', 'now') AS INTEGER) - MAX(finished_epoch)
         FROM successful",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            },
        ) {
        Ok(value) => value,
        Err(_) => {
            return CheckOutcome::not_run(
                "sync",
                "Sync metadata could not be inspected",
                "sqlite_query_failed",
            );
        }
    };
    if !table_exists(connection, "sync_lock") {
        return CheckOutcome::not_run(
            "sync",
            "Sync state tables are unavailable",
            "schema_unavailable",
        );
    }
    let lock_age_seconds = match connection.query_row(
        "SELECT CASE WHEN age_ms < 0 THEN -1 ELSE age_ms / 1000 END FROM (SELECT CAST(strftime('%s', 'now') AS INTEGER) * 1000 - locked_at_epoch_ms AS age_ms FROM sync_lock WHERE id = 1)",
        [],
        |row| row.get::<_, i64>(0),
    ).optional() {
        Ok(value) => value,
        Err(_) => {
            return CheckOutcome::not_run(
                "sync",
                "Sync metadata could not be inspected",
                "sqlite_query_failed",
            );
        }
    };
    let lock_state = match lock_age_seconds {
        None => "unlocked",
        Some(age) if age < 0 => "invalid",
        Some(age) if age > 15 * 60 => "stale",
        Some(_) => "active",
    };
    let details = SyncDetails {
        successful_runs,
        invalid_success_timestamps,
        last_success_age_seconds,
        lock_state,
        lock_age_seconds,
        cursor_count: table_count(connection, "sync_cursors"),
        resource_state_count: table_count(connection, "sync_resource_state"),
    };
    if details.cursor_count.is_none() || details.resource_state_count.is_none() {
        CheckOutcome::not_run(
            "sync",
            "Sync metadata could not be inspected",
            "sqlite_query_failed",
        )
    } else if invalid_success_timestamps > 0 {
        CheckOutcome::fail(
            "sync",
            "One or more successful sync records have invalid timestamps",
            details,
        )
    } else if lock_state == "stale" {
        CheckOutcome::fail("sync", "Sync lock is stale", details)
    } else if lock_state == "invalid" {
        CheckOutcome::fail("sync", "Sync lock timestamp is in the future", details)
    } else if last_success_age_seconds.is_some_and(|age| age < 0) {
        CheckOutcome::fail(
            "sync",
            "Last successful sync timestamp is in the future",
            details,
        )
    } else if last_success_age_seconds.is_none() {
        CheckOutcome::finding("sync", "No successful sync has been recorded", details)
    } else {
        CheckOutcome::pass("sync", "Sync metadata collected", details)
    }
}

#[derive(Serialize)]
struct OutboxDetails {
    total: i64,
    groups: Vec<OutboxGroup>,
}

#[derive(Serialize)]
struct OutboxGroup {
    status: &'static str,
    resource: &'static str,
    operation: &'static str,
    count: i64,
    max_attempt: i64,
}

fn inspect_outbox(connection: &Connection) -> CheckOutcome {
    if !table_exists(connection, "sync_outbox") {
        return CheckOutcome::not_run(
            "outbox",
            "Outbox table is unavailable",
            "schema_unavailable",
        );
    }
    let mut statement = match connection.prepare(
        "SELECT status, resource, operation, COUNT(*), COALESCE(MAX(attempt_count), 0) FROM sync_outbox GROUP BY status, resource, operation ORDER BY status, resource, operation",
    ) {
        Ok(statement) => statement,
        Err(_) => {
            return CheckOutcome::not_run(
                "outbox",
                "Outbox metadata could not be inspected",
                "sqlite_query_failed",
            );
        }
    };
    let rows = match statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
        ))
    }) {
        Ok(rows) => rows,
        Err(_) => {
            return CheckOutcome::not_run(
                "outbox",
                "Outbox metadata could not be inspected",
                "sqlite_query_failed",
            );
        }
    };
    let mut groups = Vec::new();
    for row in rows {
        let Ok((status, resource, operation, count, max_attempt)) = row else {
            return CheckOutcome::not_run(
                "outbox",
                "Outbox metadata could not be inspected",
                "sqlite_query_failed",
            );
        };
        groups.push(OutboxGroup {
            status: safe_outbox_status(&status),
            resource: safe_outbox_resource(&resource),
            operation: safe_outbox_operation(&operation),
            count,
            max_attempt,
        });
    }
    let total = groups.iter().map(|group| group.count).sum();
    let details = OutboxDetails { total, groups };
    let has_unsupported = details.groups.iter().any(|group| {
        group.status == "other" || group.resource == "other" || group.operation == "other"
    });
    let has_failed = details.groups.iter().any(|group| group.status == "failed");
    let has_processing = details
        .groups
        .iter()
        .any(|group| group.status == "processing");
    if has_unsupported {
        CheckOutcome::fail(
            "outbox",
            "Outbox contains unsupported status, resource, or operation values",
            details,
        )
    } else if has_failed {
        CheckOutcome::fail("outbox", "Outbox contains failed operations", details)
    } else if has_processing {
        CheckOutcome::finding(
            "outbox",
            "Outbox contains operations currently being processed",
            details,
        )
    } else {
        CheckOutcome::pass("outbox", "Outbox metadata collected", details)
    }
}

fn safe_outbox_status(value: &str) -> &'static str {
    match value {
        "pending" => "pending",
        "processing" => "processing",
        "failed" => "failed",
        _ => "other",
    }
}

fn safe_outbox_resource(value: &str) -> &'static str {
    match value {
        "entry" => "entry",
        "original_media" => "original_media",
        "context_item" => "context_item",
        "daily_chat_feed" => "daily_chat_feed",
        "daily_chat_settings" => "daily_chat_settings",
        "comment" => "comment",
        "journal" => "journal",
        _ => "other",
    }
}

fn safe_outbox_operation(value: &str) -> &'static str {
    match value {
        "create" => "create",
        "update" => "update",
        "delete" => "delete",
        _ => "other",
    }
}

fn table_exists(connection: &Connection, table: &str) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            params![table],
            |row| row.get::<_, bool>(0),
        )
        .unwrap_or(false)
}

fn table_count(connection: &Connection, table: &'static str) -> Option<i64> {
    if !table_exists(connection, table) {
        return None;
    }
    let sql = format!("SELECT COUNT(*) FROM {table}");
    connection.query_row(&sql, [], |row| row.get(0)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::sqlite::Store;

    fn args(config_dir: &Path) -> DoctorArgs {
        DoctorArgs {
            config_dir: Some(config_dir.to_path_buf()),
            requested_profile: Some("production".to_owned()),
            api_host_override: None,
            include_private_identifiers: false,
        }
    }

    #[test]
    fn missing_config_and_database_produce_partial_report_without_creating_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = temp.path().join("missing");
        let report = collect(&args(&missing));
        assert_eq!(report.status, "attention");
        assert!(report.check_counts.finding > 0);
        assert_eq!(report.check_counts.fail, 0);
        assert!(report.check_counts.not_run > 0);
        assert!(!report.collection_complete);
        assert!(!missing.exists());
        let config = report
            .checks
            .iter()
            .find(|check| check.check == "config")
            .expect("config check");
        assert_eq!(config.status, CheckStatus::Finding);
        assert_eq!(
            config.summary,
            "Config file is missing; built-in defaults are active"
        );
        let details = config.details.as_ref().expect("config details");
        assert_eq!(details["present"], false);
        assert_eq!(details["source"], "built_in_defaults");
        assert!(details["file_parseable"].is_null());
        assert_eq!(details["effective_profile_count"], 2);
        assert!(config.findings.iter().any(|finding| {
            finding["code"] == "config_missing" && finding["action"].is_string()
        }));
        assert!(
            report
                .checks
                .iter()
                .any(|check| { check.check == "sqlite" && check.status == CheckStatus::Finding })
        );
        let filesystem = report
            .checks
            .iter()
            .find(|check| check.check == "filesystem")
            .expect("filesystem check");
        assert_eq!(filesystem.status, CheckStatus::Finding);
        assert_eq!(
            filesystem.summary,
            "Config directory, profile directory, and SQLite database are missing"
        );
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_reports_insecure_existing_permissions_without_private_paths() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let config = AppConfig::default_config();
        config.save(temp.path()).expect("save config");
        let store =
            Store::open_for_profile(temp.path(), "production", crate::constants::PRODUCTION_URL)
                .expect("store");
        drop(store);
        let db_path = AppConfig::profile_db_path(temp.path(), "production").expect("db path");
        let profile_dir = db_path.parent().expect("profile dir");
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(
            temp.path().join("config.toml"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        fs::set_permissions(profile_dir, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&db_path, fs::Permissions::from_mode(0o644)).unwrap();
        let wal_path = sqlite_sidecar_path(&db_path, "-wal");
        fs::write(&wal_path, b"stale WAL").unwrap();
        fs::set_permissions(&wal_path, fs::Permissions::from_mode(0o644)).unwrap();

        let inspection = inspect_filesystem(Some(temp.path()), Some(&db_path));

        assert_eq!(inspection.outcome.status, CheckStatus::Finding);
        assert!(
            inspection
                .outcome
                .findings
                .iter()
                .any(|finding| { finding["code"] == "storage_permissions_insecure" })
        );
        let json = serde_json::to_string(&inspection.outcome).expect("serialize");
        assert!(json.contains("\"config_directory_permissions\":\"0755\""));
        assert!(json.contains("\"database_permissions\":\"0644\""));
        assert!(json.contains("\"database_wal_permissions\":\"0644\""));
        assert!(!json.contains(temp.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn default_report_does_not_include_names_or_identifiers() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut config = AppConfig::default_config();
        config.active_profile = "production".to_owned();
        config.save(temp.path()).expect("save config");
        let db_path = AppConfig::profile_db_path(temp.path(), "production").expect("db path");
        let store =
            Store::open_for_profile(temp.path(), "production", crate::constants::PRODUCTION_URL)
                .expect("store");
        store
            .upsert_json_row(
                "journals",
                "private-journal-id",
                None,
                None,
                r#"{"id":"private-journal-id","name":"Secret Journal","encryption":{"vault":{"keys":[{"public_key":"-----BEGIN RSA PUBLIC KEY-----"}]}}}"#,
            )
            .expect("journal");
        drop(store);
        assert!(db_path.exists());

        let report = collect(&args(temp.path()));
        assert!(report.collection_complete);
        let json = serde_json::to_string(&report).expect("serialize");
        assert!(!json.contains("private-journal-id"));
        assert!(!json.contains("Secret Journal"));
        let profile_dir = db_path.parent().expect("profile dir");
        assert!(
            std::fs::read_dir(profile_dir)
                .expect("profile dir")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().starts_with(".doctor-")),
            "temporary probes must be removed"
        );
    }

    #[test]
    fn private_identifier_mode_still_excludes_names() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = AppConfig::default_config();
        config.save(temp.path()).expect("save config");
        let store =
            Store::open_for_profile(temp.path(), "production", crate::constants::PRODUCTION_URL)
                .expect("store");
        store
            .upsert_json_row(
                "journals",
                "private-journal-id",
                None,
                None,
                r#"{"id":"private-journal-id","name":"Secret Journal","encryption":{"vault":{"keys":[{"public_key":"-----BEGIN RSA PUBLIC KEY-----"}]}}}"#,
            )
            .expect("journal");
        drop(store);

        let mut private_args = args(temp.path());
        private_args.include_private_identifiers = true;
        let json = serde_json::to_string(&collect(&private_args)).expect("serialize");
        assert!(json.contains("private-journal-id"));
        assert!(!json.contains("Secret Journal"));
    }

    #[test]
    fn custom_endpoint_is_classified_without_echoing_host() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = AppConfig::default_config();
        config.save(temp.path()).expect("save config");
        let mut custom_args = args(temp.path());
        custom_args.api_host_override = Some("https://private.example.test".to_owned());
        let json = serde_json::to_string(&collect(&custom_args)).expect("serialize");
        assert!(json.contains("\"endpoint\":\"custom\""));
        assert!(!json.contains("private.example.test"));
    }

    #[test]
    fn malformed_config_distinguishes_file_failure_from_defaults() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("config.toml"), "not valid [toml")
            .expect("write invalid config");

        let inspection = inspect_config(&args(temp.path()));
        assert_eq!(inspection.outcome.status, CheckStatus::Fail);
        assert!(
            inspection
                .outcome
                .findings
                .iter()
                .any(|finding| finding["code"] == "config_invalid")
        );
        let details = inspection.outcome.details.expect("details");
        assert_eq!(details["present"], true);
        assert_eq!(details["source"], "unavailable");
        assert_eq!(details["file_parseable"], false);
        assert!(details["effective_config_version"].is_null());
    }

    #[test]
    fn malformed_profile_origin_is_a_config_failure() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut config = AppConfig::default_config();
        config
            .profiles
            .get_mut("production")
            .expect("production profile")
            .base_url = "not a URL".to_owned();
        config.save(temp.path()).expect("save config");

        let inspection = inspect_config(&args(temp.path()));
        assert_eq!(inspection.outcome.status, CheckStatus::Fail);
        assert_eq!(
            inspection.outcome.summary,
            "Selected profile API host is invalid"
        );
        assert!(
            inspection
                .outcome
                .findings
                .iter()
                .any(|finding| finding["code"] == "profile_origin_invalid")
        );
        let details = inspection.outcome.details.expect("details");
        assert_eq!(details["source"], "config_file");
        assert_eq!(details["file_parseable"], true);
        assert_eq!(details["profile_origin_valid"], false);
        assert_eq!(details["endpoint"], "unknown");
    }

    #[test]
    fn unsupported_outbox_status_is_a_failure() {
        let connection = Connection::open_in_memory().expect("memory database");
        connection
            .execute_batch(
                "CREATE TABLE sync_outbox (
                    status TEXT,
                    resource TEXT,
                    operation TEXT,
                    attempt_count INTEGER
                 );
                 INSERT INTO sync_outbox VALUES ('corrupt', 'entry', 'update', 1);",
            )
            .expect("outbox schema");

        let outcome = inspect_outbox(&connection);
        assert_eq!(outcome.status, CheckStatus::Fail);
        assert_eq!(
            outcome.details.expect("details")["groups"][0]["status"],
            "other"
        );
    }

    #[test]
    fn processing_outbox_work_is_an_ambiguous_finding() {
        let connection = Connection::open_in_memory().expect("memory database");
        connection
            .execute_batch(
                "CREATE TABLE sync_outbox (
                    status TEXT,
                    resource TEXT,
                    operation TEXT,
                    attempt_count INTEGER
                 );
                 INSERT INTO sync_outbox VALUES ('processing', 'entry', 'update', 1);",
            )
            .expect("outbox schema");

        let outcome = inspect_outbox(&connection);
        assert_eq!(outcome.status, CheckStatus::Finding);
        assert_eq!(
            outcome.summary,
            "Outbox contains operations currently being processed"
        );
    }

    #[test]
    fn stale_probe_cleanup_removes_only_owned_old_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let id = "12345678-1234-1234-1234-123456789abc";
        let stale = temp.path().join(format!(".doctor-wal-{id}.db-wal"));
        let fresh = temp.path().join(format!(".doctor-write-{id}.tmp"));
        let unrelated = temp.path().join(".doctor-wal-not-ours.db");
        for path in [&stale, &fresh, &unrelated] {
            fs::write(path, b"probe").expect("probe file");
        }
        let file = OpenOptions::new().write(true).open(&stale).expect("stale");
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() - STALE_PROBE_AGE * 2),
        )
        .expect("set old modification time");

        assert!(remove_stale_doctor_probes(temp.path()));
        assert!(!stale.exists());
        assert!(fresh.exists());
        assert!(unrelated.exists());
    }

    #[test]
    fn sqlite_check_detects_a_gap_in_migration_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("dayone.db");
        let store = Store::open_at(&db_path).expect("store");
        let connection = store.connect().expect("connection");
        connection
            .execute("DELETE FROM schema_migrations WHERE version = 7", [])
            .expect("delete migration marker");
        drop(connection);
        drop(store);

        let inspection = inspect_sqlite(&db_path);
        assert_eq!(inspection.outcome.status, CheckStatus::Fail);
        let details = inspection.outcome.details.expect("details");
        assert_eq!(details["schema_current"], false);
        assert_eq!(
            details["applied_migration_count"],
            i64::try_from(MIGRATIONS.len()).unwrap() - 1
        );
    }

    #[test]
    fn malformed_successful_sync_timestamp_is_a_failure() {
        let connection = Connection::open_in_memory().expect("memory database");
        connection
            .execute_batch(
                "CREATE TABLE sync_runs (finished_at TEXT, status TEXT);
                 CREATE TABLE sync_lock (id INTEGER PRIMARY KEY, locked_at_epoch_ms INTEGER);
                 CREATE TABLE sync_cursors (resource_key TEXT);
                 CREATE TABLE sync_resource_state (resource_key TEXT);
                 INSERT INTO sync_runs VALUES ('not-a-timestamp', 'success');
                 INSERT INTO sync_runs VALUES ('2024-02-30 12:00:00', 'success');
                 INSERT INTO sync_runs VALUES (NULL, 'success');",
            )
            .expect("sync schema");

        let outcome = inspect_sync(&connection);
        assert_eq!(outcome.status, CheckStatus::Fail);
        assert_eq!(
            outcome.summary,
            "One or more successful sync records have invalid timestamps"
        );
        let details = outcome.details.expect("details");
        assert_eq!(details["successful_runs"], 3);
        assert_eq!(details["invalid_success_timestamps"], 3);
        assert!(details["last_success_age_seconds"].is_null());
    }

    #[test]
    fn future_sync_lock_is_reported_as_invalid() {
        let connection = Connection::open_in_memory().expect("memory database");
        connection
            .execute_batch(
                "CREATE TABLE sync_runs (finished_at TEXT, status TEXT);
                 CREATE TABLE sync_lock (id INTEGER PRIMARY KEY, locked_at_epoch_ms INTEGER);
                 CREATE TABLE sync_cursors (resource_key TEXT);
                 CREATE TABLE sync_resource_state (resource_key TEXT);",
            )
            .expect("sync schema");
        let future = time::OffsetDateTime::now_utc().unix_timestamp() * 1000 + 60_000;
        connection
            .execute("INSERT INTO sync_lock VALUES (1, ?1)", params![future])
            .expect("future lock");

        let outcome = inspect_sync(&connection);
        assert_eq!(outcome.status, CheckStatus::Fail);
        assert_eq!(outcome.summary, "Sync lock timestamp is in the future");
        assert_eq!(outcome.details.expect("details")["lock_state"], "invalid");
    }
}
