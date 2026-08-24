use crate::comment_codec::decode_comment_content_for_journal;
use crate::comment_flags::comment_deleted_flag;
use crate::store::sqlite::Store;
use crate::sync::engine::MAX_PAGE_LOOPS;
use anyhow::Result;
use serde_json::Value;
use std::future::Future;

pub(crate) enum CommentPageSyncError {
    Pagination(String),
    Other(anyhow::Error),
}

pub(crate) fn persist_comments_page(
    store: &Store,
    journal: &Value,
    journal_id: &str,
    entry_id: &str,
    comments: &[Value],
) -> Result<()> {
    for raw_comment in comments {
        let mut comment = raw_comment.clone();
        let reactions = comment
            .get("reactions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(content) = comment.get("content").and_then(Value::as_str) {
            let decoded = decode_comment_content_for_journal(journal, content);
            if let Some(object) = comment.as_object_mut() {
                object.insert("content".to_owned(), Value::String(decoded));
            }
        }
        let comment_id = comment
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if let Some(object) = comment.as_object_mut() {
            object.remove("reactions");
            object.insert(
                "journal_id".to_owned(),
                Value::String(journal_id.to_owned()),
            );
            object.insert("entry_id".to_owned(), Value::String(entry_id.to_owned()));
        }
        let deleted = comment_deleted_flag(&comment);
        store.upsert_comment_json_row(&comment.to_string())?;
        store.delete_comment_reactions_by_comment(journal_id, entry_id, &comment_id)?;
        if deleted {
            continue;
        }
        for reaction in reactions {
            let mut reaction_value = reaction.clone();
            if let Some(object) = reaction_value.as_object_mut() {
                object.insert(
                    "journal_id".to_owned(),
                    Value::String(journal_id.to_owned()),
                );
                object.insert("entry_id".to_owned(), Value::String(entry_id.to_owned()));
                object.insert("comment_id".to_owned(), Value::String(comment_id.clone()));
            }
            store.upsert_comment_reaction_json_row(&reaction_value.to_string())?;
        }
    }
    Ok(())
}

pub(crate) struct CommentPageSyncRequest<'a> {
    pub store: &'a Store,
    pub journal: &'a Value,
    pub journal_id: &'a str,
    pub entry_id: &'a str,
    pub cursor_key: &'a str,
    pub initial_cursor: Option<String>,
    pub context_label: &'a str,
}

pub(crate) async fn sync_comment_pages<F, Fut>(
    request: CommentPageSyncRequest<'_>,
    mut fetch_page: F,
) -> std::result::Result<(), CommentPageSyncError>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<Value>>,
{
    let CommentPageSyncRequest {
        store,
        journal,
        journal_id,
        entry_id,
        cursor_key,
        initial_cursor,
        context_label,
    } = request;
    let mut cursor = initial_cursor;
    let mut loops = 0usize;
    loop {
        loops += 1;
        if loops > MAX_PAGE_LOOPS {
            return Err(CommentPageSyncError::Pagination(format!(
                "{context_label} exceeded max pagination loops for journal '{}' entry '{}'",
                journal_id, entry_id
            )));
        }
        let previous_cursor = cursor.clone();
        let response = fetch_page(cursor.clone())
            .await
            .map_err(CommentPageSyncError::Other)?;
        let finished = response
            .get("finished")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let next_cursor = response
            .get("cursor")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|cursor| !cursor.is_empty())
            .map(ToOwned::to_owned);
        if !finished {
            if next_cursor.is_none() {
                return Err(CommentPageSyncError::Pagination(format!(
                    "{context_label} pagination stalled for journal '{}' entry '{}': missing cursor while finished=false",
                    journal_id, entry_id
                )));
            }
            if next_cursor == previous_cursor {
                return Err(CommentPageSyncError::Pagination(format!(
                    "{context_label} pagination stalled for journal '{}' entry '{}': cursor did not advance",
                    journal_id, entry_id
                )));
            }
        }
        let comments = response
            .get("comments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        persist_comments_page(store, journal, journal_id, entry_id, &comments)
            .map_err(CommentPageSyncError::Other)?;
        if let Some(next_cursor) = next_cursor.as_deref() {
            cursor = Some(next_cursor.to_owned());
            store
                .set_sync_cursor(cursor_key, cursor.as_deref())
                .map_err(|err| CommentPageSyncError::Other(err.into()))?;
        }
        if finished {
            break;
        }
    }
    Ok(())
}
