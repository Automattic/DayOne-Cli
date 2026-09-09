//! Advisory upload checks for entries in relaxed shared journals.

use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::Value;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::http::{ApiError, DayOneApiClient, DeviceInfo};
use crate::store::sqlite::Store;
use crate::sync::phases::outbox::OutboxProcessError;
use crate::sync::phases::pull::feature_enabled;
#[derive(Deserialize)]
struct Lease {
    holder_user_id: String,
    holder_device_id: String,
    expires_at: String,
}

fn parse_status(response: Value) -> Result<Option<Lease>> {
    let invalid = || anyhow!("entry_edit_lock_status_invalid: invalid lock status response");
    let server_time = response
        .get("server_time")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let server_time = OffsetDateTime::parse(server_time, &Rfc3339).map_err(|_| invalid())?;
    // A missing lease field is not the same as an explicit null lease.
    let lease = response.get("lease").ok_or_else(invalid)?;
    if lease.is_null() {
        return Ok(None);
    }
    let lease: Lease = serde_json::from_value(lease.clone()).map_err(|_| invalid())?;
    let expiry = OffsetDateTime::parse(&lease.expires_at, &Rfc3339).map_err(|_| invalid())?;
    if lease.holder_user_id.trim().is_empty()
        || lease.holder_device_id.trim().is_empty()
        || expiry <= server_time
    {
        return Err(invalid());
    }
    Ok(Some(lease))
}

pub(crate) async fn check_upload<C: DayOneApiClient>(
    store: &Store,
    api: &C,
    profile_id: i64,
    journal_id: &str,
    entry_id: &str,
    journal: &Value,
) -> Result<(), OutboxProcessError> {
    let shared = journal.get("is_shared").is_some_and(|value| {
        value.as_bool() == Some(true) || value.as_i64().is_some_and(|n| n != 0)
    });
    if !shared
        || journal.get("shared_permissions").and_then(Value::as_str) != Some("any_participant_full")
    {
        return Ok(());
    }
    let flags = store
        .get_json_row_by_id("feature_flags", "1")?
        .map(|raw| serde_json::from_str::<Value>(&raw))
        .transpose()?;
    if !flags
        .as_ref()
        .is_some_and(|flags| feature_enabled(flags, &["shared-journals-v2"]))
    {
        return Ok(());
    }

    let response = match api.get_entry_edit_lock(journal_id, entry_id).await {
        Ok(value) => value,
        Err(error) => {
            let http_error = error
                .chain()
                .find_map(|cause| cause.downcast_ref::<ApiError>());
            // Entry PUT is an upsert. A newly created entry has no server lease.
            // Only the documented missing-target response permits this path;
            // an absent endpoint or an HTML proxy response remains a failure.
            if http_error.is_some_and(|error| {
                error.status() == 404
                    && serde_json::from_str::<Value>(error.body())
                        .ok()
                        .as_ref()
                        .is_some_and(|body| {
                            body.get("error").and_then(Value::as_str) == Some("not_found")
                        })
            }) {
                return Ok(());
            }
            let message = match http_error {
                Some(error) => format!(
                    "entry_edit_lock_status_unavailable: HTTP {} while checking entry lock",
                    error.status()
                ),
                None => "entry_edit_lock_status_unavailable: could not check entry lock".to_owned(),
            };
            return Err(anyhow!(message).into());
        }
    };
    let Some(lease) = parse_status(response)? else {
        return Ok(());
    };
    let user_id = store.auth_user_id_for_profile(profile_id)?;
    let device = DeviceInfo::resolve(Some(&user_id));
    if lease.holder_user_id == user_id && lease.holder_device_id == device.id {
        return Ok(());
    }
    Err(OutboxProcessError::EntryEditLocked)
}
