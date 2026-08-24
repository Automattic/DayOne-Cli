use std::io::{self, Read};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;

use crate::http::client::DayOneClient;
use crate::store::StoreError;
use crate::store::sqlite::Store;

#[derive(Debug, Clone)]
pub struct AuthLoginArgs {
    pub email: String,
    pub password_stdin: bool,
}

#[derive(Debug, Serialize)]
pub struct AuthLoginOutput {
    pub ok: bool,
    pub profile: String,
    pub base_url: String,
    pub user: Value,
}

pub async fn execute(
    store: &Store,
    base_url: &str,
    args: AuthLoginArgs,
) -> Result<AuthLoginOutput> {
    let password = read_password(args.password_stdin)?;
    let client = DayOneClient::new(base_url)?;
    let login = client
        .login_v3(&args.email, &password)
        .await
        .context("authentication failed")?;

    let profile = store
        .resolve_profile_for_auth(client.base_url())
        .context("failed to resolve profile for authentication")?;
    store
        .save_auth_session(
            profile.id,
            &login.token,
            &login.created_at,
            &serde_json::to_string(&login.user).context("failed to serialize user payload")?,
        )
        .context("failed to persist auth session")?;
    if let Some(master_key) = extract_master_key_from_login_payload(&login) {
        let existing_key = match store.get_encryption_key_for_profile(profile.id) {
            Ok(value) => value,
            Err(err) if is_missing_keychain_master_key_error(&err) => None,
            Err(err) => {
                return Err(err).context("failed to load existing encryption key for profile");
            }
        };
        if existing_key.as_deref() != Some(master_key.as_str()) {
            store
                .set_encryption_key_for_profile(profile.id, &master_key)
                .context("failed to persist encryption key from login payload")?;
            store
                .clear_sync_cursors()
                .context("failed to reset sync cursors after encryption key change")?;
        }
    }

    crate::analytics::identify_on_login(&login.user);
    crate::analytics::track(store, crate::analytics::Event::UserSignIn, &[]);

    Ok(AuthLoginOutput {
        ok: true,
        profile: profile.name,
        base_url: client.base_url().to_owned(),
        user: login.user,
    })
}

fn is_missing_keychain_master_key_error(err: &StoreError) -> bool {
    matches!(err, StoreError::MissingKeychainMasterKey { .. })
}

fn extract_master_key_from_login_payload(
    login: &crate::auth::types::LoginV3Response,
) -> Option<String> {
    if let Some(master_key) = login.master_key.as_deref() {
        let trimmed = master_key.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    if let Some(display_key) = login.e2ee_display_key.as_deref() {
        let trimmed = display_key.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    login
        .user
        .get("master_key")
        .or_else(|| login.user.get("masterKey"))
        .or_else(|| login.user.get("e2ee_display_key"))
        .or_else(|| login.user.get("e2eeDisplayKey"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn read_password(from_stdin: bool) -> Result<String> {
    if from_stdin {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .context("failed reading password from stdin")?;
        let password = input.trim_end_matches(['\n', '\r']).to_owned();
        if password.is_empty() {
            bail!("password from stdin is empty");
        }
        return Ok(password);
    }

    let password = rpassword::prompt_password("Password: ")
        .map_err(|_| crate::telemetry::UserError::password_read_failed())?;
    if password.trim().is_empty() {
        bail!("password cannot be empty");
    }
    Ok(password)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::types::LoginV3Response;
    use serde_json::json;

    fn base_login() -> LoginV3Response {
        LoginV3Response {
            token: "tok".to_owned(),
            created_at: "2026-03-31T00:00:00Z".to_owned(),
            user: json!({}),
            master_key: None,
            e2ee_display_key: None,
        }
    }

    #[test]
    fn extracts_top_level_master_key_first() {
        let mut login = base_login();
        login.master_key = Some(" D1-master ".to_owned());
        login.e2ee_display_key = Some("D1-display".to_owned());
        login.user = json!({
            "masterKey": "D1-user-master",
        });
        let extracted = extract_master_key_from_login_payload(&login);
        assert_eq!(extracted.as_deref(), Some("D1-master"));
    }

    #[test]
    fn extracts_display_key_when_master_key_missing() {
        let mut login = base_login();
        login.e2ee_display_key = Some(" D1-display ".to_owned());
        let extracted = extract_master_key_from_login_payload(&login);
        assert_eq!(extracted.as_deref(), Some("D1-display"));
    }

    #[test]
    fn extracts_user_payload_fallback_keys() {
        for payload in [
            json!({ "master_key": "D1-a" }),
            json!({ "masterKey": "D1-b" }),
            json!({ "e2ee_display_key": "D1-c" }),
            json!({ "e2eeDisplayKey": "D1-d" }),
        ] {
            let mut login = base_login();
            login.user = payload;
            let extracted = extract_master_key_from_login_payload(&login);
            assert!(extracted.is_some(), "expected a key for payload variant");
        }
    }

    #[test]
    fn returns_none_when_no_usable_key_exists() {
        let mut login = base_login();
        login.master_key = Some("   ".to_owned());
        login.e2ee_display_key = Some(String::new());
        login.user = json!({
            "masterKey": " ",
        });
        assert!(extract_master_key_from_login_payload(&login).is_none());
    }

    #[test]
    fn detects_missing_keychain_master_key_error_by_type() {
        let err = StoreError::MissingKeychainMasterKey { profile_id: 42 };
        assert!(is_missing_keychain_master_key_error(&err));
    }

    #[test]
    fn ignores_non_keychain_errors_for_missing_key_detection() {
        let err = StoreError::invalid_input("some other error");
        assert!(!is_missing_keychain_master_key_error(&err));
    }
}
