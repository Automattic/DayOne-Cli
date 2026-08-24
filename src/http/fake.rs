use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::http::{ApiError, DayOneApiClient, MultipartBinaryPart};

#[derive(Clone, Debug)]
enum FakeResponse {
    Json(Value),
    JsonAllow304(Option<Value>),
    Bytes(Vec<u8>),
    Error(String),
    PutAllow409((u16, Value)),
    PutJson(Value),
    DeleteJson(Value),
    Unit,
}

#[derive(Clone, Debug)]
pub struct CapturedUpload {
    pub url: String,
    pub content_type: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Clone, Default)]
pub struct FakeApiClient {
    fixtures: Arc<RwLock<HashMap<String, VecDeque<FakeResponse>>>>,
    captured_envelope: Arc<RwLock<Option<Vec<u8>>>>,
    captured_entry_content: Arc<RwLock<Option<Vec<u8>>>>,
    captured_post_json: Arc<RwLock<HashMap<String, Vec<Value>>>>,
    captured_put_json: Arc<RwLock<HashMap<String, Vec<Value>>>>,
    captured_uploads: Arc<RwLock<Vec<CapturedUpload>>>,
}

impl FakeApiClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn with_json_fixture(self, key: impl Into<String>, value: Value) -> Self {
        self.insert(key.into(), FakeResponse::Json(value)).await;
        self
    }

    async fn insert(&self, key: String, response: FakeResponse) {
        self.fixtures
            .write()
            .await
            .entry(key)
            .or_default()
            .push_back(response);
    }

    async fn response_for(&self, key: &str) -> Result<FakeResponse> {
        let mut fixtures = self.fixtures.write().await;
        let queue = fixtures
            .get_mut(key)
            .ok_or_else(|| anyhow!("no fake fixture for key '{key}'"))?;
        if queue.is_empty() {
            bail!("fixture queue exhausted for key '{key}'");
        }
        queue
            .pop_front()
            .ok_or_else(|| anyhow!("fixture queue exhausted for key '{key}'"))
    }
}

impl DayOneApiClient for FakeApiClient {
    async fn get_json(&self, api_path: &str, _query: &[(&str, String)]) -> Result<Value> {
        let key = format!("GET_JSON {api_path}");
        match self.response_for(&key).await? {
            FakeResponse::Json(value) => Ok(value),
            other => bail!("fixture type mismatch for key '{key}': got {other:?}"),
        }
    }

    async fn get_json_allow_304(
        &self,
        api_path: &str,
        _query: &[(&str, String)],
    ) -> Result<Option<Value>> {
        let key = format!("GET_JSON_ALLOW_304 {api_path}");
        match self.response_for(&key).await? {
            FakeResponse::JsonAllow304(value) => Ok(value),
            other => bail!("fixture type mismatch for key '{key}': got {other:?}"),
        }
    }

    async fn get_bytes(&self, api_path: &str, _query: &[(&str, String)]) -> Result<Vec<u8>> {
        let key = format!("GET_BYTES {api_path}");
        match self.response_for(&key).await? {
            FakeResponse::Bytes(value) => Ok(value),
            other => bail!("fixture type mismatch for key '{key}': got {other:?}"),
        }
    }

    async fn put_json_allow_409(&self, api_path: &str, _payload: &Value) -> Result<(u16, Value)> {
        let key = format!("PUT_JSON_ALLOW_409 {api_path}");
        match self.response_for(&key).await? {
            FakeResponse::PutAllow409(value) => Ok(value),
            other => bail!("fixture type mismatch for key '{key}': got {other:?}"),
        }
    }

    async fn put_json(&self, api_path: &str, payload: &Value) -> Result<Value> {
        self.captured_put_json
            .write()
            .await
            .entry(api_path.to_owned())
            .or_default()
            .push(payload.clone());
        let key = format!("PUT_JSON {api_path}");
        match self.response_for(&key).await? {
            FakeResponse::PutJson(value) => Ok(value),
            other => bail!("fixture type mismatch for key '{key}': got {other:?}"),
        }
    }

    async fn post_json(&self, api_path: &str, payload: &Value) -> Result<Value> {
        self.captured_post_json
            .write()
            .await
            .entry(api_path.to_owned())
            .or_default()
            .push(payload.clone());
        let key = format!("POST_JSON {api_path}");
        match self.response_for(&key).await? {
            FakeResponse::Json(value) => Ok(value),
            other => bail!("fixture type mismatch for key '{key}': got {other:?}"),
        }
    }

    async fn delete_json(&self, api_path: &str, _query: &[(&str, String)]) -> Result<Value> {
        let key = format!("DELETE_JSON {api_path}");
        match self.response_for(&key).await? {
            FakeResponse::DeleteJson(value) => Ok(value),
            other => bail!("fixture type mismatch for key '{key}': got {other:?}"),
        }
    }

    async fn put_entry_multipart(
        &self,
        api_path: &str,
        envelope_json: &[u8],
        _content_field_name: &str,
        content_bytes: &[u8],
        _content_mime: &str,
        _extra_parts: &[MultipartBinaryPart],
    ) -> Result<Vec<u8>> {
        *self.captured_envelope.write().await = Some(envelope_json.to_vec());
        *self.captured_entry_content.write().await = Some(content_bytes.to_vec());
        let key = format!("PUT_ENTRY_MULTIPART {api_path}");
        match self.response_for(&key).await? {
            FakeResponse::Bytes(value) => Ok(value),
            FakeResponse::Error(message) => Err(ApiError::new(404, "Not Found", message).into()),
            other => bail!("fixture type mismatch for key '{key}': got {other:?}"),
        }
    }

    async fn put_file_to_absolute_url(
        &self,
        url: &str,
        content_type: &str,
        headers: &[(&str, String)],
        file_path: &str,
        _file_size_bytes: i64,
    ) -> Result<()> {
        let body = std::fs::read(file_path)
            .with_context(|| format!("fake client failed to read upload file '{file_path}'"))?;
        self.captured_uploads.write().await.push(CapturedUpload {
            url: url.to_owned(),
            content_type: content_type.to_owned(),
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone()))
                .collect(),
            body,
        });
        let key = format!("PUT_FILE {url}");
        match self.response_for(&key).await? {
            FakeResponse::Unit => Ok(()),
            other => bail!("fixture type mismatch for key '{key}': got {other:?}"),
        }
    }

    async fn put_bytes_to_absolute_url(
        &self,
        url: &str,
        content_type: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> Result<()> {
        self.captured_uploads.write().await.push(CapturedUpload {
            url: url.to_owned(),
            content_type: content_type.to_owned(),
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone()))
                .collect(),
            body: body.to_vec(),
        });
        let key = format!("PUT_BYTES {url}");
        match self.response_for(&key).await? {
            FakeResponse::Unit => Ok(()),
            other => bail!("fixture type mismatch for key '{key}': got {other:?}"),
        }
    }
}

// Additional fixture helpers are only used by tests.
#[cfg(test)]
impl FakeApiClient {
    pub async fn last_put_entry_envelope(&self) -> Option<serde_json::Value> {
        let guard = self.captured_envelope.read().await;
        guard
            .as_deref()
            .and_then(|bytes| serde_json::from_slice(bytes).ok())
    }

    /// The last pushed entry content part parsed as JSON. Only meaningful for
    /// plaintext journals, where the content part is the raw entry JSON.
    pub async fn last_put_entry_content_json(&self) -> Option<serde_json::Value> {
        let guard = self.captured_entry_content.read().await;
        guard
            .as_deref()
            .and_then(|bytes| serde_json::from_slice(bytes).ok())
    }

    pub async fn with_json_allow_304_fixture(
        self,
        key: impl Into<String>,
        value: Option<Value>,
    ) -> Self {
        self.insert(key.into(), FakeResponse::JsonAllow304(value))
            .await;
        self
    }

    pub async fn with_bytes_fixture(self, key: impl Into<String>, value: Vec<u8>) -> Self {
        self.insert(key.into(), FakeResponse::Bytes(value)).await;
        self
    }

    pub async fn with_error_fixture(
        self,
        key: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        self.insert(key.into(), FakeResponse::Error(message.into()))
            .await;
        self
    }

    pub async fn with_put_allow_409_fixture(
        self,
        key: impl Into<String>,
        status_and_value: (u16, Value),
    ) -> Self {
        self.insert(key.into(), FakeResponse::PutAllow409(status_and_value))
            .await;
        self
    }

    pub async fn with_put_json_fixture(self, key: impl Into<String>, value: Value) -> Self {
        self.insert(key.into(), FakeResponse::PutJson(value)).await;
        self
    }

    pub async fn with_delete_json_fixture(self, key: impl Into<String>, value: Value) -> Self {
        self.insert(key.into(), FakeResponse::DeleteJson(value))
            .await;
        self
    }

    pub async fn with_unit_fixture(self, key: impl Into<String>) -> Self {
        self.insert(key.into(), FakeResponse::Unit).await;
        self
    }

    pub async fn captured_uploads(&self) -> Vec<CapturedUpload> {
        self.captured_uploads.read().await.clone()
    }

    pub async fn captured_post_json_bodies(&self, api_path: &str) -> Vec<Value> {
        self.captured_post_json
            .read()
            .await
            .get(api_path)
            .cloned()
            .unwrap_or_default()
    }

    pub async fn captured_put_json_bodies(&self, api_path: &str) -> Vec<Value> {
        self.captured_put_json
            .read()
            .await
            .get(api_path)
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn fake_api_client_returns_registered_json_fixture() {
        let client = FakeApiClient::new()
            .with_json_fixture("GET_JSON /users/key", json!({"id": "user-key"}))
            .await;

        let response = client
            .get_json("/users/key", &[])
            .await
            .expect("fixture should be returned");
        assert_eq!(response.get("id").and_then(Value::as_str), Some("user-key"));
    }

    #[tokio::test]
    async fn fake_api_client_supports_all_fixture_types() {
        let client = FakeApiClient::new()
            .with_json_fixture("GET_JSON /users/key", json!({"id": "user-key"}))
            .await
            .with_json_allow_304_fixture("GET_JSON_ALLOW_304 /cacheable", None)
            .await
            .with_bytes_fixture("GET_BYTES /blob", vec![1, 2, 3, 4])
            .await
            .with_put_allow_409_fixture("PUT_JSON_ALLOW_409 /resource", (409, json!({"id": "x"})))
            .await
            .with_put_json_fixture("PUT_JSON /resource-2", json!({"ok": true}))
            .await
            .with_delete_json_fixture("DELETE_JSON /resource-3", json!({"ok": true}))
            .await
            .with_unit_fixture("PUT_FILE https://upload.example/test")
            .await;

        let got_allow_304 = client
            .get_json_allow_304("/cacheable", &[])
            .await
            .expect("304 fixture should be returned");
        assert!(got_allow_304.is_none());

        let got_bytes = client
            .get_bytes("/blob", &[])
            .await
            .expect("bytes fixture should be returned");
        assert_eq!(got_bytes, vec![1, 2, 3, 4]);

        let (status, body) = client
            .put_json_allow_409("/resource", &json!({}))
            .await
            .expect("409 fixture should be returned");
        assert_eq!(status, 409);
        assert_eq!(body.get("id").and_then(Value::as_str), Some("x"));

        let put = client
            .put_json("/resource-2", &json!({"hello": "world"}))
            .await
            .expect("PUT fixture should be returned");
        assert_eq!(put.get("ok").and_then(Value::as_bool), Some(true));

        let deleted = client
            .delete_json("/resource-3", &[])
            .await
            .expect("DELETE fixture should be returned");
        assert_eq!(deleted.get("ok").and_then(Value::as_bool), Some(true));

        let upload_path = std::env::temp_dir().join("dayone-fake-upload-fixture.bin");
        std::fs::write(&upload_path, b"upload-body").expect("fixture upload file should write");
        client
            .put_file_to_absolute_url(
                "https://upload.example/test",
                "application/octet-stream",
                &[],
                upload_path.to_str().expect("temp path should be utf8"),
                0,
            )
            .await
            .expect("unit fixture should be returned");
        let _ = std::fs::remove_file(&upload_path);
    }
}
