use anyhow::{Context, Result, anyhow};
use reqwest::header::CONTENT_TYPE;
use serde_json::Value;

use crate::auth::types::{LoginV3RequestBody, LoginV3Response};
use crate::diagnostics::{HttpTrace, byte_len, json_len};
use crate::http::{DayOneApiClient, bearer_headers, ensure_success, normalize_base_url};

#[derive(Clone)]
pub struct DayOneClient {
    base_url: String,
    client: reqwest::Client,
    bearer_token: Option<String>,
}

impl DayOneClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let base_url = normalize_base_url(&base_url.into())?;
        let client = reqwest::Client::builder()
            .user_agent(format!("dayone-cli/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed to initialize HTTP client")?;
        Ok(Self {
            base_url,
            client,
            bearer_token: None,
        })
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Result<Self> {
        let token = token.into();
        let normalized = token.trim();
        if normalized.is_empty() {
            return Err(anyhow!("token cannot be empty"));
        }
        self.bearer_token = Some(normalized.to_owned());
        Ok(self)
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn bearer_headers_for_api_trait(&self) -> Result<reqwest::header::HeaderMap> {
        let token = self.bearer_token.as_deref().ok_or_else(|| {
            anyhow!(
                "DayOneClient requires bearer token for DayOneApiClient calls; construct with with_bearer_token(...)"
            )
        })?;
        bearer_headers(token)
    }

    pub async fn login_v3(&self, email: &str, password: &str) -> Result<LoginV3Response> {
        let payload = LoginV3RequestBody {
            email: email.to_owned(),
            password: password.to_owned(),
        };
        let url = format!("{}/api/v3/users/login", self.base_url);
        // Omit login request bytes: their size can reveal email/password lengths.
        let mut trace = HttpTrace::api("POST", "/v3/users/login", None);
        let response = self
            .client
            .post(url)
            .json(&payload)
            .send()
            .await
            .context("failed to send login request")?;
        let status = response.status().as_u16();
        trace.response(status, response.content_length());
        let response = ensure_success(response).await?;
        let body = response
            .bytes()
            .await
            .context("failed to read successful response body")?;
        trace.response(status, byte_len(&body));
        let parsed = serde_json::from_slice::<LoginV3Response>(&body)
            .context("failed to parse successful response body")?;
        trace.finish(None);
        Ok(parsed)
    }

    pub async fn get_user_settings(&self) -> Result<Value> {
        let url = format!("{}/api/user-settings", self.base_url);
        let headers = self.bearer_headers_for_api_trait()?;
        let mut trace = HttpTrace::api("GET", "/user-settings", Some(0));
        let response = self
            .client
            .get(url)
            .headers(headers)
            .send()
            .await
            .context("failed to send user-settings request")?;
        let status = response.status().as_u16();
        trace.response(status, response.content_length());
        let response = ensure_success(response).await?;
        let body = response
            .bytes()
            .await
            .context("failed to read successful response body")?;
        trace.response(status, byte_len(&body));
        let parsed = serde_json::from_slice::<Value>(&body)
            .context("failed to parse successful response body")?;
        trace.finish(None);
        Ok(parsed)
    }
}

impl DayOneApiClient for DayOneClient {
    async fn get_json(&self, api_path: &str, query: &[(&str, String)]) -> Result<Value> {
        let url = format!("{}/api{}", self.base_url, api_path);
        let mut trace = HttpTrace::api("GET", api_path, Some(0));
        let response = self
            .client
            .get(url)
            .headers(self.bearer_headers_for_api_trait()?)
            .query(query)
            .send()
            .await
            .context("failed to send JSON GET request")?;
        trace.response(response.status().as_u16(), response.content_length());
        let response = ensure_success(response).await?;
        let body = response
            .text()
            .await
            .context("failed to read successful response body")?;
        let value =
            parse_json_body(&body).context("failed to parse successful response body as JSON")?;
        trace.finish(byte_len(&body));
        Ok(value)
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
            .headers(self.bearer_headers_for_api_trait()?)
            .query(query)
            .send()
            .await
            .context("failed to send JSON GET request")?;
        trace.response(response.status().as_u16(), response.content_length());
        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            trace.finish(Some(0));
            return Ok(None);
        }
        let response = ensure_success(response).await?;
        let body = response
            .text()
            .await
            .context("failed to read successful response body")?;
        let value =
            parse_json_body(&body).context("failed to parse successful response body as JSON")?;
        trace.finish(byte_len(&body));
        Ok(Some(value))
    }

    async fn get_bytes(&self, api_path: &str, query: &[(&str, String)]) -> Result<Vec<u8>> {
        let url = format!("{}/api{}", self.base_url, api_path);
        let mut trace = HttpTrace::api("GET", api_path, Some(0));
        let response = self
            .client
            .get(url)
            .headers(self.bearer_headers_for_api_trait()?)
            .query(query)
            .send()
            .await
            .context("failed to send bytes GET request")?;
        trace.response(response.status().as_u16(), response.content_length());
        let response = ensure_success(response).await?;
        let bytes = response
            .bytes()
            .await
            .context("failed to read successful response bytes")?;
        trace.finish(byte_len(&bytes));
        Ok(bytes.to_vec())
    }

    async fn put_json_allow_409(&self, api_path: &str, payload: &Value) -> Result<(u16, Value)> {
        let url = format!("{}/api{}", self.base_url, api_path);
        let mut trace = HttpTrace::api("PUT", api_path, json_len(payload));
        let response = self
            .client
            .put(url)
            .headers(self.bearer_headers_for_api_trait()?)
            .json(payload)
            .send()
            .await
            .context("failed to send JSON PUT request")?;
        trace.response(response.status().as_u16(), response.content_length());
        let status = response.status();
        if status.is_success() || status == reqwest::StatusCode::CONFLICT {
            let body = response
                .text()
                .await
                .context("failed to read PUT response body")?;
            let value = parse_json_body(&body).with_context(|| {
                format!("failed to decode PUT response body as JSON (status {status})")
            })?;
            trace.finish(byte_len(&body));
            return Ok((status.as_u16(), value));
        }
        let _ = ensure_success(response).await?;
        unreachable!("ensure_success returns Err for non-success responses");
    }

    async fn put_json(&self, api_path: &str, payload: &Value) -> Result<Value> {
        json_request(self, "PUT", api_path, payload).await
    }

    async fn post_json(&self, api_path: &str, payload: &Value) -> Result<Value> {
        json_request(self, "POST", api_path, payload).await
    }

    async fn delete_json(&self, api_path: &str, query: &[(&str, String)]) -> Result<Value> {
        let url = format!("{}/api{}", self.base_url, api_path);
        let mut trace = HttpTrace::api("DELETE", api_path, Some(0));
        let response = self
            .client
            .delete(url)
            .headers(self.bearer_headers_for_api_trait()?)
            .query(query)
            .send()
            .await
            .context("failed to send JSON DELETE request")?;
        trace.response(response.status().as_u16(), response.content_length());
        let response = ensure_success(response).await?;
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let body = response
            .text()
            .await
            .context("failed to read successful response body")?;
        let trimmed = body.trim();
        let value = if trimmed.is_empty() {
            Value::Null
        } else if content_type.contains("application/json") || content_type.contains("+json") {
            serde_json::from_str::<Value>(trimmed)
                .context("failed to parse successful response body as JSON")?
        } else {
            // Some Day One DELETE endpoints return plain text on success.
            Value::Null
        };
        trace.finish(byte_len(&body));
        Ok(value)
    }
}

async fn json_request(
    client: &DayOneClient,
    method: &'static str,
    api_path: &str,
    payload: &Value,
) -> Result<Value> {
    let url = format!("{}/api{}", client.base_url, api_path);
    let mut trace = HttpTrace::api(method, api_path, json_len(payload));
    let request = match method {
        "PUT" => client.client.put(url),
        "POST" => client.client.post(url),
        _ => unreachable!("JSON helper supports PUT and POST only"),
    };
    let response = request
        .headers(client.bearer_headers_for_api_trait()?)
        .json(payload)
        .send()
        .await
        .with_context(|| format!("failed to send JSON {method} request"))?;
    trace.response(response.status().as_u16(), response.content_length());
    let response = ensure_success(response).await?;
    let body = response
        .text()
        .await
        .context("failed to read successful response body")?;
    let value =
        parse_json_body(&body).context("failed to parse successful response body as JSON")?;
    trace.finish(byte_len(&body));
    Ok(value)
}

fn parse_json_body(body: &str) -> serde_json::Result<Value> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_str(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::{DayOneClient, bearer_headers};
    use crate::http::DayOneApiClient;
    use reqwest::header::AUTHORIZATION;

    #[test]
    fn auth_header_uses_bearer_scheme() {
        let headers = bearer_headers("abc123").expect("header generation should work");
        let value = headers
            .get(AUTHORIZATION)
            .expect("authorization header should exist")
            .to_str()
            .expect("header should be valid utf-8");
        assert_eq!(value, "Bearer abc123");
    }

    #[tokio::test]
    async fn trait_calls_require_bearer_token() {
        let client = DayOneClient::new("https://stg.dayone.me").expect("client should construct");
        let err = client
            .get_json("/users/key", &[])
            .await
            .expect_err("trait call should fail without configured token");
        assert!(
            err.to_string().contains("requires bearer token"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn with_bearer_token_rejects_blank_values() {
        let result = DayOneClient::new("https://stg.dayone.me")
            .expect("client should construct")
            .with_bearer_token("   ");
        assert!(result.is_err(), "blank token should fail");
    }
}
