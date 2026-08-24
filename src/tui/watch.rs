use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::store::sqlite::Store;

pub struct DbWatcher {
    conn: Connection,
    last_data_version: i64,
}

impl DbWatcher {
    pub fn new(store: &Store) -> Result<Self> {
        let conn = store
            .connect()
            .context("failed to open TUI watch connection")?;
        let last_data_version = read_data_version(&conn)?;
        Ok(Self {
            conn,
            last_data_version,
        })
    }

    pub fn changed(&mut self) -> Result<bool> {
        let current = read_data_version(&self.conn)?;
        if current == self.last_data_version {
            return Ok(false);
        }
        self.last_data_version = current;
        Ok(true)
    }
}

fn read_data_version(conn: &Connection) -> Result<i64> {
    conn.query_row("PRAGMA data_version", [], |row| row.get(0))
        .context("failed to read sqlite data_version")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watcher_detects_writes_from_another_store_connection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dayone.db");
        let store = Store::open_at(&path).expect("store");
        let writer = Store::open_at(&path).expect("writer");
        let mut watcher = DbWatcher::new(&store).expect("watcher");

        assert!(!watcher.changed().expect("initial check should succeed"));
        writer
            .upsert_json_row(
                "journals",
                "journal-1",
                Some("2026-05-01T00:00:00.000Z"),
                None,
                r#"{"id":"journal-1","name":"Journal"}"#,
            )
            .expect("writer should commit");

        assert!(watcher.changed().expect("write should be detected"));
        assert!(!watcher.changed().expect("version should be current"));
    }
}
