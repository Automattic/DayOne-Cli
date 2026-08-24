//! Detect PKCS#1 journal public keys that other Day One clients cannot import.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params, types::Type};
use serde::Serialize;
use serde_json::Value;

use super::{CheckOutcome, CheckStatus, table_exists};

const SYNC_FRESHNESS_SECS: i64 = 5 * 60;

#[derive(Debug, Serialize)]
struct Finding {
    #[serde(skip)]
    journal_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    journal_id: Option<String>,
    issue: &'static str,
    locally_fixable: bool,
    entry_count: usize,
    entries_missing_media: usize,
    sync_is_fresh: bool,
    inspection_complete: bool,
}

#[derive(Debug, Serialize)]
struct MetadataFinding {
    #[serde(skip)]
    journal_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    journal_id: Option<String>,
    issue: &'static str,
    field: String,
    status: String,
    confirmed: bool,
}

pub(super) fn run(connection: &Connection, include_private_identifiers: bool) -> CheckOutcome {
    if !table_exists(connection, "journals") || !table_exists(connection, "entries") {
        return CheckOutcome::not_run(
            "journal_key_encoding",
            "Journal key encoding could not be inspected",
            "schema_unavailable",
        );
    }

    let sync_is_fresh = recent_successful_sync(connection);
    let mut findings = Vec::new();
    let mut unreadable_journals = 0_usize;
    let Ok(mut statement) =
        connection.prepare("SELECT id, data_json FROM journals WHERE is_deleted = 0")
    else {
        return CheckOutcome::not_run(
            "journal_key_encoding",
            "Journal key encoding could not be inspected",
            "sqlite_query_failed",
        );
    };
    let Ok(rows) = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) else {
        return CheckOutcome::not_run(
            "journal_key_encoding",
            "Journal key encoding could not be inspected",
            "sqlite_query_failed",
        );
    };

    for row in rows {
        let Ok((journal_id, raw)) = row else {
            return CheckOutcome::not_run(
                "journal_key_encoding",
                "Journal key encoding could not be inspected",
                "sqlite_query_failed",
            );
        };
        let Ok(journal) = serde_json::from_str::<Value>(&raw) else {
            unreadable_journals += 1;
            continue;
        };
        if !journal_uses_pkcs1(&journal) {
            continue;
        }
        let assessment = assess_local_fixability(connection, &journal_id).ok();
        let (entry_count, entries_missing_media) = assessment.unwrap_or_default();
        findings.push(Finding {
            journal_key: journal_id.clone(),
            journal_id: include_private_identifiers.then_some(journal_id),
            issue: "pkcs1_key_encoding",
            locally_fixable: sync_is_fresh && assessment.is_some() && entries_missing_media == 0,
            entry_count,
            entries_missing_media,
            sync_is_fresh,
            inspection_complete: assessment.is_some(),
        });
    }

    let metadata_inspection_complete = match metadata_inspection_is_complete(connection) {
        Ok(complete) => complete,
        Err(_) => {
            return CheckOutcome::not_run(
                "journal_key_encoding",
                "Journal encryption state could not be inspected",
                "sqlite_query_failed",
            );
        }
    };
    let metadata_findings = match metadata_findings(connection, include_private_identifiers) {
        Ok(findings) => findings,
        Err(_) => {
            return CheckOutcome::not_run(
                "journal_key_encoding",
                "Journal encryption state could not be inspected",
                "sqlite_query_failed",
            );
        }
    };
    let confirmed_metadata = metadata_findings
        .iter()
        .filter(|finding| finding.confirmed)
        .count();
    let unverified_metadata = metadata_findings.len() - confirmed_metadata;
    let metadata_journals = metadata_findings
        .iter()
        .map(|finding| finding.journal_key.as_str())
        .collect::<HashSet<_>>()
        .len();
    let total_affected_journals = findings
        .iter()
        .map(|finding| finding.journal_key.as_str())
        .chain(
            metadata_findings
                .iter()
                .map(|finding| finding.journal_key.as_str()),
        )
        .collect::<HashSet<_>>()
        .len();
    if findings.is_empty() && confirmed_metadata == 0 && unreadable_journals == 0 {
        let details = serde_json::json!({
            "affected_journals": 0,
            "locally_fixable": 0,
            "pkcs1_journals": 0,
            "pkcs1_locally_fixable": 0,
            "metadata_journals": metadata_journals,
            "total_affected_journals": total_affected_journals,
            "confirmed_metadata_fields": 0,
            "unreadable_journals": 0,
            "unverified_metadata_fields": unverified_metadata,
            "metadata_inspection_complete": metadata_inspection_complete,
        });
        let mut outcome = if !metadata_inspection_complete {
            CheckOutcome::finding(
                "journal_key_encoding",
                "E2E journal metadata inspection has not finished",
                details,
            )
        } else if unverified_metadata == 0 {
            CheckOutcome::pass(
                "journal_key_encoding",
                "No journal encryption problems were found",
                details,
            )
        } else {
            CheckOutcome::finding(
                "journal_key_encoding",
                "Some E2E journal metadata could not be verified",
                details,
            )
        };
        outcome.findings = metadata_findings
            .into_iter()
            .filter_map(|finding| serde_json::to_value(finding).ok())
            .collect();
        outcome
    } else {
        let summary = match (
            findings.is_empty(),
            confirmed_metadata == 0,
            unreadable_journals == 0,
        ) {
            (true, false, true) => "Some E2E journals have invalid encrypted metadata",
            (_, _, false) => "Some journal encryption state is incompatible or unreadable",
            _ => "Some journals have incompatible encryption state",
        };
        let mut serialized_findings: Vec<Value> = findings
            .iter()
            .filter_map(|finding| serde_json::to_value(finding).ok())
            .collect();
        serialized_findings.extend(
            metadata_findings
                .into_iter()
                .filter_map(|finding| serde_json::to_value(finding).ok()),
        );
        CheckOutcome {
            check: "journal_key_encoding",
            status: CheckStatus::Fail,
            summary,
            details: Some(serde_json::json!({
                "affected_journals": findings.len(),
                "locally_fixable": findings.iter().filter(|finding| finding.locally_fixable).count(),
                "pkcs1_journals": findings.len(),
                "pkcs1_locally_fixable": findings.iter().filter(|finding| finding.locally_fixable).count(),
                "metadata_journals": metadata_journals,
                "total_affected_journals": total_affected_journals,
                "confirmed_metadata_fields": confirmed_metadata,
                "unverified_metadata_fields": unverified_metadata,
                "unreadable_journals": unreadable_journals,
                "metadata_inspection_complete": metadata_inspection_complete,
            })),
            findings: serialized_findings,
        }
    }
}

fn metadata_inspection_is_complete(connection: &Connection) -> rusqlite::Result<bool> {
    if table_exists(connection, "journal_metadata_inspection") {
        return connection.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM journal_metadata_inspection WHERE id = 1)",
            [],
            |row| row.get(0),
        );
    }
    Ok(false)
}

fn metadata_findings(
    connection: &Connection,
    include_private_identifiers: bool,
) -> rusqlite::Result<Vec<MetadataFinding>> {
    if !table_exists(connection, "journal_metadata_diagnostics") {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(
        "SELECT journal_id, field, status FROM journal_metadata_diagnostics ORDER BY journal_id, field",
    )?;
    statement
        .query_map([], |row| {
            let journal_id = row.get::<_, String>(0)?;
            let status = row.get::<_, String>(2)?;
            Ok(MetadataFinding {
                journal_key: journal_id.clone(),
                journal_id: include_private_identifiers.then_some(journal_id),
                issue: "metadata_encryption",
                field: row.get(1)?,
                confirmed: status != "unverified_d1",
                status,
            })
        })?
        .collect()
}

fn recent_successful_sync(connection: &Connection) -> bool {
    if !table_exists(connection, "sync_runs") {
        return false;
    }
    connection
        .query_row(
            "SELECT CAST(strftime('%s', 'now') AS INTEGER) - CAST(strftime('%s', MAX(finished_at)) AS INTEGER) FROM sync_runs WHERE status = 'success' AND finished_at IS NOT NULL",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten()
        .is_some_and(|seconds| (0..=SYNC_FRESHNESS_SECS).contains(&seconds))
}

fn assess_local_fixability(
    connection: &Connection,
    journal_id: &str,
) -> rusqlite::Result<(usize, usize)> {
    if !table_exists(connection, "entry_attachments") {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let mut statement = connection
        .prepare("SELECT id, data_json FROM entries WHERE journal_id = ?1 AND is_deleted = 0")?;
    let rows = statement.query_map(params![journal_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut entry_count = 0;
    let mut entries_missing_media = 0;
    for row in rows {
        let (entry_id, raw) = row?;
        entry_count += 1;
        let entry = serde_json::from_str::<Value>(&raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(1, Type::Text, Box::new(error))
        })?;
        let missing = entry
            .get("moments")
            .and_then(Value::as_array)
            .or_else(|| {
                entry
                    .get("payload")
                    .and_then(|payload| payload.get("moments"))
                    .and_then(Value::as_array)
            })
            .into_iter()
            .flatten()
            .filter_map(|moment| moment.get("id").and_then(Value::as_str))
            .any(|moment_id| !attachment_exists(connection, journal_id, &entry_id, moment_id));
        if missing {
            entries_missing_media += 1;
        }
    }
    Ok((entry_count, entries_missing_media))
}

fn attachment_exists(
    connection: &Connection,
    journal_id: &str,
    entry_id: &str,
    moment_id: &str,
) -> bool {
    connection
        .query_row(
            "SELECT file_path FROM entry_attachments WHERE journal_id = ?1 AND entry_id = ?2 AND moment_id = ?3",
            params![journal_id, entry_id, moment_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .is_some_and(|path| Path::new(&path).is_file())
}

fn journal_uses_pkcs1(journal: &Value) -> bool {
    journal
        .get("encryption")
        .and_then(|encryption| encryption.get("vault"))
        .and_then(|vault| vault.get("keys"))
        .and_then(Value::as_array)
        .is_some_and(|keys| {
            keys.iter().any(|key| {
                key.get("public_key")
                    .and_then(Value::as_str)
                    .is_some_and(|pem| pem.contains("BEGIN RSA PUBLIC KEY"))
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_entry_inspection_never_reports_locally_fixable() {
        let connection = Connection::open_in_memory().expect("memory database");
        connection
            .execute_batch(
                "CREATE TABLE journals (id TEXT PRIMARY KEY, data_json TEXT NOT NULL, is_deleted INTEGER);
                 CREATE TABLE entries (id TEXT PRIMARY KEY, journal_id TEXT, data_json TEXT);
                 CREATE TABLE sync_runs (status TEXT, finished_at TEXT);
                 INSERT INTO sync_runs VALUES ('success', CURRENT_TIMESTAMP);",
            )
            .expect("schema");
        connection
            .execute(
                "INSERT INTO journals VALUES (?1, ?2, 0)",
                params![
                    "private-journal",
                    r#"{"encryption":{"vault":{"keys":[{"public_key":"-----BEGIN RSA PUBLIC KEY-----"}]}}}"#
                ],
            )
            .expect("journal");

        let outcome = run(&connection, false);
        assert_eq!(outcome.status, CheckStatus::Fail);
        let finding: &Value = &outcome.findings[0];
        assert_eq!(finding["inspection_complete"], false);
        assert_eq!(finding["locally_fixable"], false);
        assert!(finding.get("journal_id").is_none());
    }

    #[test]
    fn unreadable_journal_data_is_a_failure_not_a_false_pass() {
        let connection = Connection::open_in_memory().expect("memory database");
        connection
            .execute_batch(
                "CREATE TABLE journals (id TEXT PRIMARY KEY, data_json TEXT NOT NULL, is_deleted INTEGER);
                 CREATE TABLE entries (id TEXT PRIMARY KEY, journal_id TEXT, data_json TEXT, is_deleted INTEGER);
                 INSERT INTO journals VALUES ('private-journal', 'not-json', 0);",
            )
            .expect("schema");

        let outcome = run(&connection, false);
        assert_eq!(outcome.status, CheckStatus::Fail);
        assert_eq!(outcome.details.expect("details")["unreadable_journals"], 1);
    }

    #[test]
    fn unreadable_entry_data_blocks_local_fixability() {
        let connection = Connection::open_in_memory().expect("memory database");
        connection
            .execute_batch(
                r#"CREATE TABLE journals (id TEXT PRIMARY KEY, data_json TEXT NOT NULL, is_deleted INTEGER);
                   CREATE TABLE entries (id TEXT PRIMARY KEY, journal_id TEXT, data_json TEXT, is_deleted INTEGER);
                   CREATE TABLE entry_attachments (journal_id TEXT, entry_id TEXT, moment_id TEXT, file_path TEXT);
                   INSERT INTO journals VALUES ('private-journal', '{"encryption":{"vault":{"keys":[{"public_key":"-----BEGIN RSA PUBLIC KEY-----"}]}}}', 0);
                   INSERT INTO entries VALUES ('private-entry', 'private-journal', 'not-json', 0);"#,
            )
            .expect("schema");

        let outcome = run(&connection, false);
        let finding = &outcome.findings[0];
        assert_eq!(finding["inspection_complete"], false);
        assert_eq!(finding["locally_fixable"], false);
    }

    #[test]
    fn payload_moments_are_checked_for_missing_attachments() {
        let connection = Connection::open_in_memory().expect("memory database");
        connection
            .execute_batch(
                r#"CREATE TABLE journals (id TEXT PRIMARY KEY, data_json TEXT NOT NULL, is_deleted INTEGER);
                   CREATE TABLE entries (id TEXT PRIMARY KEY, journal_id TEXT, data_json TEXT, is_deleted INTEGER);
                   CREATE TABLE entry_attachments (journal_id TEXT, entry_id TEXT, moment_id TEXT, file_path TEXT);
                   INSERT INTO journals VALUES ('private-journal', '{"encryption":{"vault":{"keys":[{"public_key":"-----BEGIN RSA PUBLIC KEY-----"}]}}}', 0);
                   INSERT INTO entries VALUES ('private-entry', 'private-journal', '{"payload":{"moments":[{"id":"private-moment"}]}}', 0);"#,
            )
            .expect("schema");

        let outcome = run(&connection, false);
        let finding = &outcome.findings[0];
        assert_eq!(finding["inspection_complete"], true);
        assert_eq!(finding["entries_missing_media"], 1);
        assert_eq!(finding["locally_fixable"], false);
    }

    #[test]
    fn deleted_journals_are_ignored() {
        let connection = Connection::open_in_memory().expect("memory database");
        connection
            .execute_batch(
                r#"CREATE TABLE journals (id TEXT PRIMARY KEY, data_json TEXT NOT NULL, is_deleted INTEGER);
                   CREATE TABLE entries (id TEXT PRIMARY KEY, journal_id TEXT, data_json TEXT, is_deleted INTEGER);
                   CREATE TABLE journal_metadata_inspection (id INTEGER PRIMARY KEY);
                   INSERT INTO journals VALUES ('private-journal', '{"encryption":{"vault":{"keys":[{"public_key":"-----BEGIN RSA PUBLIC KEY-----"}]}}}', 1);"#,
            )
            .expect("schema");

        let outcome = run(&connection, false);
        assert_eq!(outcome.status, CheckStatus::Pass);
        assert!(outcome.findings.is_empty());
    }

    #[test]
    fn pending_metadata_inspection_never_reports_clean() {
        let connection = Connection::open_in_memory().expect("memory database");
        connection
            .execute_batch(
                "CREATE TABLE journals (id TEXT PRIMARY KEY, data_json TEXT NOT NULL, is_deleted INTEGER);
                 CREATE TABLE entries (id TEXT PRIMARY KEY, journal_id TEXT, data_json TEXT, is_deleted INTEGER);
                 CREATE TABLE journal_metadata_diagnostics (journal_id TEXT, field TEXT, status TEXT);
                 CREATE TABLE journal_metadata_inspection (id INTEGER PRIMARY KEY);
                 INSERT INTO journal_metadata_inspection VALUES (1);",
            )
            .expect("schema");

        let outcome = run(&connection, false);
        assert_eq!(outcome.status, CheckStatus::Finding);
        let details = outcome.details.expect("details");
        assert_eq!(details["affected_journals"], 0);
        assert_eq!(details["locally_fixable"], 0);
        assert_eq!(details["pkcs1_journals"], 0);
        assert_eq!(details["pkcs1_locally_fixable"], 0);
        assert_eq!(details["metadata_journals"], 0);
        assert_eq!(details["total_affected_journals"], 0);
        assert_eq!(details["metadata_inspection_complete"], false);
    }

    #[test]
    fn completed_metadata_inspection_can_report_clean() {
        let connection = Connection::open_in_memory().expect("memory database");
        connection
            .execute_batch(
                "CREATE TABLE journals (id TEXT PRIMARY KEY, data_json TEXT NOT NULL, is_deleted INTEGER);
                 CREATE TABLE entries (id TEXT PRIMARY KEY, journal_id TEXT, data_json TEXT, is_deleted INTEGER);
                 CREATE TABLE journal_metadata_diagnostics (journal_id TEXT, field TEXT, status TEXT);
                 CREATE TABLE journal_metadata_inspection (id INTEGER PRIMARY KEY);",
            )
            .expect("schema");

        let outcome = run(&connection, false);
        assert_eq!(outcome.status, CheckStatus::Pass);
        assert_eq!(
            outcome.details.expect("details")["metadata_inspection_complete"],
            true
        );
    }

    #[test]
    fn overlapping_key_and_metadata_findings_count_one_affected_journal() {
        let connection = Connection::open_in_memory().expect("memory database");
        connection
            .execute_batch(
                r#"CREATE TABLE journals (id TEXT PRIMARY KEY, data_json TEXT NOT NULL, is_deleted INTEGER);
                   CREATE TABLE entries (id TEXT PRIMARY KEY, journal_id TEXT, data_json TEXT, is_deleted INTEGER);
                   CREATE TABLE entry_attachments (journal_id TEXT, entry_id TEXT, moment_id TEXT, file_path TEXT);
                   CREATE TABLE journal_metadata_diagnostics (journal_id TEXT, field TEXT, status TEXT);
                   INSERT INTO journals VALUES ('journal-1', '{"encryption":{"vault":{"keys":[{"public_key":"-----BEGIN RSA PUBLIC KEY-----"}]}}}', 0);
                   INSERT INTO journal_metadata_diagnostics VALUES ('journal-1', 'name', 'plaintext');"#,
            )
            .expect("schema");

        let outcome = run(&connection, false);
        assert_eq!(outcome.status, CheckStatus::Fail);
        let details = outcome.details.expect("details");
        assert_eq!(details["affected_journals"], 1);
        assert_eq!(details["pkcs1_journals"], 1);
        assert_eq!(details["metadata_journals"], 1);
        assert_eq!(details["total_affected_journals"], 1);
        assert_eq!(outcome.findings.len(), 2);
    }

    #[test]
    fn metadata_diagnostics_fail_only_for_confirmed_findings() {
        let connection = Connection::open_in_memory().expect("memory database");
        connection
            .execute_batch(
                "CREATE TABLE journals (id TEXT PRIMARY KEY, data_json TEXT NOT NULL, is_deleted INTEGER);
                 CREATE TABLE entries (id TEXT PRIMARY KEY, journal_id TEXT, data_json TEXT, is_deleted INTEGER);
                 CREATE TABLE journal_metadata_diagnostics (journal_id TEXT, field TEXT, status TEXT);
                 CREATE TABLE journal_metadata_inspection (id INTEGER PRIMARY KEY);
                 INSERT INTO journal_metadata_diagnostics VALUES ('private-journal', 'name', 'unverified_d1');",
            )
            .expect("schema");

        let outcome = run(&connection, false);
        assert_eq!(outcome.status, CheckStatus::Finding);
        let details = outcome.details.as_ref().expect("details");
        assert_eq!(details["metadata_journals"], 1);
        assert_eq!(details["total_affected_journals"], 1);
        assert_eq!(details["confirmed_metadata_fields"], 0);
        assert_eq!(details["unverified_metadata_fields"], 1);
        assert_eq!(details["metadata_inspection_complete"], true);
        assert_eq!(outcome.findings[0]["status"], "unverified_d1");
        assert_eq!(outcome.findings[0]["confirmed"], false);
        assert!(outcome.findings[0].get("journal_id").is_none());

        connection
            .execute(
                "UPDATE journal_metadata_diagnostics SET status = 'plaintext'",
                [],
            )
            .expect("diagnostic should update");
        let outcome = run(&connection, true);
        assert_eq!(outcome.status, CheckStatus::Fail);
        assert_eq!(
            outcome.summary,
            "Some E2E journals have invalid encrypted metadata"
        );
        let details = outcome.details.as_ref().expect("details");
        assert_eq!(details["affected_journals"], 0);
        assert_eq!(details["locally_fixable"], 0);
        assert_eq!(details["pkcs1_journals"], 0);
        assert_eq!(details["pkcs1_locally_fixable"], 0);
        assert_eq!(details["metadata_journals"], 1);
        assert_eq!(details["total_affected_journals"], 1);
        assert_eq!(details["confirmed_metadata_fields"], 1);
        assert_eq!(details["unverified_metadata_fields"], 0);
        assert_eq!(details["metadata_inspection_complete"], true);
        assert_eq!(outcome.findings[0]["issue"], "metadata_encryption");
        assert_eq!(outcome.findings[0]["field"], "name");
        assert_eq!(outcome.findings[0]["status"], "plaintext");
        assert_eq!(outcome.findings[0]["confirmed"], true);
        assert_eq!(outcome.findings[0]["journal_id"], "private-journal");
    }

    #[test]
    fn pkcs1_detection_does_not_confuse_spki() {
        let pkcs1 = serde_json::json!({
            "encryption": { "vault": { "keys": [
                { "public_key": "-----BEGIN RSA PUBLIC KEY-----" }
            ] } }
        });
        let spki = serde_json::json!({
            "encryption": { "vault": { "keys": [
                { "public_key": "-----BEGIN PUBLIC KEY-----" }
            ] } }
        });
        assert!(journal_uses_pkcs1(&pkcs1));
        assert!(!journal_uses_pkcs1(&spki));
    }
}
