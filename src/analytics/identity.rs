//! Analytics identity resolution.
//!
//! CLI telemetry uses a pseudonymous installation identity. Unlike Day One Web (which switches
//! the Tracks identity to `_ut = dayone:user_id` once signed in), every event
//! is attributed to a stable install-level anonymous id (`_ut = anon`) — before
//! *and* after sign-in. The Day One user id is never used as the Tracks
//! identity and never sent. Sign-in state is surfaced only as the
//! low-cardinality `is_signed_in` event property, alongside the subscription
//! tier.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use rsa::rand_core::{OsRng, RngCore};
use serde_json::Value;

/// Tracks user id type (`_ut`). Only the anonymous type is ever emitted; the
/// authenticated (`dayone:user_id`) type is deliberately not used.
pub(crate) const USER_ID_TYPE_ANON: &str = "anon";

/// Mutable identity snapshot held in the global analytics state.
#[derive(Debug, Clone)]
pub(crate) struct Identity {
    /// Whether the current session is signed in. Surfaced only as the
    /// `is_signed_in` event property — never as the Tracks identity.
    pub signed_in: bool,
    /// Stable anonymous id, always present and always used as the identity.
    pub anon_id: String,
    /// Account subscription level, when known (best-effort).
    pub subscription_level: Option<String>,
}

impl Identity {
    pub(crate) fn is_signed_in(&self) -> bool {
        self.signed_in
    }

    /// The `(_ui, _ut)` pair to attach to outgoing events. Always the anonymous
    /// install id: CLI telemetry is never tied to the Day One account.
    pub(crate) fn tracks_pair(&self) -> (String, &'static str) {
        (self.anon_id.clone(), USER_ID_TYPE_ANON)
    }
}

/// Generate a fresh anonymous id: 18 random bytes, base64-encoded (same shape
/// as Day One Web's `newAnonId()`).
pub(crate) fn new_anonymous_id() -> String {
    let mut bytes = [0u8; 18];
    OsRng.fill_bytes(&mut bytes);
    BASE64_STANDARD.encode(bytes)
}

pub(crate) fn is_valid_anonymous_id(value: &str) -> bool {
    BASE64_STANDARD
        .decode(value)
        .is_ok_and(|bytes| bytes.len() == 18 && BASE64_STANDARD.encode(bytes) == value)
}

/// Extract the Day One user id from a stored login/session `user` payload.
///
/// Uses the canonical session parser so `id`, `user_id`, and `userId`
/// resolve consistently across analytics, sync, and local storage.
pub(crate) fn user_id_from_user_value(user: &Value) -> Option<String> {
    crate::store::user_id_from_value(user)
}

/// Best-effort subscription level from a login/session `user` payload. The
/// value is only used as a low-cardinality analytics dimension, so unknown
/// shapes are simply omitted.
pub(crate) fn subscription_level_from_user_value(user: &Value) -> Option<String> {
    [
        "subscription_level",
        "subscription_status",
        "subscription",
        "tier",
    ]
    .iter()
    .find_map(|key| {
        user.get(key)
            .and_then(Value::as_str)
            .and_then(normalize_subscription_level)
    })
    .map(ToOwned::to_owned)
}

pub(crate) fn normalize_subscription_level(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "silver" => Some("Silver"),
        "gold" => Some("Gold"),
        "free" | "free_active_device" | "basic" => Some("Basic"),
        "grandfathered" | "plus" => Some("Plus"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn anon_identity_uses_anon_id_and_type() {
        let identity = Identity {
            signed_in: false,
            anon_id: "anon-123".to_owned(),
            subscription_level: None,
        };
        assert!(!identity.is_signed_in());
        let (ui, ut) = identity.tracks_pair();
        assert_eq!(ui, "anon-123");
        assert_eq!(ut, USER_ID_TYPE_ANON);
    }

    #[test]
    fn signed_in_identity_still_uses_anon_id_and_type() {
        // Even when signed in, the Tracks identity stays anonymous — only the
        // `is_signed_in` flag reflects the session.
        let identity = Identity {
            signed_in: true,
            anon_id: "anon-123".to_owned(),
            subscription_level: Some("Gold".to_owned()),
        };
        assert!(identity.is_signed_in());
        let (ui, ut) = identity.tracks_pair();
        assert_eq!(ui, "anon-123");
        assert_eq!(ut, USER_ID_TYPE_ANON);
    }

    #[test]
    fn user_id_honours_id_and_user_id_keys() {
        assert_eq!(
            user_id_from_user_value(&json!({"id": "abc"})).as_deref(),
            Some("abc")
        );
        assert_eq!(
            user_id_from_user_value(&json!({"id": 42})).as_deref(),
            Some("42")
        );
        assert_eq!(
            user_id_from_user_value(&json!({"id": "", "user_id": "xyz"})).as_deref(),
            Some("xyz")
        );
        assert_eq!(
            user_id_from_user_value(&json!({"id": "", "userId": "  camel-case  "})).as_deref(),
            Some("camel-case")
        );
        assert_eq!(user_id_from_user_value(&json!({"other": "x"})), None);
    }

    #[test]
    fn subscription_levels_match_the_web_tracks_categories() {
        for (raw, expected) in [
            ("silver", "Silver"),
            ("gold", "Gold"),
            ("free", "Basic"),
            ("free_active_device", "Basic"),
            ("grandfathered", "Plus"),
            ("Basic", "Basic"),
            ("Plus", "Plus"),
        ] {
            assert_eq!(normalize_subscription_level(raw), Some(expected));
        }
        assert_eq!(normalize_subscription_level("alice@example.com"), None);
        assert_eq!(normalize_subscription_level("private text"), None);
        assert_eq!(
            subscription_level_from_user_value(&json!({
                "subscription_level": "unknown",
                "subscription_status": "gold"
            })),
            Some("Gold".to_owned())
        );
    }

    #[test]
    fn new_anonymous_id_is_nonempty_and_unique() {
        let a = new_anonymous_id();
        let b = new_anonymous_id();
        assert!(is_valid_anonymous_id(&a));
        assert!(is_valid_anonymous_id(&b));
        assert_ne!(a, b, "anonymous ids should be random");
    }
}
