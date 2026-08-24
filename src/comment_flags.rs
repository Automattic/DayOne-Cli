use serde_json::Value;

pub(crate) fn comment_deleted_flag(value: &Value) -> bool {
    for key in ["deleted_at", "deletedAt"] {
        if let Some(raw) = value.get(key) {
            match raw {
                Value::Null => {}
                Value::String(text) => {
                    if !text.trim().is_empty() {
                        return true;
                    }
                }
                _ => return true,
            }
        }
    }
    false
}
