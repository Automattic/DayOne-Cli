//! Automattic Tracks transport.
//!
//! Events are recorded via the documented Tracks REST endpoint,
//! `POST https://public-api.wordpress.com/rest/v1.1/tracks/record`, which
//! accepts a JSON body of the form `{ "commonProps": {...}, "events": [...] }`
//! and natively supports batching. This is the method the Field Guide
//! recommends for standalone clients (as opposed to the browser `_tkq` pixel or
//! the PHP/native libraries), and it lets the CLI drain its whole local queue
//! in a single request per flush.

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

/// Default Tracks ingestion endpoint.
const DEFAULT_ENDPOINT: &str = "https://public-api.wordpress.com/rest/v1.1/tracks/record";

/// Env override for the endpoint (used by tests / local development to avoid
/// sending to the real analytics pipeline).
pub(crate) const ENDPOINT_ENV: &str = "DAYONE_TRACKS_ENDPOINT";

/// Per-request timeout. Analytics must never noticeably delay a command.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

fn endpoint_override() -> Option<String> {
    std::env::var(ENDPOINT_ENV)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Whether local development explicitly selected a Tracks endpoint.
pub(crate) fn endpoint_overridden() -> bool {
    endpoint_override().is_some()
}

/// Resolve the ingestion endpoint, honouring the env override.
pub(crate) fn endpoint_url() -> String {
    endpoint_override().unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned())
}

/// Validate a Tracks event or property name against `^[a-z_][a-z0-9_]*$`.
///
/// Matches the naming convention enforced by the Tracks ETL (names that fail
/// land in `tracks_rejects`), so we drop them before sending.
pub(crate) fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// Sends a batch of events to Tracks. Abstracted so the flush loop can be
/// unit-tested without touching the network.
pub(crate) trait EventSender {
    /// `body` is the full `{ "commonProps": {...}, "events": [...] }` payload.
    async fn send(&self, body: &Value) -> Result<()>;
}

/// Real sender: `POST` the batch to the Tracks REST endpoint.
pub(crate) struct TracksSender {
    client: reqwest::Client,
    url: String,
}

impl TracksSender {
    pub(crate) fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(format!("dayone-cli/{}", env!("CARGO_PKG_VERSION")))
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("failed to initialize analytics HTTP client")?;
        Ok(Self {
            client,
            url: endpoint_url(),
        })
    }
}

impl EventSender for TracksSender {
    async fn send(&self, body: &Value) -> Result<()> {
        let response = self
            .client
            .post(&self.url)
            .json(body)
            .send()
            .await
            .context("failed to send analytics batch request")?;
        // Treat any 2xx as accepted; any other status is a transient failure
        // worth retrying on the next flush.
        if response.status().is_success() {
            Ok(())
        } else {
            anyhow::bail!("analytics endpoint returned status {}", response.status());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::with_env;

    #[test]
    fn valid_names_match_tracks_pattern() {
        assert!(is_valid_name("dayone_cli_entry_create"));
        assert!(is_valid_name("_en"));
        assert!(is_valid_name("command"));
        assert!(is_valid_name("is_signed_in"));
    }

    #[test]
    fn invalid_names_are_rejected() {
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("1leading_digit"));
        assert!(!is_valid_name("Has_Uppercase"));
        assert!(!is_valid_name("has-hyphen"));
        assert!(!is_valid_name("has space"));
        assert!(!is_valid_name("dot.name"));
        // The old alias event/property names are no longer accepted verbatim:
        // CLI telemetry never emits `_aliasUser`.
        assert!(!is_valid_name("_aliasUser"));
        assert!(!is_valid_name("anonId"));
    }

    #[test]
    fn endpoint_url_honours_override() {
        with_env(
            &[(
                ENDPOINT_ENV,
                Some("https://example.test/rest/v1.1/tracks/record"),
            )],
            || {
                assert!(endpoint_overridden());
                assert_eq!(
                    endpoint_url(),
                    "https://example.test/rest/v1.1/tracks/record"
                );
            },
        );
        for value in [None, Some("  ")] {
            with_env(&[(ENDPOINT_ENV, value)], || {
                assert!(!endpoint_overridden());
                assert_eq!(endpoint_url(), DEFAULT_ENDPOINT);
            });
        }
    }
}
