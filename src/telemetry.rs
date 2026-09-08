//! Sentry-backed error monitoring for the CLI.
//!
//! When the `telemetry` cargo feature is enabled (default), this module
//! initializes Sentry after consent, installs a panic hook (via Sentry's panic
//! integration), and provides helpers for capturing `anyhow::Error` from the
//! top-level command dispatcher. With the feature disabled the binary has no
//! Sentry dependency: [`init`], [`set_command`], [`set_profile`], and
//! [`capture_anyhow`] compile down to no-ops, while the always-local helpers
//! ([`is_enabled`] reads env vars, [`is_user_error`] walks the error chain)
//! still run their checks unchanged.
//!
//! # Opt-out
//!
//! Telemetry requires a saved agreement to the current disclosure. These
//! environment settings override agreement and disable it:
//!
//! - `DAYONE_TELEMETRY=0` (or `false`/`off`/`no`/`disabled`)
//! - `DO_NOT_TRACK=1` (per <https://consoledonottrack.com/>)
//!
//! Both are read from the process environment, so they can be set in
//! `~/.config/dayone/secrets-cli` for a persistent opt-out.
//!
//! # Privacy
//!
//! This CLI handles personal journaling data, so capture is deliberately
//! conservative:
//!
//! - The `User` and `ServerName` event fields are stripped.
//! - The [`scrub`] hook redacts `$HOME` paths, URLs, email addresses, and long
//!   token-shaped strings from event messages, exception values, breadcrumbs,
//!   and stack-trace frame paths. HTTP response bodies are removed from API
//!   error exceptions.
//! - Errors marked with [`UserError`] are **not** sent — wrap expected
//!   user-facing failures (wrong password, validation, missing auth) so they
//!   don't pollute the telemetry stream.

use std::error::Error as StdError;
use std::fmt;

const AUTH_REQUIRED_MESSAGE: &str =
    "no auth session for this endpoint; run `dayone auth login` first";
const PASSWORD_READ_FAILED_MESSAGE: &str = "failed to read password";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserErrorKind {
    AuthRequired,
    PasswordReadFailed,
    Generic,
}

/// Marker error type for *expected* user-facing errors that must never be
/// reported to Sentry.
///
/// Wrap validation failures, "not found" errors, missing auth, etc. so the
/// telemetry layer skips them. Internal errors (sqlite IO, JSON decode,
/// network, panics) should bubble up as plain `anyhow::Error` so they reach
/// Sentry.
///
/// # Example
///
/// ```ignore
/// use crate::telemetry::UserError;
///
/// fn check_password(p: &str) -> anyhow::Result<()> {
///     if p.len() < 8 {
///         return Err(UserError::new("password must be at least 8 chars").into());
///     }
///     Ok(())
/// }
/// ```
#[derive(Debug)]
pub struct UserError {
    kind: UserErrorKind,
    message: String,
}

impl UserError {
    // Only test code calls this constructor today; once the first production
    // command opts an error out of capture, the attribute can come off.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            kind: UserErrorKind::Generic,
            message: message.into(),
        }
    }

    pub fn auth_required() -> Self {
        Self {
            kind: UserErrorKind::AuthRequired,
            message: AUTH_REQUIRED_MESSAGE.to_owned(),
        }
    }

    pub fn password_read_failed() -> Self {
        Self {
            kind: UserErrorKind::PasswordReadFailed,
            message: PASSWORD_READ_FAILED_MESSAGE.to_owned(),
        }
    }

    pub fn kind(&self) -> UserErrorKind {
        self.kind
    }
}

impl fmt::Display for UserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for UserError {}

/// RAII guard returned from [`init`].
///
/// Holding the guard until shutdown is what flushes Sentry's transport. With
/// the `telemetry` feature off, dropping the guard is a no-op.
#[derive(Default)]
pub struct TelemetryGuard {
    #[cfg(feature = "telemetry")]
    _inner: Option<sentry::ClientInitGuard>,
}

/// Initialise Sentry.
///
/// Returns a no-op guard when the `telemetry` feature is disabled, when the
/// user has not consented or has opted out via env var, or when no DSN is
/// configured (compile-time `DAYONE_SENTRY_DSN` empty and runtime override unset).
pub fn init(config: &crate::config::AppConfig, config_dir: &std::path::Path) -> TelemetryGuard {
    #[cfg(not(feature = "telemetry"))]
    {
        let _ = (config, config_dir);
        TelemetryGuard {}
    }

    #[cfg(feature = "telemetry")]
    {
        if !is_enabled() || !config.analytics.consent_granted() {
            return TelemetryGuard { _inner: None };
        }
        let Some(dsn) = dsn() else {
            return TelemetryGuard { _inner: None };
        };

        let config_dir = config_dir.to_owned();
        let recorded_at_ms = config.analytics.consent_recorded_at_ms.unwrap();
        let guard = sentry::init((
            dsn,
            sentry::ClientOptions {
                release: Some(env!("CARGO_PKG_VERSION").into()),
                environment: Some(environment().into()),
                send_default_pii: false,
                attach_stacktrace: true,
                before_send: Some(std::sync::Arc::new(move |event| {
                    if crate::consent::still_permitted(&config_dir, recorded_at_ms) {
                        scrub::event(event)
                    } else {
                        None
                    }
                })),
                ..Default::default()
            },
        ));

        sentry::configure_scope(|scope| {
            scope.set_tag("os.family", std::env::consts::FAMILY);
            scope.set_tag("os.os", std::env::consts::OS);
            scope.set_tag("os.arch", std::env::consts::ARCH);
            scope.set_tag(
                "build.production_default",
                cfg!(feature = "production-default"),
            );
        });

        TelemetryGuard {
            _inner: Some(guard),
        }
    }
}

/// Returns `true` when telemetry should run based on opt-out env signals.
///
/// This is only the environment gate, not permission to collect. Consent
/// and collector-specific settings must also permit reporting.
#[cfg_attr(not(feature = "telemetry"), allow(dead_code))]
pub fn is_enabled() -> bool {
    if std::env::var("DO_NOT_TRACK").as_deref().map(str::trim) == Ok("1") {
        return false;
    }
    if let Ok(value) = std::env::var("DAYONE_TELEMETRY")
        && matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off" | "disabled"
        )
    {
        return false;
    }
    true
}

/// Whether Sentry is configured to collect, before resolving consent.
pub fn would_collect() -> bool {
    #[cfg(feature = "telemetry")]
    {
        is_enabled() && dsn().is_some_and(|value| value.parse::<sentry::types::Dsn>().is_ok())
    }
    #[cfg(not(feature = "telemetry"))]
    {
        false
    }
}

/// Whether Sentry has actually been initialized after consent this invocation.
pub fn is_sentry_enabled() -> bool {
    #[cfg(feature = "telemetry")]
    {
        sentry::Hub::current()
            .client()
            .is_some_and(|client| client.is_enabled())
    }
    #[cfg(not(feature = "telemetry"))]
    {
        false
    }
}

/// Tag the current Sentry scope with the top-level command being run.
///
/// Called from `cli::run()` once `clap` has parsed the command tree. Safe to
/// call when telemetry is disabled — Sentry's hub silently ignores scope
/// configuration when no client is initialised.
pub fn set_command(name: &str) {
    #[cfg(feature = "telemetry")]
    sentry::configure_scope(|scope| scope.set_tag("command", name));
    #[cfg(not(feature = "telemetry"))]
    let _ = name;
}

/// Tag the current Sentry scope with a safe profile category, never a
/// user-controlled custom profile name.
pub fn set_profile(name: &str) {
    let category = safe_profile_category(name);
    #[cfg(feature = "telemetry")]
    sentry::configure_scope(|scope| scope.set_tag("profile", category));
    #[cfg(not(feature = "telemetry"))]
    let _ = category;
}

fn safe_profile_category(name: &str) -> &'static str {
    match name {
        "production" => "production",
        "staging" => "staging",
        _ => "custom",
    }
}

/// Tag Sentry events from an explicitly diagnostic invocation.
pub fn set_diagnostic_invocation_id(id: &str) {
    #[cfg(feature = "telemetry")]
    sentry::configure_scope(|scope| scope.set_tag("diagnostic_invocation_id", id));
    #[cfg(not(feature = "telemetry"))]
    let _ = id;
}

/// Capture an `anyhow::Error`, skipping expected user-actionable failures.
/// Returns the Sentry event id when the SDK accepted an event.
pub fn capture_anyhow(err: &anyhow::Error) -> Option<String> {
    if is_user_error(err) {
        return None;
    }
    #[cfg(feature = "telemetry")]
    {
        let event_id = sentry::integrations::anyhow::capture_anyhow(err).to_string();
        (event_id != "00000000-0000-0000-0000-000000000000").then_some(event_id)
    }
    #[cfg(not(feature = "telemetry"))]
    {
        let _ = err;
        None
    }
}

/// Returns `true` for failures the user can resolve without an engineering fix.
pub fn is_user_error(err: &anyhow::Error) -> bool {
    user_error_kind(err).is_some()
        || err.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<crate::store::StoreError>(),
                Some(crate::store::StoreError::KeychainInteractionRequired)
            )
        })
}

/// Returns the first [`UserErrorKind`] found in the error chain.
pub fn user_error_kind(err: &anyhow::Error) -> Option<UserErrorKind> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<UserError>().map(UserError::kind))
}

#[cfg(feature = "telemetry")]
fn dsn() -> Option<String> {
    fn non_empty_trimmed(value: &str) -> Option<String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    }

    if let Ok(value) = std::env::var("DAYONE_SENTRY_DSN")
        && let Some(dsn) = non_empty_trimmed(&value)
    {
        return Some(dsn);
    }
    // Trim the compile-time value too: CI secret injection often appends
    // a stray newline or padding which would otherwise make the DSN parser
    // reject the value and silently disable telemetry.
    option_env!("DAYONE_SENTRY_DSN").and_then(non_empty_trimmed)
}

#[cfg(feature = "telemetry")]
fn environment() -> &'static str {
    if cfg!(feature = "production-default") {
        "production"
    } else {
        "staging"
    }
}

#[cfg(feature = "telemetry")]
mod scrub {
    use std::sync::OnceLock;

    use regex::Regex;
    use sentry::protocol::{Breadcrumb, Event, Exception, Frame, Value};

    /// Sentry `before_send` hook: clones nothing the user didn't already give
    /// us, but redacts likely-PII shapes from common string-bearing fields.
    pub(super) fn event(mut event: Event<'static>) -> Option<Event<'static>> {
        event.user = None;
        event.server_name = None;

        if let Some(message) = event.message.take() {
            event.message = Some(scrub_text(&message));
        }

        for crumb in event.breadcrumbs.values.iter_mut() {
            scrub_breadcrumb(crumb);
        }

        for exc in event.exception.values.iter_mut() {
            scrub_exception(exc);
        }

        if let Some(stacktrace) = event.stacktrace.as_mut() {
            for frame in stacktrace.frames.iter_mut() {
                scrub_frame(frame);
            }
        }

        Some(event)
    }

    fn scrub_breadcrumb(crumb: &mut Breadcrumb) {
        if let Some(message) = crumb.message.take() {
            crumb.message = Some(scrub_text(&message));
        }
        let data = std::mem::take(&mut crumb.data);
        crumb.data = data.into_iter().map(|(k, v)| (k, scrub_value(v))).collect();
    }

    fn scrub_exception(exc: &mut Exception) {
        if let Some(value) = exc.value.take() {
            exc.value = Some(if exc.ty == "ApiError" {
                safe_http_status(&value)
            } else {
                scrub_text(&value)
            });
        }
        if let Some(stacktrace) = exc.stacktrace.as_mut() {
            for frame in stacktrace.frames.iter_mut() {
                scrub_frame(frame);
            }
        }
    }

    fn scrub_frame(frame: &mut Frame) {
        if let Some(filename) = frame.filename.take() {
            frame.filename = Some(scrub_text(&filename));
        }
        if let Some(abs_path) = frame.abs_path.take() {
            frame.abs_path = Some(scrub_text(&abs_path));
        }
    }

    fn scrub_value(v: Value) -> Value {
        match v {
            Value::String(s) => Value::String(scrub_text(&s)),
            Value::Array(items) => Value::Array(items.into_iter().map(scrub_value).collect()),
            Value::Object(map) => {
                Value::Object(map.into_iter().map(|(k, v)| (k, scrub_value(v))).collect())
            }
            other => other,
        }
    }

    /// Apply `$HOME` → `<HOME>`, URL → `<URL>`, email → `<EMAIL>`, and long
    /// token-shaped strings → `<REDACTED>` substitutions in order.
    pub(super) fn scrub_text(input: &str) -> String {
        let stripped_home = strip_home(input);
        let stripped_urls =
            url_regex().replace_all(&stripped_home, |captures: &regex::Captures| {
                let url = &captures[0];
                let trimmed =
                    url.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '"', '\'']);
                format!("<URL>{}", &url[trimmed.len()..])
            });
        let stripped_emails = email_regex().replace_all(&stripped_urls, "<EMAIL>");
        let stripped_tokens = token_regex().replace_all(&stripped_emails, "<REDACTED>");
        stripped_tokens.into_owned()
    }

    fn safe_http_status(value: &str) -> String {
        value
            .strip_prefix("HTTP ")
            .and_then(|rest| rest.split_whitespace().next())
            .filter(|status| status.parse::<u16>().is_ok())
            .map_or_else(
                || "HTTP request failed".to_owned(),
                |status| format!("HTTP {status}"),
            )
    }

    fn strip_home(input: &str) -> String {
        let Some(home) = home_dir_string() else {
            return input.to_owned();
        };
        let trimmed = home.trim_end_matches(['/', '\\']);
        if trimmed.is_empty() {
            return input.to_owned();
        }
        input.replace(trimmed, "<HOME>")
    }

    fn home_dir_string() -> Option<String> {
        dirs::home_dir().map(|p| p.to_string_lossy().into_owned())
    }

    fn url_regex() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"(?i)\bhttps?://\S+").expect("valid URL regex"))
    }

    fn email_regex() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            // Conservative: catches typical address shapes without trying to
            // be RFC 5322 compliant.
            Regex::new(r"\b[\w.+-]+@[\w-]+(?:\.[\w-]+)+\b").expect("valid email regex")
        })
    }

    fn token_regex() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            // Long base64-/hex-/url-safe alphabets are almost certainly
            // tokens, JWTs, or similar. 40 chars avoids clobbering function
            // names and short hashes.
            Regex::new(r"[A-Za-z0-9_/+\-]{40,}={0,2}").expect("valid token regex")
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn email_regex_matches_common_shapes() {
            let re = email_regex();
            assert!(re.is_match("contact me at alice@example.com please"));
            assert!(re.is_match("user.name+tag@sub.example.co.uk"));
            assert!(!re.is_match("not an email: alice at example dot com"));
        }

        #[test]
        fn token_regex_matches_long_alphanumeric_strings() {
            let re = token_regex();
            assert!(re.is_match("Bearer abcdef0123456789abcdef0123456789abcdef0123"));
            assert!(!re.is_match("short_token"));
        }

        #[test]
        fn scrub_text_redacts_url_email_and_token() {
            let input = "request (https://private.example/journals/id?cursor=secret), then email \
                         alice@example.com with token \
                         abcdef0123456789abcdef0123456789abcdef0123";
            let out = scrub_text(input);
            assert!(out.contains("request (<URL>), then email"));
            assert!(out.contains("<EMAIL>"));
            assert!(out.contains("<REDACTED>"));
            assert!(!out.contains("private.example"));
            assert!(!out.contains("alice@example.com"));
        }

        #[test]
        fn scrub_text_redacts_home_dir_when_present() {
            let Some(home) = home_dir_string() else {
                return;
            };
            let trimmed = home.trim_end_matches(['/', '\\']);
            if trimmed.is_empty() {
                return;
            }
            let input = format!("could not open {trimmed}/Library/dayone.db");
            let out = scrub_text(&input);
            assert!(out.starts_with("could not open <HOME>"));
            assert!(!out.contains(trimmed));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::with_env;

    #[test]
    fn is_enabled_default_true() {
        with_env(
            &[("DO_NOT_TRACK", None), ("DAYONE_TELEMETRY", None)],
            || {
                assert!(is_enabled());
            },
        );
    }

    #[test]
    fn is_enabled_respects_do_not_track() {
        with_env(
            &[("DO_NOT_TRACK", Some("1")), ("DAYONE_TELEMETRY", None)],
            || {
                assert!(!is_enabled());
            },
        );
    }

    #[test]
    fn do_not_track_other_values_dont_disable() {
        // Per consoledonottrack.com only "1" is the opt-out signal. Anything
        // else (including empty) means "no preference set".
        with_env(
            &[("DO_NOT_TRACK", Some("0")), ("DAYONE_TELEMETRY", None)],
            || {
                assert!(is_enabled());
            },
        );
    }

    #[test]
    fn dayone_telemetry_falsy_values_disable() {
        for value in ["0", "false", "False", "no", "off", "disabled"] {
            with_env(
                &[("DO_NOT_TRACK", None), ("DAYONE_TELEMETRY", Some(value))],
                || {
                    assert!(!is_enabled(), "value `{value}` should disable telemetry");
                },
            );
        }
    }

    #[test]
    fn dayone_telemetry_truthy_values_keep_enabled() {
        for value in ["1", "true", "yes", "on", "enabled", "anything-else"] {
            with_env(
                &[("DO_NOT_TRACK", None), ("DAYONE_TELEMETRY", Some(value))],
                || {
                    assert!(is_enabled(), "value `{value}` should keep telemetry on");
                },
            );
        }
    }

    #[test]
    fn user_error_round_trip() {
        let err: anyhow::Error = UserError::new("oops").into();
        assert_eq!(err.to_string(), "oops");
        assert!(is_user_error(&err));
        assert_eq!(user_error_kind(&err), Some(UserErrorKind::Generic));
    }

    #[test]
    fn user_error_kind_helpers_build_expected_errors() {
        let auth_required = UserError::auth_required();
        assert_eq!(auth_required.kind(), UserErrorKind::AuthRequired);
        assert_eq!(auth_required.to_string(), AUTH_REQUIRED_MESSAGE);

        let password_read_failed = UserError::password_read_failed();
        assert_eq!(
            password_read_failed.kind(),
            UserErrorKind::PasswordReadFailed
        );
        assert_eq!(
            password_read_failed.to_string(),
            PASSWORD_READ_FAILED_MESSAGE
        );
    }

    #[test]
    fn user_error_detected_when_wrapped_with_context() {
        let err: anyhow::Error =
            anyhow::Error::from(UserError::new("bad password")).context("auth login failed");
        assert!(is_user_error(&err));
        assert_eq!(user_error_kind(&err), Some(UserErrorKind::Generic));
    }

    #[test]
    fn plain_anyhow_error_is_not_user_error() {
        let err = anyhow::anyhow!("internal failure");
        assert!(!is_user_error(&err));
        assert_eq!(user_error_kind(&err), None);
    }

    #[test]
    fn interactive_keychain_requirement_is_user_actionable() {
        let err: anyhow::Error = crate::store::StoreError::KeychainInteractionRequired.into();
        assert!(is_user_error(&err));
        assert_eq!(user_error_kind(&err), None);

        let unavailable: anyhow::Error =
            crate::store::StoreError::KeychainUnavailable("locked".to_owned()).into();
        assert!(!is_user_error(&unavailable));
    }

    #[test]
    fn custom_profile_names_are_reduced_to_a_safe_category() {
        assert_eq!(safe_profile_category("production"), "production");
        assert_eq!(safe_profile_category("staging"), "staging");
        assert_eq!(safe_profile_category("Sam's private profile"), "custom");
    }

    #[cfg(feature = "telemetry")]
    #[test]
    fn sentry_event_omits_request_urls_and_api_response_bodies() {
        let err = anyhow::Error::new(crate::http::ApiError::new(
            503,
            "Service Unavailable",
            "private response body canary",
        ))
        .context(
            "request failed for https://private.example/journals/private-id?cursor=private-cursor",
        );
        let event = scrub::event(sentry::integrations::anyhow::event_from_error(&err))
            .expect("event should be retained");
        let serialized_event = serde_json::to_string(&event).expect("event should serialize");

        assert!(serialized_event.contains("HTTP 503"));
        assert!(serialized_event.contains("<URL>"));
        for private_value in [
            "private response body canary",
            "private.example",
            "private-id",
            "private-cursor",
        ] {
            assert!(!serialized_event.contains(private_value));
        }
    }

    #[cfg(feature = "telemetry")]
    #[test]
    fn diagnostic_invocation_and_profile_category_reach_sentry_scope() {
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct CaptureTransport(Mutex<Vec<sentry::protocol::Event<'static>>>);

        impl sentry::Transport for CaptureTransport {
            fn send_envelope(&self, envelope: sentry::Envelope) {
                if let Some(event) = envelope.event() {
                    self.0.lock().expect("events lock").push(event.clone());
                }
            }
        }

        let transport = Arc::new(CaptureTransport::default());
        let options = sentry::ClientOptions {
            dsn: Some("https://public@sentry.invalid/1".parse().expect("test DSN")),
            transport: Some(Arc::new(transport.clone())),
            ..Default::default()
        };
        sentry::Hub::run(
            Arc::new(sentry::Hub::new(
                Some(Arc::new(options.into())),
                Arc::new(Default::default()),
            )),
            || {
                set_profile("private-profile-name");
                set_diagnostic_invocation_id("diagnostic-123");
                sentry::capture_message("controlled failure", sentry::Level::Error);
            },
        );

        let events = transport.0.lock().expect("events lock");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].tags.get("profile").map(String::as_str),
            Some("custom")
        );
        assert_eq!(
            events[0]
                .tags
                .get("diagnostic_invocation_id")
                .map(String::as_str),
            Some("diagnostic-123")
        );
        assert!(!format!("{:?}", events[0].tags).contains("private-profile-name"));
    }

    #[test]
    fn capture_anyhow_skips_user_errors_silently() {
        // No DSN is configured in the test environment, so the only thing we
        // can assert is that calling capture on a UserError doesn't panic.
        let err: anyhow::Error = UserError::new("user-facing").into();
        capture_anyhow(&err);
    }

    /// Verifies the runtime-trim and blank-fall-through branches of
    /// [`dsn`]. The blank-runtime case can only be asserted when no
    /// compile-time DSN is baked in — otherwise the fall-through legitimately
    /// returns that compile-time value (we can't override `option_env!` from
    /// a test). Default `cargo test` runs and `tests.yml` build without a
    /// DSN, so the gated branch normally runs.
    #[cfg(feature = "telemetry")]
    #[test]
    fn dsn_trims_runtime_value_and_falls_through_when_blank() {
        with_env(
            &[("DAYONE_SENTRY_DSN", Some("  https://abc@sentry.io/1  "))],
            || {
                assert_eq!(
                    dsn().as_deref(),
                    Some("https://abc@sentry.io/1"),
                    "runtime DSN should be returned trimmed"
                );
            },
        );

        if option_env!("DAYONE_SENTRY_DSN").is_none() {
            with_env(&[("DAYONE_SENTRY_DSN", Some("   "))], || {
                assert_eq!(
                    dsn(),
                    None,
                    "all-whitespace runtime DSN should fall through to None"
                );
            });
        }
    }
}
