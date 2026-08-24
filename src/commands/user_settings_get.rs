use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::http::client::DayOneClient;
use crate::store::sqlite::Store;

#[derive(Debug, Serialize)]
pub struct UserSettingsGetOutput {
    pub ok: bool,
    pub profile_id: i64,
    pub base_url: String,
    pub data: Value,
}

pub async fn execute(store: &Store, base_url: &str) -> Result<UserSettingsGetOutput> {
    let session = store
        .get_auth_session_for_base_url(base_url)
        .context("failed to load session from local sqlite")?
        .ok_or_else(crate::telemetry::UserError::auth_required)?;

    let client = DayOneClient::new(base_url)?.with_bearer_token(&session.token)?;
    let data = client
        .get_user_settings()
        .await
        .context("failed to fetch /api/user-settings")?;

    Ok(UserSettingsGetOutput {
        ok: true,
        profile_id: session.profile_id,
        base_url: client.base_url().to_owned(),
        data,
    })
}
