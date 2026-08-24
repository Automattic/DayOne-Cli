use std::io::{self, Read};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::store::sqlite::Store;

#[derive(Debug, Clone)]
pub struct AuthKeySetArgs {
    pub key: Option<String>,
    pub key_stdin: bool,
}

#[derive(Debug, Serialize)]
pub struct AuthKeySetOutput {
    pub ok: bool,
    pub profile_id: i64,
    pub base_url: String,
    pub storage: String,
    pub notice: String,
}

pub fn execute(store: &Store, base_url: &str, args: AuthKeySetArgs) -> Result<AuthKeySetOutput> {
    let session = store
        .get_auth_session_for_base_url(base_url)?
        .ok_or_else(crate::telemetry::UserError::auth_required)?;

    let key = read_key(args)?;
    let storage = store
        .set_encryption_key_for_profile(session.profile_id, &key)
        .context("failed to persist encryption key")?;
    store
        .clear_sync_cursors()
        .context("failed to reset sync cursors after encryption key update")?;

    Ok(AuthKeySetOutput {
        ok: true,
        profile_id: session.profile_id,
        base_url: base_url.to_owned(),
        storage: storage.as_str().to_owned(),
        notice: "sync cursors were reset; run `dayone sync` now to re-sync and apply decryption"
            .to_owned(),
    })
}

fn read_key(args: AuthKeySetArgs) -> Result<String> {
    if let Some(value) = args.key {
        let trimmed = value.trim().to_owned();
        if trimmed.is_empty() {
            bail!("--key cannot be empty");
        }
        return Ok(trimmed);
    }

    if args.key_stdin {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .context("failed reading encryption key from stdin")?;
        let key = input.trim_end_matches(['\n', '\r']).trim().to_owned();
        if key.is_empty() {
            bail!("encryption key from stdin is empty");
        }
        return Ok(key);
    }

    let key = rpassword::prompt_password("Encryption key: ")
        .context("failed to read encryption key from prompt")?;
    let key = key.trim().to_owned();
    if key.is_empty() {
        bail!("encryption key cannot be empty");
    }
    Ok(key)
}
