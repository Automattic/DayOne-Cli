use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use reqwest::Url;
use serde::Serialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::http::{DayOneApiClient, client::DayOneClient, invitation};
use crate::store::sqlite::Store;
use crate::store::user_id_from_session_json;
use crate::sync::invitation_crypto::{build_proof, verify_owner_key};
use crate::telemetry::UserError;

// Deliberately no Debug: the token and fragment are invitation credentials.
struct Invitation {
    token: String,
    key: [u8; 32],
}

impl Invitation {
    fn parse(link: &str, base_url: &str) -> Result<Self> {
        let invalid = || {
            UserError::new(
                "invalid invitation link; use the complete HTTPS /accept-journal-invite?token=...#... URL",
            )
        };
        let url = Url::parse(link.trim()).map_err(|_| invalid())?;
        let base = Url::parse(base_url)?;
        let loopback = base.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        ensure!(
            base.scheme() == "https" || (base.scheme() == "http" && loopback),
            UserError::new(
                "joining requires an HTTPS API host; HTTP is supported only for loopback development"
            )
        );
        ensure!(
            url.scheme() == "https"
                && url.path() == "/accept-journal-invite"
                && url.username().is_empty()
                && url.password().is_none(),
            invalid()
        );
        ensure!(
            url.host_str() == base.host_str()
                && url.port_or_known_default() == base.port_or_known_default(),
            UserError::new(
                "invitation link does not match the selected API host; select a matching authenticated --profile"
            )
        );
        let tokens: Vec<_> = url.query_pairs().filter(|(k, _)| k == "token").collect();
        ensure!(
            tokens.len() == 1 && !tokens[0].1.trim().is_empty(),
            invalid()
        );
        let fragment = url.fragment().ok_or_else(invalid)?;
        let key = BASE64
            .decode(percent_encoding::percent_decode_str(fragment).collect::<Vec<_>>())
            .map_err(|_| invalid())?;
        let key: [u8; 32] = key.try_into().map_err(|_| invalid())?;
        Ok(Self {
            token: tokens[0].1.to_string(),
            key,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct JournalJoinOutput {
    pub ok: bool,
    pub journal_id: String,
    pub status: &'static str,
    pub message: &'static str,
}

pub async fn execute(store: &Store, base_url: &str, link: &str) -> Result<JournalJoinOutput> {
    let invitation = Invitation::parse(link, base_url)?;
    let session = store
        .get_auth_session_for_exact_base_url(base_url)?
        .ok_or_else(UserError::auth_required)?;
    let user_id = user_id_from_session_json(&session.user_json)?
        .ok_or_else(|| UserError::new("session has no user ID; run `dayone auth login` again"))?;
    let client = DayOneClient::new(base_url)?.with_bearer_token(&session.token)?;
    request_access(&client, store, session.profile_id, &user_id, invitation).await
}

async fn request_access(
    client: &impl DayOneApiClient,
    store: &Store,
    profile_id: i64,
    user_id: &str,
    invitation: Invitation,
) -> Result<JournalJoinOutput> {
    // Only send the token to the selected API host. The fragment never leaves this process.
    let info = invitation::fetch(client, &invitation.token).await?;
    let expires = OffsetDateTime::parse(&info.expiration_date, &Rfc3339)
        .context("invalid invitation expiration date")?;
    ensure!(
        expires > OffsetDateTime::now_utc(),
        UserError::new("invitation link expired; ask the owner for a new link")
    );
    ensure!(
        !info.journal_id.is_empty(),
        "invitation response is missing the journal ID"
    );
    let owner_key = info
        .owner_public_key
        .as_deref()
        .ok_or_else(|| UserError::new("invitation is missing the owner's public key"))?;
    let owner_hmac = info
        .owner_public_key_hmac
        .as_deref()
        .ok_or_else(|| UserError::new("invitation is missing the owner's key verification"))?;
    verify_owner_key(&invitation.key, owner_key, owner_hmac)
        .map_err(|error| UserError::new(error.to_string()))?;

    if info
        .participants
        .iter()
        .any(|p| p.id == user_id && p.role != "previous")
    {
        return Ok(JournalJoinOutput {
            ok: true,
            journal_id: info.journal_id,
            status: "already_member",
            message: "You already belong to this journal. Run `dayone sync` to refresh local journals.",
        });
    }
    let master_key = store.get_encryption_key_for_profile(profile_id)?
        .ok_or_else(|| UserError::new("joining requires an encryption key; run `dayone auth key-set` and `dayone sync` first"))?;
    let user_keys = store
        .get_user_identity_key_json()?
        .ok_or_else(|| UserError::new("synced user keys are missing; run `dayone sync` first"))?;
    let proof = build_proof(&master_key, &user_keys, &invitation.key, owner_key)
        .map_err(|error| UserError::new(error.to_string()))?;
    invitation::request_access(client, &invitation.token, &proof).await?;
    Ok(JournalJoinOutput {
        ok: true,
        journal_id: info.journal_id,
        status: "pending_approval",
        message: "Request sent. The journal owner must approve it. Run `dayone sync` after approval to download the journal.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::crypto::derive_d1_symmetric_key_from_master_key;
    use hmac::{Hmac, Mac};
    use rsa::pkcs8::{DecodePublicKey, EncodePublicKey, LineEnding};
    use rsa::{Pkcs1v15Sign, RsaPublicKey};
    use serde_json::Value;
    use sha2::{Digest, Sha256};
    type HmacSha256 = Hmac<Sha256>;
    use crate::http::fake::FakeApiClient;
    use crate::store::InMemorySecureMasterKeyStore;
    use crate::sync::crypto::{encrypt_d1_mode0_with_key, test_support::TEST_MASTER_KEY};
    use rsa::{RsaPrivateKey, pkcs8::EncodePrivateKey, rand_core::OsRng};
    use serde_json::json;
    use std::sync::{Arc, OnceLock};

    const BASE: &str = "https://dayone.me";
    const TOKEN: &str = "test-invitation-token";
    const KEY: [u8; 32] = [7; 32];

    fn link() -> String {
        format!(
            "{BASE}/accept-journal-invite?token={TOKEN}#{}",
            BASE64.encode(KEY)
        )
    }

    fn private_key() -> &'static RsaPrivateKey {
        static KEY_PAIR: OnceLock<RsaPrivateKey> = OnceLock::new();
        KEY_PAIR.get_or_init(|| RsaPrivateKey::new(&mut OsRng, 2048).unwrap())
    }

    fn public_pem() -> String {
        private_key()
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap()
    }

    fn user_keys() -> String {
        let symmetric = derive_d1_symmetric_key_from_master_key(TEST_MASTER_KEY).unwrap();
        let private = private_key().to_pkcs8_der().unwrap();
        json!({"userKey": {
            "publicKey": public_pem(),
            "encryptedPrivateKey": encrypt_d1_mode0_with_key(private.as_bytes(), &symmetric).unwrap()
        }}).to_string()
    }

    fn owner_private_key() -> &'static RsaPrivateKey {
        static OWNER: OnceLock<RsaPrivateKey> = OnceLock::new();
        OWNER.get_or_init(|| RsaPrivateKey::new(&mut OsRng, 2048).unwrap())
    }

    fn owner_pem() -> String {
        owner_private_key()
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap()
    }

    fn invite_info() -> Value {
        let owner = owner_pem();
        let mut mac = HmacSha256::new_from_slice(&KEY).unwrap();
        mac.update(owner.as_bytes());
        json!({
            "journal_id": "shared-journal-id",
            "expiration_date": "2099-01-01T00:00:00Z",
            "owner_public_key": owner,
            "owner_public_key_hmac": BASE64.encode(mac.finalize().into_bytes()),
            "participants": []
        })
    }

    fn store(with_keys: bool) -> (tempfile::TempDir, Store, i64) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_at_with_secure_master_key_store(
            dir.path().join("dayone.db"),
            Arc::new(InMemorySecureMasterKeyStore::default()),
        )
        .unwrap();
        let profile = store.get_or_create_profile_for_base_url(BASE).unwrap();
        store
            .save_auth_session(
                profile.id,
                "test-token",
                "2099-01-01T00:00:00Z",
                r#"{"id":"participant"}"#,
            )
            .unwrap();
        if with_keys {
            store
                .set_encryption_key_for_profile(profile.id, TEST_MASTER_KEY)
                .unwrap();
            store
                .upsert_singleton_json_row("user_identity_key", 1, &user_keys(), None)
                .unwrap();
        }
        store
            .enqueue_outbox_item(
                "unrelated",
                "entry",
                "update",
                "existing-entry",
                r#"{"body":"existing pending work"}"#,
                123456,
            )
            .unwrap();
        store.connect().unwrap().execute("UPDATE sync_outbox SET attempt_count = 3, last_error = 'retry later', status = 'processing' WHERE id = 'unrelated'", []).unwrap();
        (dir, store, profile.id)
    }

    fn outbox_snapshot(store: &Store) -> Vec<Vec<String>> {
        let conn = store.connect().unwrap();
        let mut statement = conn
            .prepare("SELECT * FROM sync_outbox ORDER BY id")
            .unwrap();
        let count = statement.column_count();
        statement
            .query_map([], |row| {
                (0..count)
                    .map(|index| Ok(format!("{:?}", row.get_ref(index)?)))
                    .collect()
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    }

    async fn api(info: Value) -> FakeApiClient {
        FakeApiClient::new()
            .with_json_fixture(format!("GET_JSON /shares/invite/{TOKEN}"), info)
            .await
            .with_json_fixture("POST_JSON /shares/request-access", Value::Null)
            .await
    }

    #[test]
    fn canonical_pem_and_proofs_match_independent_webcrypto_fixture() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/shared-journal-invitation.json"
        ))
        .unwrap();
        let proof = build_proof(
            TEST_MASTER_KEY,
            &fixture["user_keys"].to_string(),
            &KEY,
            fixture["owner_public_key"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(
            proof.participant_public_key_hmac,
            fixture["participant_public_key_hmac"]
        );
        assert_eq!(
            proof.owner_public_key_signature_by_participant,
            fixture["owner_public_key_signature_by_participant"]
        );
        for role in ["participant", "owner"] {
            let pem = fixture[format!("{role}_public_key")].as_str().unwrap();
            // Web re-exports imported keys as SPKI/LF, regardless of input line endings.
            let imported = RsaPublicKey::from_public_key_pem(&pem.replace('\n', "\r\n")).unwrap();
            let canonical = imported.to_public_key_pem(LineEnding::LF).unwrap();
            assert_eq!(canonical, pem);
            let mut mac = HmacSha256::new_from_slice(&KEY).unwrap();
            mac.update(canonical.as_bytes());
            mac.verify_slice(
                &BASE64
                    .decode(fixture[format!("{role}_public_key_hmac")].as_str().unwrap())
                    .unwrap(),
            )
            .unwrap();
        }
        RsaPublicKey::from_public_key_pem(fixture["participant_public_key"].as_str().unwrap())
            .unwrap()
            .verify(
                Pkcs1v15Sign::new::<Sha256>(),
                &Sha256::digest(fixture["owner_public_key"].as_str().unwrap().as_bytes()),
                &BASE64
                    .decode(
                        fixture["owner_public_key_signature_by_participant"]
                            .as_str()
                            .unwrap(),
                    )
                    .unwrap(),
            )
            .unwrap();
    }

    #[test]
    fn parses_complete_and_percent_encoded_links_without_losing_plus() {
        let input = format!(
            "{BASE}/accept-journal-invite?token=a%2Bb%2Fc#{}",
            BASE64
                .encode([251; 32])
                .replace('+', "%2B")
                .replace('/', "%2F")
                .replace('=', "%3D")
        );
        let invitation = Invitation::parse(&input, BASE).unwrap();
        assert_eq!(invitation.token, "a+b/c");
        assert_eq!(invitation.key, [251; 32]);
        assert_eq!(Invitation::parse(&link(), BASE).unwrap().key, KEY);
    }

    #[test]
    fn rejects_malformed_links_and_wrong_environment_without_echoing_credentials() {
        for input in [
            "not a URL".to_owned(),
            link().replace("https:", "http:"),
            link().replace("dayone.me", "evil.example"),
            link().replace("accept-journal-invite", "family/invite"),
            link().split('#').next().unwrap().to_owned(),
            link().replace("?token=", "?other="),
            link().replace("?token=", "?token=duplicate&token="),
            link().replace(&BASE64.encode(KEY), "short"),
            link().replace(&BASE64.encode(KEY), &BASE64.encode([0_u8; 16])),
            link().replace(&BASE64.encode(KEY), &BASE64.encode([0_u8; 31])),
            link().replace(&BASE64.encode(KEY), &BASE64.encode([0_u8; 33])),
            link().replace("https://", "https://user:password@"),
        ] {
            let err = Invitation::parse(&input, BASE)
                .err()
                .expect("must reject invalid link");
            assert!(!format!("{err:#}").contains(TOKEN));
        }
        assert!(Invitation::parse(&link(), "https://stg.dayone.me").is_err());
        assert!(Invitation::parse(&link(), "http://dayone.me:443").is_err());
        assert!(
            Invitation::parse(
                &link().replace("dayone.me", "127.0.0.1"),
                "http://127.0.0.1:443"
            )
            .is_ok()
        );
        assert!(
            Invitation::parse(&link().replace("dayone.me", "[::1]"), "http://[::1]:443").is_ok()
        );
    }

    #[tokio::test]
    async fn sends_web_compatible_proof_and_reports_pending_without_local_membership() {
        let (_dir, store, profile_id) = store(true);
        let before = outbox_snapshot(&store);
        let api = api(invite_info()).await;
        let result = request_access(
            &api,
            &store,
            profile_id,
            "participant",
            Invitation::parse(&link(), BASE).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(result.status, "pending_approval");
        assert_eq!(result.journal_id, "shared-journal-id");
        let bodies = api
            .captured_post_json_bodies("/shares/request-access")
            .await;
        assert_eq!(bodies.len(), 1);
        let body = &bodies[0];
        assert_eq!(body["token"], TOKEN);
        assert_eq!(body.as_object().unwrap().len(), 3);
        let mut mac = HmacSha256::new_from_slice(&KEY).unwrap();
        mac.update(public_pem().as_bytes());
        mac.verify_slice(
            &BASE64
                .decode(body["participant_public_key_hmac"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        private_key()
            .to_public_key()
            .verify(
                Pkcs1v15Sign::new::<Sha256>(),
                &Sha256::digest(owner_pem().as_bytes()),
                &BASE64
                    .decode(
                        body["owner_public_key_signature_by_participant"]
                            .as_str()
                            .unwrap(),
                    )
                    .unwrap(),
            )
            .unwrap();
        assert!(
            owner_private_key()
                .to_public_key()
                .verify(
                    Pkcs1v15Sign::new::<Sha256>(),
                    &Sha256::digest(owner_pem().as_bytes()),
                    &BASE64
                        .decode(
                            body["owner_public_key_signature_by_participant"]
                                .as_str()
                                .unwrap()
                        )
                        .unwrap(),
                )
                .is_err()
        );
        assert!(
            private_key()
                .to_public_key()
                .verify(
                    Pkcs1v15Sign::new::<Sha256>(),
                    &Sha256::digest(public_pem().as_bytes()),
                    &BASE64
                        .decode(
                            body["owner_public_key_signature_by_participant"]
                                .as_str()
                                .unwrap()
                        )
                        .unwrap(),
                )
                .is_err()
        );
        assert!(!serde_json::to_string(&result).unwrap().contains(TOKEN));
        assert!(!body.to_string().contains(&BASE64.encode(KEY)));
        assert!(
            store
                .get_json_row_by_id("journals", "shared-journal-id")
                .unwrap()
                .is_none()
        );
        assert_eq!(outbox_snapshot(&store), before);
    }

    #[tokio::test]
    async fn refuses_expired_tampered_or_incomplete_invites_before_posting() {
        let (_dir, store, profile_id) = store(true);
        for (field, value) in [
            ("expiration_date", json!("2000-01-01T00:00:00Z")),
            ("expiration_date", json!("invalid")),
            ("owner_public_key_hmac", json!(BASE64.encode([0; 32]))),
            ("owner_public_key", Value::Null),
            ("owner_public_key_hmac", Value::Null),
            ("journal_id", json!("")),
        ] {
            let mut info = invite_info();
            info[field] = value;
            let api = api(info).await;
            assert!(
                request_access(
                    &api,
                    &store,
                    profile_id,
                    "participant",
                    Invitation::parse(&link(), BASE).unwrap()
                )
                .await
                .is_err()
            );
            assert!(
                api.captured_post_json_bodies("/shares/request-access")
                    .await
                    .is_empty()
            );
        }
        let malformed_owner = "authenticated text that is not an RSA public key";
        let mut mac = HmacSha256::new_from_slice(&KEY).unwrap();
        mac.update(malformed_owner.as_bytes());
        let mut info = invite_info();
        info["owner_public_key"] = json!(malformed_owner);
        info["owner_public_key_hmac"] = json!(BASE64.encode(mac.finalize().into_bytes()));
        let api = api(info).await;
        let error = request_access(
            &api,
            &store,
            profile_id,
            "participant",
            Invitation::parse(&link(), BASE).unwrap(),
        )
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("invalid invitation owner public key")
        );
        assert!(
            api.captured_post_json_bodies("/shares/request-access")
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn already_member_does_not_require_keys_or_post_again() {
        let (_dir, store, profile_id) = store(false);
        let mut info = invite_info();
        info["participants"] = json!([{"id": "participant", "role": "member"}]);
        let before = outbox_snapshot(&store);
        let api = api(info).await;
        let result = request_access(
            &api,
            &store,
            profile_id,
            "participant",
            Invitation::parse(&link(), BASE).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(result.status, "already_member");
        assert!(
            api.captured_post_json_bodies("/shares/request-access")
                .await
                .is_empty()
        );
        assert_eq!(outbox_snapshot(&store), before);
    }

    #[tokio::test]
    async fn other_participants_do_not_block_new_or_previous_members() {
        let (_dir, store, profile_id) = store(true);
        for was_previous in [false, true] {
            let mut info = invite_info();
            info["participants"] = json!([{"id": "someone-else", "role": "member"}]);
            if was_previous {
                info["participants"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"id":"participant", "role":"previous"}));
            }
            let api = api(info).await;
            assert_eq!(
                request_access(
                    &api,
                    &store,
                    profile_id,
                    "participant",
                    Invitation::parse(&link(), BASE).unwrap()
                )
                .await
                .unwrap()
                .status,
                "pending_approval"
            );
            assert_eq!(
                api.captured_post_json_bodies("/shares/request-access")
                    .await
                    .len(),
                1
            );
        }
    }

    #[tokio::test]
    async fn missing_setup_fails_before_posting() {
        let (_dir, store, profile_id) = store(false);
        for has_master_key in [false, true] {
            if has_master_key {
                store
                    .set_encryption_key_for_profile(profile_id, TEST_MASTER_KEY)
                    .unwrap();
            }
            let api = api(invite_info()).await;
            let err = request_access(
                &api,
                &store,
                profile_id,
                "participant",
                Invitation::parse(&link(), BASE).unwrap(),
            )
            .await
            .unwrap_err();
            assert!(err.to_string().contains(if has_master_key {
                "synced user keys"
            } else {
                "encryption key"
            }));
            assert!(
                api.captured_post_json_bodies("/shares/request-access")
                    .await
                    .is_empty()
            );
        }
    }

    #[test]
    fn wrong_master_key_and_mismatched_user_keys_cannot_produce_proofs() {
        assert!(build_proof("D1-wrong-key", &user_keys(), &KEY, &public_pem()).is_err());
        let mut keys: Value = serde_json::from_str(&user_keys()).unwrap();
        let other = RsaPrivateKey::new(&mut OsRng, 2048)
            .unwrap()
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap();
        keys["userKey"]["publicKey"] = json!(other);
        assert!(build_proof(TEST_MASTER_KEY, &keys.to_string(), &KEY, &public_pem()).is_err());
        let short = RsaPrivateKey::new(&mut OsRng, 1024).unwrap();
        let symmetric = derive_d1_symmetric_key_from_master_key(TEST_MASTER_KEY).unwrap();
        let short_keys = json!({"userKey": {
            "publicKey": short.to_public_key().to_public_key_pem(LineEnding::LF).unwrap(),
            "encryptedPrivateKey": encrypt_d1_mode0_with_key(short.to_pkcs8_der().unwrap().as_bytes(), &symmetric).unwrap()
        }});
        assert!(build_proof(TEST_MASTER_KEY, &short_keys.to_string(), &KEY, &owner_pem()).is_err());
    }

    #[tokio::test]
    async fn real_http_handles_empty_success_and_rejections_without_local_writes() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        let reserved_token = "a+b/c%?#";
        let encoded_link = format!(
            "{BASE}/accept-journal-invite?token=a%2Bb%2Fc%25%3F%23#{}",
            BASE64.encode(KEY)
        );
        for status in [200, 403, 409] {
            let (_dir, store, profile_id) = store(true);
            let before = outbox_snapshot(&store);
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let info = invite_info().to_string();
            let server = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut requests = Vec::new();
                for (response_status, response_body) in [
                    (200, info),
                    (status, if status == 200 { String::new() } else { link() }),
                ] {
                    let mut stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                                assert!(Instant::now() < deadline, "HTTP request timed out");
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            Err(err) => panic!("{err}"),
                        }
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    loop {
                        let mut chunk = [0; 4096];
                        let count = stream.read(&mut chunk).unwrap();
                        assert!(count > 0, "incomplete request");
                        bytes.extend_from_slice(&chunk[..count]);
                        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&bytes[..end]);
                            let length: usize = headers
                                .lines()
                                .find_map(|line| {
                                    line.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|value| value.trim().parse().unwrap())
                                })
                                .unwrap_or(0);
                            if bytes.len() >= end + 4 + length {
                                break;
                            }
                        }
                    }
                    requests.push(String::from_utf8(bytes).unwrap());
                    write!(stream, "HTTP/1.1 {} response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response_status, response_body.len(), response_body).unwrap();
                }
                requests
            });
            let client = DayOneClient::new(format!("http://{address}"))
                .unwrap()
                .with_bearer_token("test-bearer")
                .unwrap();
            let result = request_access(
                &client,
                &store,
                profile_id,
                "participant",
                Invitation::parse(&encoded_link, BASE).unwrap(),
            )
            .await;
            if status == 200 {
                assert_eq!(result.unwrap().status, "pending_approval");
            } else {
                let error = format!("{:#}", result.unwrap_err());
                assert!(error.contains(&format!("HTTP {status}")));
                assert!(!error.contains(TOKEN));
                assert!(!error.contains(reserved_token));
                assert!(!error.contains(&BASE64.encode(KEY)));
            }
            assert_eq!(outbox_snapshot(&store), before);
            let requests = server.join().unwrap();
            assert!(requests[0].starts_with("GET /api/shares/invite/a%2Bb%2Fc%25%3F%23 HTTP/1.1"));
            assert!(requests[1].starts_with("POST /api/shares/request-access HTTP/1.1"));
            for request in &requests {
                assert!(
                    request
                        .to_ascii_lowercase()
                        .contains("authorization: bearer test-bearer")
                );
                assert!(!request.contains(&BASE64.encode(KEY)));
            }
            let payload: Value =
                serde_json::from_str(requests[1].split("\r\n\r\n").nth(1).unwrap()).unwrap();
            assert_eq!(payload["token"], reserved_token);
            assert!(payload["participant_public_key_hmac"].is_string());
            assert!(payload["owner_public_key_signature_by_participant"].is_string());
        }
    }

    #[tokio::test]
    async fn execute_uses_only_the_requested_hosts_session() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_for_profile(dir.path(), "host-a", BASE).unwrap();
        let host_a = store.get_or_create_profile_for_base_url(BASE).unwrap();
        store
            .save_auth_session(
                host_a.id,
                "test-token",
                "2099-01-01T00:00:00Z",
                r#"{"id":"participant"}"#,
            )
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{address}");
        let link_b = format!(
            "https://{address}/accept-journal-invite?token={TOKEN}#{}",
            BASE64.encode(KEY)
        );
        let error = execute(&store, &base, &link_b).await.unwrap_err();
        assert!(error.to_string().contains("auth login"));
        assert!(
            matches!(listener.accept(), Err(err) if err.kind() == std::io::ErrorKind::WouldBlock)
        );
        let store = Store::open_for_profile(dir.path(), "host-b", &base).unwrap();
        let host_b = store.get_or_create_profile_for_base_url(&base).unwrap();
        store
            .save_auth_session(
                host_b.id,
                "host-b-token",
                "2099-01-01T00:00:00Z",
                r#"{"id":"participant"}"#,
            )
            .unwrap();
        let mut info = invite_info();
        info["participants"] = json!([{"id":"participant", "role":"member"}]);
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline);
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(err) => panic!("{err}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let mut bytes = [0; 2048];
                let count = stream.read(&mut bytes).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&bytes[..count]);
            }
            let info = info.to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", info.len(), info).unwrap();
            String::from_utf8(request).unwrap()
        });
        assert_eq!(
            execute(&store, &base, &link_b).await.unwrap().status,
            "already_member"
        );
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(request.contains("authorization: bearer host-b-token"));
        assert!(!request.contains("bearer test-token"));
    }

    #[tokio::test]
    async fn missing_auth_fails_before_any_network_access() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_at(dir.path().join("dayone.db")).unwrap();
        let err = execute(&store, BASE, &link()).await.unwrap_err();
        assert!(err.to_string().contains("auth login"));
    }
}
