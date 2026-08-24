use rusqlite::{OptionalExtension, params};

use crate::entry_embeddings::ENTRY_EMBEDDING_DIMENSION;
use crate::store::StoreResult;
use crate::store::sqlite::Store;

const ENTRY_VEC_KNN_CANDIDATES: i64 = 2048;

impl Store {
    pub fn upsert_entry_embedding(
        &self,
        entry_id: &str,
        journal_id: &str,
        source_hash: &str,
        embedding: &[f64],
    ) -> StoreResult<()> {
        let embedding_json = serde_json::to_string(embedding)?;
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        tx.execute(
            r#"
            INSERT INTO entry_embeddings (entry_id, journal_id, source_hash, embedding_json, dimension, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, CURRENT_TIMESTAMP)
            ON CONFLICT(entry_id) DO UPDATE SET
              journal_id = excluded.journal_id,
              source_hash = excluded.source_hash,
              embedding_json = excluded.embedding_json,
              dimension = excluded.dimension,
              updated_at = CURRENT_TIMESTAMP
            "#,
            params![entry_id, journal_id, source_hash, embedding_json, embedding.len() as i64],
        )?;
        if embedding.len() == ENTRY_EMBEDDING_DIMENSION {
            tx.execute(
                "DELETE FROM entry_embeddings_vec WHERE entry_id = ?1",
                params![entry_id],
            )?;
            tx.execute(
                r#"
                INSERT INTO entry_embeddings_vec (entry_id, journal_id, embedding)
                VALUES (?1, ?2, ?3)
                "#,
                params![entry_id, journal_id, embedding_json],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_entry_embedding(&self, entry_id: &str) -> StoreResult<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM entry_embeddings WHERE entry_id = ?1",
            params![entry_id],
        )?;
        tx.execute(
            "DELETE FROM entry_embeddings_vec WHERE entry_id = ?1",
            params![entry_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_stale_or_deleted_entry_embeddings(&self) -> StoreResult<usize> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let deleted = tx.execute(
            r#"
            DELETE FROM entry_embeddings
            WHERE entry_id IN (SELECT id FROM entries WHERE is_deleted = 1)
               OR entry_id NOT IN (SELECT id FROM entries)
            "#,
            [],
        )?;
        tx.execute(
            r#"
            DELETE FROM entry_embeddings_vec
            WHERE entry_id NOT IN (SELECT entry_id FROM entry_embeddings)
            "#,
            [],
        )?;
        tx.commit()?;
        Ok(deleted)
    }

    pub fn get_entry_embedding_source_hash(&self, entry_id: &str) -> StoreResult<Option<String>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT source_hash FROM entry_embeddings WHERE entry_id = ?1 LIMIT 1",
            params![entry_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_entries_with_embeddings(
        &self,
        journal_id: Option<&str>,
        entry_id: Option<&str>,
    ) -> StoreResult<Vec<(String, Option<Vec<f64>>)>> {
        let conn = self.connect()?;
        let mut out = Vec::new();
        if let (Some(journal_id), Some(entry_id)) = (journal_id, entry_id) {
            let mut stmt = conn.prepare(
                r#"
                SELECT e.data_json, ee.embedding_json
                FROM entries e
                LEFT JOIN entry_embeddings ee ON ee.entry_id = e.id
                WHERE e.is_deleted = 0 AND e.journal_id = ?1 AND e.id = ?2
                ORDER BY COALESCE(e.updated_at, '') DESC, e.id DESC
                "#,
            )?;
            let rows = stmt.query_map(params![journal_id, entry_id], |row| {
                let data_json: String = row.get(0)?;
                let embedding_json: Option<String> = row.get(1)?;
                let embedding = match embedding_json {
                    Some(raw) => Some(serde_json::from_str::<Vec<f64>>(&raw).map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(err),
                        )
                    })?),
                    None => None,
                };
                Ok((data_json, embedding))
            })?;
            for row in rows {
                out.push(row?);
            }
        } else if let Some(journal_id) = journal_id {
            let mut stmt = conn.prepare(
                r#"
                SELECT e.data_json, ee.embedding_json
                FROM entries e
                LEFT JOIN entry_embeddings ee ON ee.entry_id = e.id
                WHERE e.is_deleted = 0 AND e.journal_id = ?1
                ORDER BY COALESCE(e.updated_at, '') DESC, e.id DESC
                "#,
            )?;
            let rows = stmt.query_map(params![journal_id], |row| {
                let data_json: String = row.get(0)?;
                let embedding_json: Option<String> = row.get(1)?;
                let embedding = match embedding_json {
                    Some(raw) => Some(serde_json::from_str::<Vec<f64>>(&raw).map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(err),
                        )
                    })?),
                    None => None,
                };
                Ok((data_json, embedding))
            })?;
            for row in rows {
                out.push(row?);
            }
        } else if let Some(entry_id) = entry_id {
            let mut stmt = conn.prepare(
                r#"
                SELECT e.data_json, ee.embedding_json
                FROM entries e
                LEFT JOIN entry_embeddings ee ON ee.entry_id = e.id
                WHERE e.is_deleted = 0 AND e.id = ?1
                ORDER BY COALESCE(e.updated_at, '') DESC, e.id DESC
                "#,
            )?;
            let rows = stmt.query_map(params![entry_id], |row| {
                let data_json: String = row.get(0)?;
                let embedding_json: Option<String> = row.get(1)?;
                let embedding = match embedding_json {
                    Some(raw) => Some(serde_json::from_str::<Vec<f64>>(&raw).map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(err),
                        )
                    })?),
                    None => None,
                };
                Ok((data_json, embedding))
            })?;
            for row in rows {
                out.push(row?);
            }
        } else {
            let mut stmt = conn.prepare(
                r#"
                SELECT e.data_json, ee.embedding_json
                FROM entries e
                LEFT JOIN entry_embeddings ee ON ee.entry_id = e.id
                WHERE e.is_deleted = 0
                ORDER BY COALESCE(e.updated_at, '') DESC, e.id DESC
                "#,
            )?;
            let rows = stmt.query_map([], |row| {
                let data_json: String = row.get(0)?;
                let embedding_json: Option<String> = row.get(1)?;
                let embedding = match embedding_json {
                    Some(raw) => Some(serde_json::from_str::<Vec<f64>>(&raw).map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(err),
                        )
                    })?),
                    None => None,
                };
                Ok((data_json, embedding))
            })?;
            for row in rows {
                out.push(row?);
            }
        }
        Ok(out)
    }

    pub fn search_entry_semantic_scores(
        &self,
        query_embedding: &[f64],
        threshold: f64,
        journal_id: Option<&str>,
    ) -> StoreResult<Vec<(String, f64)>> {
        let conn = self.connect()?;
        let query_json = serde_json::to_string(query_embedding)?;
        let mut out = Vec::new();

        // sqlite-vec's default vec0 metric is L2 distance. Our entry embeddings
        // are normalized, so cosine similarity is `1 - (l2_distance^2 / 2)`.
        if let Some(journal_id) = journal_id {
            let mut stmt = conn.prepare(
                r#"
                SELECT e.id, (1.0 - ((v.distance * v.distance) / 2.0)) AS similarity
                FROM entry_embeddings_vec v
                JOIN entries e ON e.id = v.entry_id
                WHERE v.embedding MATCH ?1
                  AND v.k = ?2
                  AND e.is_deleted = 0
                  AND e.journal_id = ?3
                  AND (1.0 - ((v.distance * v.distance) / 2.0)) >= ?4
                ORDER BY v.distance ASC, e.id ASC
                "#,
            )?;
            let rows = stmt.query_map(
                params![query_json, ENTRY_VEC_KNN_CANDIDATES, journal_id, threshold],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
            )?;
            for row in rows {
                out.push(row?);
            }
        } else {
            let mut stmt = conn.prepare(
                r#"
                SELECT e.id, (1.0 - ((v.distance * v.distance) / 2.0)) AS similarity
                FROM entry_embeddings_vec v
                JOIN entries e ON e.id = v.entry_id
                WHERE v.embedding MATCH ?1
                  AND v.k = ?2
                  AND e.is_deleted = 0
                  AND (1.0 - ((v.distance * v.distance) / 2.0)) >= ?3
                ORDER BY v.distance ASC, e.id ASC
                "#,
            )?;
            let rows = stmt.query_map(
                params![query_json, ENTRY_VEC_KNN_CANDIDATES, threshold],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
            )?;
            for row in rows {
                out.push(row?);
            }
        }
        Ok(out)
    }
}
