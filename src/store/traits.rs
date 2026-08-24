use crate::models::{Entry, Journal};
use crate::store::OutboxItem;
use crate::store::StoreResult;
use crate::store::sqlite::Store;

/// Outbox operations used by sync to queue and mutate pending push work.
#[expect(
    dead_code,
    reason = "fake stores exercise these trait APIs in sync tests"
)]
pub(crate) trait OutboxStore {
    fn enqueue_outbox_item(
        &self,
        id: &str,
        resource: &str,
        operation: &str,
        object_id: &str,
        payload_json: &str,
        next_attempt_epoch_ms: i64,
    ) -> StoreResult<()>;
    fn lease_outbox_items(&self, now_epoch_ms: i64, limit: usize) -> StoreResult<Vec<OutboxItem>>;
    fn mark_outbox_item_succeeded(&self, id: &str, leased_payload_json: &str) -> StoreResult<()>;
    fn mark_outbox_item_retry(
        &self,
        id: &str,
        leased_payload_json: &str,
        next_attempt_epoch_ms: i64,
        error: Option<&str>,
    ) -> StoreResult<()>;
    fn mark_outbox_item_failed(
        &self,
        id: &str,
        leased_payload_json: &str,
        final_attempt_count: i64,
        error: Option<&str>,
    ) -> StoreResult<()>;
}

/// Sync metadata operations (cursors + timestamps/error state).
#[expect(dead_code, reason = "sync tests keep this trait as a store boundary")]
pub(crate) trait SyncStateStore {
    fn get_sync_cursor(&self, resource_key: &str) -> StoreResult<Option<String>>;
    fn set_sync_cursor(&self, resource_key: &str, cursor: Option<&str>) -> StoreResult<()>;
    fn set_sync_resource_state_success(&self, resource_key: &str) -> StoreResult<()>;
    fn set_sync_resource_state_error(&self, resource_key: &str, error: &str) -> StoreResult<()>;
}

/// Read-only typed entry/journal fetches by ID.
#[expect(
    dead_code,
    reason = "typed read boundary retained for sync test doubles"
)]
pub(crate) trait EntryReadStore {
    fn get_entry_by_id(&self, id: &str) -> StoreResult<Option<Entry>>;
    fn get_journal_by_id(&self, id: &str) -> StoreResult<Option<Journal>>;
}

impl OutboxStore for Store {
    fn enqueue_outbox_item(
        &self,
        id: &str,
        resource: &str,
        operation: &str,
        object_id: &str,
        payload_json: &str,
        next_attempt_epoch_ms: i64,
    ) -> StoreResult<()> {
        Store::enqueue_outbox_item(
            self,
            id,
            resource,
            operation,
            object_id,
            payload_json,
            next_attempt_epoch_ms,
        )
    }

    fn lease_outbox_items(&self, now_epoch_ms: i64, limit: usize) -> StoreResult<Vec<OutboxItem>> {
        Store::lease_outbox_items(self, now_epoch_ms, limit)
    }

    fn mark_outbox_item_succeeded(&self, id: &str, leased_payload_json: &str) -> StoreResult<()> {
        Store::mark_outbox_item_succeeded(self, id, leased_payload_json)
    }

    fn mark_outbox_item_retry(
        &self,
        id: &str,
        leased_payload_json: &str,
        next_attempt_epoch_ms: i64,
        error: Option<&str>,
    ) -> StoreResult<()> {
        Store::mark_outbox_item_retry(self, id, leased_payload_json, next_attempt_epoch_ms, error)
    }

    fn mark_outbox_item_failed(
        &self,
        id: &str,
        leased_payload_json: &str,
        final_attempt_count: i64,
        error: Option<&str>,
    ) -> StoreResult<()> {
        Store::mark_outbox_item_failed(self, id, leased_payload_json, final_attempt_count, error)
    }
}

impl SyncStateStore for Store {
    fn get_sync_cursor(&self, resource_key: &str) -> StoreResult<Option<String>> {
        Store::get_sync_cursor(self, resource_key)
    }

    fn set_sync_cursor(&self, resource_key: &str, cursor: Option<&str>) -> StoreResult<()> {
        Store::set_sync_cursor(self, resource_key, cursor)
    }

    fn set_sync_resource_state_success(&self, resource_key: &str) -> StoreResult<()> {
        Store::set_sync_resource_state_success(self, resource_key)
    }

    fn set_sync_resource_state_error(&self, resource_key: &str, error: &str) -> StoreResult<()> {
        Store::set_sync_resource_state_error(self, resource_key, error)
    }
}

impl EntryReadStore for Store {
    fn get_entry_by_id(&self, id: &str) -> StoreResult<Option<Entry>> {
        self.get_json_row_by_id("entries", id)?
            .map(|json| serde_json::from_str::<Entry>(&json))
            .transpose()
            .map_err(Into::into)
    }

    fn get_journal_by_id(&self, id: &str) -> StoreResult<Option<Journal>> {
        self.get_json_row_by_id("journals", id)?
            .map(|json| serde_json::from_str::<Journal>(&json))
            .transpose()
            .map_err(Into::into)
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FakeOutboxStatus {
    Pending,
    Processing,
    Failed,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FakeOutboxState {
    pub status: FakeOutboxStatus,
    pub attempt_count: i64,
    pub next_attempt_epoch_ms: i64,
    pub last_error: Option<String>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct FakeOutboxRow {
    item: OutboxItem,
    status: FakeOutboxStatus,
    next_attempt_epoch_ms: i64,
    last_error: Option<String>,
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct FakeOutboxStore {
    #[cfg(test)]
    rows: std::sync::Mutex<Vec<FakeOutboxRow>>,
}

#[cfg(test)]
impl FakeOutboxStore {
    pub(crate) fn state_for_id(&self, id: &str) -> Option<FakeOutboxState> {
        let rows = self.rows.lock().expect("fake outbox mutex poisoned");
        rows.iter()
            .find(|row| row.item.id == id)
            .map(|row| FakeOutboxState {
                status: row.status.clone(),
                attempt_count: row.item.attempt_count,
                next_attempt_epoch_ms: row.next_attempt_epoch_ms,
                last_error: row.last_error.clone(),
            })
    }
}

#[cfg(test)]
impl OutboxStore for FakeOutboxStore {
    fn enqueue_outbox_item(
        &self,
        id: &str,
        resource: &str,
        operation: &str,
        object_id: &str,
        payload_json: &str,
        next_attempt_epoch_ms: i64,
    ) -> StoreResult<()> {
        let mut rows = self.rows.lock().expect("fake outbox mutex poisoned");
        if let Some(existing) = rows.iter_mut().find(|row| row.item.id == id) {
            existing.item.resource = resource.to_owned();
            existing.item.operation = operation.to_owned();
            existing.item.object_id = object_id.to_owned();
            existing.item.payload_json = payload_json.to_owned();
            if existing.status != FakeOutboxStatus::Processing {
                existing.status = FakeOutboxStatus::Pending;
                existing.item.attempt_count = 0;
                existing.next_attempt_epoch_ms = next_attempt_epoch_ms;
                existing.last_error = None;
            }
            return Ok(());
        }
        rows.push(FakeOutboxRow {
            item: OutboxItem {
                id: id.to_owned(),
                resource: resource.to_owned(),
                operation: operation.to_owned(),
                object_id: object_id.to_owned(),
                payload_json: payload_json.to_owned(),
                attempt_count: 0,
            },
            status: FakeOutboxStatus::Pending,
            next_attempt_epoch_ms,
            last_error: None,
        });
        Ok(())
    }

    fn lease_outbox_items(&self, now_epoch_ms: i64, limit: usize) -> StoreResult<Vec<OutboxItem>> {
        let mut rows = self.rows.lock().expect("fake outbox mutex poisoned");
        let mut leased = Vec::new();
        for row in rows.iter_mut() {
            if leased.len() >= limit {
                break;
            }
            if row.status == FakeOutboxStatus::Pending && row.next_attempt_epoch_ms <= now_epoch_ms
            {
                row.status = FakeOutboxStatus::Processing;
                leased.push(row.item.clone());
            }
        }
        Ok(leased)
    }

    fn mark_outbox_item_succeeded(&self, id: &str, leased_payload_json: &str) -> StoreResult<()> {
        let mut rows = self.rows.lock().expect("fake outbox mutex poisoned");
        if let Some(index) = rows.iter().position(|row| {
            row.item.id == id
                && row.status == FakeOutboxStatus::Processing
                && row.item.payload_json == leased_payload_json
        }) {
            rows.remove(index);
            return Ok(());
        }
        if let Some(row) = rows
            .iter_mut()
            .find(|row| row.item.id == id && row.status == FakeOutboxStatus::Processing)
        {
            row.status = FakeOutboxStatus::Pending;
        }
        Ok(())
    }

    fn mark_outbox_item_retry(
        &self,
        id: &str,
        leased_payload_json: &str,
        next_attempt_epoch_ms: i64,
        error: Option<&str>,
    ) -> StoreResult<()> {
        let mut rows = self.rows.lock().expect("fake outbox mutex poisoned");
        if let Some(row) = rows.iter_mut().find(|row| {
            row.item.id == id
                && row.status == FakeOutboxStatus::Processing
                && row.item.payload_json == leased_payload_json
        }) {
            row.status = FakeOutboxStatus::Pending;
            row.item.attempt_count += 1;
            row.next_attempt_epoch_ms = next_attempt_epoch_ms;
            row.last_error = error.map(str::to_owned);
            return Ok(());
        }
        if let Some(row) = rows
            .iter_mut()
            .find(|row| row.item.id == id && row.status == FakeOutboxStatus::Processing)
        {
            row.status = FakeOutboxStatus::Pending;
        }
        Ok(())
    }

    fn mark_outbox_item_failed(
        &self,
        id: &str,
        leased_payload_json: &str,
        final_attempt_count: i64,
        error: Option<&str>,
    ) -> StoreResult<()> {
        let mut rows = self.rows.lock().expect("fake outbox mutex poisoned");
        if let Some(row) = rows.iter_mut().find(|row| {
            row.item.id == id
                && row.status == FakeOutboxStatus::Processing
                && row.item.payload_json == leased_payload_json
        }) {
            row.status = FakeOutboxStatus::Failed;
            row.item.attempt_count = final_attempt_count;
            row.last_error = error.map(str::to_owned);
            return Ok(());
        }
        if let Some(row) = rows
            .iter_mut()
            .find(|row| row.item.id == id && row.status == FakeOutboxStatus::Processing)
        {
            row.status = FakeOutboxStatus::Pending;
        }
        Ok(())
    }
}
