use anyhow::{Context, Result};
use serde_json::Value;

use crate::store::sqlite::{
    EntryListSort, EntryListSortMethod, ListJsonTable, ListPageRequest, Store,
};

const LARGE_LIMIT: usize = 10_000;

#[derive(Debug, Clone)]
pub struct JournalItem {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    pub sort_order: i64,
}

#[derive(Debug, Clone)]
pub struct EntryPreview {
    pub id: String,
    pub entry_date: String,
    pub preview: String,
    pub tags: Vec<String>,
    pub media_count: usize,
}

#[derive(Debug, Clone)]
pub struct EntryBody {
    pub body_markdown: String,
}

#[derive(Debug, Clone)]
pub struct DailyChatDay {
    pub id: String,
    pub date: String,
    pub message_count: usize,
}

#[derive(Debug, Clone)]
pub struct DailyChatMessage {
    pub role: String,
    pub text: String,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UserSummary {
    pub id: String,
    pub name: Option<String>,
    pub email: Option<String>,
}

pub fn load_user_summary(store: &Store, base_url: &str) -> Option<UserSummary> {
    let session = store
        .get_auth_session_for_base_url(base_url)
        .ok()
        .flatten()?;
    let value: Value = serde_json::from_str(&session.user_json).ok()?;
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| value.get("user_id").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string();
    let name = value
        .get("display_name")
        .and_then(Value::as_str)
        .or_else(|| value.get("name").and_then(Value::as_str))
        .or_else(|| value.get("full_name").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .filter(|s| !s.is_empty());
    let email = value
        .get("email")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .filter(|s| !s.is_empty());
    Some(UserSummary { id, name, email })
}

pub fn load_journals(store: &Store) -> Result<Vec<JournalItem>> {
    let page = ListPageRequest {
        limit: Some(LARGE_LIMIT),
        offset: None,
        cursor: None,
    };
    let result = store
        .list_journals_json_rows_paged(&page, false)
        .context("failed to list journals")?;
    let mut items = Vec::with_capacity(result.rows.len());
    for raw in result.rows {
        let value: Value = serde_json::from_str(&raw).context("invalid journal json")?;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            continue;
        }
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("(unnamed)")
            .to_string();
        let color = value
            .get("color")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let sort_order = value
            .get("sort_order")
            .and_then(Value::as_i64)
            .unwrap_or(i64::MAX);
        items.push(JournalItem {
            id,
            name,
            color,
            sort_order,
        });
    }
    let order = load_user_journal_order(store);
    let pos_of =
        |id: &str| -> usize { order.iter().position(|oid| oid == id).unwrap_or(usize::MAX) };
    items.sort_by(|a, b| {
        pos_of(&a.id)
            .cmp(&pos_of(&b.id))
            .then_with(|| a.sort_order.cmp(&b.sort_order))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(items)
}

/// Read the authoritative journal display order from the synced `user_profile`
/// singleton. The field lives under `unifiedJournalOrder` and contains the
/// journal IDs for both personal and shared journals, interleaved in the
/// order the user chose in the main app. Values are sometimes encoded as
/// JSON numbers and sometimes as strings, so both are normalized to strings.
/// Returns an empty vec when the row is missing (the old fallback ordering —
/// sort_order then alpha — kicks in).
fn load_user_journal_order(store: &Store) -> Vec<String> {
    let Ok(Some(raw)) = store.get_json_row_by_id("user_profile", "1") else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    let arr = value
        .get("unifiedJournalOrder")
        .or_else(|| value.get("journalOrder"))
        .and_then(Value::as_array);
    let Some(arr) = arr else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .collect()
}

pub fn load_entries(store: &Store, journal_id: &str) -> Result<Vec<EntryPreview>> {
    let page = ListPageRequest {
        limit: Some(LARGE_LIMIT),
        offset: None,
        cursor: None,
    };
    let result = store
        .list_entries_json_rows_by_journal_id_paged(
            journal_id,
            &page,
            EntryListSort::Desc,
            EntryListSortMethod::EntryDate,
        )
        .with_context(|| format!("failed to list entries for journal {journal_id}"))?;
    let mut items = Vec::with_capacity(result.rows.len());
    for raw in result.rows {
        let value: Value = serde_json::from_str(&raw).context("invalid entry json")?;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            continue;
        }
        let body = extract_entry_body(&value);
        let entry_date = extract_entry_date(&value);
        let tags = value
            .get("tags")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str().map(ToOwned::to_owned))
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let media_count = count_media(&value);
        items.push(EntryPreview {
            id,
            entry_date,
            preview: super::preview::preview(&body, 160),
            tags,
            media_count,
        });
    }
    Ok(items)
}

fn count_media(value: &Value) -> usize {
    let direct = value
        .get("moments")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    if direct > 0 {
        return direct;
    }
    value
        .get("payload")
        .and_then(|p| p.get("moments"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

fn extract_entry_date(value: &Value) -> String {
    if let Some(s) = value.get("creation_date").and_then(Value::as_str)
        && !s.is_empty()
    {
        return format_iso8601_date(s);
    }
    match value.get("date") {
        Some(Value::String(s)) if !s.is_empty() => {
            if let Ok(ms) = s.parse::<i64>() {
                format_epoch_ms_date(ms)
            } else {
                format_iso8601_date(s)
            }
        }
        Some(Value::Number(n)) => {
            if let Some(ms) = n.as_i64() {
                format_epoch_ms_date(ms)
            } else if let Some(ms) = n.as_f64() {
                format_epoch_ms_date(ms as i64)
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

fn format_epoch_ms_date(ms: i64) -> String {
    let secs = ms / 1000;
    let Ok(odt) = time::OffsetDateTime::from_unix_timestamp(secs) else {
        return String::new();
    };
    format_offset_datetime(odt)
}

fn format_iso8601_date(raw: &str) -> String {
    // Accept anything starting with YYYY-MM-DD; format via time crate for consistency.
    if raw.len() >= 10 && raw.as_bytes()[4] == b'-' && raw.as_bytes()[7] == b'-' {
        let year: i32 = raw[0..4].parse().unwrap_or(0);
        let month: u8 = raw[5..7].parse().unwrap_or(1);
        let day: u8 = raw[8..10].parse().unwrap_or(1);
        if let Ok(date) = time::Date::from_calendar_date(
            year,
            time::Month::try_from(month).unwrap_or(time::Month::January),
            day,
        ) {
            let weekday = short_weekday(date.weekday());
            let month_name = short_month(date.month());
            return format!("{weekday}, {month_name} {}, {year}", date.day());
        }
    }
    raw.to_string()
}

fn format_offset_datetime(odt: time::OffsetDateTime) -> String {
    let weekday = short_weekday(odt.weekday());
    let month_name = short_month(odt.month());
    format!("{weekday}, {month_name} {}, {}", odt.day(), odt.year())
}

fn short_weekday(w: time::Weekday) -> &'static str {
    match w {
        time::Weekday::Monday => "Mon",
        time::Weekday::Tuesday => "Tue",
        time::Weekday::Wednesday => "Wed",
        time::Weekday::Thursday => "Thu",
        time::Weekday::Friday => "Fri",
        time::Weekday::Saturday => "Sat",
        time::Weekday::Sunday => "Sun",
    }
}

fn short_month(m: time::Month) -> &'static str {
    match m {
        time::Month::January => "Jan",
        time::Month::February => "Feb",
        time::Month::March => "Mar",
        time::Month::April => "Apr",
        time::Month::May => "May",
        time::Month::June => "Jun",
        time::Month::July => "Jul",
        time::Month::August => "Aug",
        time::Month::September => "Sep",
        time::Month::October => "Oct",
        time::Month::November => "Nov",
        time::Month::December => "Dec",
    }
}

pub fn load_entry_body(store: &Store, entry_id: &str) -> Result<EntryBody> {
    let raw = store
        .get_json_row_by_id("entries", entry_id)
        .with_context(|| format!("failed to load entry {entry_id}"))?
        .ok_or_else(|| anyhow::anyhow!("entry {entry_id} not found"))?;
    let value: Value = serde_json::from_str(&raw).context("invalid entry json")?;
    Ok(EntryBody {
        body_markdown: extract_entry_body(&value),
    })
}

pub fn load_daily_chat_days(store: &Store) -> Result<Vec<DailyChatDay>> {
    let page = ListPageRequest {
        limit: Some(LARGE_LIMIT),
        offset: None,
        cursor: None,
    };
    let result = store
        .list_json_rows_paged(ListJsonTable::DailyChatFeed, &page)
        .context("failed to list daily chat feed")?;
    let mut days = Vec::with_capacity(result.rows.len());
    for raw in result.rows {
        let value: Value = serde_json::from_str(&raw).context("invalid daily chat json")?;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            continue;
        }
        let date = value
            .get("date")
            .and_then(Value::as_str)
            .unwrap_or(&id)
            .to_string();
        let message_count = messages_array(&value).map(|arr| arr.len()).unwrap_or(0);
        days.push(DailyChatDay {
            id,
            date,
            message_count,
        });
    }
    days.sort_by(|a, b| b.date.cmp(&a.date));
    Ok(days)
}

pub fn load_daily_chat_messages(store: &Store, id: &str) -> Result<Vec<DailyChatMessage>> {
    let raw = store
        .get_json_row_by_id("daily_chat_feed", id)
        .with_context(|| format!("failed to load daily chat {id}"))?;
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let value: Value = serde_json::from_str(&raw).context("invalid daily chat json")?;
    let Some(arr) = messages_array(&value) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for msg in arr {
        let role = msg
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user")
            .to_string();
        let text = msg
            .get("text")
            .or_else(|| msg.get("content"))
            .or_else(|| msg.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let created_at = msg
            .get("created_at")
            .or_else(|| msg.get("timestamp"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        out.push(DailyChatMessage {
            role,
            text,
            created_at,
        });
    }
    Ok(out)
}

fn extract_entry_body(value: &Value) -> String {
    value
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| value.get("body").and_then(Value::as_str))
        .or_else(|| value.get("markdown").and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

fn messages_array(value: &Value) -> Option<&Vec<Value>> {
    value
        .get("chat_history")
        .and_then(Value::as_array)
        .or_else(|| value.get("messages").and_then(Value::as_array))
        .or_else(|| {
            value
                .get("data")
                .and_then(|d| d.get("messages"))
                .and_then(Value::as_array)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_store(name: &str) -> Store {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path: PathBuf = std::env::temp_dir().join(format!("dayone-cli-tui-{name}-{unique}.db"));
        Store::open_at(&path).expect("store")
    }

    #[test]
    fn load_journals_filters_deleted_and_sorts_by_sort_order() {
        let store = test_store("journals");
        store
            .upsert_json_row(
                "journals",
                "j-a",
                Some("2026-04-01T00:00:00.000Z"),
                None,
                r#"{"id":"j-a","name":"A","sort_order":2}"#,
            )
            .unwrap();
        store
            .upsert_json_row(
                "journals",
                "j-b",
                Some("2026-04-02T00:00:00.000Z"),
                None,
                r#"{"id":"j-b","name":"B","sort_order":1}"#,
            )
            .unwrap();
        store
            .upsert_json_row(
                "journals",
                "j-c",
                Some("2026-04-03T00:00:00.000Z"),
                Some("2026-04-04T00:00:00.000Z"),
                r#"{"id":"j-c","name":"C","sort_order":0}"#,
            )
            .unwrap();

        let journals = load_journals(&store).unwrap();
        let names: Vec<_> = journals.iter().map(|j| j.name.clone()).collect();
        assert_eq!(names, vec!["B", "A"]);
    }

    #[test]
    fn load_journals_uses_unified_journal_order_from_user_profile() {
        let store = test_store("journals-user-order");
        for (id, name) in [("j-a", "A"), ("j-b", "B"), ("j-c", "C")] {
            store
                .upsert_json_row(
                    "journals",
                    id,
                    Some("2026-04-01T00:00:00.000Z"),
                    None,
                    &format!(r#"{{"id":"{id}","name":"{name}"}}"#),
                )
                .unwrap();
        }
        // Mixed-type array (numbers and strings), matching what the server
        // actually emits. Order intentionally differs from alpha sort.
        store
            .upsert_singleton_json_row(
                "user_profile",
                1,
                r#"{"unifiedJournalOrder":["j-c", "j-a", "j-b"]}"#,
                Some("2026-04-10T00:00:00.000Z"),
            )
            .unwrap();

        let journals = load_journals(&store).unwrap();
        let names: Vec<_> = journals.iter().map(|j| j.name.clone()).collect();
        assert_eq!(names, vec!["C", "A", "B"]);
    }

    #[test]
    fn load_journals_falls_back_to_sort_order_when_user_profile_missing() {
        // Same as the existing sort_order test but ensures it still passes
        // when there's no user_profile row at all.
        let store = test_store("journals-no-profile");
        store
            .upsert_json_row(
                "journals",
                "j-a",
                Some("2026-04-01T00:00:00.000Z"),
                None,
                r#"{"id":"j-a","name":"A","sort_order":2}"#,
            )
            .unwrap();
        store
            .upsert_json_row(
                "journals",
                "j-b",
                Some("2026-04-02T00:00:00.000Z"),
                None,
                r#"{"id":"j-b","name":"B","sort_order":1}"#,
            )
            .unwrap();

        let journals = load_journals(&store).unwrap();
        let names: Vec<_> = journals.iter().map(|j| j.name.clone()).collect();
        assert_eq!(names, vec!["B", "A"]);
    }

    #[test]
    fn load_entries_returns_descending_and_has_preview() {
        let store = test_store("entries");
        store
            .upsert_json_row(
                "journals",
                "j-1",
                Some("2026-04-01T00:00:00.000Z"),
                None,
                r#"{"id":"j-1","name":"J"}"#,
            )
            .unwrap();
        store
            .upsert_entry_json_row(
                "e-1",
                "j-1",
                Some("2026-04-10T12:00:00.000Z"),
                None,
                r##"{"id":"e-1","journal_id":"j-1","creation_date":"2026-04-10T12:00:00.000Z","text":"# First\nbody"}"##,
            )
            .unwrap();
        store
            .upsert_entry_json_row(
                "e-2",
                "j-1",
                Some("2026-04-12T12:00:00.000Z"),
                None,
                r#"{"id":"e-2","journal_id":"j-1","creation_date":"2026-04-12T12:00:00.000Z","text":"Second body line"}"#,
            )
            .unwrap();

        let entries = load_entries(&store, "j-1").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "e-2");
        assert_eq!(entries[0].entry_date, "Sun, Apr 12, 2026");
        assert_eq!(entries[0].preview, "Second body line");
        assert_eq!(entries[1].id, "e-1");
        assert_eq!(entries[1].preview, "First");
    }

    #[test]
    fn load_entry_body_extracts_markdown() {
        let store = test_store("entry-body");
        store
            .upsert_json_row(
                "journals",
                "j-1",
                Some("2026-04-01T00:00:00.000Z"),
                None,
                r#"{"id":"j-1","name":"J"}"#,
            )
            .unwrap();
        store
            .upsert_entry_json_row(
                "e-1",
                "j-1",
                Some("2026-04-10T12:00:00.000Z"),
                None,
                r##"{"id":"e-1","journal_id":"j-1","creation_date":"2026-04-10T12:00:00.000Z","text":"# Hi\n\nworld"}"##,
            )
            .unwrap();

        let body = load_entry_body(&store, "e-1").unwrap();
        assert_eq!(body.body_markdown, "# Hi\n\nworld");
    }

    #[test]
    fn load_daily_chat_days_counts_messages_and_sorts_desc() {
        let store = test_store("chats");
        store
            .upsert_json_row(
                "daily_chat_feed",
                "2026-04-10",
                Some("2026-04-10T00:00:00.000Z"),
                None,
                r#"{"id":"2026-04-10","date":"2026-04-10","chat_history":[{"role":"user","text":"hi"}]}"#,
            )
            .unwrap();
        store
            .upsert_json_row(
                "daily_chat_feed",
                "2026-04-12",
                Some("2026-04-12T00:00:00.000Z"),
                None,
                r#"{"id":"2026-04-12","date":"2026-04-12","chat_history":[{"role":"user","text":"a"},{"role":"assistant","text":"b"}]}"#,
            )
            .unwrap();

        let days = load_daily_chat_days(&store).unwrap();
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].date, "2026-04-12");
        assert_eq!(days[0].message_count, 2);
        assert_eq!(days[1].date, "2026-04-10");
        assert_eq!(days[1].message_count, 1);
    }
}
