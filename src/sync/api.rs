use anyhow::{Context, Result};
use futures_util::TryStreamExt;
use http_body::Frame;
use http_body_util::StreamBody;
use reqwest::StatusCode;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use reqwest::multipart::{Form, Part};
use serde_json::Value;
use tokio_util::io::ReaderStream;

use crate::diagnostics::{HttpTrace, MediaTrace, byte_len, json_len};
use crate::http::{
    DayOneApiClient, DeviceInfo, MultipartBinaryPart, bearer_web_headers, ensure_success,
    normalize_base_url, replace_content_headers,
};

#[derive(Clone)]
pub struct SyncApiClient {
    base_url: String,
    token: String,
    device_info: DeviceInfo,
    client: reqwest::Client,
}

impl SyncApiClient {
    pub fn new(base_url: &str, token: &str, device_info: DeviceInfo) -> Result<Self> {
        Ok(Self {
            base_url: normalize_base_url(base_url)?,
            token: token.to_owned(),
            device_info,
            client: reqwest::Client::builder()
                .user_agent(format!("dayone-cli/{}", env!("CARGO_PKG_VERSION")))
                .build()
                .context("failed to initialize sync HTTP client")?,
        })
    }
}

impl DayOneApiClient for SyncApiClient {
    async fn get_entry_edit_lock(&self, journal_id: &str, entry_id: &str) -> Result<Value> {
        let path = crate::http::entry_edit_lock_path(journal_id, entry_id);
        let trace = HttpTrace::api("GET", &path, Some(0));
        let response = self
            .client
            .get(format!("{}/api{}", self.base_url, path))
            .headers(bearer_web_headers(&self.token, &self.device_info)?)
            .header(reqwest::header::CACHE_CONTROL, "no-cache")
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .context("failed to read entry edit-lock status")?;
        parse_value_or_error(response, trace).await
    }

    async fn get_json(&self, api_path: &str, query: &[(&str, String)]) -> Result<Value> {
        let url = format!("{}/api{}", self.base_url, api_path);
        let trace = HttpTrace::api("GET", api_path, Some(0));
        let response = self
            .client
            .get(url)
            .headers(bearer_web_headers(&self.token, &self.device_info)?)
            .query(query)
            .send()
            .await
            .context("failed to send sync GET request")?;
        parse_value_or_error(response, trace).await
    }

    async fn get_json_allow_304(
        &self,
        api_path: &str,
        query: &[(&str, String)],
    ) -> Result<Option<Value>> {
        let url = format!("{}/api{}", self.base_url, api_path);
        let mut trace = HttpTrace::api("GET", api_path, Some(0));
        let response = self
            .client
            .get(url)
            .headers(bearer_web_headers(&self.token, &self.device_info)?)
            .query(query)
            .send()
            .await
            .context("failed to send sync GET request")?;
        trace.response(response.status().as_u16(), response.content_length());
        if response.status() == StatusCode::NOT_MODIFIED {
            trace.finish(Some(0));
            return Ok(None);
        }
        let value = parse_value_or_error(response, trace).await?;
        Ok(Some(value))
    }

    async fn get_bytes(&self, api_path: &str, query: &[(&str, String)]) -> Result<Vec<u8>> {
        let url = format!("{}/api{}", self.base_url, api_path);
        let mut trace = HttpTrace::api("GET", api_path, Some(0));
        let response = self
            .client
            .get(url)
            .headers(bearer_web_headers(&self.token, &self.device_info)?)
            .query(query)
            .send()
            .await
            .context("failed to send sync GET request")?;
        trace.response(response.status().as_u16(), response.content_length());
        let response = ensure_success(response).await?;
        let bytes = response
            .bytes()
            .await
            .context("failed to read successful sync response bytes")?;
        trace.finish(byte_len(&bytes));
        Ok(bytes.to_vec())
    }

    async fn put_json_allow_409(&self, api_path: &str, payload: &Value) -> Result<(u16, Value)> {
        let url = format!("{}/api{}", self.base_url, api_path);
        let mut trace = HttpTrace::api("PUT", api_path, json_len(payload));
        let response = self
            .client
            .put(url)
            .headers(bearer_web_headers(&self.token, &self.device_info)?)
            .json(payload)
            .send()
            .await
            .context("failed to send sync PUT request")?;
        trace.response(response.status().as_u16(), response.content_length());
        let status = response.status();
        if status.is_success() || status == StatusCode::CONFLICT {
            let body = response
                .text()
                .await
                .context("failed to read sync PUT response body")?;
            let trimmed = body.trim();
            let json = if trimmed.is_empty() {
                Value::Null
            } else {
                serde_json::from_str::<Value>(trimmed).with_context(|| {
                    format!("failed to decode PUT response body as JSON (status {status})")
                })?
            };
            trace.finish(byte_len(&body));
            return Ok((status.as_u16(), json));
        }
        let _ = ensure_success(response).await?;
        unreachable!("ensure_success returns Err for non-success responses");
    }

    async fn post_json(&self, api_path: &str, payload: &Value) -> Result<Value> {
        let url = format!("{}/api{}", self.base_url, api_path);
        let trace = HttpTrace::api("POST", api_path, json_len(payload));
        let response = self
            .client
            .post(url)
            .headers(bearer_web_headers(&self.token, &self.device_info)?)
            .json(payload)
            .send()
            .await
            .context("failed to send sync POST request")?;
        parse_value_or_error(response, trace).await
    }

    async fn put_json(&self, api_path: &str, payload: &Value) -> Result<Value> {
        let url = format!("{}/api{}", self.base_url, api_path);
        let trace = HttpTrace::api("PUT", api_path, json_len(payload));
        let response = self
            .client
            .put(url)
            .headers(bearer_web_headers(&self.token, &self.device_info)?)
            .json(payload)
            .send()
            .await
            .context("failed to send sync PUT request")?;
        parse_value_or_error(response, trace).await
    }

    async fn delete_json(&self, api_path: &str, query: &[(&str, String)]) -> Result<Value> {
        let url = format!("{}/api{}", self.base_url, api_path);
        let mut trace = HttpTrace::api("DELETE", api_path, Some(0));
        let response = self
            .client
            .delete(url)
            .headers(bearer_web_headers(&self.token, &self.device_info)?)
            .query(query)
            .send()
            .await
            .context("failed to send sync DELETE request")?;
        trace.response(response.status().as_u16(), response.content_length());
        let response = ensure_success(response).await?;
        let is_json_or_ndjson = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_ascii_lowercase)
            .is_some_and(|content_type| {
                content_type.contains("application/json")
                    || content_type.contains("+json")
                    || content_type.contains("application/x-ndjson")
            });
        if !is_json_or_ndjson {
            let bytes = response
                .bytes()
                .await
                .context("failed to drain successful sync DELETE response body")?;
            trace.finish(byte_len(&bytes));
            return Ok(Value::Null);
        }
        let body = response
            .text()
            .await
            .context("failed to read successful sync response body")?;
        let value = parse_json_or_ndjson(&body)?;
        trace.finish(byte_len(&body));
        Ok(value)
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
        let url = format!("{}/api{}", self.base_url, api_path);
        let request_bytes = envelope_json
            .len()
            .saturating_add(content_bytes.len())
            .saturating_add(
                extra_parts
                    .iter()
                    .map(|part| part.bytes.len())
                    .sum::<usize>(),
            );
        let mut trace = HttpTrace::api("PUT", api_path, u64::try_from(request_bytes).ok());
        let mut form = Form::new()
            .part(
                "envelope",
                Part::bytes(envelope_json.to_vec())
                    .file_name("envelope")
                    .mime_str("application/json")
                    .context("invalid envelope mime type")?,
            )
            .part(
                content_field_name.to_owned(),
                Part::bytes(content_bytes.to_vec())
                    .file_name(content_field_name.to_owned())
                    .mime_str(content_mime)
                    .with_context(|| format!("invalid content mime type '{content_mime}'"))?,
            );
        for part in extra_parts {
            form = form.part(
                part.name.clone(),
                Part::bytes(part.bytes.clone())
                    .file_name(part.file_name.clone())
                    .mime_str(&part.content_type)
                    .with_context(|| {
                        format!(
                            "invalid multipart content mime type '{}'",
                            part.content_type
                        )
                    })?,
            );
        }

        let response = self
            .client
            .put(url)
            .headers(bearer_web_headers(&self.token, &self.device_info)?)
            .multipart(form)
            .send()
            .await
            .context("failed to send entry multipart PUT request")?;
        trace.response(response.status().as_u16(), response.content_length());
        let response = ensure_success(response).await?;
        let bytes = response
            .bytes()
            .await
            .context("failed to read successful multipart PUT response bytes")?;
        trace.finish(byte_len(&bytes));
        Ok(bytes.to_vec())
    }

    async fn put_file_to_absolute_url(
        &self,
        url: &str,
        content_type: &str,
        headers: &[(&str, String)],
        file_path: &str,
        file_size_bytes: i64,
    ) -> Result<()> {
        let request_bytes = u64::try_from(file_size_bytes).ok();
        let mut trace = HttpTrace::media_upload(request_bytes);
        let media = MediaTrace::upload(content_type, request_bytes.unwrap_or_default());
        let mut header_map =
            replace_content_headers(HeaderMap::new(), content_type, file_size_bytes)?;
        for (name, value) in headers {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .with_context(|| format!("invalid header name '{name}'"))?;
            let header_value = HeaderValue::from_str(value)
                .with_context(|| format!("invalid header value for '{name}'"))?;
            header_map.insert(header_name, header_value);
        }
        let file = tokio::fs::File::open(file_path)
            .await
            .with_context(|| format!("failed to open upload file '{file_path}'"))?;
        let stream = ReaderStream::new(file).map_ok(Frame::data);
        let body = reqwest::Body::wrap(StreamBody::new(stream));
        let response = self
            .client
            .put(url)
            .headers(header_map)
            .body(body)
            .send()
            .await
            .context("failed to send binary PUT request")?;
        trace.response(response.status().as_u16(), response.content_length());
        let _ = ensure_success(response).await?;
        trace.finish(Some(0));
        media.finish();
        Ok(())
    }

    async fn put_bytes_to_absolute_url(
        &self,
        url: &str,
        content_type: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> Result<()> {
        let content_length =
            i64::try_from(body.len()).context("encrypted media body length exceeds i64 range")?;
        let mut trace = HttpTrace::media_upload(u64::try_from(body.len()).ok());
        let media = MediaTrace::upload(content_type, u64::try_from(body.len()).unwrap_or(u64::MAX));
        let mut header_map =
            replace_content_headers(HeaderMap::new(), content_type, content_length)?;
        for (name, value) in headers {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .with_context(|| format!("invalid header name '{name}'"))?;
            let header_value = HeaderValue::from_str(value)
                .with_context(|| format!("invalid header value for '{name}'"))?;
            header_map.insert(header_name, header_value);
        }
        let response = self
            .client
            .put(url)
            .headers(header_map)
            .body(body.to_vec())
            .send()
            .await
            .context("failed to send binary byte PUT request")?;
        trace.response(response.status().as_u16(), response.content_length());
        let _ = ensure_success(response).await?;
        trace.finish(Some(0));
        media.finish();
        Ok(())
    }
}

async fn parse_value_or_error(response: reqwest::Response, mut trace: HttpTrace) -> Result<Value> {
    trace.response(response.status().as_u16(), response.content_length());
    let response = ensure_success(response).await?;
    let body = response
        .text()
        .await
        .context("failed to read successful sync response body")?;
    let value = parse_json_or_ndjson(&body)?;
    trace.finish(byte_len(&body));
    Ok(value)
}

fn parse_json_or_ndjson(body: &str) -> Result<Value> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Ok(Value::Null);
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Ok(value);
    }

    let mut items = Vec::new();
    for (line_index, line) in body.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        items.push(
            serde_json::from_str::<Value>(line).with_context(|| {
                format!("failed to parse NDJSON response line {}", line_index + 1)
            })?,
        );
    }
    Ok(Value::Array(items))
}

#[cfg(test)]
mod tests {
    use super::{DeviceInfo, SyncApiClient, parse_json_or_ndjson};

    #[test]
    fn ndjson_rejects_a_malformed_line_instead_of_dropping_it() {
        let error = parse_json_or_ndjson("\n{\"id\":1}\nnot-json\n{\"id\":2}\n")
            .expect_err("a malformed line must fail the response");
        assert!(error.to_string().contains("line 3"));
    }
    #[tokio::test]
    async fn entry_lock_http_contract_handles_absent_targets_and_http_errors() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        for (status, body, permitted) in [
            (
                200,
                r#"{"lease":null,"server_time":"2026-09-01T00:00:00Z"}"#,
                true,
            ),
            (404, r#"{"error":"not_found"}"#, true),
            (404, "endpoint not deployed", false),
            (401, r#"{"error":"Unauthorized"}"#, false),
            (403, r#"{"error":"forbidden"}"#, false),
            (429, r#"{"error":"rate_limited"}"#, false),
            (503, "upstream-private-response-marker", false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                write!(socket, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                String::from_utf8(request).unwrap()
            });
            let dir = tempfile::tempdir().unwrap();
            let store = crate::store::sqlite::Store::open_at(&dir.path().join("test.db")).unwrap();
            let profile = store.get_or_create_profile_for_base_url(&url).unwrap();
            store
                .save_auth_session(
                    profile.id,
                    "synthetic-token",
                    "2026-09-01T00:00:00Z",
                    r#"{"user_id":77}"#,
                )
                .unwrap();
            store
                .upsert_singleton_json_row(
                    "feature_flags",
                    1,
                    r#"{"shared-journals-v2":true}"#,
                    None,
                )
                .unwrap();
            let device = DeviceInfo::resolve(Some("77"));
            let expected_id = device.id.clone();
            let api = SyncApiClient::new(&url, "synthetic-token", device).unwrap();
            let result = crate::sync::entry_lock::check_upload(
                &store,
                &api,
                profile.id,
                "j /",
                "e /",
                &serde_json::json!({"is_shared":true,"shared_permissions":"any_participant_full"}),
            )
            .await;
            assert_eq!(result.is_ok(), permitted, "status {status}");
            let request = server.join().unwrap();
            assert!(request.starts_with("GET /api/shares/j%20%2F/entries/e%20%2F/lock HTTP/1.1"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("cache-control: no-cache")
            );
            assert!(request.contains(&format!("Id=\"{expected_id}\"")));
            if let Err(error) = result {
                assert!(!error.to_string().contains(body));
            }
        }
    }
}
