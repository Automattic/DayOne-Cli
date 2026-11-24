use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use serde_json::Value;

use crate::store::entries::json::rebuild_entry_content;
use crate::store::entries::media::MomentMediaKind;
use crate::store::sqlite::Store;
use crate::util::normalize_entry_id;

#[derive(Debug, Clone)]
pub struct EntryReadArgs {
    pub journal_id: Option<String>,
    pub entry_id: String,
}

#[derive(Debug, Serialize)]
pub struct EntryReadOutput {
    pub ok: bool,
    pub journal_id: String,
    pub entry_id: String,
    pub updated_at: Option<String>,
    pub user_edit_date: Option<String>,
    /// The reconstructed entry content: known fields (date, title, body,
    /// richTextJSON, timeZone, tags, moments, clientMeta) plus any preserved
    /// passthrough fields (location, weather, ...), matching the shape
    /// synced to the API. Moments whose media was downloaded with
    /// `dayone sync --media` also carry `local_path` (and
    /// `thumbnail.local_path`): absolute paths to the decrypted files.
    pub entry: Value,
}

/// Local single-entry fetch from the SQLite store. No network access.
pub fn execute(store: &Store, args: EntryReadArgs) -> Result<EntryReadOutput> {
    let journal_id = args.journal_id.as_deref().map(str::trim);
    if journal_id == Some("") {
        bail!("journal_id must not be empty");
    }
    let entry_id = normalize_entry_id(&args.entry_id);
    if entry_id.is_empty() {
        bail!("entry_id must not be empty");
    }

    let record = store.get_entry_row_record(&entry_id)?.ok_or_else(|| {
        anyhow!(
            "entry '{}' not found locally; run `dayone sync` first",
            entry_id
        )
    })?;
    if let Some(journal_id) = journal_id
        && record.journal_id != journal_id
    {
        bail!(
            "entry '{}' belongs to journal '{}', not '{}'",
            entry_id,
            record.journal_id,
            journal_id
        );
    }

    let local: Value = serde_json::from_str(&record.data_json)
        .map_err(|e| anyhow!("failed to parse JSON for entry '{}': {}", entry_id, e))?;
    let mut entry = rebuild_entry_content(&local, record.extra_fields_json.as_deref(), &entry_id);
    attach_local_media_paths(store, &record.journal_id, &entry_id, &mut entry);

    Ok(EntryReadOutput {
        ok: true,
        journal_id: record.journal_id,
        entry_id,
        updated_at: record.updated_at,
        user_edit_date: record.user_edit_date,
        entry,
    })
}

/// Adds `local_path` to each moment (and its thumbnail) whose file was
/// downloaded by `sync --media` and is still on disk. Entries without
/// downloaded media are left exactly as stored.
fn attach_local_media_paths(store: &Store, journal_id: &str, entry_id: &str, entry: &mut Value) {
    let Ok(rows) = store.list_moment_media_for_entry(journal_id, entry_id) else {
        return;
    };
    if rows.is_empty() {
        return;
    }
    let Some(moments) = entry.get_mut("moments").and_then(Value::as_array_mut) else {
        return;
    };
    for moment in moments {
        let Some(moment_object) = moment.as_object_mut() else {
            continue;
        };
        let Some(moment_id) = moment_object
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        for row in rows.iter().filter(|row| row.moment_id == moment_id) {
            let Some(path) = store
                .media_absolute_path(&row.rel_path)
                .filter(|path| path.is_file())
            else {
                continue;
            };
            let path = Value::String(path.to_string_lossy().into_owned());
            if row.kind == MomentMediaKind::Thumbnail.as_str() {
                let thumbnail = moment_object
                    .entry("thumbnail")
                    .or_insert_with(|| Value::Object(Default::default()));
                if let Some(thumbnail) = thumbnail.as_object_mut() {
                    thumbnail.insert("local_path".to_owned(), path);
                }
            } else {
                moment_object.insert("local_path".to_owned(), path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::sqlite::Store;

    fn setup_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir should create");
        let store = Store::open_at(dir.path().join("dayone.db")).expect("store should open");
        (dir, store)
    }

    #[test]
    fn read_returns_full_reconstructed_entry() {
        let (_dir, store) = setup_store();
        store
            .upsert_entry_json_row_with_edit_date(
                "entry-1",
                "journal-1",
                Some("1742169600000"),
                Some("2026-03-17T00:00:00.000Z"),
                None,
                r#"{
                    "id":"entry-1",
                    "date":"2026-03-17T10:00:00.000Z",
                    "title":"Captain's log",
                    "body":"USS Voyager\nStardate 41153.7",
                    "tags":["ship","ops"],
                    "moments":[{"id":"m1","momentType":"image"}],
                    "weather":{"conditionCode":"clear"},
                    "location":{"city":"Portland"}
                }"#,
            )
            .expect("entry should save");

        let out = execute(
            &store,
            EntryReadArgs {
                journal_id: Some("journal-1".to_owned()),
                entry_id: "entry-1".to_owned(),
            },
        )
        .expect("read should succeed");

        assert!(out.ok);
        assert_eq!(out.journal_id, "journal-1");
        assert_eq!(out.entry_id, "entry-1");
        assert_eq!(out.updated_at.as_deref(), Some("2026-03-17T00:00:00.000Z"));
        assert_eq!(out.user_edit_date.as_deref(), Some("1742169600000"));
        assert_eq!(out.entry["id"], "entry-1");
        assert_eq!(out.entry["title"], "Captain's log");
        assert_eq!(out.entry["body"], "USS Voyager\nStardate 41153.7");
        assert_eq!(out.entry["tags"], serde_json::json!(["ship", "ops"]));
        assert_eq!(out.entry["moments"][0]["id"], "m1");
        // Passthrough fields preserved via extra_fields_json.
        assert_eq!(out.entry["weather"]["conditionCode"], "clear");
        assert_eq!(out.entry["location"]["city"], "Portland");
    }

    #[test]
    fn read_rejects_missing_entry() {
        let (_dir, store) = setup_store();
        let err = execute(
            &store,
            EntryReadArgs {
                journal_id: Some("journal-1".to_owned()),
                entry_id: "missing".to_owned(),
            },
        )
        .expect_err("read should fail when entry missing");
        assert!(err.to_string().contains("not found locally"));
    }

    #[test]
    fn read_rejects_journal_entry_mismatch() {
        let (_dir, store) = setup_store();
        store
            .upsert_entry_json_row(
                "entry-1",
                "journal-1",
                Some("2026-03-17T00:00:00.000Z"),
                None,
                r#"{"id":"entry-1","body":"hello"}"#,
            )
            .expect("entry should save");

        let err = execute(
            &store,
            EntryReadArgs {
                journal_id: Some("journal-2".to_owned()),
                entry_id: "entry-1".to_owned(),
            },
        )
        .expect_err("read should fail when journal mismatches");
        assert!(err.to_string().contains("belongs to journal"));
    }

    #[test]
    fn read_normalizes_lowercase_hex_entry_id() {
        let (_dir, store) = setup_store();
        store
            .upsert_entry_json_row(
                "A65AFA22B98A0DEC62EAAD3CD1CCF45D",
                "journal-1",
                Some("2026-03-17T00:00:00.000Z"),
                None,
                r#"{"id":"A65AFA22B98A0DEC62EAAD3CD1CCF45D","body":"hello"}"#,
            )
            .expect("entry should save");

        let out = execute(
            &store,
            EntryReadArgs {
                journal_id: Some("journal-1".to_owned()),
                entry_id: "a65afa22b98a0dec62eaad3cd1ccf45d".to_owned(),
            },
        )
        .expect("read should succeed");
        assert_eq!(out.entry_id, "A65AFA22B98A0DEC62EAAD3CD1CCF45D");
    }

    #[test]
    fn read_adds_local_paths_only_for_downloaded_media() {
        use crate::store::entries::media::MomentMediaUpsert;

        let (dir, store) = setup_store();
        store
            .upsert_entry_json_row(
                "entry-1",
                "journal-1",
                Some("2026-03-17T00:00:00.000Z"),
                None,
                r#"{"id":"entry-1","moments":[
                    {"id":"m1","momentType":"image","thumbnail":{"md5":"t"}},
                    {"id":"m2","momentType":"image"}
                ]}"#,
            )
            .expect("entry should save");
        let original = dir.path().join("media/journal-1/entry-1/m1.jpg");
        std::fs::create_dir_all(original.parent().unwrap()).unwrap();
        std::fs::write(&original, b"jpg").unwrap();
        store
            .upsert_moment_media(&MomentMediaUpsert {
                journal_id: "journal-1",
                entry_id: "entry-1",
                moment_id: "m1",
                kind: MomentMediaKind::Original,
                rel_path: "media/journal-1/entry-1/m1.jpg",
                file_size_bytes: 3,
                md5: "x",
                source_md5: None,
            })
            .unwrap();
        // Recorded but missing on disk: no path is reported.
        store
            .upsert_moment_media(&MomentMediaUpsert {
                journal_id: "journal-1",
                entry_id: "entry-1",
                moment_id: "m1",
                kind: MomentMediaKind::Thumbnail,
                rel_path: "media/journal-1/entry-1/m1.thumb.jpg",
                file_size_bytes: 3,
                md5: "x",
                source_md5: None,
            })
            .unwrap();

        let out = execute(
            &store,
            EntryReadArgs {
                journal_id: None,
                entry_id: "entry-1".to_owned(),
            },
        )
        .expect("read should succeed");
        let moments = &out.entry["moments"];
        assert_eq!(
            moments[0]["local_path"],
            original.to_string_lossy().as_ref()
        );
        assert!(moments[0]["thumbnail"].get("local_path").is_none());
        assert!(moments[1].get("local_path").is_none());
    }
}
