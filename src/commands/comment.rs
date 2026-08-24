use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::comment_feed::{CommentPageSyncError, CommentPageSyncRequest, sync_comment_pages};
use crate::http::DayOneApiClient;
use crate::http::client::DayOneClient;
use crate::store::sqlite::{AuthSession, ListPageRequest, Store};
use crate::store::user_id_from_session_json;
use crate::util::{encode_path_segment, now_rfc3339_utc};

#[derive(Debug, Clone, Default)]
pub struct CommentPaginationInput {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CommentListArgs {
    pub journal_id: String,
    pub entry_id: String,
    pub pagination: CommentPaginationInput,
    pub refresh: bool,
}

#[derive(Debug, Clone)]
pub struct CommentWriteArgs {
    pub journal_id: String,
    pub entry_id: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct CommentUpdateArgs {
    pub journal_id: String,
    pub entry_id: String,
    pub comment_id: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct CommentDeleteArgs {
    pub journal_id: String,
    pub entry_id: String,
    pub comment_id: String,
}

#[derive(Debug, Clone)]
pub struct CommentReactionArgs {
    pub journal_id: String,
    pub entry_id: String,
    pub comment_id: String,
    pub reaction: String,
}

#[derive(Debug, Clone)]
pub struct CommentUnreactArgs {
    pub journal_id: String,
    pub entry_id: String,
    pub comment_id: String,
}

#[derive(Debug, Serialize)]
pub struct CommentListOutput {
    pub ok: bool,
    pub count: usize,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub items: Vec<Value>,
}

#[derive(Debug, Serialize)]
pub struct CommentMutationOutput {
    pub ok: bool,
    pub base_url: String,
    pub profile_id: i64,
    pub operation: String,
    pub queued: bool,
    pub synced: bool,
    pub outbox_id: Option<String>,
    pub item: Value,
}

#[derive(Debug, Serialize)]
pub struct CommentReactionOutput {
    pub ok: bool,
    pub base_url: String,
    pub profile_id: i64,
    pub operation: String,
    pub reaction: Option<Value>,
}

pub async fn list(
    store: &Store,
    base_url: &str,
    args: CommentListArgs,
) -> Result<CommentListOutput> {
    let session = auth_session(store, base_url)?;
    if args.refresh {
        refresh_comments_for_entry(
            store,
            base_url,
            &session.token,
            &args.journal_id,
            &args.entry_id,
        )
        .await?;
    }

    let page = ListPageRequest {
        limit: args.pagination.limit,
        offset: args.pagination.offset,
        cursor: args.pagination.cursor.clone(),
    };
    let rows =
        store.list_comments_json_rows_by_entry_paged(&args.journal_id, &args.entry_id, &page)?;
    let mut parsed_comments: Vec<Value> = Vec::with_capacity(rows.rows.len());
    let mut comment_ids: Vec<String> = Vec::with_capacity(rows.rows.len());
    for row in &rows.rows {
        let value: Value =
            serde_json::from_str(row).context("failed to parse stored comment JSON row")?;
        if let Some(comment_id) = value.get("id").and_then(Value::as_str) {
            comment_ids.push(comment_id.to_owned());
        }
        parsed_comments.push(value);
    }
    let mut reactions_by_comment: HashMap<String, Vec<Value>> = HashMap::new();
    for (comment_id, reaction_json) in store.list_comment_reactions_json_rows_by_comment_ids(
        &args.journal_id,
        &args.entry_id,
        &comment_ids,
    )? {
        let parsed = serde_json::from_str::<Value>(&reaction_json)
            .context("failed to parse stored reaction JSON row")?;
        reactions_by_comment
            .entry(comment_id)
            .or_default()
            .push(parsed);
    }
    let mut items = Vec::with_capacity(parsed_comments.len());
    for mut value in parsed_comments {
        if let Some(comment_id) = value.get("id").and_then(Value::as_str) {
            let comment_id = comment_id.to_owned();
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "reactions".to_owned(),
                    Value::Array(reactions_by_comment.remove(&comment_id).unwrap_or_default()),
                );
            }
        }
        items.push(value);
    }

    Ok(CommentListOutput {
        ok: true,
        count: items.len(),
        limit: args.pagination.limit,
        offset: args.pagination.offset,
        next_cursor: rows.next_cursor,
        has_more: rows.has_more,
        items,
    })
}

pub async fn write(
    store: &Store,
    base_url: &str,
    args: CommentWriteArgs,
) -> Result<CommentMutationOutput> {
    let session = auth_session(store, base_url)?;
    let body = args.body.trim();
    if body.is_empty() {
        bail!("comment body must not be empty");
    }
    validate_write_target_exists(store, &args.journal_id, &args.entry_id)?;
    let comment_id = local_comment_id();
    let now = now_rfc3339_utc();
    let author_id = required_session_user_id(&session)?;
    let comment = json!({
        "id": comment_id,
        "journal_id": args.journal_id,
        "entry_id": args.entry_id,
        "author_id": author_id,
        "content": body,
        "created_at": now,
        "updated_at": "",
        "deleted_at": ""
    });
    let outbox_id = enqueue_comment_outbox(store, "create", &comment)?;
    store.upsert_comment_json_row(&comment.to_string())?;
    Ok(CommentMutationOutput {
        ok: true,
        base_url: base_url.to_owned(),
        profile_id: session.profile_id,
        operation: "write".to_owned(),
        queued: true,
        synced: false,
        outbox_id: Some(outbox_id),
        item: comment,
    })
}

pub async fn update(
    store: &Store,
    base_url: &str,
    args: CommentUpdateArgs,
) -> Result<CommentMutationOutput> {
    let session = auth_session(store, base_url)?;
    let body = args.body.trim();
    if body.is_empty() {
        bail!("comment body must not be empty");
    }
    let existing = store
        .get_comment_json_row_by_id(&args.comment_id)?
        .ok_or_else(|| {
            anyhow!(
                "comment '{}' not found locally; run `dayone comment list --refresh` first",
                args.comment_id
            )
        })?;
    let mut comment: Value =
        serde_json::from_str(&existing).context("failed to parse local comment JSON")?;
    let (stored_journal_id, stored_entry_id) =
        validate_comment_location(&comment, &args.comment_id, &args.journal_id, &args.entry_id)?;
    let now = now_rfc3339_utc();
    if let Some(object) = comment.as_object_mut() {
        object.insert("journal_id".to_owned(), Value::String(stored_journal_id));
        object.insert("entry_id".to_owned(), Value::String(stored_entry_id));
        object.insert("content".to_owned(), Value::String(body.to_owned()));
        object.insert("updated_at".to_owned(), Value::String(now));
    }
    let outbox_id = enqueue_comment_outbox(store, "update", &comment)?;
    store.upsert_comment_json_row(&comment.to_string())?;
    Ok(CommentMutationOutput {
        ok: true,
        base_url: base_url.to_owned(),
        profile_id: session.profile_id,
        operation: "update".to_owned(),
        queued: true,
        synced: false,
        outbox_id: Some(outbox_id),
        item: comment,
    })
}

pub async fn delete(
    store: &Store,
    base_url: &str,
    args: CommentDeleteArgs,
) -> Result<CommentMutationOutput> {
    let session = auth_session(store, base_url)?;
    let existing = store
        .get_comment_json_row_by_id(&args.comment_id)?
        .ok_or_else(|| {
            anyhow!(
                "comment '{}' not found locally; run `dayone comment list --refresh` first",
                args.comment_id
            )
        })?;
    let mut comment: Value =
        serde_json::from_str(&existing).context("failed to parse local comment JSON")?;
    let (stored_journal_id, stored_entry_id) =
        validate_comment_location(&comment, &args.comment_id, &args.journal_id, &args.entry_id)?;
    let now = now_rfc3339_utc();
    if let Some(object) = comment.as_object_mut() {
        object.insert("journal_id".to_owned(), Value::String(stored_journal_id));
        object.insert("entry_id".to_owned(), Value::String(stored_entry_id));
        object.insert("deleted_at".to_owned(), Value::String(now));
    }
    let outbox_id = enqueue_comment_outbox(store, "delete", &comment)?;
    store.upsert_comment_json_row(&comment.to_string())?;
    Ok(CommentMutationOutput {
        ok: true,
        base_url: base_url.to_owned(),
        profile_id: session.profile_id,
        operation: "delete".to_owned(),
        queued: true,
        synced: false,
        outbox_id: Some(outbox_id),
        item: comment,
    })
}

pub async fn react(
    store: &Store,
    base_url: &str,
    args: CommentReactionArgs,
) -> Result<CommentReactionOutput> {
    let session = auth_session(store, base_url)?;
    let user_id = required_session_user_id(&session)?;
    ensure_shared_journal(store, &args.journal_id)?;
    let client = DayOneClient::new(base_url)?.with_bearer_token(&session.token)?;
    let path = format!(
        "/shares/{}/entries/{}/comments/{}/reaction",
        encode_path_segment(&args.journal_id),
        encode_path_segment(&args.entry_id),
        encode_path_segment(&args.comment_id)
    );
    let response = client
        .put_json(&path, &json!({ "reaction": args.reaction }))
        .await?;
    let normalized = normalize_reaction_payload(
        response,
        &args.journal_id,
        &args.entry_id,
        &args.comment_id,
        Some(user_id),
    );
    if let Some(value) = normalized.as_ref() {
        store.upsert_comment_reaction_json_row(&value.to_string())?;
    }
    Ok(CommentReactionOutput {
        ok: true,
        base_url: base_url.to_owned(),
        profile_id: session.profile_id,
        operation: "react".to_owned(),
        reaction: normalized,
    })
}

pub async fn unreact(
    store: &Store,
    base_url: &str,
    args: CommentUnreactArgs,
) -> Result<CommentReactionOutput> {
    let session = auth_session(store, base_url)?;
    let user_id = required_session_user_id(&session)?;
    ensure_shared_journal(store, &args.journal_id)?;
    let client = DayOneClient::new(base_url)?.with_bearer_token(&session.token)?;
    let path = format!(
        "/shares/{}/entries/{}/comments/{}/reaction",
        encode_path_segment(&args.journal_id),
        encode_path_segment(&args.entry_id),
        encode_path_segment(&args.comment_id)
    );
    let response = client.delete_json(&path, &[]).await?;
    store.delete_comment_reaction_by_user(
        &args.journal_id,
        &args.entry_id,
        &args.comment_id,
        &user_id,
    )?;
    Ok(CommentReactionOutput {
        ok: true,
        base_url: base_url.to_owned(),
        profile_id: session.profile_id,
        operation: "unreact".to_owned(),
        reaction: if response.is_null() {
            None
        } else {
            Some(response)
        },
    })
}

fn local_comment_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("local:{now}")
}

fn enqueue_comment_outbox(store: &Store, operation: &str, comment: &Value) -> Result<String> {
    let comment_id = comment
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("comment payload missing id"))?;
    let journal_id = comment
        .get("journal_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("comment payload missing journal_id"))?;
    let entry_id = comment
        .get("entry_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("comment payload missing entry_id"))?;
    let outbox_id = format!("comment:{operation}:{journal_id}:{entry_id}:{comment_id}");
    let object_id = format!("{journal_id}:{entry_id}:{comment_id}");
    store.enqueue_outbox_item(
        &outbox_id,
        "comment",
        operation,
        &object_id,
        &comment.to_string(),
        now_epoch_ms(),
    )?;
    Ok(outbox_id)
}

fn ensure_shared_journal(store: &Store, journal_id: &str) -> Result<()> {
    let row = store.get_json_row_by_id("journals", journal_id)?;
    let Some(row) = row else {
        bail!("journal '{journal_id}' not found locally; run `dayone sync` first");
    };
    let journal: Value =
        serde_json::from_str(&row).context("failed to parse local journal JSON")?;
    let is_shared = journal
        .get("is_shared")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !is_shared {
        bail!("comment reactions are currently supported only for shared journals");
    }
    Ok(())
}

fn normalize_reaction_payload(
    response: Value,
    journal_id: &str,
    entry_id: &str,
    comment_id: &str,
    fallback_user_id: Option<String>,
) -> Option<Value> {
    let mut reaction = response.as_object()?.clone();
    reaction
        .entry("journal_id".to_owned())
        .or_insert_with(|| Value::String(journal_id.to_owned()));
    reaction
        .entry("entry_id".to_owned())
        .or_insert_with(|| Value::String(entry_id.to_owned()));
    reaction
        .entry("comment_id".to_owned())
        .or_insert_with(|| Value::String(comment_id.to_owned()));
    if !reaction.contains_key("user_id")
        && let Some(user_id) = fallback_user_id
    {
        reaction.insert("user_id".to_owned(), Value::String(user_id));
    }
    Some(Value::Object(reaction))
}

fn validate_comment_location(
    comment: &Value,
    comment_id: &str,
    expected_journal_id: &str,
    expected_entry_id: &str,
) -> Result<(String, String)> {
    let stored_journal_id = comment
        .get("journal_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("comment '{comment_id}' is missing journal_id locally"))?
        .to_owned();
    let stored_entry_id = comment
        .get("entry_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("comment '{comment_id}' is missing entry_id locally"))?
        .to_owned();
    if stored_journal_id != expected_journal_id || stored_entry_id != expected_entry_id {
        bail!(
            "comment '{}' belongs to journal '{}' entry '{}' locally, not journal '{}' entry '{}'",
            comment_id,
            stored_journal_id,
            stored_entry_id,
            expected_journal_id,
            expected_entry_id
        );
    }
    Ok((stored_journal_id, stored_entry_id))
}

fn now_epoch_ms() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    now.as_millis() as i64
}

fn auth_session(store: &Store, base_url: &str) -> Result<AuthSession> {
    Ok(store
        .get_auth_session_for_base_url(base_url)?
        .ok_or_else(crate::telemetry::UserError::auth_required)?)
}

fn required_session_user_id(session: &AuthSession) -> Result<String> {
    Ok(user_id_from_session_json(&session.user_json)?.ok_or(
        crate::store::StoreError::InvalidAuthSessionUserId {
            profile_id: session.profile_id,
        },
    )?)
}

async fn refresh_comments_for_entry(
    store: &Store,
    base_url: &str,
    token: &str,
    journal_id: &str,
    entry_id: &str,
) -> Result<()> {
    let client = DayOneClient::new(base_url)?.with_bearer_token(token)?;
    let journal_json = store
        .get_json_row_by_id("journals", journal_id)?
        .ok_or_else(|| {
            anyhow!("journal '{journal_id}' not found locally; run `dayone sync` first")
        })?;
    let journal: Value =
        serde_json::from_str(&journal_json).context("failed to parse local journal JSON")?;
    let cursor_key = Store::comments_cursor_key(journal_id, entry_id);
    let initial_cursor = store.get_sync_cursor(&cursor_key)?;
    let api_path = format!(
        "/journals/{}/entries/{}/comments",
        encode_path_segment(journal_id),
        encode_path_segment(entry_id)
    );
    let sync = sync_comment_pages(
        CommentPageSyncRequest {
            store,
            journal: &journal,
            journal_id,
            entry_id,
            cursor_key: &cursor_key,
            initial_cursor,
            context_label: "comment refresh",
        },
        |cursor| async {
            let mut query = Vec::new();
            if let Some(current_cursor) = cursor {
                query.push(("cursor", current_cursor));
            }
            client.get_json(&api_path, &query).await
        },
    )
    .await;
    if let Err(err) = sync {
        return match err {
            CommentPageSyncError::Pagination(reason) => Err(anyhow!(reason)),
            CommentPageSyncError::Other(source) => Err(source),
        };
    }
    Ok(())
}

fn validate_write_target_exists(store: &Store, journal_id: &str, entry_id: &str) -> Result<()> {
    store
        .get_json_row_by_id("journals", journal_id)?
        .ok_or_else(|| anyhow!("journal missing locally: {journal_id}"))?;
    let entry_journal_id = store
        .get_entry_journal_id(entry_id)?
        .ok_or_else(|| anyhow!("entry missing locally: {entry_id}"))?;
    let entry_journal_id = entry_journal_id.trim();
    if entry_journal_id.is_empty() {
        bail!("entry '{entry_id}' is missing journal id locally");
    }
    if entry_journal_id != journal_id {
        bail!("entry '{entry_id}' does not belong to journal '{journal_id}'");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::normalize_base_url;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEST_DB_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn setup_store() -> Result<Store> {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
            + TEST_DB_COUNTER.fetch_add(1, Ordering::Relaxed) as u128;
        let db_path = std::env::temp_dir().join(format!(
            "dayone-comment-cmd-{}-{unique}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db_path);
        let store = Store::open_at(&db_path)?;
        let base_url = normalize_base_url("https://api.dayone.test")?;
        let profile = store.get_or_create_profile_for_base_url(&base_url)?;
        store.save_auth_session(
            profile.id,
            "token-1",
            "2026-04-21T00:00:00.000Z",
            r#"{"id":"user-1"}"#,
        )?;
        store.upsert_json_row(
            "journals",
            "journal-1",
            Some("2026-04-21T00:00:00.000Z"),
            None,
            r#"{"id":"journal-1","name":"Test Journal"}"#,
        )?;
        store.upsert_entry_json_row(
            "entry-1",
            "journal-1",
            Some("2026-04-21T00:00:00.000Z"),
            None,
            r#"{"id":"entry-1","body":"entry body"}"#,
        )?;
        Ok(store)
    }

    #[test]
    fn required_session_user_id_rejects_a_missing_id() {
        let session = AuthSession {
            profile_id: 42,
            token: "token".to_owned(),
            token_created_at: "2026-04-21T00:00:00.000Z".to_owned(),
            user_json: "{}".to_owned(),
        };

        assert!(required_session_user_id(&session).is_err());
    }

    #[tokio::test]
    async fn write_stages_comment_and_queues_outbox() -> Result<()> {
        let store = setup_store()?;
        let output = write(
            &store,
            "https://api.dayone.test",
            CommentWriteArgs {
                journal_id: "journal-1".to_owned(),
                entry_id: "entry-1".to_owned(),
                body: "Hello world".to_owned(),
            },
        )
        .await?;
        assert!(output.queued);
        let id = output
            .item
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("comment id should exist"))?;
        let stored = store
            .get_comment_json_row_by_id(id)?
            .ok_or_else(|| anyhow!("comment should exist"))?;
        let stored_value: Value = serde_json::from_str(&stored)?;
        assert_eq!(
            stored_value.get("content").and_then(Value::as_str),
            Some("Hello world")
        );
        let leased = store.lease_outbox_items(now_epoch_ms(), 10)?;
        assert_eq!(leased.len(), 1);
        assert_eq!(leased[0].resource, "comment");
        assert_eq!(leased[0].operation, "create");
        Ok(())
    }

    #[tokio::test]
    async fn list_includes_cached_reactions() -> Result<()> {
        let store = setup_store()?;
        store.upsert_comment_json_row(
            &json!({
                "id": "comment-1",
                "journal_id": "journal-1",
                "entry_id": "entry-1",
                "created_at": "2026-04-21T00:00:00.000Z",
                "content": "hi"
            })
            .to_string(),
        )?;
        store.upsert_comment_reaction_json_row(
            &json!({
                "id": "reaction-1",
                "journal_id": "journal-1",
                "entry_id": "entry-1",
                "comment_id": "comment-1",
                "user_id": "user-1",
                "reaction": "like"
            })
            .to_string(),
        )?;
        let output = list(
            &store,
            "https://api.dayone.test",
            CommentListArgs {
                journal_id: "journal-1".to_owned(),
                entry_id: "entry-1".to_owned(),
                pagination: CommentPaginationInput::default(),
                refresh: false,
            },
        )
        .await?;
        assert_eq!(output.count, 1);
        let reactions = output.items[0]
            .get("reactions")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("reactions should be present"))?;
        assert_eq!(reactions.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn update_rejects_journal_or_entry_mismatch() -> Result<()> {
        let store = setup_store()?;
        store.upsert_comment_json_row(
            &json!({
                "id": "comment-1",
                "journal_id": "journal-a",
                "entry_id": "entry-a",
                "content": "hello"
            })
            .to_string(),
        )?;
        let err = update(
            &store,
            "https://api.dayone.test",
            CommentUpdateArgs {
                journal_id: "journal-b".to_owned(),
                entry_id: "entry-a".to_owned(),
                comment_id: "comment-1".to_owned(),
                body: "updated".to_owned(),
            },
        )
        .await
        .err()
        .ok_or_else(|| anyhow!("mismatched update should fail"))?;
        assert!(
            err.to_string().contains("belongs to journal"),
            "unexpected error: {err}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn delete_rejects_journal_or_entry_mismatch() -> Result<()> {
        let store = setup_store()?;
        store.upsert_comment_json_row(
            &json!({
                "id": "comment-2",
                "journal_id": "journal-a",
                "entry_id": "entry-a",
                "content": "hello"
            })
            .to_string(),
        )?;
        let err = delete(
            &store,
            "https://api.dayone.test",
            CommentDeleteArgs {
                journal_id: "journal-a".to_owned(),
                entry_id: "entry-b".to_owned(),
                comment_id: "comment-2".to_owned(),
            },
        )
        .await
        .err()
        .ok_or_else(|| anyhow!("mismatched delete should fail"))?;
        assert!(
            err.to_string().contains("belongs to journal"),
            "unexpected error: {err}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn write_rejects_missing_local_journal_or_entry() -> Result<()> {
        let store = setup_store()?;
        let missing_journal = write(
            &store,
            "https://api.dayone.test",
            CommentWriteArgs {
                journal_id: "journal-missing".to_owned(),
                entry_id: "entry-1".to_owned(),
                body: "Hello world".to_owned(),
            },
        )
        .await
        .err()
        .ok_or_else(|| anyhow!("missing journal should fail"))?;
        assert!(
            missing_journal
                .to_string()
                .contains("journal missing locally"),
            "unexpected error: {missing_journal}"
        );

        let missing_entry = write(
            &store,
            "https://api.dayone.test",
            CommentWriteArgs {
                journal_id: "journal-1".to_owned(),
                entry_id: "entry-missing".to_owned(),
                body: "Hello world".to_owned(),
            },
        )
        .await
        .err()
        .ok_or_else(|| anyhow!("missing entry should fail"))?;
        assert!(
            missing_entry.to_string().contains("entry missing locally"),
            "unexpected error: {missing_entry}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn write_rejects_entry_journal_mismatch() -> Result<()> {
        let store = setup_store()?;
        store.upsert_json_row(
            "journals",
            "journal-2",
            Some("2026-04-21T00:00:00.000Z"),
            None,
            r#"{"id":"journal-2","name":"Other Journal"}"#,
        )?;
        let err = write(
            &store,
            "https://api.dayone.test",
            CommentWriteArgs {
                journal_id: "journal-2".to_owned(),
                entry_id: "entry-1".to_owned(),
                body: "Hello world".to_owned(),
            },
        )
        .await
        .err()
        .ok_or_else(|| anyhow!("mismatched journal should fail"))?;
        assert!(
            err.to_string().contains("does not belong to journal"),
            "unexpected error: {err}"
        );
        Ok(())
    }
}
