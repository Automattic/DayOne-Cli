use super::*;
use crate::util::now_rfc3339_utc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequiredSingletonErrorKind {
    Fetch,
    Persist,
}

#[derive(Debug)]
pub(crate) struct RequiredSingletonError {
    kind: RequiredSingletonErrorKind,
    source: anyhow::Error,
}

impl RequiredSingletonError {
    pub(crate) fn fetch(source: anyhow::Error) -> Self {
        Self {
            kind: RequiredSingletonErrorKind::Fetch,
            source,
        }
    }

    pub(crate) fn persist(source: anyhow::Error) -> Self {
        Self {
            kind: RequiredSingletonErrorKind::Persist,
            source,
        }
    }

    pub(crate) fn kind(&self) -> RequiredSingletonErrorKind {
        self.kind
    }

    pub(crate) fn into_inner(self) -> anyhow::Error {
        self.source
    }
}

impl std::fmt::Display for RequiredSingletonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(f)
    }
}

impl std::error::Error for RequiredSingletonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.source()
    }
}

pub(crate) fn build_journals_sync_params(
    cursor: Option<String>,
    local_journal_count: i64,
) -> Vec<(String, String)> {
    if cursor.unwrap_or_default().is_empty() && local_journal_count == 0 {
        vec![("excludeDeleted".to_owned(), "true".to_owned())]
    } else {
        Vec::new()
    }
}

async fn run_singleton_resource_internal<F, Fut, P>(
    store: &Store,
    run_id: i64,
    resources: &mut Vec<ResourceSyncOutput>,
    resource_key: &str,
    fetcher: F,
    mut persister: P,
) -> Result<Value>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
    P: FnMut(&Store, &Value) -> Result<i64>,
{
    log_sync(format!("resource start kind=singleton key={resource_key}"));
    let started = now_epoch_ms();
    let mut changed_count = 0_i64;
    let mut status = "success".to_owned();
    let mut error: Option<String> = None;
    let result: Result<Value> = match fetcher().await {
        Ok(value) => match persister(store, &value) {
            Ok(changed) => {
                changed_count = changed;
                let _ = store.set_sync_resource_state_success(resource_key);
                Ok(value)
            }
            Err(err) => {
                status = "error".to_owned();
                let err_msg = err.to_string();
                let _ = store.set_sync_resource_state_error(resource_key, &err_msg);
                error = Some(err_msg);
                Err(err)
            }
        },
        Err(err) => {
            status = "error".to_owned();
            let err_msg = err.to_string();
            let _ = store.set_sync_resource_state_error(resource_key, &err_msg);
            error = Some(err_msg);
            Err(err)
        }
    };

    let finished = now_epoch_ms();
    resources.push(ResourceSyncOutput {
        resource: resource_key.to_owned(),
        started_at_epoch_ms: started,
        finished_at_epoch_ms: finished,
        changed_count,
        cursor_advanced: false,
        status: status.clone(),
        error: error.clone(),
    });
    let _ = store.save_sync_resource_run(SyncResourceRunSave {
        run_id,
        resource_key,
        changed_count,
        cursor_before: None,
        cursor_after: None,
        status: &status,
        error: error.as_deref(),
    });
    log_sync(format!(
        "resource done kind=singleton key={} status={} changed_count={}",
        resource_key, status, changed_count
    ));
    result
}

pub(super) async fn run_singleton_resource<F, Fut, P>(
    store: &Store,
    run_id: i64,
    resources: &mut Vec<ResourceSyncOutput>,
    resource_key: &str,
    fetcher: F,
    persister: P,
) where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
    P: FnMut(&Store, &Value) -> Result<i64>,
{
    let _ =
        run_singleton_resource_internal(store, run_id, resources, resource_key, fetcher, persister)
            .await;
}

pub(super) async fn run_required_singleton_resource<F, Fut, P>(
    store: &Store,
    run_id: i64,
    resources: &mut Vec<ResourceSyncOutput>,
    resource_key: &str,
    fetcher: F,
    persister: P,
) -> std::result::Result<Value, RequiredSingletonError>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
    P: FnMut(&Store, &Value) -> Result<i64>,
{
    let fetched = fetcher;
    let mut persisted = persister;
    run_singleton_resource_internal(
        store,
        run_id,
        resources,
        resource_key,
        || async {
            fetched()
                .await
                .map_err(RequiredSingletonError::fetch)
                .map_err(anyhow::Error::new)
        },
        |store, value| {
            persisted(store, value)
                .map_err(RequiredSingletonError::persist)
                .map_err(anyhow::Error::new)
        },
    )
    .await
    .map_err(|err| match err.downcast::<RequiredSingletonError>() {
        Ok(required) => required,
        Err(other) => RequiredSingletonError::fetch(other),
    })
}

pub(super) async fn run_cursor_resource<C: DayOneApiClient, P>(
    api: &C,
    store: &Store,
    run_id: i64,
    resources: &mut Vec<ResourceSyncOutput>,
    params: CursorResourceParams<'_>,
    id_selector: fn(&Value) -> Option<String>,
    mut persister: P,
) -> bool
where
    P: FnMut(&Store, &str, &Value) -> Result<()>,
{
    let CursorResourceParams {
        resource_key,
        api_path,
        static_params,
    } = params;
    log_sync(format!("resource start kind=cursor key={resource_key}"));
    let started = now_epoch_ms();
    let cursor_before = store.get_sync_cursor(resource_key).ok().flatten();
    let mut cursor = cursor_before.clone().unwrap_or_default();
    let mut changed_count = 0_i64;
    let mut status = "success".to_owned();
    let mut error: Option<String> = None;
    let mut loops = 0usize;
    loop {
        loops += 1;
        if loops > MAX_PAGE_LOOPS {
            status = "error".to_owned();
            error = Some("max page loops exceeded".to_owned());
            break;
        }

        let mut query: Vec<(&str, String)> = Vec::new();
        if !cursor.is_empty() {
            query.push(("cursor", cursor.clone()));
        }
        for (k, v) in static_params {
            query.push((k.as_str(), v.clone()));
        }

        let response = match api.get_json(api_path, &query).await {
            Ok(v) => v,
            Err(err) => {
                status = "error".to_owned();
                error = Some(err.to_string());
                break;
            }
        };

        let items = extract_items(&response);
        for item in &items {
            if let Some(id) = id_selector(item) {
                if let Err(err) = persister(store, &id, item) {
                    status = "error".to_owned();
                    error = Some(err.to_string());
                    break;
                }
                changed_count += 1;
            }
        }
        if status == "error" {
            break;
        }

        let next_cursor = extract_cursor(&response).unwrap_or_else(|| cursor.clone());
        let has_more = extract_has_more(&response);
        log_sync(format!(
            "resource page key={} items={} has_more={} cursor_advanced={}",
            resource_key,
            items.len(),
            has_more,
            next_cursor != cursor
        ));

        if next_cursor != cursor {
            let _ = store.set_sync_cursor(resource_key, Some(&next_cursor));
        }
        cursor = next_cursor;

        if !has_more || items.is_empty() {
            break;
        }
    }

    if status == "success" {
        let _ = store.set_sync_resource_state_success(resource_key);
    } else if let Some(ref err) = error {
        let _ = store.set_sync_resource_state_error(resource_key, err);
    }

    let cursor_after = if cursor.is_empty() {
        None
    } else {
        Some(cursor.clone())
    };
    let cursor_advanced =
        cursor_before.as_deref().unwrap_or("") != cursor_after.as_deref().unwrap_or("");
    let finished = now_epoch_ms();
    resources.push(ResourceSyncOutput {
        resource: resource_key.to_owned(),
        started_at_epoch_ms: started,
        finished_at_epoch_ms: finished,
        changed_count,
        cursor_advanced,
        status: status.clone(),
        error: error.clone(),
    });
    let _ = store.save_sync_resource_run(SyncResourceRunSave {
        run_id,
        resource_key,
        changed_count,
        cursor_before: cursor_before.as_deref(),
        cursor_after: cursor_after.as_deref(),
        status: &status,
        error: error.as_deref(),
    });
    log_sync(format!(
        "resource done kind=cursor key={} status={} changed_count={} cursor_advanced={}",
        resource_key, status, changed_count, cursor_advanced
    ));
    status == "success"
}

pub(crate) fn extract_items(response: &Value) -> Vec<Value> {
    if let Some(array) = response.as_array() {
        return array.clone();
    }
    response
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| response.get("data").and_then(Value::as_array))
        .or_else(|| response.get("entries").and_then(Value::as_array))
        .or_else(|| response.get("journals").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default()
}

pub(super) fn extract_cursor(response: &Value) -> Option<String> {
    let from_object = response
        .get("cursor")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            response
                .get("next_cursor")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        });
    if from_object.is_some() {
        return from_object;
    }

    response.as_array().and_then(|items| {
        items
            .iter()
            .rev()
            .find_map(|item| item.get("cursor").map(value_to_string))
    })
}

pub(super) fn extract_has_more(response: &Value) -> bool {
    response
        .get("has_more")
        .and_then(Value::as_bool)
        .or_else(|| {
            response
                .get("finished")
                .and_then(Value::as_bool)
                .map(|f| !f)
        })
        .unwrap_or(false)
}

pub(super) fn persist_items_from_array(
    store: &Store,
    table: &str,
    payload: &Value,
    fallback_prefix: Option<&str>,
) -> Result<i64> {
    let mut changed = 0_i64;
    let arr = payload.as_array().cloned().unwrap_or_default();
    for (idx, item) in arr.into_iter().enumerate() {
        let id = value_id(&item)
            .unwrap_or_else(|| format!("{}-{idx}", fallback_prefix.unwrap_or("row")));
        let updated_at = item
            .get("updated_at")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(now_rfc3339_utc);
        let deleted_at = item.get("deleted_at").and_then(Value::as_str);
        store.upsert_json_row(
            table,
            &id,
            Some(updated_at.as_str()),
            deleted_at,
            &item.to_string(),
        )?;
        changed += 1;
    }
    Ok(changed)
}

pub(crate) fn feature_enabled(feature_flags: &Value, names: &[&str]) -> bool {
    if let Some(flags) = feature_flags.as_array() {
        for flag in flags {
            let name = flag.get("name").and_then(Value::as_str).unwrap_or_default();
            let id = flag.get("id").and_then(Value::as_str).unwrap_or_default();
            let enabled = flag
                .get("enabled")
                .and_then(Value::as_bool)
                .or_else(|| flag.get("user_value").and_then(Value::as_bool))
                .or_else(|| flag.get("default_value").and_then(Value::as_bool))
                .unwrap_or(false);
            if enabled
                && names.iter().any(|needle| {
                    needle.eq_ignore_ascii_case(name) || needle.eq_ignore_ascii_case(id)
                })
            {
                return true;
            }
        }
        return false;
    }

    for name in names {
        if feature_flags
            .get(*name)
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

pub(crate) fn value_id(value: &Value) -> Option<String> {
    value
        .get("id")
        .map(value_to_string)
        .or_else(|| value.get("uuid").map(value_to_string))
        .or_else(|| value.get("fingerprint").map(value_to_string))
        .or_else(|| value.get("date").map(value_to_string))
        .or_else(|| {
            let journal_id = value.get("journal_id").map(value_to_string)?;
            let entry_id = value
                .get("entry_id")
                .map(value_to_string)
                .or_else(|| value.get("id").map(value_to_string))?;
            Some(format!("{journal_id}:{entry_id}"))
        })
        .or_else(|| {
            value
                .get("revision")
                .and_then(|r| r.get("entryId"))
                .map(value_to_string)
        })
}

pub(crate) fn value_to_string(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        return s.to_owned();
    }
    if let Some(i) = v.as_i64() {
        return i.to_string();
    }
    if let Some(u) = v.as_u64() {
        return u.to_string();
    }
    if let Some(f) = v.as_f64() {
        return f.to_string();
    }
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::sqlite::Store;
    use rusqlite::Connection;
    use serde_json::json;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn open_sync_test_store(label: &str) -> (TempDir, PathBuf, Store) {
        let temp_dir = tempfile::Builder::new()
            .prefix(&format!("dayone-cli-sync-tests-{label}-"))
            .tempdir()
            .expect("temp dir should be created");
        let path = temp_dir.path().join("store.db");
        let store = Store::open_at(&path).expect("store should open");
        (temp_dir, path, store)
    }

    #[test]
    fn persist_items_from_array_sets_updated_at_for_unread_entries_without_timestamp() {
        let (_temp_dir, path, store) = open_sync_test_store("unread-entries-default-updated-at");
        let payload = json!([{ "id": "unread-1", "entry_id": "entry-1" }]);

        let changed = persist_items_from_array(&store, "unread_entries", &payload, None)
            .expect("persist should succeed");
        assert_eq!(changed, 1);

        let conn = Connection::open(&path).expect("raw connection should open");
        let (updated_at, deleted_at, is_deleted): (Option<String>, Option<String>, i64) = conn
            .query_row(
                "SELECT updated_at, deleted_at, is_deleted FROM unread_entries WHERE id = ?1",
                ["unread-1"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("persisted row should exist");

        assert!(
            updated_at.as_deref().is_some_and(|ts| !ts.is_empty()),
            "updated_at should be populated when payload omits it"
        );
        assert!(
            deleted_at.is_none(),
            "deleted_at should be NULL for active rows"
        );
        assert_eq!(is_deleted, 0);
    }

    #[test]
    fn persist_items_from_array_forwards_deleted_at_for_unread_entries() {
        let (_temp_dir, path, store) = open_sync_test_store("unread-entries-deleted-at");
        let payload = json!([{
            "id": "unread-2",
            "entry_id": "entry-2",
            "updated_at": "2026-04-09T06:00:00.000Z",
            "deleted_at": "2026-04-09T06:30:00.000Z"
        }]);

        let changed = persist_items_from_array(&store, "unread_entries", &payload, None)
            .expect("persist should succeed");
        assert_eq!(changed, 1);

        let conn = Connection::open(&path).expect("raw connection should open");
        let (updated_at, deleted_at, is_deleted): (Option<String>, Option<String>, i64) = conn
            .query_row(
                "SELECT updated_at, deleted_at, is_deleted FROM unread_entries WHERE id = ?1",
                ["unread-2"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("persisted row should exist");

        assert_eq!(updated_at.as_deref(), Some("2026-04-09T06:00:00.000Z"));
        assert_eq!(deleted_at.as_deref(), Some("2026-04-09T06:30:00.000Z"));
        assert_eq!(is_deleted, 1);
    }
}
