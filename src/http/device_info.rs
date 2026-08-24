//! Builds the `Device-Info` header sent to the Day One API.
//!
//! Mirrors the Day One Web client (`deviceHeaders.ts` / `clientMeta.ts`):
//! the device id is a deterministic fingerprint derived from device
//! characteristics (so the same machine + user always maps to the same
//! device row), and the header uses the same quoted `Key="value"` format.

use md5::{Digest, Md5};

const APP_ID: &str = "com.bloombuilt.dayone-cli";
const PRODUCT_NAME: &str = "dayone-cli";
const DEFAULT_LANGUAGE: &str = "en-US";
const DEFAULT_COUNTRY: &str = "US";

/// Identifying facts about the current device, resolved once per process.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Stable device fingerprint (md5 hex), sent as `Id`.
    pub id: String,
    /// Human-readable model, e.g. `macos aarch64`.
    pub model: String,
    /// Device name (hostname).
    pub name: String,
    pub language: String,
    pub country: String,
}

impl DeviceInfo {
    /// Resolve device info for the active user.
    ///
    /// The `user_id` is folded into the fingerprint (as the web client does)
    /// so a single machine shared by multiple accounts yields a distinct
    /// device id per account.
    pub fn resolve(user_id: Option<&str>) -> Self {
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        let hostname = current_hostname();
        let (language, country) = locale_language_country();

        Self {
            id: fingerprint(os, arch, &hostname, user_id),
            model: format!("{os} {arch}"),
            name: hostname,
            language,
            country,
        }
    }

    /// Render the `Device-Info` header value.
    ///
    /// Values are wrapped in quotes (matching the web client) so the server's
    /// primary `Key="value"` parser handles them directly.
    pub fn header_value(&self) -> String {
        format!(
            "Id=\"{}\"; Model=\"{}\"; Name=\"{}\"; Language=\"{}\"; Country=\"{}\"; platform=\"rust\"; app_id=\"{}\"",
            self.id, self.model, self.name, self.language, self.country, APP_ID,
        )
    }

    pub fn x_user_agent(&self) -> String {
        format!("{PRODUCT_NAME}/{}", env!("CARGO_PKG_VERSION"))
    }
}

/// Deterministic device fingerprint: `md5(os::arch::hostname::userId)`.
fn fingerprint(os: &str, arch: &str, hostname: &str, user_id: Option<&str>) -> String {
    let mut parts = vec![os.to_owned(), arch.to_owned(), hostname.to_owned()];
    if let Some(user_id) = user_id {
        parts.push(user_id.to_owned());
    }
    let joined = parts.join("::");
    format!("{:x}", Md5::digest(joined.as_bytes()))
}

fn current_hostname() -> String {
    let raw = hostname::get()
        .ok()
        .and_then(|os_str| os_str.into_string().ok())
        .map(|h| sanitize_value(&h))
        .filter(|h| !h.is_empty());
    raw.unwrap_or_else(|| PRODUCT_NAME.to_owned())
}

/// Strip characters that would break the header grammar (`"` and `;`) or that
/// are not valid in an HTTP header value (non-printable / non-ASCII), so the
/// resolved info always produces a well-formed, parseable header.
fn sanitize_value(value: &str) -> String {
    value
        .chars()
        .filter(|&c| c.is_ascii_graphic() || c == ' ')
        .filter(|&c| c != '"' && c != ';' && c != '\\')
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Derive an IETF-style language tag and country from the POSIX locale env
/// vars, e.g. `en_US.UTF-8` -> (`en-US`, `US`). Falls back to `en-US`/`US`.
fn locale_language_country() -> (String, String) {
    // Filter per-variable (not after `find_map`) so an empty or `C`/`POSIX`
    // `LC_ALL` is treated as unset and we fall through to `LC_MESSAGES`/`LANG`,
    // matching how a POSIX shell resolves the effective locale.
    let raw = ["LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .filter(|v| !v.is_empty() && v != "C" && v != "POSIX")
    });

    let Some(raw) = raw else {
        return (DEFAULT_LANGUAGE.to_owned(), DEFAULT_COUNTRY.to_owned());
    };

    // Strip encoding (`.UTF-8`) and modifier (`@euro`) suffixes. The locale
    // comes from user-controlled env vars, so sanitize it the same way as the
    // hostname to keep the `Device-Info` header grammar well-formed. `country`
    // is derived from the sanitized `base`, so it inherits the sanitization.
    let base = sanitize_value(
        raw.split(['.', '@'])
            .next()
            .unwrap_or(&raw)
            .replace('_', "-")
            .as_str(),
    );

    if base.is_empty() {
        return (DEFAULT_LANGUAGE.to_owned(), DEFAULT_COUNTRY.to_owned());
    }

    let country = base
        .split('-')
        .nth(1)
        .filter(|c| !c.is_empty())
        .unwrap_or(DEFAULT_COUNTRY)
        .to_owned();

    (base, country)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_deterministic_and_user_specific() {
        let a = fingerprint("macos", "aarch64", "host", Some("user-1"));
        let b = fingerprint("macos", "aarch64", "host", Some("user-1"));
        let c = fingerprint("macos", "aarch64", "host", Some("user-2"));
        let none = fingerprint("macos", "aarch64", "host", None);

        assert_eq!(a, b, "same inputs should hash identically");
        assert_ne!(a, c, "different user should change the fingerprint");
        assert_ne!(a, none, "missing user should change the fingerprint");
        assert_eq!(a.len(), 32, "md5 hex should be 32 chars");
        assert!(a.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn header_uses_quoted_web_format_with_required_keys() {
        let info = DeviceInfo {
            id: "abc123".to_owned(),
            model: "macos aarch64".to_owned(),
            name: "my-machine".to_owned(),
            language: "en-US".to_owned(),
            country: "US".to_owned(),
        };
        let value = info.header_value();
        assert_eq!(
            value,
            "Id=\"abc123\"; Model=\"macos aarch64\"; Name=\"my-machine\"; Language=\"en-US\"; Country=\"US\"; platform=\"rust\"; app_id=\"com.bloombuilt.dayone-cli\""
        );
    }

    #[test]
    fn locale_parsing_handles_posix_locale() {
        use crate::test_util::with_env;

        with_env(&[("LC_ALL", Some("en_US.UTF-8"))], || {
            assert_eq!(
                locale_language_country(),
                ("en-US".to_owned(), "US".to_owned())
            );
        });
        with_env(&[("LC_ALL", Some("fr_CA"))], || {
            assert_eq!(
                locale_language_country(),
                ("fr-CA".to_owned(), "CA".to_owned())
            );
        });
        // Clear LANG/LC_MESSAGES alongside the `C` LC_ALL so the fallback path
        // is exercised deterministically regardless of the ambient env.
        with_env(
            &[("LC_ALL", Some("C")), ("LANG", None), ("LC_MESSAGES", None)],
            || {
                assert_eq!(
                    locale_language_country(),
                    ("en-US".to_owned(), "US".to_owned())
                );
            },
        );
        // An empty `LC_ALL` must be treated as unset and fall through to `LANG`
        // rather than short-circuiting to the default locale.
        with_env(
            &[
                ("LC_ALL", Some("")),
                ("LC_MESSAGES", None),
                ("LANG", Some("fr_CA.UTF-8")),
            ],
            || {
                assert_eq!(
                    locale_language_country(),
                    ("fr-CA".to_owned(), "CA".to_owned())
                );
            },
        );
    }

    #[test]
    fn locale_parsing_sanitizes_user_controlled_values() {
        use crate::test_util::with_env;

        // A malicious locale must not be able to inject header grammar chars.
        with_env(
            &[
                ("LC_ALL", Some("en_US\"; injected=\"x")),
                ("LANG", None),
                ("LC_MESSAGES", None),
            ],
            || {
                let (language, country) = locale_language_country();
                for component in [&language, &country] {
                    assert!(!component.contains('"'));
                    assert!(!component.contains(';'));
                    assert!(!component.contains('\\'));
                }
            },
        );
    }
}
