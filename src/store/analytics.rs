//! Durable queue for product analytics events (Automattic Tracks).
//!
//! Events are enqueued locally and drained on a best-effort background flush
//! (see [`crate::analytics`]). Keeping them in SQLite means an offline or
//! short-lived CLI invocation never loses events and never blocks on the
//! network: unsent rows simply flush on the next run or `dayone sync`.

use rusqlite::{OptionalExtension, params, params_from_iter};

use crate::store::StoreResult;
use crate::store::sqlite::Store;

/// Hard cap on queued events. Bounds growth if the machine is offline for a
/// long time; the oldest rows beyond this are dropped on enqueue.
pub(crate) const MAX_QUEUED_EVENTS: i64 = 500;

/// A single queued analytics event. Crate-internal: the queue shape is an
/// implementation detail of analytics flushing, not part of the public `Store`
/// API.
#[derive(Debug, Clone)]
pub(crate) struct AnalyticsEventRow {
    pub id: i64,
    pub params_json: String,
    pub created_at_ms: i64,
    pub attempts: i64,
    pub tracks_ui: Option<String>,
    pub tracks_ut: Option<String>,
}

impl Store {
    /// Append an event to the local queue, pruning the oldest rows when the
    /// queue exceeds [`MAX_QUEUED_EVENTS`].
    pub(crate) fn enqueue_analytics_event(
        &self,
        event_name: &str,
        params_json: &str,
        created_at_ms: i64,
        tracks_ui: Option<&str>,
        tracks_ut: Option<&str>,
    ) -> StoreResult<()> {
        let mut conn = self.connect()?;
        // Insert and cap-enforcement run in one transaction so the queue cap is
        // applied atomically (and as a single WAL commit) rather than as two
        // separate writes.
        let tx = conn.transaction()?;
        tx.execute(
            r#"
            INSERT INTO analytics_events (
                event_name, params_json, created_at_ms, attempts, tracks_ui, tracks_ut
            )
            VALUES (?1, ?2, ?3, 0, ?4, ?5)
            "#,
            params![event_name, params_json, created_at_ms, tracks_ui, tracks_ut],
        )?;
        // Keep only the newest MAX_QUEUED_EVENTS rows.
        tx.execute(
            r#"
            DELETE FROM analytics_events
            WHERE id NOT IN (
                SELECT id FROM analytics_events
                ORDER BY id DESC
                LIMIT ?1
            )
            "#,
            params![MAX_QUEUED_EVENTS],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Fetch up to `limit` of the oldest queued events for sending.
    pub(crate) fn take_analytics_events(
        &self,
        limit: usize,
    ) -> StoreResult<Vec<AnalyticsEventRow>> {
        let conn = self.connect()?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = conn.prepare(
            r#"
            SELECT id, params_json, created_at_ms, attempts, tracks_ui, tracks_ut
            FROM analytics_events
            ORDER BY id ASC
            LIMIT ?1
            "#,
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(AnalyticsEventRow {
                id: row.get(0)?,
                params_json: row.get(1)?,
                created_at_ms: row.get(2)?,
                attempts: row.get(3)?,
                tracks_ui: row.get(4)?,
                tracks_ut: row.get(5)?,
            })
        })?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    /// Remove successfully-sent (or unparseable) events from the queue in a
    /// single statement on one connection, avoiding a fresh connection per row
    /// during a flush.
    pub(crate) fn delete_analytics_events(&self, ids: &[i64]) -> StoreResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let conn = self.connect()?;
        let placeholders = vec!["?"; ids.len()].join(",");
        conn.execute(
            &format!("DELETE FROM analytics_events WHERE id IN ({placeholders})"),
            params_from_iter(ids),
        )?;
        Ok(())
    }

    /// Record a failed send attempt for several events at once so stuck events
    /// can be aged out, again on a single connection.
    pub(crate) fn bump_analytics_event_attempts(&self, ids: &[i64]) -> StoreResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let conn = self.connect()?;
        let placeholders = vec!["?"; ids.len()].join(",");
        conn.execute(
            &format!(
                "UPDATE analytics_events SET attempts = attempts + 1 WHERE id IN ({placeholders})"
            ),
            params_from_iter(ids),
        )?;
        Ok(())
    }

    /// Remove expired events and records outside the current consent identity
    /// before the flush limit is applied. IS NOT also matches missing identities.
    pub(crate) fn prune_analytics_events(
        &self,
        cutoff_ms: i64,
        tracks_ui: &str,
        tracks_ut: &str,
    ) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute(
            "DELETE FROM analytics_events WHERE created_at_ms < ?1
             OR tracks_ui IS NOT ?2 OR tracks_ut IS NOT ?3",
            params![cutoff_ms, tracks_ui, tracks_ut],
        )?;
        Ok(())
    }

    /// Read the locally cached account settings payload (`/api/user-settings`),
    /// used to honour the server-side analytics opt-out.
    pub(crate) fn get_user_settings_data_json(&self) -> StoreResult<Option<String>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT data_json FROM user_settings WHERE id = 1 LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(Into::into)
    }
}
