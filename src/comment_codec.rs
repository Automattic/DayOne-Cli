use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::Value;

use crate::sync::crypto::encrypt_entry_content_for_journal;

fn journal_uses_e2e(journal: &Value) -> bool {
    journal
        .get("encryption")
        .and_then(|value| value.get("vault"))
        .is_some()
}

pub fn encode_comment_content_for_journal(
    journal: &Value,
    content: &str,
    master_key: Option<&str>,
    user_keys_json: Option<&str>,
) -> Result<String> {
    if journal_uses_e2e(journal) {
        let encrypted = encrypt_entry_content_for_journal(
            journal,
            &Value::String(content.to_owned()),
            master_key,
            user_keys_json,
        )?;
        return Ok(BASE64_STANDARD.encode(encrypted.bytes));
    }
    Ok(BASE64_STANDARD.encode(content.as_bytes()))
}

pub fn decode_comment_content_for_journal(journal: &Value, encoded: &str) -> String {
    if encoded.trim().is_empty() {
        return String::new();
    }
    if journal_uses_e2e(journal) {
        // We preserve encrypted payloads for now; callers can still surface raw content.
        return encoded.to_owned();
    }
    BASE64_STANDARD
        .decode(encoded)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| encoded.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decode_plain_comment_content_round_trips_base64() {
        let journal = json!({"id": "journal-1"});
        let encoded = BASE64_STANDARD.encode("hello world");
        let decoded = decode_comment_content_for_journal(&journal, &encoded);
        assert_eq!(decoded, "hello world");
    }

    #[test]
    fn decode_plain_comment_content_falls_back_to_raw_string() {
        let journal = json!({"id": "journal-1"});
        let decoded = decode_comment_content_for_journal(&journal, "not-base64");
        assert_eq!(decoded, "not-base64");
    }
}
