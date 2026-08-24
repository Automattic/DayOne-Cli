use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub fn extract_edit_timestamp(item: &Value) -> Option<i128> {
    [
        "userEditDate",
        "user_edit_date",
        "editDate",
        "updatedAt",
        "updated_at",
    ]
    .into_iter()
    .find_map(|key| item.get(key).and_then(timestamp_value_to_epoch_ms))
    .or_else(|| {
        item.get("revision")
            .and_then(|revision| revision.get("editDate"))
            .and_then(timestamp_value_to_epoch_ms)
    })
}

fn timestamp_value_to_epoch_ms(value: &Value) -> Option<i128> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .map(i128::from)
            .or_else(|| number.as_u64().and_then(|value| i128::try_from(value).ok())),
        Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return None;
            }
            trimmed.parse::<i128>().ok().or_else(|| {
                OffsetDateTime::parse(trimmed, &Rfc3339)
                    .ok()
                    .map(|dt| i128::from(dt.unix_timestamp_nanos() / 1_000_000))
            })
        }
        _ => None,
    }
}

pub fn local_is_newer(local: &Value, server: &Value) -> bool {
    match (
        extract_edit_timestamp(local),
        extract_edit_timestamp(server),
    ) {
        (Some(local_ts), Some(server_ts)) => local_ts > server_ts,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prefers_user_edit_date_when_present() {
        let local = json!({ "user_edit_date": "2026-03-06T12:00:00Z" });
        let server = json!({
            "user_edit_date": "2026-03-06T10:00:00Z",
            "updated_at": "2026-03-06T13:00:00Z"
        });
        assert_eq!(
            extract_edit_timestamp(&local),
            Some(1_772_798_400_000),
            "ISO strings should parse as epoch milliseconds"
        );
        assert!(local_is_newer(&local, &server));
    }

    #[test]
    fn prefers_camel_case_user_edit_date_over_snake_case() {
        let item = json!({
            "userEditDate": 1_700_000_002_000_i64,
            "user_edit_date": 1_700_000_001_000_i64,
            "updatedAt": 1_700_000_000_000_i64
        });
        assert_eq!(extract_edit_timestamp(&item), Some(1_700_000_002_000));
    }

    #[test]
    fn compares_numeric_server_contract_edit_dates() {
        let local = json!({ "userEditDate": 1_700_000_002_000_i64 });
        let server = json!({ "revision": { "editDate": 1_700_000_001_000_i64 } });
        assert!(local_is_newer(&local, &server));
    }

    #[test]
    fn falls_back_to_updated_at() {
        let local = json!({ "updated_at": "2026-03-06T10:00:00Z" });
        let server = json!({ "updatedAt": "2026-03-06T12:00:00Z" });
        assert!(!local_is_newer(&local, &server));
    }
}
