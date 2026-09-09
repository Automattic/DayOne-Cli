use anyhow::{Context, Result, bail};
use reqwest::header::{
    AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue,
};
use serde_json::Value;
use std::error::Error as StdError;
use std::fmt;

pub mod client;
pub mod device_info;
#[cfg(test)]
pub mod fake;

pub use device_info::DeviceInfo;

use crate::util::encode_path_segment;

pub(crate) fn entry_edit_lock_path(journal_id: &str, entry_id: &str) -> String {
    format!(
        "/shares/{}/entries/{}/lock",
        encode_path_segment(journal_id),
        encode_path_segment(entry_id)
    )
}

#[derive(Clone, Debug)]
pub struct MultipartBinaryPart {
    pub name: String,
    pub file_name: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

pub trait DayOneApiClient: Send + Sync {
    async fn get_json(&self, api_path: &str, query: &[(&str, String)]) -> Result<Value>;
    async fn get_json_allow_304(
        &self,
        api_path: &str,
        query: &[(&str, String)],
    ) -> Result<Option<Value>>;
    async fn get_bytes(&self, api_path: &str, query: &[(&str, String)]) -> Result<Vec<u8>>;
    async fn put_json_allow_409(&self, api_path: &str, payload: &Value) -> Result<(u16, Value)>;
    async fn put_json(&self, api_path: &str, payload: &Value) -> Result<Value>;
    async fn post_json(&self, api_path: &str, payload: &Value) -> Result<Value>;
    async fn delete_json(&self, api_path: &str, query: &[(&str, String)]) -> Result<Value>;

    async fn get_entry_edit_lock(&self, journal_id: &str, entry_id: &str) -> Result<Value> {
        self.get_json(&entry_edit_lock_path(journal_id, entry_id), &[])
            .await
    }

    async fn put_entry_multipart(
        &self,
        api_path: &str,
        envelope_json: &[u8],
        content_field_name: &str,
        content_bytes: &[u8],
        content_mime: &str,
        extra_parts: &[MultipartBinaryPart],
    ) -> Result<Vec<u8>> {
        let _ = (
            api_path,
            envelope_json,
            content_field_name,
            content_bytes,
            content_mime,
            extra_parts,
        );
        bail!("multipart entry upload is not supported by this client")
    }

    async fn put_file_to_absolute_url(
        &self,
        url: &str,
        content_type: &str,
        headers: &[(&str, String)],
        file_path: &str,
        file_size_bytes: i64,
    ) -> Result<()> {
        let _ = (url, content_type, headers, file_path, file_size_bytes);
        bail!("binary absolute URL PUT is not supported by this client")
    }

    /// PUTs an in-memory byte buffer to an absolute URL. Used to upload
    /// client-side-encrypted media (D1 ciphertext) for E2E journals, where the
    /// bytes do not live on disk in their final form.
    async fn put_bytes_to_absolute_url(
        &self,
        url: &str,
        content_type: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> Result<()> {
        let _ = (url, content_type, headers, body);
        bail!("binary absolute URL byte PUT is not supported by this client")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BaseUrlError {
    #[error("base URL cannot be empty")]
    Empty,
    #[error("base URL must start with http:// or https://")]
    Scheme,
    #[error("base URL is not valid")]
    Invalid,
}

pub fn normalize_base_url(value: &str) -> Result<String> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(BaseUrlError::Empty.into());
    }
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        return Err(BaseUrlError::Scheme.into());
    }
    let parsed = reqwest::Url::parse(trimmed).map_err(|_| BaseUrlError::Invalid)?;
    if parsed.host_str().is_none()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(BaseUrlError::Invalid.into());
    }
    Ok(trimmed.to_owned())
}

pub fn bearer_auth_header(token: &str) -> Result<(HeaderName, HeaderValue)> {
    if token.trim().is_empty() {
        bail!("token cannot be empty");
    }
    let auth_value = format!("Bearer {}", token.trim());
    let header = HeaderValue::from_str(&auth_value).context("invalid bearer token format")?;
    Ok((AUTHORIZATION, header))
}

pub fn bearer_headers(token: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    let (name, value) = bearer_auth_header(token)?;
    headers.insert(name, value);
    Ok(headers)
}

pub fn bearer_web_headers(token: &str, device_info: &DeviceInfo) -> Result<HeaderMap> {
    const DEVICE_INFO_HEADER: &str = "Device-Info";
    const X_USER_AGENT_HEADER: &str = "X-User-Agent";

    let mut headers = bearer_headers(token)?;
    headers.insert(
        X_USER_AGENT_HEADER,
        HeaderValue::from_str(&device_info.x_user_agent())
            .context("invalid x-user-agent format")?,
    );
    headers.insert(
        DEVICE_INFO_HEADER,
        HeaderValue::from_str(&device_info.header_value()).context("invalid device-info format")?,
    );
    Ok(headers)
}

#[derive(Debug, Clone)]
pub struct ApiError {
    status: u16,
    status_text: String,
    body: String,
}

impl ApiError {
    pub fn new(status: u16, status_text: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            status,
            status_text: status_text.into(),
            body: body.into(),
        }
    }

    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn body(&self) -> &str {
        &self.body
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.body.is_empty() {
            write!(
                f,
                "HTTP {} {} with empty response body",
                self.status, self.status_text
            )
        } else {
            write!(
                f,
                "HTTP {} {}: {}",
                self.status, self.status_text, self.body
            )
        }
    }
}

impl StdError for ApiError {}

pub fn error_chain_contains_http_status_and_body(
    err: &(dyn StdError + 'static),
    status: u16,
    body_needle: &str,
) -> bool {
    let body_needle = body_needle.to_ascii_lowercase();
    let mut current = Some(err);
    while let Some(error) = current {
        if let Some(api_error) = error.downcast_ref::<ApiError>() {
            return api_error.status() == status
                && api_error.body().to_ascii_lowercase().contains(&body_needle);
        }
        current = error.source();
    }
    false
}

pub async fn ensure_success(response: reqwest::Response) -> Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "<unreadable response body>".to_owned());
    Err(ApiError::new(
        status.as_u16(),
        status.canonical_reason().unwrap_or(""),
        body,
    )
    .into())
}

pub fn replace_content_headers(
    mut headers: HeaderMap,
    content_type: &str,
    content_length: i64,
) -> Result<HeaderMap> {
    let content_length_u64 = u64::try_from(content_length)
        .with_context(|| format!("invalid negative content length '{content_length}'"))?;
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(content_type)
            .with_context(|| format!("invalid content type header '{content_type}'"))?,
    );
    headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&content_length_u64.to_string())
            .with_context(|| format!("invalid content length '{content_length_u64}'"))?,
    );
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use reqwest::header::{CONTENT_LENGTH, HeaderMap};

    use super::{DeviceInfo, bearer_web_headers, normalize_base_url, replace_content_headers};

    #[test]
    fn bearer_web_headers_sets_device_info_with_required_keys() {
        // Build a fixed DeviceInfo rather than `resolve`-ing from process state:
        // resolving reads locale env vars, which can race with parallel tests
        // that mutate the environment via `with_env`.
        let info = DeviceInfo {
            id: "abc123".to_owned(),
            model: "macos aarch64".to_owned(),
            name: "my-machine".to_owned(),
            language: "en-US".to_owned(),
            country: "US".to_owned(),
        };
        let headers = bearer_web_headers("token-abc", &info).expect("web headers should build");
        let device_info = headers
            .get("Device-Info")
            .expect("device-info should be set")
            .to_str()
            .expect("device-info should be valid UTF-8");
        assert!(
            device_info.contains(&format!("Id=\"{}\"", info.id)),
            "device-info should carry the id: {device_info}"
        );
        assert!(
            device_info.contains("Model=\""),
            "missing model: {device_info}"
        );
        assert!(
            device_info.contains("Name=\""),
            "missing name: {device_info}"
        );
        assert!(
            device_info.contains("app_id=\"com.bloombuilt.dayone-cli\""),
            "missing app_id: {device_info}"
        );
    }

    #[test]
    fn normalize_base_url_requires_a_parseable_http_origin() {
        assert_eq!(
            normalize_base_url("https://example.com/").expect("valid URL"),
            "https://example.com"
        );
        assert!(normalize_base_url("https://").is_err());
        assert!(normalize_base_url("https://exa mple.com").is_err());
        assert!(normalize_base_url("https://example.com?token=private").is_err());
        assert!(normalize_base_url("https://example.com#private").is_err());
        assert!(normalize_base_url("https://user:password@example.com").is_err());
        assert!(normalize_base_url("file:///tmp/dayone").is_err());
    }

    #[test]
    fn replace_content_headers_rejects_negative_content_length() {
        let err = replace_content_headers(HeaderMap::new(), "application/octet-stream", -1)
            .expect_err("negative content length should fail");
        assert!(
            err.to_string().contains("invalid negative content length"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn replace_content_headers_sets_valid_content_length_header() {
        let headers = replace_content_headers(HeaderMap::new(), "application/octet-stream", 42)
            .expect("positive content length should work");
        let value = headers
            .get(CONTENT_LENGTH)
            .expect("content-length should be set")
            .to_str()
            .expect("content-length should be valid UTF-8");
        assert_eq!(value, "42");
    }
}
