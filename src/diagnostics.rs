//! Opt-in, privacy-safe diagnostic reports and command traces.
//!
//! This is deliberately not a general logging framework. Callers can emit only
//! typed, allowlisted fields; arbitrary strings, URLs, SQL, paths, and payloads
//! have no trace API.

use std::collections::{BTreeMap, HashMap};
use std::error::Error as StdError;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use rsa::rand_core::{OsRng, RngCore};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const MAX_TRACE_BYTES: usize = 1024 * 1024;
const NORMAL_RESERVE_BYTES: usize = 128 * 1024;
const SUMMARY_RESERVE_BYTES: usize = 64 * 1024;
const FINAL_ERROR_RESERVE_BYTES: usize = 4 * 1024;
const TRUNCATION_MARKER_RESERVE_BYTES: usize = 1024;
const FINISH_RESERVE_BYTES: usize = 2 * 1024;
const NORMAL_TRACE_BYTES: usize = MAX_TRACE_BYTES - NORMAL_RESERVE_BYTES;
const ESSENTIAL_TRACE_BYTES: usize =
    MAX_TRACE_BYTES - SUMMARY_RESERVE_BYTES - FINAL_ERROR_RESERVE_BYTES;
const FINAL_ERROR_TRACE_BYTES: usize = MAX_TRACE_BYTES - SUMMARY_RESERVE_BYTES;
const SUMMARY_TRACE_BYTES: usize =
    MAX_TRACE_BYTES - FINISH_RESERVE_BYTES - TRUNCATION_MARKER_RESERVE_BYTES;
const TRUNCATION_TRACE_BYTES: usize = MAX_TRACE_BYTES - FINISH_RESERVE_BYTES;

#[derive(Debug, Clone, Serialize)]
pub struct Environment {
    pub cli_version: &'static str,
    pub build_sha: &'static str,
    pub build_sha_known: bool,
    pub os: &'static str,
    pub architecture: &'static str,
    pub features: BuildFeatures,
    pub custom_config_directory: bool,
    pub sentry_enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuildFeatures {
    pub embeddings: bool,
    pub telemetry: bool,
    pub production_default: bool,
}

pub fn environment() -> Environment {
    let build_sha = option_env!("DAYONE_BUILD_SHA").unwrap_or("unknown");
    Environment {
        cli_version: env!("CARGO_PKG_VERSION"),
        build_sha,
        build_sha_known: build_sha != "unknown",
        os: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        features: BuildFeatures {
            embeddings: cfg!(feature = "embeddings"),
            telemetry: cfg!(feature = "telemetry"),
            production_default: cfg!(feature = "production-default"),
        },
        custom_config_directory: std::env::var_os("DAYONE_CONFIG_DIR").is_some(),
        sentry_enabled: crate::telemetry::is_sentry_enabled(),
    }
}

pub fn endpoint_class(url: &str) -> &'static str {
    let normalized = url.trim().trim_end_matches('/');
    if normalized == crate::constants::PRODUCTION_URL {
        "production"
    } else if normalized == crate::constants::STAGING_URL {
        "staging"
    } else {
        "custom"
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SafeError {
    pub category: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sqlite_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub io_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    pub expected: bool,
}

impl SafeError {
    fn category(category: &'static str) -> Self {
        Self {
            category,
            sqlite_code: None,
            io_kind: None,
            http_status: None,
            expected: false,
        }
    }
}

pub fn classify_error(error: &(dyn StdError + 'static)) -> SafeError {
    let mut current = Some(error);
    let mut fallback = SafeError::category("internal");
    while let Some(cause) = current {
        if let Some(user_error) = cause.downcast_ref::<crate::telemetry::UserError>() {
            return SafeError {
                category: match user_error.kind() {
                    crate::telemetry::UserErrorKind::AuthRequired => "auth_required",
                    crate::telemetry::UserErrorKind::PasswordReadFailed => "input",
                    crate::telemetry::UserErrorKind::Generic => "user_error",
                },
                expected: true,
                ..SafeError::category("user_error")
            };
        }
        if let Some(sqlite) = cause.downcast_ref::<rusqlite::Error>() {
            let code = match sqlite {
                rusqlite::Error::SqliteFailure(inner, _) => Some(inner.extended_code),
                _ => None,
            };
            return SafeError {
                category: "sqlite",
                sqlite_code: code,
                ..SafeError::category("sqlite")
            };
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return SafeError {
                category: "io",
                io_kind: Some(io_kind(io.kind())),
                ..SafeError::category("io")
            };
        }
        if cause.downcast_ref::<crate::http::BaseUrlError>().is_some() {
            let mut safe = SafeError::category("config");
            safe.expected = true;
            return safe;
        }
        if let Some(api) = cause.downcast_ref::<crate::http::ApiError>() {
            return SafeError {
                category: "http",
                http_status: Some(api.status()),
                ..SafeError::category("http")
            };
        }
        if let Some(reqwest) = cause.downcast_ref::<reqwest::Error>() {
            return SafeError {
                category: "http",
                http_status: reqwest.status().map(|status| status.as_u16()),
                ..SafeError::category("http")
            };
        }
        if cause.downcast_ref::<serde_json::Error>().is_some() {
            fallback = SafeError::category("serialization");
        }
        if let Some(config) = cause.downcast_ref::<crate::config::ConfigError>() {
            fallback = match config {
                crate::config::ConfigError::Io(_, error) => SafeError {
                    category: "io",
                    io_kind: Some(io_kind(error.kind())),
                    ..SafeError::category("io")
                },
                crate::config::ConfigError::Parse(_, _)
                | crate::config::ConfigError::Serialize(_, _) => SafeError::category("config"),
                crate::config::ConfigError::ProfileNotFound(_)
                | crate::config::ConfigError::InvalidProfileName(_) => {
                    let mut safe = SafeError::category("user_error");
                    safe.expected = true;
                    safe
                }
            };
        }
        if let Some(store) = cause.downcast_ref::<crate::store::StoreError>() {
            fallback = match store {
                crate::store::StoreError::KeychainInteractionRequired => {
                    let mut safe = SafeError::category("keychain");
                    safe.expected = true;
                    safe
                }
                crate::store::StoreError::KeychainUnavailable(_)
                | crate::store::StoreError::Keychain(_)
                | crate::store::StoreError::MissingKeychainMasterKey { .. } => {
                    SafeError::category("keychain")
                }
                crate::store::StoreError::InvalidAuthSessionUserId { .. }
                | crate::store::StoreError::AuthSessionAccountMismatch { .. } => {
                    SafeError::category("auth_session")
                }
                crate::store::StoreError::InvalidInput(_)
                | crate::store::StoreError::NotFound { .. } => {
                    let mut safe = SafeError::category("user_error");
                    safe.expected = true;
                    safe
                }
                crate::store::StoreError::Json(_) => SafeError::category("serialization"),
                crate::store::StoreError::IoContext { .. } => SafeError::category("io"),
                crate::store::StoreError::Sqlite(_)
                | crate::store::StoreError::SqliteContext { .. } => SafeError::category("sqlite"),
            };
        }
        current = cause.source();
    }
    fallback
}

fn io_kind(kind: std::io::ErrorKind) -> &'static str {
    use std::io::ErrorKind;
    match kind {
        ErrorKind::NotFound => "not_found",
        ErrorKind::PermissionDenied => "permission_denied",
        ErrorKind::AlreadyExists => "already_exists",
        ErrorKind::InvalidInput | ErrorKind::InvalidData => "invalid_data",
        ErrorKind::TimedOut => "timed_out",
        ErrorKind::WriteZero => "write_zero",
        ErrorKind::StorageFull => "storage_full",
        ErrorKind::ReadOnlyFilesystem => "read_only_filesystem",
        ErrorKind::Interrupted => "interrupted",
        _ => "other",
    }
}

#[derive(Default)]
struct DiagnosticsState {
    invocation_id: Option<String>,
    command: Option<&'static str>,
    subcommand: Option<&'static str>,
    started: Option<Instant>,
    recorder: Option<Recorder>,
    write_failed: bool,
    warned_write_failed: bool,
    aliases: HashMap<(&'static str, String), String>,
    alias_counts: HashMap<&'static str, usize>,
    outbox: BTreeMap<(&'static str, &'static str, &'static str), CountAggregate>,
    sqlite: BTreeMap<(&'static str, &'static str, &'static str), CountAggregate>,
    crypto: BTreeMap<(&'static str, &'static str, &'static str), CountAggregate>,
    media: BTreeMap<(&'static str, &'static str, &'static str), ByteAggregate>,
}

#[derive(Default, Clone, Copy, Serialize)]
struct CountAggregate {
    count: u64,
    max: u64,
}

#[derive(Default, Clone, Copy, Serialize)]
struct ByteAggregate {
    count: u64,
    bytes: u64,
}

static STATE: OnceLock<Mutex<DiagnosticsState>> = OnceLock::new();

fn state() -> &'static Mutex<DiagnosticsState> {
    STATE.get_or_init(|| Mutex::new(DiagnosticsState::default()))
}

fn lock_state() -> std::sync::MutexGuard<'static, DiagnosticsState> {
    state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn existing_state() -> Option<std::sync::MutexGuard<'static, DiagnosticsState>> {
    STATE.get().map(|state| {
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    })
}

pub fn start_doctor(command: &'static str, subcommand: Option<&'static str>) -> Result<String> {
    start(command, subcommand, None)
}

pub fn start_trace(
    path: &Path,
    command: &'static str,
    subcommand: Option<&'static str>,
) -> Result<String> {
    start(command, subcommand, Some(path))
}

fn start(
    command: &'static str,
    subcommand: Option<&'static str>,
    trace_path: Option<&Path>,
) -> Result<String> {
    if existing_state().is_some_and(|state| state.invocation_id.is_some()) {
        return Err(anyhow!("diagnostics already initialized"));
    }
    let invocation_id = new_invocation_id();
    let mut recorder = if let Some(path) = trace_path {
        let file = create_trace_file(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                anyhow!(
                    "Diagnostic trace file already exists; choose a new --trace-file path or remove the existing file"
                )
            } else {
                anyhow!("Failed to create diagnostic trace file: {error}")
            }
        })?;
        Some(Recorder::new(file, invocation_id.clone()))
    } else {
        None
    };

    if let Some(active_recorder) = recorder.as_mut() {
        let initialized = active_recorder
            .write(
                "invocation_started",
                &InvocationStarted {
                    environment: environment(),
                    command,
                    subcommand,
                },
                true,
            )
            .and_then(|()| {
                active_recorder.write(
                    "command_started",
                    &CommandStarted {
                        command,
                        subcommand,
                    },
                    true,
                )
            });
        if let Err(error) = initialized {
            drop(recorder.take());
            if let Some(path) = trace_path {
                let _ = std::fs::remove_file(path);
            }
            return Err(error).context("failed to initialize diagnostic trace");
        }
    }

    let mut current = lock_state();
    if current.invocation_id.is_some() {
        drop(current);
        drop(recorder);
        if let Some(path) = trace_path {
            let _ = std::fs::remove_file(path);
        }
        return Err(anyhow!("diagnostics already initialized"));
    }
    current.invocation_id = Some(invocation_id.clone());
    current.command = Some(command);
    current.subcommand = subcommand;
    current.started = Some(Instant::now());
    current.recorder = recorder;
    drop(current);

    Ok(invocation_id)
}

fn create_trace_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

pub fn invocation_id() -> Option<String> {
    existing_state().and_then(|state| state.invocation_id.clone())
}

pub fn is_tracing() -> bool {
    existing_state().is_some_and(|state| state.recorder.is_some())
}

#[derive(Serialize)]
struct InvocationStarted {
    environment: Environment,
    command: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    subcommand: Option<&'static str>,
}

#[derive(Serialize)]
struct CommandStarted {
    command: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    subcommand: Option<&'static str>,
}

#[derive(Serialize)]
struct AnalyticsRejectionEvent {
    event: &'static str,
    rejected_count: usize,
}

pub fn record_analytics_rejection(event: &'static str, rejected_count: usize) {
    emit(
        "analytics_properties_rejected",
        &AnalyticsRejectionEvent {
            event,
            rejected_count,
        },
        false,
    );
}

#[derive(Serialize)]
struct ConfigEvent {
    result: &'static str,
    endpoint: &'static str,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<SafeError>,
}

pub fn record_config(
    result: &'static str,
    endpoint: &'static str,
    duration: Duration,
    error: Option<&(dyn StdError + 'static)>,
) {
    emit(
        "config",
        &ConfigEvent {
            result,
            endpoint,
            duration_ms: duration_ms(duration),
            error: error.map(classify_error),
        },
        error.is_some(),
    );
}

#[derive(Serialize)]
struct StoreEvent {
    operation: &'static str,
    result: &'static str,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<SafeError>,
}

pub fn record_store(
    operation: &'static str,
    result: &'static str,
    duration: Duration,
    error: Option<&(dyn StdError + 'static)>,
) {
    emit(
        "store",
        &StoreEvent {
            operation,
            result,
            duration_ms: duration_ms(duration),
            error: error.map(classify_error),
        },
        error.is_some(),
    );
}

#[derive(Serialize)]
struct SyncRunEvent<'a> {
    phase: &'static str,
    result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_ref: Option<&'a str>,
    duration_ms: u64,
}

pub fn record_sync_run(
    phase: &'static str,
    result: &'static str,
    run_id: Option<i64>,
    duration: Duration,
) {
    let Some(mut current) = existing_state() else {
        return;
    };
    if current.invocation_id.is_none() {
        return;
    }
    let run_ref = run_id.map(|run_id| alias_locked(&mut current, "sync_run", &run_id.to_string()));
    emit_locked(
        &mut current,
        "sync_run",
        &SyncRunEvent {
            phase,
            result,
            run_ref: run_ref.as_deref(),
            duration_ms: duration_ms(duration),
        },
        result == "error",
    );
}

#[derive(Serialize)]
struct SyncPhaseEvent {
    phase: &'static str,
    result: &'static str,
    duration_ms: u64,
}

pub fn record_sync_phase(phase: &'static str, result: &'static str, duration: Duration) {
    emit(
        "sync_phase",
        &SyncPhaseEvent {
            phase,
            result,
            duration_ms: duration_ms(duration),
        },
        result == "error",
    );
}

#[derive(Serialize)]
struct SyncResourceEvent<'a> {
    resource: &'a str,
    result: &'static str,
    changed_count: i64,
    cursor_advanced: bool,
    duration_ms: u64,
}

pub fn record_sync_resource(
    resource: &str,
    result: &str,
    changed_count: i64,
    cursor_advanced: bool,
    duration_ms_value: u64,
) {
    let safe_resource = safe_sync_resource(resource);
    let safe_result = safe_result(result);
    emit(
        "sync_resource",
        &SyncResourceEvent {
            resource: safe_resource,
            result: safe_result,
            changed_count,
            cursor_advanced,
            duration_ms: duration_ms_value,
        },
        safe_result == "error",
    );
}

pub struct HttpTrace {
    started: Instant,
    method: &'static str,
    route: &'static str,
    request_bytes: Option<u64>,
    status: Option<u16>,
    response_bytes: Option<u64>,
    finished: bool,
}

impl HttpTrace {
    pub fn api(method: &'static str, api_path: &str, request_bytes: Option<u64>) -> Self {
        Self::new(method, route_template(api_path), request_bytes)
    }

    pub fn media_upload(request_bytes: Option<u64>) -> Self {
        Self::new("PUT", "media_upload", request_bytes)
    }

    fn new(method: &'static str, route: &'static str, request_bytes: Option<u64>) -> Self {
        Self {
            started: Instant::now(),
            method,
            route,
            request_bytes,
            status: None,
            response_bytes: None,
            finished: false,
        }
    }

    pub fn response(&mut self, status: u16, response_bytes: Option<u64>) {
        self.status = Some(status);
        self.response_bytes = response_bytes;
    }

    pub fn finish(mut self, response_bytes: Option<u64>) {
        if response_bytes.is_some() {
            self.response_bytes = response_bytes;
        }
        self.finished = true;
        record_http(
            self.method,
            self.route,
            self.status,
            self.started.elapsed(),
            self.request_bytes,
            self.response_bytes,
            "success",
        );
    }
}

impl Drop for HttpTrace {
    fn drop(&mut self) {
        if !self.finished {
            record_http(
                self.method,
                self.route,
                self.status,
                self.started.elapsed(),
                self.request_bytes,
                self.response_bytes,
                "error",
            );
        }
    }
}

#[derive(Serialize)]
struct HttpEvent {
    method: &'static str,
    route: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    duration_ms: u64,
    retry_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_bytes: Option<u64>,
    result: &'static str,
}

fn record_http(
    method: &'static str,
    route: &'static str,
    status: Option<u16>,
    duration: Duration,
    request_bytes: Option<u64>,
    response_bytes: Option<u64>,
    result: &'static str,
) {
    emit(
        "http_request",
        &HttpEvent {
            method,
            route,
            status,
            duration_ms: duration_ms(duration),
            retry_count: 0,
            request_bytes,
            response_bytes,
            result,
        },
        result == "error",
    );
}

pub fn route_template(path: &str) -> &'static str {
    match path {
        "/users/" => "/users/",
        "/users/key" => "/users/key",
        "/v3/users/login" => "/v3/users/login",
        "/user-settings" => "/user-settings",
        "/v2/feature-flags" => "/v2/feature-flags",
        "/v4/sync/changes/contentKeys" => "/v4/sync/changes/contentKeys",
        "/v4/sync/named/cryptoKeys" => "/v4/sync/named/cryptoKeys",
        "/v4/sync/changes/template" => "/v4/sync/changes/template",
        "/v6/sync/journals" => "/v6/sync/journals",
        "/v3/sync/journals" => "/v3/sync/journals",
        "/users/notifications/feed" => "/users/notifications/feed",
        "/v2/templates" => "/v2/templates",
        "/v2/templates/categories" => "/v2/templates/categories",
        "/v1/journals/hierarchy" => "/v1/journals/hierarchy",
        "/shares/unread-entries" => "/shares/unread-entries",
        "/labs/ai/daily-chat/feed" => "/labs/ai/daily-chat/feed",
        "/labs/ai/daily-chat/settings" => "/labs/ai/daily-chat/settings",
        "/labs/ai/daily-chat/ephemeral/memory-updates" => {
            "/labs/ai/daily-chat/ephemeral/memory-updates"
        }
        "/labs/context-library/feed" => "/labs/context-library/feed",
        "/labs/context-library/items" => "/labs/context-library/items",
        "/pbcms/gallery" => "/pbcms/gallery",
        "/shares" => "/shares",
        _ if path.starts_with("/v2/sync/entries/") && path.ends_with("/feed") => {
            "/v2/sync/entries/{journal}/feed"
        }
        _ if path.starts_with("/shares/")
            && path.contains("/entries/")
            && path.ends_with("/lock") =>
        {
            "/shares/{journal}/entries/{entry}/lock"
        }
        _ if path.starts_with("/v3/sync/entries/") => "/v3/sync/entries/{journal}/{entry}",
        _ if path.starts_with("/v3/sync/journals/") => "/v3/sync/journals/{journal}",
        _ if path.starts_with("/labs/ai/daily-chat/") => "/labs/ai/daily-chat/{date}",
        _ if path.contains("/entries/") && path.contains("/comments") => {
            "/journals/{journal}/entries/{entry}/comments/{comment_action}"
        }
        _ => "unknown",
    }
}

pub fn record_outbox_result(
    resource: &str,
    operation: &str,
    object_id: &str,
    attempt: i64,
    result: &'static str,
    error: Option<&(dyn StdError + 'static)>,
) {
    let resource = safe_outbox_resource(resource);
    let operation = safe_outbox_operation(operation);
    let result = safe_result(result);
    let Some(mut current) = existing_state() else {
        return;
    };
    if current.invocation_id.is_none() {
        return;
    }
    let aggregate = current
        .outbox
        .entry((resource, operation, result))
        .or_default();
    aggregate.count = aggregate.count.saturating_add(1);
    aggregate.max = aggregate
        .max
        .max(u64::try_from(attempt).unwrap_or_default());
    if result == "error" || result == "deferred" {
        let object_ref = alias_locked(&mut current, "object", object_id);
        let safe_error = error
            .map(classify_error)
            .unwrap_or_else(|| SafeError::category("internal"));
        emit_locked(
            &mut current,
            "outbox_failure",
            &OutboxFailureEvent {
                resource,
                operation,
                object_ref: &object_ref,
                attempt,
                result,
                error: safe_error,
            },
            true,
        );
    }
}

#[derive(Serialize)]
struct OutboxFailureEvent<'a> {
    resource: &'static str,
    operation: &'static str,
    object_ref: &'a str,
    attempt: i64,
    result: &'static str,
    error: SafeError,
}

pub fn record_sqlite_success(
    operation: &'static str,
    table: &'static str,
    rows: u64,
    duration: Duration,
) {
    let Some(mut current) = existing_state() else {
        return;
    };
    if current.invocation_id.is_none() {
        return;
    }
    let aggregate = current
        .sqlite
        .entry((operation, table, "success"))
        .or_default();
    aggregate.count = aggregate.count.saturating_add(rows.max(1));
    aggregate.max = aggregate.max.max(duration_ms(duration));
}

pub fn record_sqlite_failure(
    operation: &'static str,
    table: &'static str,
    duration: Duration,
    error: &(dyn StdError + 'static),
) {
    emit(
        "sqlite_failure",
        &SqliteFailureEvent {
            operation,
            table,
            duration_ms: duration_ms(duration),
            error: classify_error(error),
        },
        true,
    );
}

#[derive(Serialize)]
struct SqliteFailureEvent {
    operation: &'static str,
    table: &'static str,
    duration_ms: u64,
    error: SafeError,
}

pub fn record_crypto(
    stage: &'static str,
    key_source: &'static str,
    result: &'static str,
    duration: Duration,
    error: Option<&(dyn StdError + 'static)>,
) {
    let result = safe_result(result);
    let Some(mut current) = existing_state() else {
        return;
    };
    if current.invocation_id.is_none() {
        return;
    }
    let aggregate = current
        .crypto
        .entry((stage, safe_key_source(key_source), result))
        .or_default();
    aggregate.count = aggregate.count.saturating_add(1);
    aggregate.max = aggregate.max.max(duration_ms(duration));
    if let Some(error) = error {
        emit_locked(
            &mut current,
            "crypto_failure",
            &CryptoFailureEvent {
                stage,
                key_source: safe_key_source(key_source),
                duration_ms: duration_ms(duration),
                error: classify_error(error),
            },
            true,
        );
    }
}

#[derive(Serialize)]
struct CryptoFailureEvent {
    stage: &'static str,
    key_source: &'static str,
    duration_ms: u64,
    error: SafeError,
}

pub struct MediaTrace {
    started: Instant,
    media_type: &'static str,
    direction: &'static str,
    bytes: u64,
    finished: bool,
}

impl MediaTrace {
    pub fn upload(content_type: &str, bytes: u64) -> Self {
        Self {
            started: Instant::now(),
            media_type: media_type_from_content_type(content_type),
            direction: "upload",
            bytes,
            finished: false,
        }
    }

    pub fn finish(mut self) {
        self.finished = true;
        record_media(
            self.media_type,
            self.direction,
            self.bytes,
            "success",
            self.started.elapsed(),
            None,
        );
    }
}

impl Drop for MediaTrace {
    fn drop(&mut self) {
        if !self.finished {
            record_media(
                self.media_type,
                self.direction,
                self.bytes,
                "error",
                self.started.elapsed(),
                None,
            );
        }
    }
}

fn record_media(
    media_type: &'static str,
    direction: &'static str,
    bytes: u64,
    result: &'static str,
    duration: Duration,
    error: Option<&(dyn StdError + 'static)>,
) {
    let result = safe_result(result);
    let Some(mut current) = existing_state() else {
        return;
    };
    if current.invocation_id.is_none() {
        return;
    }
    let aggregate = current
        .media
        .entry((safe_media_type(media_type), direction, result))
        .or_default();
    aggregate.count = aggregate.count.saturating_add(1);
    aggregate.bytes = aggregate.bytes.saturating_add(bytes);
    if result == "error" {
        emit_locked(
            &mut current,
            "media_failure",
            &MediaFailureEvent {
                media_type: safe_media_type(media_type),
                direction,
                bytes,
                duration_ms: duration_ms(duration),
                error: error
                    .map(classify_error)
                    .unwrap_or_else(|| SafeError::category("media")),
            },
            true,
        );
    }
}

#[derive(Serialize)]
struct MediaFailureEvent {
    media_type: &'static str,
    direction: &'static str,
    bytes: u64,
    duration_ms: u64,
    error: SafeError,
}

pub fn finish(result: &anyhow::Result<()>, sentry_event_id: Option<&str>) {
    let Some(mut current) = existing_state() else {
        return;
    };
    if current.invocation_id.is_none() {
        return;
    }
    let outcome = match result {
        Ok(()) => "success",
        Err(error) if crate::telemetry::is_user_error(error) => "user_error",
        Err(_) => "error",
    };
    if let Err(error) = result {
        emit_locked(
            &mut current,
            "error",
            &ErrorEvent {
                stage: "command",
                error: classify_error(error.as_ref()),
                sentry_event_id,
            },
            true,
        );
    }
    flush_aggregates(&mut current);
    let duration = current
        .started
        .map(|started| started.elapsed())
        .unwrap_or_default();
    let trace_write_failed = current.write_failed;
    let trace_truncated = current
        .recorder
        .as_ref()
        .is_some_and(|recorder| recorder.truncated);
    let suppressed_events = current
        .recorder
        .as_ref()
        .map(|recorder| recorder.suppressed_events)
        .unwrap_or_default();
    emit_locked(
        &mut current,
        "command_finished",
        &FinishedEvent {
            outcome,
            duration_ms: duration_ms(duration),
            trace_write_failed,
            trace_truncated,
            suppressed_events,
        },
        true,
    );
    emit_locked(
        &mut current,
        "invocation_finished",
        &FinishedEvent {
            outcome,
            duration_ms: duration_ms(duration),
            trace_write_failed,
            trace_truncated,
            suppressed_events,
        },
        true,
    );
}

#[derive(Serialize)]
struct ErrorEvent<'a> {
    stage: &'static str,
    error: SafeError,
    #[serde(skip_serializing_if = "Option::is_none")]
    sentry_event_id: Option<&'a str>,
}

#[derive(Serialize)]
struct FinishedEvent {
    outcome: &'static str,
    duration_ms: u64,
    trace_write_failed: bool,
    trace_truncated: bool,
    suppressed_events: u64,
}

pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(state) = STATE.get()
            && let Ok(mut current) = state.try_lock()
            && current.invocation_id.is_some()
        {
            emit_locked(&mut current, "panic", &PanicEvent {}, true);
        }
        previous(info);
    }));
}

#[derive(Serialize)]
struct PanicEvent {}

fn flush_aggregates(current: &mut DiagnosticsState) {
    let outbox = std::mem::take(&mut current.outbox);
    for ((resource, operation, result), aggregate) in outbox {
        emit_locked(
            current,
            "outbox_summary",
            &OutboxSummaryEvent {
                resource,
                operation,
                result,
                count: aggregate.count,
                max_attempt: aggregate.max,
            },
            true,
        );
    }
    let sqlite = std::mem::take(&mut current.sqlite);
    for ((operation, table, result), aggregate) in sqlite {
        emit_locked(
            current,
            "sqlite_operation",
            &SqliteSummaryEvent {
                operation,
                table,
                result,
                operation_count: aggregate.count,
                max_duration_ms: aggregate.max,
            },
            true,
        );
    }
    let crypto = std::mem::take(&mut current.crypto);
    for ((stage, key_source, result), aggregate) in crypto {
        emit_locked(
            current,
            "crypto_summary",
            &CryptoSummaryEvent {
                stage,
                key_source,
                result,
                count: aggregate.count,
                max_duration_ms: aggregate.max,
            },
            true,
        );
    }
    let media = std::mem::take(&mut current.media);
    for ((media_type, direction, result), aggregate) in media {
        emit_locked(
            current,
            "media_summary",
            &MediaSummaryEvent {
                media_type,
                direction,
                result,
                count: aggregate.count,
                bytes: aggregate.bytes,
            },
            true,
        );
    }
}

#[derive(Serialize)]
struct OutboxSummaryEvent {
    resource: &'static str,
    operation: &'static str,
    result: &'static str,
    count: u64,
    max_attempt: u64,
}

#[derive(Serialize)]
struct SqliteSummaryEvent {
    operation: &'static str,
    table: &'static str,
    result: &'static str,
    operation_count: u64,
    max_duration_ms: u64,
}

#[derive(Serialize)]
struct CryptoSummaryEvent {
    stage: &'static str,
    key_source: &'static str,
    result: &'static str,
    count: u64,
    max_duration_ms: u64,
}

#[derive(Serialize)]
struct MediaSummaryEvent {
    media_type: &'static str,
    direction: &'static str,
    result: &'static str,
    count: u64,
    bytes: u64,
}

fn emit<T: Serialize>(event: &'static str, data: &T, essential: bool) {
    let Some(mut current) = existing_state() else {
        return;
    };
    if current.invocation_id.is_some() {
        emit_locked(&mut current, event, data, essential);
    }
}

fn emit_locked<T: Serialize>(
    current: &mut DiagnosticsState,
    event: &'static str,
    data: &T,
    essential: bool,
) {
    let Some(recorder) = current.recorder.as_mut() else {
        return;
    };
    if let Err(error) = recorder.write(event, data, essential) {
        current.write_failed = true;
        current.recorder = None;
        if !current.warned_write_failed {
            current.warned_write_failed = true;
            eprintln!(
                "Diagnostic trace stopped: write failed ({})",
                io_kind(error.kind())
            );
        }
    }
}

fn alias_locked(current: &mut DiagnosticsState, kind: &'static str, raw: &str) -> String {
    let key = (kind, raw.to_owned());
    if let Some(existing) = current.aliases.get(&key) {
        return existing.clone();
    }
    let count = current.alias_counts.entry(kind).or_default();
    *count += 1;
    let alias = format!("{kind}_{count}");
    current.aliases.insert(key, alias.clone());
    alias
}

struct Recorder {
    writer: BufWriter<File>,
    invocation_id: String,
    sequence: u64,
    bytes_written: usize,
    truncated: bool,
    suppressed_events: u64,
}

impl Recorder {
    fn new(file: File, invocation_id: String) -> Self {
        Self {
            writer: BufWriter::new(file),
            invocation_id,
            sequence: 0,
            bytes_written: 0,
            truncated: false,
            suppressed_events: 0,
        }
    }

    fn write<T: Serialize>(
        &mut self,
        event: &'static str,
        data: &T,
        essential: bool,
    ) -> std::io::Result<()> {
        if self.truncated && !essential {
            self.suppressed_events = self.suppressed_events.saturating_add(1);
            return Ok(());
        }
        let line = self.serialize(event, data)?;
        let limit = if matches!(event, "command_finished" | "invocation_finished" | "panic") {
            MAX_TRACE_BYTES
        } else if event == "error" {
            FINAL_ERROR_TRACE_BYTES
        } else if matches!(
            event,
            "outbox_summary" | "sqlite_operation" | "crypto_summary" | "media_summary"
        ) {
            SUMMARY_TRACE_BYTES
        } else if essential {
            ESSENTIAL_TRACE_BYTES
        } else {
            NORMAL_TRACE_BYTES
        };
        if self.bytes_written.saturating_add(line.len()) > limit {
            if !self.truncated {
                self.truncated = true;
                let marker = self.serialize(
                    "trace_truncated",
                    &TraceTruncated {
                        limit_bytes: MAX_TRACE_BYTES,
                    },
                )?;
                if self.bytes_written.saturating_add(marker.len()) <= TRUNCATION_TRACE_BYTES {
                    self.write_line(&marker)?;
                }
                eprintln!("Diagnostic trace truncated at 1 MiB");
            }
            self.suppressed_events = self.suppressed_events.saturating_add(1);
            return Ok(());
        }
        self.write_line(&line)
    }

    fn serialize<T: Serialize>(
        &mut self,
        event: &'static str,
        data: &T,
    ) -> std::io::Result<Vec<u8>> {
        self.sequence = self.sequence.saturating_add(1);
        let record = Record {
            timestamp: utc_timestamp(),
            sequence: self.sequence,
            invocation_id: &self.invocation_id,
            event,
            data,
        };
        let mut line = serde_json::to_vec(&record).map_err(std::io::Error::other)?;
        line.push(b'\n');
        Ok(line)
    }

    fn write_line(&mut self, line: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(line)?;
        self.writer.flush()?;
        self.bytes_written = self.bytes_written.saturating_add(line.len());
        Ok(())
    }
}

#[derive(Serialize)]
struct Record<'a, T> {
    timestamp: String,
    sequence: u64,
    invocation_id: &'a str,
    event: &'static str,
    data: &'a T,
}

#[derive(Serialize)]
struct TraceTruncated {
    limit_bytes: usize,
}

pub fn utc_timestamp() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_owned())
}

fn new_invocation_id() -> String {
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub fn byte_len(value: impl AsRef<[u8]>) -> Option<u64> {
    u64::try_from(value.as_ref().len()).ok()
}

pub fn json_len(value: &serde_json::Value) -> Option<u64> {
    if !is_tracing() {
        return None;
    }
    serde_json::to_vec(value).ok().and_then(byte_len)
}

fn safe_result(result: &str) -> &'static str {
    match result {
        "success" => "success",
        "error" => "error",
        "deferred" => "deferred",
        "skipped" => "skipped",
        "locked" => "locked",
        _ => "other",
    }
}

fn safe_sync_resource(resource: &str) -> &'static str {
    if resource.starts_with("entries:") {
        return "entries";
    }
    match resource {
        "user_profile" => "user_profile",
        "feature_flags" => "feature_flags",
        "user_key" => "user_key",
        "content_keys" => "content_keys",
        "named_crypto_keys" => "named_crypto_keys",
        "journals" => "journals",
        "notifications" => "notifications",
        "templates" => "templates",
        "template_categories" => "template_categories",
        "journal_hierarchy" => "journal_hierarchy",
        "unread_entries" => "unread_entries",
        "daily_chat_feed" => "daily_chat_feed",
        "daily_chat_settings" => "daily_chat_settings",
        "context_items" => "context_items",
        "user_settings" => "user_settings",
        "pbc_template_categories" => "pbc_template_categories",
        "pbc_templates" => "pbc_templates",
        "pbc_prompt_pack_gallery" => "pbc_prompt_pack_gallery",
        "context_library" => "context_library",
        "outbox:post" => "outbox",
        _ => "other",
    }
}

fn safe_outbox_resource(resource: &str) -> &'static str {
    match resource {
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

fn safe_outbox_operation(operation: &str) -> &'static str {
    match operation {
        "create" => "create",
        "update" => "update",
        "delete" => "delete",
        _ => "other",
    }
}

fn safe_key_source(source: &str) -> &'static str {
    match source {
        "keychain" => "keychain",
        "sqlite" => "sqlite",
        "journal" => "journal",
        "missing" => "missing",
        _ => "unknown",
    }
}

pub fn media_type_from_content_type(content_type: &str) -> &'static str {
    let family = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .split('/')
        .next()
        .unwrap_or_default();
    match family {
        "image" => "image",
        "video" => "video",
        "audio" => "audio",
        _ if content_type.trim().starts_with("application/pdf") => "pdf",
        _ => "other",
    }
}

fn safe_media_type(media_type: &str) -> &'static str {
    match media_type {
        "image" => "image",
        "video" => "video",
        "audio" => "audio",
        "pdf" => "pdf",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_templates_never_return_dynamic_segments() {
        assert_eq!(
            route_template("/v2/sync/entries/private-journal-id/feed"),
            "/v2/sync/entries/{journal}/feed"
        );
        assert_eq!(route_template("/v3/sync/journals"), "/v3/sync/journals");
        assert_eq!(
            route_template("/v4/sync/changes/template"),
            "/v4/sync/changes/template"
        );
        assert_eq!(route_template("/completely/private/value"), "unknown");
    }

    #[test]
    fn sync_resource_allowlist_covers_static_pull_resources() {
        for resource in [
            "unread_entries",
            "user_settings",
            "pbc_template_categories",
            "pbc_templates",
            "pbc_prompt_pack_gallery",
            "context_library",
        ] {
            assert_eq!(safe_sync_resource(resource), resource);
        }
    }

    #[test]
    fn error_classification_omits_messages() {
        let error = std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "/Users/alice/private/dayone.db",
        );
        let safe = classify_error(&error);
        let json = serde_json::to_string(&safe).expect("serialize");
        assert_eq!(safe.category, "io");
        assert_eq!(safe.io_kind, Some("permission_denied"));
        assert!(!json.contains("alice"));
        assert!(!json.contains("dayone.db"));
    }

    #[test]
    fn interactive_keychain_requirement_is_expected() {
        let safe = classify_error(&crate::store::StoreError::KeychainInteractionRequired);
        assert_eq!(safe.category, "keychain");
        assert!(safe.expected);

        let unavailable = classify_error(&crate::store::StoreError::KeychainUnavailable(
            "locked".to_owned(),
        ));
        assert_eq!(unavailable.category, "keychain");
        assert!(!unavailable.expected);
    }

    #[test]
    fn persistent_ids_are_replaced_with_stable_invocation_aliases() {
        let mut state = DiagnosticsState::default();
        assert_eq!(alias_locked(&mut state, "sync_run", "987654"), "sync_run_1");
        assert_eq!(alias_locked(&mut state, "sync_run", "987654"), "sync_run_1");
        assert_eq!(alias_locked(&mut state, "sync_run", "123456"), "sync_run_2");
    }

    #[test]
    fn invocation_ids_are_unique_uuid_shapes() {
        let first = new_invocation_id();
        let second = new_invocation_id();
        assert_ne!(first, second);
        assert_eq!(first.len(), 36);
        assert_eq!(first.chars().filter(|c| *c == '-').count(), 4);
    }

    #[test]
    fn endpoint_class_does_not_echo_custom_hosts() {
        assert_eq!(
            endpoint_class(crate::constants::PRODUCTION_URL),
            "production"
        );
        assert_eq!(endpoint_class(crate::constants::STAGING_URL), "staging");
        assert_eq!(endpoint_class("https://private.example.test"), "custom");
    }

    #[test]
    fn recorder_caps_output_and_keeps_complete_json_lines() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("trace.jsonl");
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("trace file");
        let mut recorder = Recorder::new(file, new_invocation_id());
        let payload = serde_json::json!({ "value": "x".repeat(16 * 1024) });
        for _ in 0..200 {
            recorder
                .write("config", &payload, false)
                .expect("bounded write");
        }
        let essential_payload = serde_json::json!({ "value": "x".repeat(256) });
        for _ in 0..200 {
            recorder
                .write("sqlite_failure", &essential_payload, true)
                .expect("bounded essential write");
        }
        recorder
            .write(
                "error",
                &serde_json::json!({ "category": "safe_final" }),
                true,
            )
            .expect("final error");
        recorder
            .write("outbox_summary", &serde_json::json!({ "count": 1 }), true)
            .expect("summary");
        recorder
            .write("command_finished", &serde_json::json!({}), true)
            .expect("command finish");
        recorder
            .write("invocation_finished", &serde_json::json!({}), true)
            .expect("invocation finish");
        assert!(recorder.truncated);
        drop(recorder);

        let bytes = std::fs::read(&path).expect("read trace");
        assert!(bytes.len() <= MAX_TRACE_BYTES);
        let records = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<serde_json::Value>(line).expect("valid JSONL"))
            .collect::<Vec<_>>();
        assert!(
            records
                .iter()
                .any(|record| record["event"] == "trace_truncated")
        );
        assert!(records.iter().any(|record| {
            record["event"] == "error" && record["data"]["category"] == "safe_final"
        }));
        assert!(
            records
                .iter()
                .any(|record| record["event"] == "outbox_summary")
        );
        assert_eq!(records.last().unwrap()["event"], "invocation_finished");
    }
}
