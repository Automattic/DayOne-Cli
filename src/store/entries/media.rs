use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};

use crate::store::StoreResult;
use crate::store::sqlite::Store;

/// Directory (inside the profile directory) that holds downloaded media.
pub(crate) const MEDIA_DIR_NAME: &str = "media";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MomentMediaKind {
    Original,
    Thumbnail,
}

impl MomentMediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Thumbnail => "thumbnail",
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MomentMediaRow {
    pub journal_id: String,
    pub entry_id: String,
    pub moment_id: String,
    pub kind: String,
    pub rel_path: String,
    pub file_size_bytes: i64,
    pub md5: String,
    pub source_md5: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MomentMediaUpsert<'a> {
    pub journal_id: &'a str,
    pub entry_id: &'a str,
    pub moment_id: &'a str,
    pub kind: MomentMediaKind,
    pub rel_path: &'a str,
    pub file_size_bytes: i64,
    pub md5: &'a str,
    pub source_md5: Option<&'a str>,
}

/// An entry that carries moments, as stored locally.
#[derive(Debug, Clone)]
pub struct EntryMomentsRecord {
    pub entry_id: String,
    pub journal_id: String,
    pub moments_json: String,
}

impl Store {
    /// The profile directory: the parent of the profile's SQLite database.
    pub fn profile_dir(&self) -> Option<&Path> {
        self.db_path().parent()
    }

    /// Absolute path of a media file recorded with a profile-relative path.
    pub fn media_absolute_path(&self, rel_path: &str) -> Option<PathBuf> {
        self.profile_dir().map(|dir| dir.join(rel_path))
    }

    pub fn upsert_moment_media(&self, media: &MomentMediaUpsert<'_>) -> StoreResult<()> {
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO moment_media (
              journal_id, entry_id, moment_id, kind, rel_path, file_size_bytes, md5, source_md5,
              downloaded_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, CURRENT_TIMESTAMP)
            ON CONFLICT(journal_id, entry_id, moment_id, kind) DO UPDATE SET
              rel_path = excluded.rel_path,
              file_size_bytes = excluded.file_size_bytes,
              md5 = excluded.md5,
              source_md5 = excluded.source_md5,
              downloaded_at = CURRENT_TIMESTAMP
            "#,
            params![
                media.journal_id,
                media.entry_id,
                media.moment_id,
                media.kind.as_str(),
                media.rel_path,
                media.file_size_bytes,
                media.md5,
                media.source_md5,
            ],
        )?;
        Ok(())
    }

    pub fn get_moment_media(
        &self,
        journal_id: &str,
        entry_id: &str,
        moment_id: &str,
        kind: MomentMediaKind,
    ) -> StoreResult<Option<MomentMediaRow>> {
        let conn = self.connect()?;
        conn.query_row(
            r#"
            SELECT journal_id, entry_id, moment_id, kind, rel_path, file_size_bytes, md5, source_md5
            FROM moment_media
            WHERE journal_id = ?1 AND entry_id = ?2 AND moment_id = ?3 AND kind = ?4
            "#,
            params![journal_id, entry_id, moment_id, kind.as_str()],
            map_moment_media_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_moment_media_for_entry(
        &self,
        journal_id: &str,
        entry_id: &str,
    ) -> StoreResult<Vec<MomentMediaRow>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT journal_id, entry_id, moment_id, kind, rel_path, file_size_bytes, md5, source_md5
            FROM moment_media
            WHERE journal_id = ?1 AND entry_id = ?2
            ORDER BY moment_id ASC, kind ASC
            "#,
        )?;
        let rows = stmt.query_map(params![journal_id, entry_id], map_moment_media_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Live entries (and their live journals) that have a non-empty moments list.
    pub fn list_entries_with_moments(&self) -> StoreResult<Vec<EntryMomentsRecord>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT e.id, e.journal_id, e.moments_json
            FROM entries e
            LEFT JOIN journals j ON j.id = e.journal_id
            WHERE e.is_deleted = 0
              AND e.deleted_at IS NULL
              AND (j.id IS NULL OR (j.is_deleted = 0 AND j.deleted_at IS NULL))
              AND e.moments_json IS NOT NULL
              AND e.moments_json NOT IN ('', '[]', 'null')
            ORDER BY e.journal_id ASC, e.id ASC
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(EntryMomentsRecord {
                entry_id: row.get(0)?,
                journal_id: row.get(1)?,
                moments_json: row.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Removes the downloaded-media rows and files for a deleted journal or entry.
    pub(crate) fn remove_downloaded_media(&self, journal_id: &str, entry_id: Option<&str>) {
        let Some(profile_dir) = self.profile_dir() else {
            return;
        };
        let mut dir = profile_dir
            .join(MEDIA_DIR_NAME)
            .join(sanitize_path_component(journal_id));
        if let Some(entry_id) = entry_id {
            dir = dir.join(sanitize_path_component(entry_id));
        }
        // Best-effort: stale files are harmless, rows are what the CLI reads.
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Server identifiers are hex/UUID-like, but they end up in file paths, so
/// anything outside a conservative alphabet is replaced rather than trusted.
pub(crate) fn sanitize_path_component(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "_".to_owned()
    } else {
        cleaned
    }
}

fn map_moment_media_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MomentMediaRow> {
    Ok(MomentMediaRow {
        journal_id: row.get(0)?,
        entry_id: row.get(1)?,
        moment_id: row.get(2)?,
        kind: row.get(3)?,
        rel_path: row.get(4)?,
        file_size_bytes: row.get(5)?,
        md5: row.get(6)?,
        source_md5: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_at(dir.path().join("dayone.db")).unwrap();
        (dir, store)
    }

    #[test]
    fn upsert_get_and_list_round_trip() {
        let (_dir, store) = store();
        let media = MomentMediaUpsert {
            journal_id: "j1",
            entry_id: "e1",
            moment_id: "m1",
            kind: MomentMediaKind::Original,
            rel_path: "media/j1/e1/m1.jpg",
            file_size_bytes: 12,
            md5: "aa",
            source_md5: Some("bb"),
        };
        store.upsert_moment_media(&media).unwrap();
        store
            .upsert_moment_media(&MomentMediaUpsert {
                kind: MomentMediaKind::Thumbnail,
                rel_path: "media/j1/e1/m1.thumb.jpg",
                ..media.clone()
            })
            .unwrap();
        // Re-upserting replaces instead of duplicating.
        store
            .upsert_moment_media(&MomentMediaUpsert {
                file_size_bytes: 20,
                ..media.clone()
            })
            .unwrap();

        let row = store
            .get_moment_media("j1", "e1", "m1", MomentMediaKind::Original)
            .unwrap()
            .expect("row");
        assert_eq!(row.file_size_bytes, 20);
        assert_eq!(row.source_md5.as_deref(), Some("bb"));
        assert_eq!(
            store.list_moment_media_for_entry("j1", "e1").unwrap().len(),
            2
        );
        assert!(
            store
                .get_moment_media("j1", "e1", "other", MomentMediaKind::Original)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn list_entries_with_moments_skips_deleted_and_empty() {
        let (_dir, store) = store();
        store
            .upsert_entry_json_row(
                "e-media",
                "j1",
                Some("2026-03-17T00:00:00.000Z"),
                None,
                r#"{"id":"e-media","moments":[{"id":"m1","momentType":"image"}]}"#,
            )
            .unwrap();
        store
            .upsert_entry_json_row(
                "e-none",
                "j1",
                Some("2026-03-17T00:00:00.000Z"),
                None,
                r#"{"id":"e-none","body":"x"}"#,
            )
            .unwrap();
        store
            .upsert_entry_json_row(
                "e-empty",
                "j1",
                Some("2026-03-17T00:00:00.000Z"),
                None,
                r#"{"id":"e-empty","moments":[]}"#,
            )
            .unwrap();
        let listed = store.list_entries_with_moments().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].entry_id, "e-media");
    }

    #[test]
    fn sanitize_path_component_blocks_traversal() {
        assert_eq!(sanitize_path_component("../../etc"), "______etc");
        assert_eq!(sanitize_path_component("AB-12_cd"), "AB-12_cd");
        assert_eq!(sanitize_path_component(""), "_");
    }
}
