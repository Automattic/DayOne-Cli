use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use rsa::rand_core::{OsRng, RngCore};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::store::sqlite::Store;
use crate::util::{normalize_yyyy_mm_dd, now_rfc3339_utc};

#[derive(Debug, Clone)]
pub struct DailyChatAddArgs {
    pub message: Option<String>,
    pub message_stdin: bool,
    pub date: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DailyChatAddOutput {
    pub ok: bool,
    pub base_url: String,
    pub profile_id: i64,
    pub date: String,
    pub daily_chat_id: String,
    pub message_id: String,
    pub created_chat: bool,
    pub queued: bool,
    pub synced: bool,
    pub outbox_id: String,
}

pub async fn execute(
    store: &Store,
    base_url: &str,
    args: DailyChatAddArgs,
) -> Result<DailyChatAddOutput> {
    let message = read_message(args.message, args.message_stdin)?;
    let date = resolve_date(args.date.as_deref())?;
    let now_iso = now_rfc3339_utc();
    let session = store
        .get_auth_session_for_base_url(base_url)?
        .ok_or_else(crate::telemetry::UserError::auth_required)?;
    let existing = store.get_daily_chat_row_by_date(&date)?;

    let (row_id, mut chat, created_chat) = if let Some((existing_id, row)) = existing {
        let value: Value = serde_json::from_str(&row)
            .with_context(|| format!("failed to parse local daily chat JSON for date {date}"))?;
        (existing_id, value, false)
    } else {
        (
            date.clone(),
            json!({
                "id": date,
                "date": date,
                "timezone": local_timezone_name(),
                "chat_history": [],
                "context": {},
                "entry_id": Value::Null,
            }),
            true,
        )
    };

    let (daily_chat_id, message_id) =
        append_user_message(&mut chat, &row_id, &date, &message, &now_iso)?;
    let deleted_at = chat.get("deleted_at").and_then(Value::as_str);
    store.upsert_json_row(
        "daily_chat_feed",
        &row_id,
        Some(&now_iso),
        deleted_at,
        &chat.to_string(),
    )?;

    let outbox_id = format!("daily_chat_feed:{date}");
    store.enqueue_outbox_item(
        &outbox_id,
        "daily_chat_feed",
        "update",
        &row_id,
        &chat.to_string(),
        now_epoch_ms(),
    )?;

    Ok(DailyChatAddOutput {
        ok: true,
        base_url: base_url.to_owned(),
        profile_id: session.profile_id,
        date,
        daily_chat_id,
        message_id,
        created_chat,
        queued: true,
        synced: false,
        outbox_id,
    })
}

fn append_user_message(
    chat: &mut Value,
    fallback_chat_id: &str,
    date: &str,
    message: &str,
    now_iso: &str,
) -> Result<(String, String)> {
    let obj = chat
        .as_object_mut()
        .ok_or_else(|| anyhow!("daily chat row must be a JSON object"))?;
    if !obj.contains_key("date")
        || obj
            .get("date")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .is_empty()
    {
        obj.insert("date".to_owned(), Value::String(date.to_owned()));
    }
    let timezone_missing = obj.get("timezone").map(Value::is_null).unwrap_or(true)
        || obj.get("timezone").and_then(Value::as_str).is_none();
    if timezone_missing {
        obj.insert("timezone".to_owned(), Value::String(local_timezone_name()));
    }

    let chat_id = obj
        .get("id")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .unwrap_or(fallback_chat_id)
        .to_owned();
    obj.insert("id".to_owned(), Value::String(chat_id.clone()));
    obj.insert(
        "user_edit_date".to_owned(),
        Value::String(now_iso.to_owned()),
    );
    obj.insert("updated_at".to_owned(), Value::String(now_iso.to_owned()));

    let messages = obj
        .entry("chat_history".to_owned())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| anyhow!("daily chat field 'chat_history' must be an array"))?;
    let message_id = generate_message_id();
    messages.push(json!({
        "id": message_id,
        "role": "user",
        "content": message,
        "timestamp": now_iso,
        "user_edit_date": now_iso,
    }));
    Ok((chat_id, message_id))
}

fn resolve_date(input: Option<&str>) -> Result<String> {
    if let Some(date) = input {
        return normalize_date(date);
    }
    let local_now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    let fmt =
        time::format_description::parse("[year]-[month]-[day]").expect("date format must be valid");
    Ok(local_now
        .format(&fmt)
        .expect("local date formatting should not fail"))
}

fn normalize_date(input: &str) -> Result<String> {
    normalize_yyyy_mm_dd(input).ok_or_else(|| anyhow!("date must be in YYYY-MM-DD format"))
}

fn read_message(message: Option<String>, message_stdin: bool) -> Result<String> {
    if message_stdin && message.is_some() {
        bail!("use either --message or --message-stdin, not both");
    }
    let raw = if let Some(v) = message {
        v
    } else if message_stdin {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("failed reading message from stdin")?;
        buf
    } else {
        bail!("missing message; provide --message or --message-stdin");
    };
    if raw.trim().is_empty() {
        bail!("message cannot be empty");
    }
    Ok(raw)
}

fn generate_message_id() -> String {
    let mut hasher = Sha256::new();
    hasher.update(now_epoch_ms().to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    let mut nonce = [0_u8; 16];
    let mut rng = OsRng;
    rng.fill_bytes(&mut nonce);
    hasher.update(nonce);
    let digest = hasher.finalize();
    let hex = format!("{:x}", digest);
    hex[0..32].to_ascii_uppercase()
}

fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn local_timezone_name() -> String {
    std::env::var("TZ")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "UTC".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_validation_accepts_yyyy_mm_dd() {
        assert_eq!(
            normalize_date("2026-03-17").expect("date should parse"),
            "2026-03-17"
        );
    }

    #[test]
    fn date_validation_rejects_invalid() {
        assert!(normalize_date("2026/03/17").is_err());
        assert!(normalize_date("2026-3-17").is_err());
    }

    #[test]
    fn read_message_rejects_empty() {
        assert!(read_message(Some("   ".to_owned()), false).is_err());
    }

    #[test]
    fn append_user_message_adds_user_role_message() {
        let mut chat = json!({
            "id": "chat-1",
            "date": "2026-03-17",
            "chat_history": []
        });
        let (chat_id, message_id) = append_user_message(
            &mut chat,
            "fallback-id",
            "2026-03-17",
            "hello",
            "2026-03-17T10:00:00.000Z",
        )
        .expect("append should succeed");
        assert_eq!(chat_id, "chat-1");
        assert!(!message_id.is_empty());
        assert_eq!(chat["chat_history"][0]["role"], "user");
        assert_eq!(chat["chat_history"][0]["content"], "hello");
    }
}
