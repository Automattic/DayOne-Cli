use std::io::{self, Write};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;

use crate::commands::{auth_key_set, auth_login};
use crate::constants::{PRODUCTION_URL, STAGING_URL};
use crate::store::sqlite::Store;

#[derive(Debug, Clone)]
pub struct AuthSetupArgs {}

#[derive(Debug, Serialize)]
pub struct AuthSetupOutput {
    pub ok: bool,
    pub profile: String,
    pub profile_id: i64,
    pub base_url: String,
    pub user: serde_json::Value,
    pub notice: String,
}

pub async fn execute(
    store: &Store,
    default_base_url: &str,
    _args: AuthSetupArgs,
) -> Result<AuthSetupOutput> {
    let email = prompt_email()?;
    let base_url = resolve_setup_base_url(store, default_base_url, &email)?;
    let login = auth_login::execute(
        store,
        &base_url,
        auth_login::AuthLoginArgs {
            email,
            password_stdin: false,
        },
    )
    .await?;

    let session = store
        .get_auth_session_for_base_url(&base_url)?
        .ok_or_else(|| anyhow!("setup completed login but no auth session was found"))?;

    let notice = if prompt_set_encryption_key_now()? {
        let key_set = auth_key_set::execute(
            store,
            &base_url,
            auth_key_set::AuthKeySetArgs {
                key: None,
                key_stdin: false,
            },
        )?;
        key_set.notice
    } else {
        "encryption key setup was skipped; run `dayone auth key-set` when ready".to_owned()
    };

    Ok(AuthSetupOutput {
        ok: true,
        profile: login.profile,
        profile_id: session.profile_id,
        base_url: login.base_url,
        user: login.user,
        notice,
    })
}

fn prompt_email() -> Result<String> {
    let mut stdout = io::stdout();
    stdout
        .write_all(b"Email: ")
        .context("failed to write email prompt")?;
    stdout.flush().context("failed to flush email prompt")?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read email from prompt")?;

    let email = input.trim().to_owned();
    if email.is_empty() {
        bail!("email cannot be empty");
    }
    Ok(email)
}

fn resolve_setup_base_url(store: &Store, default_base_url: &str, email: &str) -> Result<String> {
    if !should_prompt_profile_selection(email) {
        return Ok(default_base_url.to_owned());
    }

    match prompt_profile_selection()? {
        ProfileSelection::Named(profile) => resolve_named_profile_base_url(store, profile),
        ProfileSelection::CustomUrl(custom_url) => {
            let profile = store
                .get_or_create_profile_for_base_url(&custom_url)
                .context("failed to resolve profile for custom API host")?;
            let active = store
                .set_active_profile(&profile.name, None)
                .context("failed to set custom profile as active")?;
            Ok(active.base_url)
        }
    }
}

fn resolve_named_profile_base_url(store: &Store, profile: &str) -> Result<String> {
    let base_url = match profile {
        "staging" => STAGING_URL,
        "production" => PRODUCTION_URL,
        _ => bail!("unsupported setup profile: {profile}"),
    };

    let active = store
        .set_active_profile(profile, Some(base_url))
        .context("failed to set selected profile as active")?;
    Ok(active.base_url)
}

fn is_automattic_email(email: &str) -> bool {
    email
        .trim()
        .to_ascii_lowercase()
        .ends_with("@automattic.com")
}

fn is_dev_override_email(email: &str) -> bool {
    email.trim().eq_ignore_ascii_case("dev@example.com")
}

fn should_prompt_profile_selection(email: &str) -> bool {
    is_automattic_email(email) || is_dev_override_email(email)
}

enum ProfileSelection {
    Named(&'static str),
    CustomUrl(String),
}

fn prompt_profile_selection() -> Result<ProfileSelection> {
    let mut stdout = io::stdout();
    loop {
        stdout
            .write_all(b"Select profile (staging/production/custom): ")
            .context("failed to write profile selection prompt")?;
        stdout
            .flush()
            .context("failed to flush profile selection prompt")?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("failed to read profile selection from prompt")?;

        match input.trim().to_ascii_lowercase().as_str() {
            "staging" => return Ok(ProfileSelection::Named("staging")),
            "production" => return Ok(ProfileSelection::Named("production")),
            "custom" => return Ok(ProfileSelection::CustomUrl(prompt_custom_api_host()?)),
            _ => {
                stdout
                    .write_all(b"Please enter 'staging', 'production', or 'custom'.\n")
                    .context("failed to write profile selection hint")?;
                stdout
                    .flush()
                    .context("failed to flush profile selection hint")?;
            }
        }
    }
}

fn prompt_custom_api_host() -> Result<String> {
    let mut stdout = io::stdout();
    stdout
        .write_all(b"Custom API host URL: ")
        .context("failed to write custom API host prompt")?;
    stdout
        .flush()
        .context("failed to flush custom API host prompt")?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read custom API host URL from prompt")?;

    let value = input.trim().to_owned();
    if value.is_empty() {
        bail!("custom API host URL cannot be empty");
    }
    Ok(value)
}

fn prompt_set_encryption_key_now() -> Result<bool> {
    let mut stdout = io::stdout();
    loop {
        stdout
            .write_all(b"Add your current encryption key now? [y/N]: ")
            .context("failed to write encryption key setup prompt")?;
        stdout
            .flush()
            .context("failed to flush encryption key setup prompt")?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("failed to read encryption key setup choice")?;

        if let Some(choice) = parse_yes_no(&input) {
            return Ok(choice);
        }

        stdout
            .write_all(b"Please enter 'y' or 'n'.\n")
            .context("failed to write yes/no hint")?;
        stdout.flush().context("failed to flush yes/no hint")?;
    }
}

fn parse_yes_no(input: &str) -> Option<bool> {
    match input.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "" | "n" | "no" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::sqlite::Store;
    use tempfile::TempDir;

    fn test_dir(label: &str) -> TempDir {
        tempfile::Builder::new()
            .prefix(&format!("dayone-cli-auth-setup-{label}-"))
            .tempdir()
            .expect("should create temp dir")
    }

    #[test]
    fn automattic_email_detection_is_case_insensitive() {
        assert!(should_prompt_profile_selection("person@automattic.com"));
        assert!(should_prompt_profile_selection("PERSON@AUTOMATTIC.COM"));
        assert!(should_prompt_profile_selection(" person@automattic.com "));
    }

    #[test]
    fn profile_selection_prompt_supports_dev_override_email() {
        assert!(should_prompt_profile_selection("dev@example.com"));
        assert!(should_prompt_profile_selection("DEV@example.com"));
    }

    #[test]
    fn profile_selection_prompt_rejects_other_domains() {
        assert!(!should_prompt_profile_selection("person@example.com"));
        assert!(!should_prompt_profile_selection("person@notautomattic.com"));
    }

    #[test]
    fn parse_yes_no_accepts_expected_values() {
        assert_eq!(parse_yes_no("y"), Some(true));
        assert_eq!(parse_yes_no("YES"), Some(true));
        assert_eq!(parse_yes_no(""), Some(false));
        assert_eq!(parse_yes_no("n"), Some(false));
        assert_eq!(parse_yes_no("No"), Some(false));
    }

    #[test]
    fn parse_yes_no_rejects_invalid_values() {
        assert_eq!(parse_yes_no("maybe"), None);
    }

    #[test]
    fn named_profile_selection_creates_missing_profile_row() {
        let dir = test_dir("named-profile-selection");
        let store = Store::open_for_profile(dir.path(), "production", PRODUCTION_URL)
            .expect("store should open");

        let base_url = resolve_named_profile_base_url(&store, "staging")
            .expect("named profile resolution should succeed");

        assert_eq!(base_url, STAGING_URL);

        let profiles = store.list_profiles().expect("list_profiles should succeed");
        assert_eq!(profiles.len(), 2, "staging row should be created on demand");

        let active = store
            .get_active_profile()
            .expect("get_active_profile should succeed")
            .expect("an active profile should exist");
        assert_eq!(active.name, "staging");
        assert_eq!(active.base_url, STAGING_URL);
    }
}
