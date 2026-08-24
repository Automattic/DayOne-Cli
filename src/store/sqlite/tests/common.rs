use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::super::*;
use crate::store::entries::json::rebuild_entry_content;
use serde_json::Value;

/// A temporary database path backed by a [`tempfile::TempDir`].
///
/// The directory (and its DB file) are automatically deleted when this value
/// is dropped.  Implements `Deref<Target = Path>` and `AsRef<OsStr>` so
/// `&TestDb` satisfies `Into<PathBuf>` (via the blanket impl) and can be
/// passed directly to [`Store::open_at`].
pub struct TestDb {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl std::ops::Deref for TestDb {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TestDb {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<OsStr> for TestDb {
    fn as_ref(&self) -> &OsStr {
        self.path.as_os_str()
    }
}

pub fn test_store_path(name: &str) -> TestDb {
    let dir = tempfile::TempDir::new().expect("should create temp dir");
    let path = dir.path().join(format!("{name}.db"));
    TestDb { _dir: dir, path }
}

/// Full round-trip: upsert an entry, read it back, rebuild via `rebuild_entry_content`.
pub fn roundtrip(store: &Store, id: &str, journal_id: &str, data_json: &str) -> Value {
    store
        .upsert_entry_json_row(
            id,
            journal_id,
            Some("2026-03-01T00:00:00Z"),
            None,
            data_json,
        )
        .expect("upsert should succeed");
    read_rebuilt_entry(store, id)
}

/// Same as [`roundtrip`] but uses `upsert_entry_json_row_with_edit_date` (sync / entry-write path).
pub fn roundtrip_with_user_edit_date(
    store: &Store,
    id: &str,
    journal_id: &str,
    data_json: &str,
) -> Value {
    store
        .upsert_entry_json_row_with_edit_date(
            id,
            journal_id,
            Some("2026-03-01T12:00:00.000Z"),
            Some("2026-03-01T00:00:00Z"),
            None,
            data_json,
        )
        .expect("upsert with edit date should succeed");
    read_rebuilt_entry(store, id)
}

pub fn read_rebuilt_entry(store: &Store, id: &str) -> Value {
    let (raw, extra) = store
        .get_entry_json_row_with_extra_fields(id)
        .expect("read should succeed")
        .expect("entry should exist");
    let local: Value = serde_json::from_str(&raw).expect("data_json should parse");
    rebuild_entry_content(&local, extra.as_deref(), id)
}

/// Assert the rebuilt JSON matches the original, accounting for intentionally-dropped fields.
///
/// - Every key in `original` must exist in `rebuilt` with an identical value (unless in `dropped`).
/// - `rebuilt` must not contain keys absent from `original` (except `id`, always re-injected).
pub fn assert_roundtrip_eq(original: &Value, rebuilt: &Value, dropped: &[&str]) {
    let orig_obj = original.as_object().expect("original should be an object");
    let rebuilt_obj = rebuilt.as_object().expect("rebuilt should be an object");

    for (key, val) in orig_obj {
        if dropped.contains(&key.as_str()) {
            continue;
        }
        assert!(
            rebuilt_obj.contains_key(key),
            "rebuilt is missing key `{key}`"
        );
        assert_eq!(
            rebuilt_obj.get(key),
            Some(val),
            "value mismatch for key `{key}`"
        );
    }

    for key in rebuilt_obj.keys() {
        if key == "id" {
            continue;
        }
        assert!(
            orig_obj.contains_key(key),
            "rebuilt has unexpected extra key `{key}`"
        );
    }
}
