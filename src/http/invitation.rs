use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::to_value;

use crate::http::{ApiError, DayOneApiClient};
use crate::sync::invitation_crypto::InvitationProof;
use crate::telemetry::UserError;
use crate::util::encode_path_segment;

/// Credential-safe categories: never retain response values or request URLs.
#[derive(Debug, thiserror::Error)]
pub enum InvitationError {
    #[error("invalid shared journal invitation response")]
    Response,
    #[error("invitation transport failed; check the API host and network connection")]
    Transport,
    #[error("unexpected invitation client failure")]
    Client,
}

#[derive(Deserialize)]
pub struct InviteInfo {
    pub journal_id: String,
    pub expiration_date: String,
    pub owner_public_key: Option<String>,
    pub owner_public_key_hmac: Option<String>,
    #[serde(default)]
    pub participants: Vec<Participant>,
}

#[derive(Deserialize)]
pub struct Participant {
    pub id: String,
    pub role: String,
}

pub async fn fetch(client: &impl DayOneApiClient, token: &str) -> Result<InviteInfo> {
    let info = client
        .get_json(
            &format!("/shares/invite/{}", encode_path_segment(token)),
            &[],
        )
        .await
        .map_err(|err| request_error(err, "fetch invitation"))?;
    serde_json::from_value(info).map_err(|_| InvitationError::Response.into())
}

pub async fn request_access(
    client: &impl DayOneApiClient,
    token: &str,
    proof: &InvitationProof,
) -> Result<()> {
    #[derive(Serialize)]
    struct RequestAccess<'a> {
        token: &'a str,
        participant_public_key_hmac: &'a str,
        owner_public_key_signature_by_participant: &'a str,
    }
    let payload = to_value(RequestAccess {
        token,
        participant_public_key_hmac: &proof.participant_public_key_hmac,
        owner_public_key_signature_by_participant: &proof.owner_public_key_signature_by_participant,
    })?;
    client
        .post_json("/shares/request-access", &payload)
        .await
        .map_err(|err| request_error(err, "request access"))?;
    Ok(())
}

// Invitation URLs and even server error bodies may contain credentials. Do not
// retain the HTTP error chain (reqwest includes its request URL).
fn request_error(err: anyhow::Error, operation: &str) -> anyhow::Error {
    if let Some(api) = err.downcast_ref::<ApiError>() {
        let expected_invitation_rejection = api.status() == 400
            && operation == "request access"
            && matches!(
                api.body().trim(),
                "Invalid token."
                    | "Expired invitation link"
                    | "Journal has been deleted"
                    | "Ineligible user due to pending approval or is already participating in the shared journal."
            );
        if !expected_invitation_rejection
            && !matches!(api.status(), 401 | 403 | 404 | 409 | 410 | 429)
        {
            return anyhow::Error::new(ApiError::new(
                api.status(),
                "invitation request failed",
                "",
            ))
            .context(format!("failed to {operation}"));
        }
        let hint = match api.status() {
            401 => "run `dayone auth login` again",
            403 => "this account is not permitted to join the journal",
            404 | 410 => "ask the owner for a new invitation link",
            429 => "too many requests; retry later",
            _ => "the link may be expired or revoked, or access may already be pending/granted",
        };
        return UserError::new(format!(
            "failed to {operation} (HTTP {}); {hint}",
            api.status()
        ))
        .into();
    }
    let category = if err.downcast_ref::<serde_json::Error>().is_some() {
        InvitationError::Response
    } else if let Some(transport) = err.downcast_ref::<reqwest::Error>() {
        if transport.is_decode() {
            InvitationError::Response
        } else {
            InvitationError::Transport
        }
    } else {
        InvitationError::Client
    };
    anyhow::Error::new(category).context(format!("failed to {operation}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    const SECRET: &str =
        "https://dayone.me/accept-journal-invite?token=private-token#private-fragment";

    #[tokio::test]
    async fn malformed_success_responses_are_scrubbed_and_classified() {
        let client = crate::http::fake::FakeApiClient::new()
            .with_json_fixture(
                "GET_JSON /shares/invite/test",
                serde_json::json!({
                    "journal_id":"id", "expiration_date":"2099-01-01T00:00:00Z",
                    "owner_public_key":null, "owner_public_key_hmac":null,
                    "participants":[SECRET]
                }),
            )
            .await;
        let error = fetch(&client, "test").await.err().unwrap();
        assert!(!format!("{error:#}").contains("private-token"));
        assert_eq!(
            crate::diagnostics::classify_error(error.as_ref()).category,
            "serialization"
        );
        let decoding = serde_json::from_str::<serde_json::Value>("invalid JSON").unwrap_err();
        let error = request_error(decoding.into(), "fetch invitation");
        assert_eq!(
            crate::diagnostics::classify_error(error.as_ref()).category,
            "serialization"
        );
        let response = reqwest::Client::new()
            .get("http://127.0.0.1:0/private-token")
            .send()
            .await
            .unwrap_err();
        let error = request_error(response.into(), "fetch invitation");
        assert_eq!(
            crate::diagnostics::classify_error(error.as_ref()).category,
            "http"
        );
        assert!(!format!("{error:#}").contains("private-token"));
    }

    #[test]
    fn errors_keep_credentials_private_and_preserve_classification() {
        for status in [400, 401, 403, 404, 422, 503] {
            let safe = request_error(
                ApiError::new(status, "error", SECRET).into(),
                "request access",
            );
            assert!(!format!("{safe:#}").contains("private-token"));
            assert!(!format!("{safe:#}").contains("private-fragment"));
            assert_eq!(
                crate::telemetry::is_user_error(&safe),
                matches!(status, 401 | 403 | 404)
            );
            if status == 503 {
                assert_eq!(safe.downcast_ref::<ApiError>().unwrap().status(), 503);
            }
            if status == 401 {
                assert!(safe.to_string().contains("auth login"));
            }
        }
        let expected = request_error(
            ApiError::new(400, "Bad Request", "Expired invitation link").into(),
            "request access",
        );
        assert!(crate::telemetry::is_user_error(&expected));
        let unexpected = request_error(
            ApiError::new(
                400,
                "Bad Request",
                "The owner's public key signature is invalid.",
            )
            .into(),
            "request access",
        );
        assert!(!crate::telemetry::is_user_error(&unexpected));
        assert_eq!(
            crate::diagnostics::classify_error(unexpected.as_ref()).http_status,
            Some(400)
        );
        let safe = request_error(
            anyhow::anyhow!("request failed for {SECRET}"),
            "fetch invitation",
        );
        assert!(!format!("{safe:#}").contains("private-token"));
        assert!(!crate::telemetry::is_user_error(&safe));
    }
}
