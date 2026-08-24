use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use rsa::rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::commands::search;
use crate::http::DayOneApiClient;
use crate::http::client::DayOneClient;
use crate::store::sqlite::Store;
use crate::util::now_rfc3339_utc;

const MEMORY_UPDATES_PATH: &str = "/labs/ai/daily-chat/ephemeral/memory-updates";
const MEMORY_TOOL_NAME: &str = "in_house_memory";
const MAX_MEMORY_UPDATE_LOOPS: usize = 60;
const MEMORY_UPDATE_RETRY_DELAY_MS: u64 = 50;
const MEMORY_SEARCH_THRESHOLD: f64 = 0.45;
const MEMORY_SEARCH_LIMIT: usize = 50;

#[derive(Debug, Clone)]
pub struct MemoryProcessArgs {
    pub json: Option<String>,
    pub json_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MemoryChange {
    pub memory_id: String,
    pub change: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryProcessOutput {
    pub ok: bool,
    pub base_url: String,
    pub profile_id: i64,
    pub processed: bool,
    pub skipped_reason: Option<String>,
    pub api_loops: usize,
    pub new_message_count: usize,
    pub context_message_count: usize,
    pub last_memory_check_message_id: Option<String>,
    pub updated_summary: Option<String>,
    pub memory_changes: Vec<MemoryChange>,
}

#[derive(Debug, Deserialize)]
struct ProcessInput {
    conversation: Option<ConversationInput>,
    messages: Option<Vec<Value>>,
    last_memory_check_message_id: Option<String>,
    existing_summary: Option<String>,
    date: Option<String>,
    timezone: Option<String>,
    locale: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConversationInput {
    messages: Vec<Value>,
    last_memory_check_message_id: Option<String>,
}

pub async fn execute(
    store: &Store,
    base_url: &str,
    args: MemoryProcessArgs,
) -> Result<MemoryProcessOutput> {
    let session = store
        .get_auth_session_for_base_url(base_url)?
        .ok_or_else(crate::telemetry::UserError::auth_required)?;
    let client = DayOneClient::new(base_url)?.with_bearer_token(&session.token)?;
    let input = parse_input(args)?;
    execute_with_client(store, &client, base_url, session.profile_id, input).await
}

async fn execute_with_client<C: DayOneApiClient>(
    store: &Store,
    api: &C,
    base_url: &str,
    profile_id: i64,
    input: ProcessInput,
) -> Result<MemoryProcessOutput> {
    let all_messages = resolve_messages(&input)?;
    let last_check_id = resolve_last_check_id(&input);
    let last_check_index = last_check_id.as_ref().and_then(|id| {
        all_messages
            .iter()
            .position(|msg| message_id(msg) == Some(id.as_str()))
    });

    let messages_since_last_check = if let Some(index) = last_check_index {
        all_messages[index + 1..].to_vec()
    } else {
        all_messages.clone()
    };
    let last_non_meta_id = messages_since_last_check
        .iter()
        .rev()
        .find(|message| !is_meta_message(message) && !is_deleted_message(message))
        .and_then(message_id)
        .map(ToOwned::to_owned);

    let cleaned_new_messages = clean_messages_for_memory_api(&messages_since_last_check);
    let cleaned_context_messages = if let Some(index) = last_check_index {
        let start = index.saturating_sub(4);
        clean_messages_for_memory_api(&all_messages[start..index])
    } else {
        Vec::new()
    };
    let latest_non_meta_all_messages = all_messages
        .iter()
        .rev()
        .find(|message| !is_meta_message(message) && !is_deleted_message(message))
        .and_then(message_id)
        .map(ToOwned::to_owned);

    if cleaned_new_messages.is_empty() {
        return Ok(MemoryProcessOutput {
            ok: true,
            base_url: base_url.to_owned(),
            profile_id,
            processed: false,
            skipped_reason: Some("no_new_messages".to_owned()),
            api_loops: 0,
            new_message_count: 0,
            context_message_count: cleaned_context_messages.len(),
            last_memory_check_message_id: last_check_id.or(latest_non_meta_all_messages),
            updated_summary: None,
            memory_changes: Vec::new(),
        });
    }

    let has_user_messages = cleaned_new_messages
        .iter()
        .any(|message| message_role(message) == Some("user"));
    if !has_user_messages {
        return Ok(MemoryProcessOutput {
            ok: true,
            base_url: base_url.to_owned(),
            profile_id,
            processed: false,
            skipped_reason: Some("no_new_user_messages".to_owned()),
            api_loops: 0,
            new_message_count: cleaned_new_messages.len(),
            context_message_count: cleaned_context_messages.len(),
            last_memory_check_message_id: last_non_meta_id,
            updated_summary: None,
            memory_changes: Vec::new(),
        });
    }

    let mut continuation: Option<Value> = None;
    let mut updated_summary: Option<String> = None;
    let mut memory_changes: Vec<MemoryChange> = Vec::new();
    let mut api_loops = 0_usize;

    loop {
        if api_loops >= MAX_MEMORY_UPDATE_LOOPS {
            bail!(
                "memory processing exceeded max loops ({MAX_MEMORY_UPDATE_LOOPS}); possible infinite loop"
            );
        }
        api_loops += 1;

        let request_body = if let Some(continuation_payload) = continuation.as_ref() {
            json!({
                "tools": memory_tools(),
                "continuation": continuation_payload,
            })
        } else {
            json!({
                "tools": memory_tools(),
                "start": {
                    "new_messages": cleaned_new_messages,
                    "context_messages": cleaned_context_messages,
                    "existing_summary": input.existing_summary.clone(),
                    "date": input.date.clone(),
                    "timezone": input.timezone.clone(),
                    "locale": input.locale.clone(),
                }
            })
        };

        let response = api
            .post_json(MEMORY_UPDATES_PATH, &request_body)
            .await
            .context("failed to POST memory updates")?;

        if let Some(summary) = response.get("updated_summary").and_then(Value::as_str) {
            updated_summary = Some(summary.to_owned());
        }

        match response.get("status").and_then(Value::as_str) {
            Some("complete") => break,
            Some("processing") => {
                let tool_calls = response
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if let Some(unexpected) = tool_calls
                    .iter()
                    .find(|tool_call| tool_name(tool_call).as_deref() != Some(MEMORY_TOOL_NAME))
                {
                    let name = tool_name(unexpected).unwrap_or_else(|| "<missing>".to_owned());
                    bail!("memory updates returned unsupported tool call '{name}'");
                }
                let mut tool_results: Vec<Value> = Vec::new();
                for tool_call in tool_calls.iter() {
                    let tool_call_id = tool_call
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| anyhow!("memory tool call missing non-empty id"))?;
                    let arguments = tool_call
                        .get("function")
                        .and_then(Value::as_object)
                        .and_then(|f| f.get("arguments"))
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow!("memory tool call missing function.arguments"))?;
                    let args_json: Value = serde_json::from_str(arguments)
                        .context("memory tool call arguments are not valid JSON")?;
                    let tool_result =
                        apply_in_house_memory_tool(store, &args_json, &mut memory_changes)?;
                    let tool_result_content = serde_json::to_string(&tool_result)
                        .context("failed to serialize memory tool result")?;
                    tool_results.push(json!({
                        "id": generate_id("tool"),
                        "timestamp": now_rfc3339_utc(),
                        "role": "tool",
                        "content": tool_result_content,
                        "tool_call_id": tool_call_id,
                    }));
                }

                let continuation_payload = response
                    .get("continuation")
                    .ok_or_else(|| anyhow!("memory updates response missing continuation"))?;
                let continuation_object = continuation_payload
                    .as_object()
                    .ok_or_else(|| anyhow!("memory updates continuation must be an object"))?;
                let memories = continuation_object
                    .get("memories")
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| {
                        anyhow!("memory updates continuation.memories must be an array")
                    })?;
                let memories_queue = continuation_object
                    .get("memories_queue")
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| {
                        anyhow!("memory updates continuation.memories_queue must be an array")
                    })?;
                let mut history = continuation_object
                    .get("conversation_history")
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| {
                        anyhow!("memory updates continuation.conversation_history must be an array")
                    })?;
                if !tool_calls.is_empty() {
                    history.push(json!({
                        "id": generate_id("assistant"),
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": tool_calls,
                        "timestamp": now_rfc3339_utc(),
                    }));
                    history.extend(tool_results);
                }
                continuation = Some(json!({
                    "memories": memories,
                    "memories_queue": memories_queue,
                    "conversation_history": history,
                }));
                tokio::time::sleep(Duration::from_millis(MEMORY_UPDATE_RETRY_DELAY_MS)).await;
            }
            Some(other) => bail!("invalid memory updates status '{other}'"),
            None => bail!("memory updates response missing status"),
        }
    }

    Ok(MemoryProcessOutput {
        ok: true,
        base_url: base_url.to_owned(),
        profile_id,
        processed: true,
        skipped_reason: None,
        api_loops,
        new_message_count: cleaned_new_messages.len(),
        context_message_count: cleaned_context_messages.len(),
        last_memory_check_message_id: last_non_meta_id,
        updated_summary,
        memory_changes,
    })
}

fn parse_input(args: MemoryProcessArgs) -> Result<ProcessInput> {
    if args.json.is_some() && args.json_file.is_some() {
        bail!("use either --json or --json-file, not both");
    }
    let raw = if let Some(json) = args.json {
        json
    } else if let Some(path) = args.json_file {
        fs::read_to_string(&path).with_context(|| format!("failed to read JSON file '{path}'"))?
    } else {
        bail!("missing payload; provide --json or --json-file");
    };
    serde_json::from_str(&raw).context("invalid memory process JSON payload")
}

fn resolve_messages(input: &ProcessInput) -> Result<Vec<Value>> {
    let messages = if let Some(conversation) = input.conversation.as_ref() {
        conversation.messages.clone()
    } else if let Some(messages) = input.messages.as_ref() {
        messages.clone()
    } else {
        bail!("payload must include either 'conversation.messages' or top-level 'messages'");
    };
    validate_messages(&messages)?;
    Ok(messages)
}

fn validate_messages(messages: &[Value]) -> Result<()> {
    for (index, message) in messages.iter().enumerate() {
        let object = message
            .as_object()
            .ok_or_else(|| anyhow!("message at index {index} must be a JSON object"))?;
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("message at index {index} is missing string field 'id'"))?;
        if id.trim().is_empty() {
            bail!("message at index {index} has empty 'id'");
        }
        let role = object
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("message at index {index} is missing string field 'role'"))?;
        if role.trim().is_empty() {
            bail!("message at index {index} has empty 'role'");
        }
        if let Some(content) = object.get("content")
            && !content.is_string()
            && !content.is_null()
        {
            bail!("message at index {index} has invalid 'content' type; expected string or null");
        }
    }
    Ok(())
}

fn resolve_last_check_id(input: &ProcessInput) -> Option<String> {
    if let Some(conversation) = input.conversation.as_ref() {
        return conversation.last_memory_check_message_id.clone();
    }
    input.last_memory_check_message_id.clone()
}

fn clean_messages_for_memory_api(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .filter(|message| !is_meta_message(message))
        .filter(|message| !is_deleted_message(message))
        .map(|message| {
            let mut cleaned = message.clone();
            if let Some(object) = cleaned.as_object_mut() {
                object.remove("memory_changes");
            }
            cleaned
        })
        .collect()
}

fn is_meta_message(message: &Value) -> bool {
    message_role(message) == Some("meta")
}

fn is_deleted_message(message: &Value) -> bool {
    message
        .as_object()
        .and_then(|object| object.get("deletion_requested"))
        .is_some_and(|value| !value.is_null())
}

fn message_role(message: &Value) -> Option<&str> {
    message
        .as_object()
        .and_then(|object| object.get("role"))
        .and_then(Value::as_str)
}

fn message_id(message: &Value) -> Option<&str> {
    message
        .as_object()
        .and_then(|object| object.get("id"))
        .and_then(Value::as_str)
}

fn tool_name(tool_call: &Value) -> Option<String> {
    tool_call
        .get("function")
        .and_then(Value::as_object)
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn apply_in_house_memory_tool(
    store: &Store,
    args: &Value,
    memory_changes: &mut Vec<MemoryChange>,
) -> Result<Value> {
    let command = args
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("memory tool arguments missing 'command'"))?;
    match command {
        "search" => {
            let terms = search_terms_from_tool_args(args);
            let mut memories: Vec<Value> = Vec::new();
            let mut found_ids = std::collections::HashSet::<String>::new();

            for term in terms {
                let result = search::context_items(
                    store,
                    search::SearchContextItemsArgs {
                        query: term,
                        threshold: MEMORY_SEARCH_THRESHOLD,
                        limit: Some(MEMORY_SEARCH_LIMIT),
                        category: Some("in-house-memory".to_owned()),
                    },
                )?;
                for item in result.items {
                    if found_ids.contains(&item.id) {
                        continue;
                    }
                    let content = item
                        .item
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let created_at = item.item.get("created_at").and_then(Value::as_str);
                    let updated_at = item.item.get("updated_at").and_then(Value::as_str);
                    found_ids.insert(item.id.clone());
                    memories.push(json!({
                        "id": item.id,
                        "content": content,
                        "similarity": item.similarity,
                        "created_at": created_at,
                        "updated_at": updated_at,
                    }));
                }
            }
            Ok(json!({
                "success": true,
                "memories": memories,
                "total": memories.len(),
            }))
        }
        "create" => {
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("memory create requires 'content'"))?;
            let id = generate_id("memory");
            let now_iso = now_rfc3339_utc();
            let item = json!({
                "id": id,
                "content": content,
                "category": "in-house-memory",
                "source": {
                    "type": "manual",
                    "manual_source": "ai_memory_reconciliation",
                },
                "tags": [],
                "updated_at": now_iso,
            });
            save_context_item(store, &item)?;
            memory_changes.push(MemoryChange {
                memory_id: id.clone(),
                change: "create".to_owned(),
            });
            Ok(json!({
                "success": true,
                "id": id,
                "content": content,
            }))
        }
        "update" => {
            let updates = args
                .get("updates")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("memory update requires 'updates' array"))?;
            let mut results = Vec::new();
            let mut updated = 0_u64;
            let mut failed = 0_u64;
            for update in updates {
                let id = update
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("memory update item requires id"))?;
                let content = update
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("memory update item requires content"))?;
                if let Some(existing_json) =
                    store.get_json_row_by_id("context_library_items", id)?
                {
                    let mut existing_value: Value = serde_json::from_str(&existing_json)
                        .with_context(|| format!("failed to parse local context item '{id}'"))?;
                    let now_iso = now_rfc3339_utc();
                    if let Some(object) = existing_value.as_object_mut() {
                        object.insert("content".to_owned(), Value::String(content.to_owned()));
                        object.insert("updated_at".to_owned(), Value::String(now_iso));
                        object.insert("deleted_at".to_owned(), Value::Null);
                    }
                    save_context_item(store, &existing_value)?;
                    updated += 1;
                    memory_changes.push(MemoryChange {
                        memory_id: id.to_owned(),
                        change: "update".to_owned(),
                    });
                    results.push(json!({
                        "id": id,
                        "success": true,
                        "content": content,
                    }));
                } else {
                    failed += 1;
                    results.push(json!({
                        "id": id,
                        "success": false,
                        "error": "Memory not found",
                    }));
                }
            }
            Ok(json!({
                "success": true,
                "results": results,
                "updated": updated,
                "failed": failed,
            }))
        }
        "delete" => {
            let ids_csv = args
                .get("ids")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("memory delete requires 'ids'"))?;
            let ids: Vec<String> = ids_csv
                .split(',')
                .map(|id| id.trim())
                .filter(|id| !id.is_empty())
                .map(ToOwned::to_owned)
                .collect();
            let mut deleted = 0_u64;
            for id in &ids {
                if let Some(existing_json) =
                    store.get_json_row_by_id("context_library_items", id)?
                {
                    let mut existing_value: Value = serde_json::from_str(&existing_json)
                        .with_context(|| format!("failed to parse local context item '{id}'"))?;
                    let now_iso = now_rfc3339_utc();
                    if let Some(object) = existing_value.as_object_mut() {
                        object.insert("updated_at".to_owned(), Value::String(now_iso.clone()));
                        object.insert("deleted_at".to_owned(), Value::String(now_iso));
                    }
                    save_context_item(store, &existing_value)?;
                    deleted += 1;
                    memory_changes.push(MemoryChange {
                        memory_id: id.clone(),
                        change: "delete".to_owned(),
                    });
                }
            }
            Ok(json!({
                "success": true,
                "deleted": deleted,
                "total": ids.len(),
            }))
        }
        other => bail!("unsupported in_house_memory command '{other}'"),
    }
}

fn save_context_item(store: &Store, value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("context item payload must be an object"))?;
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("context item payload missing id"))?;
    let updated_at = object.get("updated_at").and_then(Value::as_str);
    let deleted_at = object.get("deleted_at").and_then(Value::as_str);
    let serialized = serde_json::to_string(value).context("failed to serialize context item")?;
    store.upsert_json_row_with_sync_state(
        "context_library_items",
        id,
        updated_at,
        deleted_at,
        Some(true),
        &serialized,
    )?;
    let outbox_id = format!("context_item:{id}");
    store.enqueue_outbox_item(
        &outbox_id,
        "context_item",
        "update",
        id,
        &serialized,
        now_epoch_ms(),
    )?;
    Ok(())
}

fn memory_tools() -> Vec<Value> {
    vec![json!({
        "type": "function",
        "function": {
            "name": MEMORY_TOOL_NAME,
            "description": "Manage the user's long-term memories. Use command 'create' to store a new memory, 'update' to modify existing memories, 'delete' to remove memories, or 'search' to find relevant memories.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "enum": ["search", "create", "update", "delete"],
                        "description": "The memory operation to perform."
                    },
                    "content": {
                        "type": "string",
                        "description": "The memory content to store. Required for 'create'."
                    },
                    "query": {
                        "type": "string",
                        "description": "Legacy single search query string for 'search'. Prefer 'terms'."
                    },
                    "terms": {
                        "type": "string",
                        "description": "Comma-separated search terms for 'search' (Web parity)."
                    },
                    "updates": {
                        "type": "array",
                        "description": "Array of memory updates. Required for 'update'.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "ID of the memory to update." },
                                "content": { "type": "string", "description": "New content for the memory." }
                            },
                            "required": ["id", "content"]
                        }
                    },
                    "ids": {
                        "type": "string",
                        "description": "Comma-separated list of memory IDs to delete. Required for 'delete'."
                    }
                },
                "required": ["command"]
            }
        }
    })]
}

fn search_terms_from_tool_args(args: &Value) -> Vec<String> {
    let terms_raw = args
        .get("terms")
        .and_then(Value::as_str)
        .or_else(|| args.get("query").and_then(Value::as_str))
        .unwrap_or("");
    terms_raw
        .split(',')
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn generate_id(prefix: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(now_epoch_ms().to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    let mut nonce = [0_u8; 16];
    let mut rng = OsRng;
    rng.fill_bytes(&mut nonce);
    hasher.update(nonce);
    let digest = hasher.finalize();
    let hex = format!("{:x}", digest);
    format!("{prefix}-{}", &hex[0..16])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::FakeApiClient;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn open_test_store(name: &str) -> (TempDir, Store) {
        let temp_dir = TempDir::new().unwrap_or_else(|err| {
            panic!("failed to create temporary directory for '{name}': {err}")
        });
        let path = temp_dir.path().join(format!("{name}.db"));
        let store = Store::open_at(&path)
            .unwrap_or_else(|err| panic!("failed to open temporary store for '{name}': {err}"));
        (temp_dir, store)
    }

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/memory-process")
            .join(name)
    }

    fn read_fixture_json(name: &str) -> Value {
        let raw = fs::read_to_string(fixture_path(name))
            .unwrap_or_else(|err| panic!("failed reading fixture '{name}': {err}"));
        serde_json::from_str(&raw)
            .unwrap_or_else(|err| panic!("failed parsing fixture '{name}' as json: {err}"))
    }

    #[test]
    fn clean_messages_excludes_meta_and_memory_changes() {
        let messages = vec![
            json!({"id":"m1","role":"meta","content":"skip"}),
            json!({"id":"m2","role":"assistant","content":"ok","memory_changes":[{"x":1}]}),
            json!({"id":"m3","role":"user","content":"ok","deletion_requested":"2026-01-01T00:00:00.000Z"}),
        ];
        let cleaned = clean_messages_for_memory_api(&messages);
        assert_eq!(cleaned.len(), 1);
        assert_eq!(cleaned[0]["id"], "m2");
        assert!(cleaned[0].get("memory_changes").is_none());
    }

    #[test]
    fn resolve_messages_rejects_invalid_shape() {
        let input = ProcessInput {
            conversation: Some(ConversationInput {
                messages: vec![json!("bad-shape")],
                last_memory_check_message_id: None,
            }),
            messages: None,
            last_memory_check_message_id: None,
            existing_summary: None,
            date: None,
            timezone: None,
            locale: None,
        };
        let err = resolve_messages(&input).expect_err("invalid message shape should fail");
        assert!(err.to_string().contains("must be a JSON object"));
    }

    #[tokio::test]
    async fn execute_with_client_allows_empty_messages_and_skips() {
        let (_temp_dir, store) = open_test_store("empty-messages");
        let input = ProcessInput {
            conversation: Some(ConversationInput {
                messages: vec![],
                last_memory_check_message_id: None,
            }),
            messages: None,
            last_memory_check_message_id: None,
            existing_summary: None,
            date: None,
            timezone: None,
            locale: None,
        };
        let api = FakeApiClient::new();

        let output = execute_with_client(&store, &api, "https://stg.dayone.me", 1, input)
            .await
            .expect("empty messages should be treated as no-new-messages");

        assert!(!output.processed);
        assert_eq!(output.skipped_reason.as_deref(), Some("no_new_messages"));
        assert_eq!(output.last_memory_check_message_id, None);
        let captured = api.captured_post_json_bodies(MEMORY_UPDATES_PATH).await;
        assert!(captured.is_empty());
    }

    #[tokio::test]
    async fn execute_with_client_preserves_bookmark_when_no_new_messages() {
        let (_temp_dir, store) = open_test_store("bookmark-preserved");
        let input = ProcessInput {
            conversation: Some(ConversationInput {
                messages: vec![
                    json!({"id":"m1","role":"user","content":"one"}),
                    json!({"id":"m2","role":"assistant","content":"two"}),
                ],
                last_memory_check_message_id: Some("m2".to_owned()),
            }),
            messages: None,
            last_memory_check_message_id: None,
            existing_summary: None,
            date: None,
            timezone: None,
            locale: None,
        };
        let api = FakeApiClient::new();

        let output = execute_with_client(&store, &api, "https://stg.dayone.me", 1, input)
            .await
            .expect("bookmark-preserved no-op should succeed");

        assert!(!output.processed);
        assert_eq!(output.skipped_reason.as_deref(), Some("no_new_messages"));
        assert_eq!(output.last_memory_check_message_id.as_deref(), Some("m2"));
        let captured = api.captured_post_json_bodies(MEMORY_UPDATES_PATH).await;
        assert!(captured.is_empty());
    }

    #[tokio::test]
    async fn execute_with_client_processes_continuation_and_persists_memory() {
        let (_temp_dir, store) = open_test_store("continuation");
        let input: ProcessInput =
            serde_json::from_value(read_fixture_json("conversation-process-basic.json"))
                .expect("conversation fixture should parse as ProcessInput");
        let expected = read_fixture_json("expected-process-basic.json");
        let api = FakeApiClient::new()
            .with_json_fixture(
                format!("POST_JSON {MEMORY_UPDATES_PATH}"),
                read_fixture_json("api-processing-round-1.json"),
            )
            .await
            .with_json_fixture(
                format!("POST_JSON {MEMORY_UPDATES_PATH}"),
                read_fixture_json("api-processing-round-2-complete.json"),
            )
            .await;

        let output = execute_with_client(&store, &api, "https://stg.dayone.me", 1, input)
            .await
            .expect("memory process should succeed");

        assert!(output.processed);
        assert_eq!(
            output.api_loops,
            expected["api_loops"].as_u64().unwrap_or_default() as usize
        );
        assert_eq!(
            output.new_message_count,
            expected["new_message_count"].as_u64().unwrap_or_default() as usize
        );
        assert_eq!(
            output.context_message_count,
            expected["context_message_count"]
                .as_u64()
                .unwrap_or_default() as usize
        );
        assert_eq!(
            output.updated_summary.as_deref(),
            expected["updated_summary"].as_str()
        );
        assert_eq!(
            output.memory_changes.len(),
            expected["memory_changes"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default()
        );
        assert_eq!(
            output.memory_changes[0].change,
            expected["memory_changes"][0]["change"]
                .as_str()
                .unwrap_or_default()
        );

        let rows = store
            .list_json_rows("context_library_items")
            .expect("rows should list");
        assert_eq!(rows.len(), 1);

        let captured = api.captured_post_json_bodies(MEMORY_UPDATES_PATH).await;
        assert_eq!(
            captured.len(),
            2,
            "should issue start + continuation requests"
        );
        assert_eq!(
            captured[0]["start"]["new_messages"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            2
        );
        assert!(captured[1].get("continuation").is_some());
    }

    #[test]
    fn search_terms_from_tool_args_supports_terms_and_query_fallback() {
        let terms_only = json!({"terms":"Margherita, daughter , 11"});
        assert_eq!(
            search_terms_from_tool_args(&terms_only),
            vec![
                "Margherita".to_owned(),
                "daughter".to_owned(),
                "11".to_owned()
            ]
        );

        let query_only = json!({"query":"Margherita, daughter"});
        assert_eq!(
            search_terms_from_tool_args(&query_only),
            vec!["Margherita".to_owned(), "daughter".to_owned()]
        );
    }

    #[test]
    fn memory_tool_search_uses_term_splitting_and_deduplication() {
        let (_temp_dir, store) = open_test_store("memory-search-terms");
        let item_1 = json!({
            "id": "memory-1",
            "content": "# person: Margherita\n- relationship to me: daughter",
            "category": "in-house-memory",
            "updated_at": "2026-04-09T00:00:00.000Z",
            "deleted_at": Value::Null,
        });
        let item_2 = json!({
            "id": "memory-2",
            "content": "# person: Luca\n- relationship to me: daughter",
            "category": "in-house-memory",
            "updated_at": "2026-04-09T00:00:01.000Z",
            "deleted_at": Value::Null,
        });
        let other_category = json!({
            "id": "memory-3",
            "content": "# person: Margherita\n- note: should be excluded by category",
            "category": "not-in-house-memory",
            "updated_at": "2026-04-09T00:00:02.000Z",
            "deleted_at": Value::Null,
        });
        save_context_item(&store, &item_1).expect("item_1 should save");
        save_context_item(&store, &item_2).expect("item_2 should save");
        save_context_item(&store, &other_category).expect("other category should save");

        let result = apply_in_house_memory_tool(
            &store,
            &json!({"command":"search","terms":"Margherita,daughter"}),
            &mut Vec::new(),
        )
        .expect("search should succeed");

        assert_eq!(result["success"], true);
        assert_eq!(result["total"], 2);
        let memories = result["memories"]
            .as_array()
            .expect("memories should be an array");
        let ids: std::collections::HashSet<String> = memories
            .iter()
            .filter_map(|m| m.get("id").and_then(Value::as_str))
            .map(ToOwned::to_owned)
            .collect();
        assert!(ids.contains("memory-1"));
        assert!(ids.contains("memory-2"));
        assert!(!ids.contains("memory-3"));
    }

    #[tokio::test]
    async fn execute_with_client_skips_when_no_new_user_messages() {
        let (_temp_dir, store) = open_test_store("skip-no-user");
        let input = ProcessInput {
            conversation: Some(ConversationInput {
                messages: vec![json!({"id":"m1","role":"assistant","content":"hello"})],
                last_memory_check_message_id: None,
            }),
            messages: None,
            last_memory_check_message_id: None,
            existing_summary: None,
            date: None,
            timezone: None,
            locale: None,
        };
        let api = FakeApiClient::new();

        let output = execute_with_client(&store, &api, "https://stg.dayone.me", 1, input)
            .await
            .expect("memory process should succeed");

        assert!(!output.processed);
        assert_eq!(
            output.skipped_reason.as_deref(),
            Some("no_new_user_messages")
        );
        let captured = api.captured_post_json_bodies(MEMORY_UPDATES_PATH).await;
        assert!(captured.is_empty());
    }

    #[tokio::test]
    async fn execute_with_client_limits_context_messages_to_four() {
        let (_temp_dir, store) = open_test_store("context-window");
        let input = ProcessInput {
            conversation: Some(ConversationInput {
                messages: vec![
                    json!({"id":"m1","role":"user","content":"one"}),
                    json!({"id":"m2","role":"assistant","content":"two"}),
                    json!({"id":"m3","role":"assistant","content":"three"}),
                    json!({"id":"m4","role":"assistant","content":"four"}),
                    json!({"id":"m5","role":"assistant","content":"five"}),
                    json!({"id":"m6","role":"user","content":"six"}),
                ],
                last_memory_check_message_id: Some("m5".to_owned()),
            }),
            messages: None,
            last_memory_check_message_id: None,
            existing_summary: None,
            date: Some("2026-04-08".to_owned()),
            timezone: Some("UTC".to_owned()),
            locale: Some("en-US".to_owned()),
        };
        let api = FakeApiClient::new()
            .with_json_fixture(
                format!("POST_JSON {MEMORY_UPDATES_PATH}"),
                json!({"status":"complete"}),
            )
            .await;

        let output = execute_with_client(&store, &api, "https://stg.dayone.me", 1, input)
            .await
            .expect("memory process should succeed");
        assert!(output.processed);
        assert_eq!(output.new_message_count, 1);
        assert_eq!(output.context_message_count, 4);

        let captured = api.captured_post_json_bodies(MEMORY_UPDATES_PATH).await;
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0]["start"]["context_messages"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            4
        );
        assert_eq!(
            captured[0]["start"]["context_messages"][0]["id"]
                .as_str()
                .unwrap_or_default(),
            "m1"
        );
        assert_eq!(
            captured[0]["start"]["context_messages"][3]["id"]
                .as_str()
                .unwrap_or_default(),
            "m4"
        );
    }

    #[tokio::test]
    async fn execute_with_client_applies_update_and_delete_memory_commands() {
        let (_temp_dir, store) = open_test_store("update-delete");

        let seeded = json!({
            "id": "memory-existing-1",
            "content": "Old content",
            "category": "in-house-memory",
            "updated_at": "2026-04-01T00:00:00.000Z",
            "deleted_at": Value::Null,
        });
        save_context_item(&store, &seeded).expect("seed context item should save");

        let input = ProcessInput {
            conversation: Some(ConversationInput {
                messages: vec![
                    json!({"id":"m1","role":"user","content":"please update and delete memories"}),
                    json!({"id":"m2","role":"assistant","content":"working on it"}),
                ],
                last_memory_check_message_id: None,
            }),
            messages: None,
            last_memory_check_message_id: None,
            existing_summary: None,
            date: Some("2026-04-08".to_owned()),
            timezone: Some("UTC".to_owned()),
            locale: Some("en-US".to_owned()),
        };

        let api = FakeApiClient::new()
            .with_json_fixture(
                format!("POST_JSON {MEMORY_UPDATES_PATH}"),
                json!({
                    "status":"processing",
                    "tool_calls":[
                        {
                            "id":"tc-update",
                            "type":"function",
                            "function":{
                                "name":"in_house_memory",
                                "arguments":"{\"command\":\"update\",\"updates\":[{\"id\":\"memory-existing-1\",\"content\":\"New content\"}]}"
                            }
                        },
                        {
                            "id":"tc-delete",
                            "type":"function",
                            "function":{
                                "name":"in_house_memory",
                                "arguments":"{\"command\":\"delete\",\"ids\":\"memory-existing-1\"}"
                            }
                        }
                    ],
                    "continuation":{
                        "memories":[],
                        "memories_queue":[],
                        "conversation_history":[]
                    }
                }),
            )
            .await
            .with_json_fixture(
                format!("POST_JSON {MEMORY_UPDATES_PATH}"),
                json!({"status":"complete"}),
            )
            .await;

        let output = execute_with_client(&store, &api, "https://stg.dayone.me", 1, input)
            .await
            .expect("memory process should succeed");

        assert_eq!(output.memory_changes.len(), 2);
        assert_eq!(output.memory_changes[0].change, "update");
        assert_eq!(output.memory_changes[1].change, "delete");

        let stored = store
            .get_json_row_by_id("context_library_items", "memory-existing-1")
            .expect("row lookup should work")
            .expect("row should exist");
        let value: Value = serde_json::from_str(&stored).expect("stored row should parse");
        assert_eq!(value["content"], "New content");
        assert!(
            value
                .get("deleted_at")
                .and_then(Value::as_str)
                .is_some_and(|v| !v.is_empty()),
            "deleted_at should be set after delete command"
        );
    }
}
