use serde_json::Value;

use crate::store::{StoreError, StoreResult};

pub fn updated_tags(
    entry: &Value,
    additions: &[String],
    removals: &[String],
) -> StoreResult<Vec<String>> {
    if additions
        .iter()
        .chain(removals)
        .any(|tag| tag.trim().is_empty())
    {
        return Err(StoreError::invalid_input("tag names must not be empty"));
    }
    let mut tags = match entry.get("tags") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(tags)) => tags
            .iter()
            .map(|tag| {
                tag.as_str().map(str::to_owned).ok_or_else(|| {
                    StoreError::invalid_input("entry tags must be an array of strings")
                })
            })
            .collect::<StoreResult<Vec<_>>>()?,
        _ => {
            return Err(StoreError::invalid_input(
                "entry tags must be an array of strings",
            ));
        }
    };
    for tag in additions {
        if !tags.contains(tag) {
            tags.push(tag.clone());
        }
    }
    tags.retain(|tag| !removals.contains(tag));
    Ok(tags)
}
