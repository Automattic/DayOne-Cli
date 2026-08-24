use std::fs;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use rsa::pkcs1::DecodeRsaPublicKey;
use rsa::pkcs8::{DecodePublicKey, EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::rand_core::{OsRng, RngCore};
use rsa::{Oaep, RsaPrivateKey, RsaPublicKey};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::store::sqlite::Store;
use crate::sync::crypto::{
    bytes_to_hex_lower, decrypt_d1_mode0_with_key, derive_d1_symmetric_key_from_master_key,
    encrypt_d1_mode0_with_key, parse_private_key_bytes,
};
use crate::util::now_rfc3339_utc;

#[derive(Debug, Clone)]
pub struct JournalCreateArgs {
    pub json: Option<String>,
    pub json_file: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub color: Option<String>,
    pub shared: bool,
    pub e2e: bool,
    pub plaintext: bool,
    pub sort_method: Option<String>,
    pub hide_on_this_day: Option<bool>,
    pub hide_all_entries: Option<bool>,
    pub conceal: Option<bool>,
    pub add_location_to_new_entries: Option<bool>,
    pub comments_disabled: Option<bool>,
    pub template_id: Option<String>,
    pub preset_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct JournalCreateOutput {
    pub ok: bool,
    pub base_url: String,
    pub profile_id: i64,
    pub journal_id: String,
    pub shared: bool,
    pub encrypted: bool,
    pub queued: bool,
    pub synced: bool,
    pub outbox_id: String,
    pub journal: Value,
}

#[derive(Debug, Clone)]
struct UserKeyMaterial {
    user_id: String,
    user_fingerprint: String,
    user_public_key_pem: String,
    user_private_key: RsaPrivateKey,
    owner_public_key_signature: Option<String>,
}

pub async fn execute(
    store: &Store,
    base_url: &str,
    args: JournalCreateArgs,
) -> Result<JournalCreateOutput> {
    let session = store
        .get_auth_session_for_base_url(base_url)?
        .ok_or_else(crate::telemetry::UserError::auth_required)?;

    let mut payload = load_payload(&args)?;
    merge_convenience_flags(&mut payload, &args)?;
    filter_create_payload(&mut payload);

    let (shared, _api_path) = resolve_create_route(&payload, args.shared);
    let payload_requests_e2e = payload_has_e2e_request(&payload);
    let payload_requests_plaintext = payload_has_plaintext_request(&payload);
    let explicit_e2e = args.e2e || payload_requests_e2e;
    let explicit_plaintext = args.plaintext || payload_requests_plaintext;
    if explicit_e2e && explicit_plaintext {
        bail!(
            "conflicting encryption options; choose E2E (`--e2e` or payload encryption object) or plaintext (`--plaintext`/`--no-e2e` or payload `\"encryption\":\"plaintext\"`)"
        );
    }
    let mut profile_master_key: Option<String> = None;
    let wants_e2e = if explicit_e2e || explicit_plaintext {
        resolve_wants_e2e(explicit_e2e, explicit_plaintext, false)
    } else {
        profile_master_key = load_profile_master_key(store, session.profile_id)?;
        resolve_wants_e2e(
            explicit_e2e,
            explicit_plaintext,
            profile_master_key.is_some(),
        )
    };

    if shared && !wants_e2e {
        bail!("{}", shared_journal_e2e_error_message(explicit_plaintext));
    }

    if wants_e2e && !payload_requests_e2e {
        if profile_master_key.is_none() {
            profile_master_key = load_profile_master_key(store, session.profile_id)?;
        }
        let master_key = profile_master_key.ok_or_else(|| {
            anyhow!(
                "missing encryption key for E2E journal create; run `dayone auth key-set` first"
            )
        })?;
        let user_key_json = store.get_user_identity_key_json()?.ok_or_else(|| {
            anyhow!("missing synced user key; run `dayone sync` before creating an E2E journal")
        })?;
        let key_material = parse_user_key_material(&master_key, &user_key_json)?;
        payload.insert(
            "encryption".to_owned(),
            build_e2e_encryption_payload(&key_material)?,
        );

        if shared && !payload.contains_key("owner_public_key_signature_by_owner") {
            let signature = key_material
                .owner_public_key_signature
                .clone()
                .or_else(|| sign_owner_public_key(&key_material).ok())
                .ok_or_else(|| {
                    anyhow!(
                        "unable to generate owner_public_key_signature_by_owner for shared journal"
                    )
                })?;
            payload.insert(
                "owner_public_key_signature_by_owner".to_owned(),
                Value::String(signature),
            );
        }
    }

    payload.remove("is_shared");

    let now_ms = now_epoch_ms();
    let now_iso = now_rfc3339_utc();
    let journal_id = payload
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("pending-{}", short_hash(&format!("{:?}-{now_ms}", payload))));
    payload.insert("id".to_owned(), Value::String(journal_id.clone()));
    payload.insert("pending_sync".to_owned(), Value::Bool(true));
    payload.insert("is_shared".to_owned(), Value::Bool(shared));
    payload.insert("updated_at".to_owned(), Value::String(now_iso));

    let local_journal = Value::Object(payload.clone());
    let updated_at = local_journal.get("updated_at").and_then(Value::as_str);
    let deleted_at = local_journal.get("deleted_at").and_then(Value::as_str);
    store.upsert_json_row(
        "journals",
        &journal_id,
        updated_at,
        deleted_at,
        &local_journal.to_string(),
    )?;
    let outbox_id = format!("journal:{journal_id}");
    store.enqueue_outbox_item(
        &outbox_id,
        "journal",
        "create",
        &journal_id,
        &local_journal.to_string(),
        now_ms,
    )?;

    Ok(JournalCreateOutput {
        ok: true,
        base_url: base_url.to_owned(),
        profile_id: session.profile_id,
        journal_id,
        shared: is_shared_journal_response(&local_journal, shared),
        encrypted: !is_plaintext_journal(&local_journal),
        queued: true,
        synced: false,
        outbox_id,
        journal: local_journal,
    })
}

fn load_payload(args: &JournalCreateArgs) -> Result<Map<String, Value>> {
    if args.json.is_some() && args.json_file.is_some() {
        bail!("use either --json or --json-file, not both");
    }

    let raw = if let Some(inline) = args.json.as_deref() {
        inline.to_owned()
    } else if let Some(path) = args.json_file.as_deref() {
        fs::read_to_string(path).with_context(|| format!("failed to read JSON file '{path}'"))?
    } else {
        "{}".to_owned()
    };

    let value: Value = serde_json::from_str(&raw).context("invalid JSON payload")?;
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow!("journal create JSON payload must be an object"))?;
    Ok(obj.clone())
}

fn merge_convenience_flags(
    payload: &mut Map<String, Value>,
    args: &JournalCreateArgs,
) -> Result<()> {
    set_if_absent(payload, "name", args.name.clone().map(Value::String));
    set_if_absent(
        payload,
        "description_v2",
        args.description.clone().map(Value::String),
    );
    set_if_absent(payload, "color", args.color.clone().map(Value::String));
    set_if_absent(
        payload,
        "sort_method",
        args.sort_method.clone().map(Value::String),
    );
    set_if_absent(
        payload,
        "hide_on_this_day",
        args.hide_on_this_day.map(Value::Bool),
    );
    set_if_absent(
        payload,
        "hide_all_entries",
        args.hide_all_entries.map(Value::Bool),
    );
    set_if_absent(payload, "conceal", args.conceal.map(Value::Bool));
    set_if_absent(
        payload,
        "add_location_to_new_entries",
        args.add_location_to_new_entries.map(Value::Bool),
    );
    set_if_absent(
        payload,
        "comments_disabled",
        args.comments_disabled.map(Value::Bool),
    );
    set_if_absent(
        payload,
        "template_id",
        args.template_id.clone().map(Value::String),
    );
    set_if_absent(
        payload,
        "preset_id",
        args.preset_id.clone().map(Value::String),
    );
    if args.shared {
        set_if_absent(payload, "is_shared", Some(Value::Bool(true)));
    }

    // Validate that name exists, is a string, and is not empty.
    let name_value = payload.get_mut("name").ok_or_else(|| {
        anyhow!("journal name is required; pass --name or include name in --json/--json-file")
    })?;
    match name_value {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                bail!("journal name cannot be empty");
            }
            if trimmed.len() != s.len() {
                *s = trimmed.to_owned();
            }
        }
        _ => bail!("journal name must be a string"),
    }
    Ok(())
}

fn set_if_absent(payload: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if payload.contains_key(key) {
        return;
    }
    if let Some(value) = value {
        payload.insert(key.to_owned(), value);
    }
}

fn resolve_create_route(payload: &Map<String, Value>, shared_flag: bool) -> (bool, &'static str) {
    let is_shared_payload = payload
        .get("is_shared")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let shared = shared_flag || is_shared_payload;
    if shared {
        (true, "/shares")
    } else {
        (false, "/v3/sync/journals")
    }
}

/// Journal metadata fields a caller may set via `--json`/`--json-file`.
/// Anything outside this allowlist — server-managed identity/state
/// (`participants`, `owner_id`, `state`, `invite_list`, …) and the E2E
/// `encryption` vault — is dropped, so dumping a full journal JSON back in can
/// never corrupt managed data.
pub(crate) const EDITABLE_JOURNAL_FIELDS: &[&str] = &[
    "name",
    "description",
    "description_v2",
    "color",
    "sort_method",
    "hide_on_this_day",
    "hide_all_entries",
    "hide_streaks",
    "conceal",
    "add_location_to_new_entries",
    "comments_disabled",
    "template_id",
    "preset_id",
];

/// Filters a create payload to the editable allowlist plus the fields `create`
/// legitimately reads as construction controls: `id` (optional client id),
/// `is_shared` (routing), and the `"encryption": "plaintext"` opt-out marker. The
/// E2E vault object and every other managed field are dropped — the vault is
/// generated internally from `--e2e`.
fn filter_create_payload(payload: &mut Map<String, Value>) {
    payload.retain(|key, value| {
        EDITABLE_JOURNAL_FIELDS.contains(&key.as_str())
            || matches!(key.as_str(), "id" | "is_shared")
            || (key == "encryption"
                && value
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case("plaintext")))
    });
}

fn payload_has_e2e_request(payload: &Map<String, Value>) -> bool {
    payload
        .get("encryption")
        .map(|v| matches!(v, Value::Object(_)))
        .unwrap_or(false)
}

fn payload_has_plaintext_request(payload: &Map<String, Value>) -> bool {
    payload
        .get("encryption")
        .map(|v| matches!(v, Value::String(s) if s.eq_ignore_ascii_case("plaintext")))
        .unwrap_or(false)
}

fn resolve_wants_e2e(
    explicit_e2e: bool,
    explicit_plaintext: bool,
    profile_has_encryption_key: bool,
) -> bool {
    explicit_e2e || (!explicit_plaintext && profile_has_encryption_key)
}

fn shared_journal_e2e_error_message(explicit_plaintext: bool) -> &'static str {
    if explicit_plaintext {
        "shared journal creation requires E2E encryption; remove plaintext opt-out and either provide encrypted payload JSON or configure an encryption key with `dayone auth key-set` (run `dayone sync` too if synced user keys are not yet available) before retrying"
    } else {
        "shared journal creation requires E2E encryption; either provide encrypted payload JSON or configure an encryption key with `dayone auth key-set` (run `dayone sync` too if synced user keys are not yet available) before retrying"
    }
}

fn load_profile_master_key(store: &Store, profile_id: i64) -> Result<Option<String>> {
    Ok(store
        .get_encryption_key_for_profile(profile_id)?
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty()))
}

fn is_plaintext_journal(journal: &Value) -> bool {
    !matches!(journal.get("encryption"), Some(Value::Object(_)))
}

fn is_shared_journal_response(journal: &Value, fallback: bool) -> bool {
    journal
        .get("is_shared")
        .and_then(Value::as_bool)
        .unwrap_or(fallback)
}

fn parse_user_key_material(master_key: &str, user_key_json: &str) -> Result<UserKeyMaterial> {
    let user_id = master_key
        .split('-')
        .nth(1)
        .ok_or_else(|| anyhow!("invalid encryption key format; missing user id"))?
        .to_owned();
    let derived = derive_d1_symmetric_key_from_master_key(master_key)
        .ok_or_else(|| anyhow!("invalid encryption key format; expected D1 key"))?;

    let root: Value = serde_json::from_str(user_key_json).context("invalid user key JSON")?;
    let expanded = if let Some(blob_b64) = root.get("blob").and_then(Value::as_str) {
        let blob = BASE64_STANDARD
            .decode(blob_b64)
            .context("failed to decode legacy user key blob")?;
        serde_json::from_slice::<Value>(&blob)
            .context("failed to parse decoded legacy user key blob")?
    } else {
        root
    };

    // `/users/key` returns the key directly. Wrapped forms remain supported
    // only for profiles upgrading from the legacy shared `user_keys` row.
    let direct_user_key = expanded.get("fingerprint").is_some()
        && expanded
            .get("publicKey")
            .or_else(|| expanded.get("public_key"))
            .is_some()
        && expanded
            .get("encryptedPrivateKey")
            .or_else(|| expanded.get("encrypted_private_key"))
            .is_some();
    let user_key = expanded
        .get("userKey")
        .or_else(|| expanded.get("user_key"))
        .or(direct_user_key.then_some(&expanded))
        .ok_or_else(|| anyhow!("user key material missing from synced user key"))?;

    let user_fingerprint = user_key
        .get("fingerprint")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("user key fingerprint missing"))?
        .to_ascii_lowercase();
    let user_public_key_pem = user_key
        .get("publicKey")
        .or_else(|| user_key.get("public_key"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("user public key missing"))?
        .to_owned();
    let encrypted_private_key = user_key
        .get("encryptedPrivateKey")
        .or_else(|| user_key.get("encrypted_private_key"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("user encrypted private key missing"))?;

    let owner_public_key_signature = user_key
        .get("publicKeySignature")
        .or_else(|| user_key.get("public_key_signature"))
        .or_else(|| user_key.get("publicKeyHMAC"))
        .or_else(|| user_key.get("public_key_hmac"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    let user_private_key_bytes = decrypt_d1_mode0_with_key(encrypted_private_key, &derived)
        .context("failed to decrypt user private key")?;
    let user_private_key = parse_private_key_bytes(&user_private_key_bytes)?;

    Ok(UserKeyMaterial {
        user_id,
        user_fingerprint,
        user_public_key_pem,
        user_private_key,
        owner_public_key_signature,
    })
}

fn parse_public_key_pem(pem: &str) -> Result<RsaPublicKey> {
    if let Ok(pk) = RsaPublicKey::from_public_key_pem(pem) {
        return Ok(pk);
    }
    Ok(RsaPublicKey::from_pkcs1_pem(pem)?)
}

fn build_e2e_encryption_payload(keys: &UserKeyMaterial) -> Result<Value> {
    let user_public_key = parse_public_key_pem(&keys.user_public_key_pem)?;

    let mut rng = OsRng;
    let journal_private = RsaPrivateKey::new(&mut rng, 2048)?;
    let journal_public = journal_private.to_public_key();

    let mut vault_key = [0_u8; 32];
    rng.fill_bytes(&mut vault_key);
    let vault_key_fingerprint = bytes_to_hex_lower(&Sha256::digest(vault_key));

    let user_encrypted_vault_key =
        user_public_key.encrypt(&mut rng, Oaep::new::<Sha1>(), &vault_key)?;
    let user_encrypted_vault_key_b64 = BASE64_STANDARD.encode(user_encrypted_vault_key);

    // Day One web (WebCrypto) and Apple clients require the journal keypair in
    // PKCS#8 (private) and SPKI (public) encoding. Emitting PKCS#1 here makes the
    // keys unimportable on web (`crypto.subtle.importKey("pkcs8", ...)` rejects
    // PKCS#1 DER), which silently locks every entry out of decryption.
    let journal_private_pem = journal_private
        .to_pkcs8_pem(LineEnding::LF)?
        .as_bytes()
        .to_vec();
    let encrypted_private_key = encrypt_d1_mode0_with_key(&journal_private_pem, &vault_key)?;

    let journal_public_pem = journal_public.to_public_key_pem(LineEnding::LF)?;
    let journal_fingerprint = bytes_to_hex_lower(&Sha256::digest(
        journal_public.to_public_key_der()?.as_bytes(),
    ));

    // A journal key carries two signatures. The authenticity check that clients
    // verify is `updated.signature` (a user-key signature proving the key was
    // authorized by a trusted user). `journal_signature` is also populated but is
    // currently unverified/reserved (see DOSERVE-435). Both are
    // RSASSA-PKCS1-v1_5/SHA-256 over the *raw* (base64-decoded) locked-key bytes;
    // `encrypted_private_key` is the base64 STANDARD encoding of those bytes, so
    // re-decode the string we just produced to recover the exact bytes that get
    // signed/verified.
    //
    // See web `makeNewJournalKey` (src/crypto/DOCrypto/JournalVault/makeJournalVault.ts)
    // and `JournalKeyChecker.buildSignatureData`
    // (core/DOCore/DOCore/Sync/Crypto/Signatures/JournalKeyChecker.swift).
    let raw_locked_key_bytes = BASE64_STANDARD
        .decode(&encrypted_private_key)
        .context("failed to decode locked journal key for signing")?;

    // `journal_signature` = JOURNAL private key over the raw locked-key bytes.
    let journal_signature = rsa_pkcs1v15_sha256_sign(&journal_private, &raw_locked_key_bytes)?;

    // `updated.signature` = USER private key over
    // `utf8(public_key PEM) ++ raw locked-key bytes` — exactly what a verifying
    // client reconstructs in `buildSignatureData` (public key UTF-8 bytes, then
    // the base64-decoded encryptedPrivateKey).
    let mut updated_data_to_sign = journal_public_pem.as_bytes().to_vec();
    updated_data_to_sign.extend_from_slice(&raw_locked_key_bytes);
    let updated_signature =
        rsa_pkcs1v15_sha256_sign(&keys.user_private_key, &updated_data_to_sign)?;

    // `updated.fingerprint` mirrors web's `Fingerprint.forPrivateKey`: SHA-256
    // (hex, lowercase) of the user private key's PKCS#8 DER bytes. A verifying
    // client resolves the signing key by either this fingerprint or the grant's
    // `user_fingerprint`, so it is informational; we match the web producer.
    let user_private_key_der = keys.user_private_key.to_pkcs8_der()?;
    let updated_fingerprint = bytes_to_hex_lower(&Sha256::digest(user_private_key_der.as_bytes()));

    Ok(json!({
        "vault": {
            "vault_key_fingerprint": vault_key_fingerprint,
            "grants": [{
                "user_id": keys.user_id,
                "create_date": Value::Null,
                "user_fingerprint": keys.user_fingerprint,
                "vault_key_fingerprint": vault_key_fingerprint,
                "encrypted_vault_key": user_encrypted_vault_key_b64
            }],
            "keys": [{
                "fingerprint": journal_fingerprint,
                "public_key": journal_public_pem,
                "journal_signature": journal_signature,
                "encrypted_private_key": encrypted_private_key,
                "created": Value::Null,
                "updated": {
                    "at": now_epoch_ms(),
                    "by_id": keys.user_id,
                    "fingerprint": updated_fingerprint,
                    "signature": updated_signature
                }
            }]
        }
    }))
}

/// Produces a base64 (STANDARD) RSASSA-PKCS1-v1_5 signature over `data`, hashed
/// with SHA-256 — matching WebCrypto's `crypto.subtle.sign({ name:
/// "RSASSA-PKCS1-v1_5" }, key, data)`, which hashes the message internally. The
/// `rsa` crate's `Pkcs1v15Sign::new::<Sha256>()` expects the already-computed
/// digest, so we hash first and sign the digest.
fn rsa_pkcs1v15_sha256_sign(key: &RsaPrivateKey, data: &[u8]) -> Result<String> {
    let digest = Sha256::digest(data);
    let signature = key.sign(
        rsa::pkcs1v15::Pkcs1v15Sign::new::<Sha256>(),
        digest.as_slice(),
    )?;
    Ok(BASE64_STANDARD.encode(signature))
}

fn sign_owner_public_key(keys: &UserKeyMaterial) -> Result<String> {
    let digest = Sha256::digest(keys.user_public_key_pem.as_bytes());
    let signature = keys.user_private_key.sign(
        rsa::pkcs1v15::Pkcs1v15Sign::new::<Sha256>(),
        digest.as_slice(),
    )?;
    Ok(BASE64_STANDARD.encode(signature))
}

fn now_epoch_ms() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    now.as_millis() as i64
}

fn short_hash(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    format!("{:x}", digest)[0..12].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::store::InMemorySecureMasterKeyStore;
    use crate::sync::crypto::test_support::{TEST_MASTER_KEY, TEST_USER_ID};
    use rsa::pkcs1::EncodeRsaPrivateKey;
    use rsa::pkcs8::{DecodePrivateKey, EncodePublicKey};
    use tempfile::TempDir;

    fn setup_store(base_url: &str) -> (TempDir, Store, i64) {
        let temp_dir = tempfile::tempdir().expect("temp dir should create");
        let path = temp_dir.path().join("dayone.db");
        let store = Store::open_at_with_secure_master_key_store(
            &path,
            Arc::new(InMemorySecureMasterKeyStore::default()),
        )
        .expect("store should open");
        let profile = store
            .get_or_create_profile_for_base_url(base_url)
            .expect("profile should create");
        store
            .save_auth_session(
                profile.id,
                "tok",
                "2026-03-17T00:00:00.000Z",
                r#"{"id":"test-user"}"#,
            )
            .expect("session should save");
        (temp_dir, store, profile.id)
    }

    fn default_args(name: &str) -> JournalCreateArgs {
        JournalCreateArgs {
            json: None,
            json_file: None,
            name: Some(name.to_owned()),
            description: None,
            color: None,
            shared: false,
            e2e: false,
            plaintext: false,
            sort_method: None,
            hide_on_this_day: None,
            hide_all_entries: None,
            conceal: None,
            add_location_to_new_entries: None,
            comments_disabled: None,
            template_id: None,
            preset_id: None,
        }
    }

    #[test]
    fn selects_shared_endpoint_with_flag() {
        let payload = Map::new();
        let (shared, path) = resolve_create_route(&payload, true);
        assert!(shared);
        assert_eq!(path, "/shares");
    }

    #[test]
    fn selects_shared_endpoint_with_payload_field() {
        let mut payload = Map::new();
        payload.insert("is_shared".to_owned(), Value::Bool(true));
        let (shared, path) = resolve_create_route(&payload, false);
        assert!(shared);
        assert_eq!(path, "/shares");
    }

    #[test]
    fn merge_preserves_existing_payload_values() {
        let args = JournalCreateArgs {
            json: None,
            json_file: None,
            name: Some("flag-name".to_owned()),
            description: Some("flag-description".to_owned()),
            color: None,
            shared: false,
            e2e: false,
            plaintext: false,
            sort_method: None,
            hide_on_this_day: None,
            hide_all_entries: None,
            conceal: None,
            add_location_to_new_entries: None,
            comments_disabled: None,
            template_id: None,
            preset_id: None,
        };
        let mut payload = Map::new();
        payload.insert("name".to_owned(), Value::String("json-name".to_owned()));
        payload.insert(
            "description_v2".to_owned(),
            Value::String("json-description".to_owned()),
        );

        merge_convenience_flags(&mut payload, &args).expect("merge should succeed");
        assert_eq!(
            payload.get("name").and_then(Value::as_str),
            Some("json-name")
        );
        assert_eq!(
            payload.get("description_v2").and_then(Value::as_str),
            Some("json-description")
        );
    }

    #[test]
    fn merge_rejects_non_string_or_empty_name() {
        let args = JournalCreateArgs {
            json: None,
            json_file: None,
            name: None,
            description: None,
            color: None,
            shared: false,
            e2e: false,
            plaintext: false,
            sort_method: None,
            hide_on_this_day: None,
            hide_all_entries: None,
            conceal: None,
            add_location_to_new_entries: None,
            comments_disabled: None,
            template_id: None,
            preset_id: None,
        };

        let mut payload = Map::new();
        payload.insert("name".to_owned(), Value::Null);
        assert!(merge_convenience_flags(&mut payload, &args).is_err());

        let mut payload2 = Map::new();
        payload2.insert("name".to_owned(), Value::String("   ".to_owned()));
        assert!(merge_convenience_flags(&mut payload2, &args).is_err());
    }

    #[tokio::test]
    async fn e2e_create_uses_direct_user_key_and_accepts_legacy_shapes() {
        let derived = derive_d1_symmetric_key_from_master_key(TEST_MASTER_KEY)
            .expect("master key should derive");
        let mut rng = OsRng;
        let private = RsaPrivateKey::new(&mut rng, 2048).expect("rsa key should generate");
        let public_pem = private
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .expect("public pem");
        let private_pem = private
            .to_pkcs1_pem(LineEnding::LF)
            .expect("private pem")
            .to_string();
        let encrypted_private = encrypt_d1_mode0_with_key(private_pem.as_bytes(), &derived)
            .expect("private key should encrypt");
        let direct = json!({
            "fingerprint": "abc123",
            "publicKey": public_pem,
            "encryptedPrivateKey": encrypted_private
        });
        let legacy_blob = json!({
            "userKey": direct.clone(),
            "contentKeys": []
        });
        let legacy = json!({
            "blob": BASE64_STANDARD.encode(legacy_blob.to_string().as_bytes())
        });

        for payload in [direct.clone(), legacy] {
            let material = parse_user_key_material(TEST_MASTER_KEY, &payload.to_string())
                .expect("user key material should parse");
            assert_eq!(material.user_id, TEST_USER_ID);
            assert_eq!(material.user_fingerprint, "abc123");
            assert_eq!(material.user_public_key_pem, public_pem);
        }
        let err = parse_user_key_material(TEST_MASTER_KEY, r#"{"contentKeys":[]}"#)
            .expect_err("supplementary content keys are not a user identity key");
        assert!(
            err.to_string()
                .contains("user key material missing from synced user key")
        );

        let base_url = "https://stg.dayone.me";
        let (_temp_dir, store, profile_id) = setup_store(base_url);
        store
            .set_encryption_key_for_profile(profile_id, TEST_MASTER_KEY)
            .expect("encryption key should save");
        store
            .upsert_singleton_json_row("user_identity_key", 1, &direct.to_string(), None)
            .expect("direct user key should save");
        store
            .upsert_singleton_json_row(
                "user_keys",
                1,
                r#"{"userKey":{"fingerprint":"stale"}}"#,
                None,
            )
            .expect("legacy content-key bundle should save");

        let output = execute(&store, base_url, default_args("Direct user key"))
            .await
            .expect("E2E create should use the direct user key");
        assert!(output.encrypted);
    }

    #[test]
    fn build_e2e_payload_has_required_fields_and_decryptable_private_key() {
        let mut rng = OsRng;
        let user_private = RsaPrivateKey::new(&mut rng, 2048).expect("rsa key should generate");
        let user_public = user_private.to_public_key();
        let keys = UserKeyMaterial {
            user_id: TEST_USER_ID.to_owned(),
            user_fingerprint: "ff11".to_owned(),
            user_public_key_pem: user_public
                .to_public_key_pem(LineEnding::LF)
                .expect("public pem"),
            user_private_key: user_private.clone(),
            owner_public_key_signature: None,
        };

        let payload = build_e2e_encryption_payload(&keys).expect("e2e payload should build");
        let vault = payload
            .get("vault")
            .and_then(Value::as_object)
            .expect("vault should exist");
        let grants = vault
            .get("grants")
            .and_then(Value::as_array)
            .expect("grants should be an array");
        let keys_arr = vault
            .get("keys")
            .and_then(Value::as_array)
            .expect("keys should be an array");
        assert_eq!(grants.len(), 1);
        assert_eq!(keys_arr.len(), 1);

        let grant = grants[0].as_object().expect("grant should be object");
        let encrypted_vault_key = grant
            .get("encrypted_vault_key")
            .and_then(Value::as_str)
            .expect("encrypted vault key should exist");
        let encrypted_vault_key = BASE64_STANDARD
            .decode(encrypted_vault_key)
            .expect("vault key should decode");
        let vault_key = user_private
            .decrypt(Oaep::new::<Sha1>(), &encrypted_vault_key)
            .expect("vault key should decrypt");

        let journal_key = keys_arr[0].as_object().expect("key should be object");
        let encrypted_private_key = journal_key
            .get("encrypted_private_key")
            .and_then(Value::as_str)
            .expect("encrypted private key should exist");
        let decrypted_private_key =
            decrypt_d1_mode0_with_key(encrypted_private_key, &vault_key).expect("should decrypt");
        let decrypted_pem = std::str::from_utf8(&decrypted_private_key).expect("utf8");
        // The journal private key must be PKCS#8 (`BEGIN PRIVATE KEY`), not PKCS#1
        // (`BEGIN RSA PRIVATE KEY`), or WebCrypto cannot import it.
        assert!(
            decrypted_pem.contains("BEGIN PRIVATE KEY"),
            "journal private key must be PKCS#8, got: {decrypted_pem}"
        );
        assert!(RsaPrivateKey::from_pkcs8_pem(decrypted_pem).is_ok());

        // The journal public key must be SPKI (`BEGIN PUBLIC KEY`), not PKCS#1.
        let journal_public_pem = journal_key
            .get("public_key")
            .and_then(Value::as_str)
            .expect("public key should exist");
        assert!(
            journal_public_pem.contains("BEGIN PUBLIC KEY")
                && !journal_public_pem.contains("BEGIN RSA PUBLIC KEY"),
            "journal public key must be SPKI, got: {journal_public_pem}"
        );

        // The fingerprint must be the SHA-256 of the SPKI DER, matching the
        // web/Apple convention so freshly created journals are self-consistent.
        let public_key =
            RsaPublicKey::from_public_key_pem(journal_public_pem).expect("public key parses");
        let expected_fingerprint = bytes_to_hex_lower(&Sha256::digest(
            public_key.to_public_key_der().unwrap().as_bytes(),
        ));
        assert_eq!(
            journal_key.get("fingerprint").and_then(Value::as_str),
            Some(expected_fingerprint.as_str())
        );

        // --- Signature verification (the core of this change) --------------
        //
        // Both signatures sign the *raw* (base64-decoded) locked-key bytes,
        // not the base64 string. Recover those bytes to reconstruct the signed
        // payloads exactly as Apple's `JournalKeyChecker` does.
        let raw_locked_key_bytes = BASE64_STANDARD
            .decode(encrypted_private_key)
            .expect("locked key should base64-decode");

        // `journal_signature` must verify against the JOURNAL public key over
        // the raw locked-key bytes (RSASSA-PKCS1-v1_5/SHA-256).
        let journal_signature_b64 = journal_key
            .get("journal_signature")
            .and_then(Value::as_str)
            .expect("journal_signature should be present");
        let journal_signature = BASE64_STANDARD
            .decode(journal_signature_b64)
            .expect("journal_signature should base64-decode");
        public_key
            .verify(
                rsa::pkcs1v15::Pkcs1v15Sign::new::<Sha256>(),
                Sha256::digest(&raw_locked_key_bytes).as_slice(),
                &journal_signature,
            )
            .expect("journal_signature must verify with the journal public key");

        // `updated` object: at (number), by_id, fingerprint, signature.
        let updated = journal_key
            .get("updated")
            .and_then(Value::as_object)
            .expect("updated should be an object");
        assert_eq!(
            updated.get("by_id").and_then(Value::as_str),
            Some(TEST_USER_ID),
            "updated.by_id must be the user id"
        );
        assert!(
            updated.get("at").and_then(Value::as_i64).is_some(),
            "updated.at must be a JSON number (epoch ms)"
        );

        // `updated.fingerprint` == SHA-256 (hex) of the user private key PKCS#8
        // DER, matching web's `Fingerprint.forPrivateKey`.
        let expected_user_fp = bytes_to_hex_lower(&Sha256::digest(
            user_private.to_pkcs8_der().unwrap().as_bytes(),
        ));
        assert_eq!(
            updated.get("fingerprint").and_then(Value::as_str),
            Some(expected_user_fp.as_str()),
            "updated.fingerprint must be SHA-256 of the user PKCS#8 DER"
        );

        // `updated.signature` must verify against the USER public key over
        // `utf8(public_key PEM) ++ raw locked-key bytes`.
        let updated_signature_b64 = updated
            .get("signature")
            .and_then(Value::as_str)
            .expect("updated.signature should be present");
        let updated_signature = BASE64_STANDARD
            .decode(updated_signature_b64)
            .expect("updated.signature should base64-decode");
        let mut updated_signed_data = journal_public_pem.as_bytes().to_vec();
        updated_signed_data.extend_from_slice(&raw_locked_key_bytes);
        user_public
            .verify(
                rsa::pkcs1v15::Pkcs1v15Sign::new::<Sha256>(),
                Sha256::digest(&updated_signed_data).as_slice(),
                &updated_signature,
            )
            .expect("updated.signature must verify with the user public key");
    }

    #[test]
    fn resolve_wants_e2e_prefers_explicit_e2e() {
        assert!(resolve_wants_e2e(true, false, false));
        assert!(resolve_wants_e2e(true, false, true));
    }

    #[test]
    fn resolve_wants_e2e_respects_explicit_plaintext_opt_out() {
        assert!(!resolve_wants_e2e(false, true, true));
        assert!(!resolve_wants_e2e(false, true, false));
    }

    #[test]
    fn resolve_wants_e2e_defaults_to_key_presence() {
        assert!(resolve_wants_e2e(false, false, true));
        assert!(!resolve_wants_e2e(false, false, false));
    }

    #[test]
    fn shared_journal_error_message_depends_on_plaintext_opt_out() {
        assert!(
            shared_journal_e2e_error_message(true).contains("remove plaintext opt-out"),
            "explicit plaintext should mention opt-out removal"
        );
        assert!(
            shared_journal_e2e_error_message(false)
                .contains("either provide encrypted payload JSON or configure an encryption key"),
            "implicit plaintext should recommend enabling E2E"
        );
        assert!(
            shared_journal_e2e_error_message(false).contains("run `dayone sync` too"),
            "shared guidance should mention syncing user keys when needed"
        );
    }

    #[test]
    fn filter_create_payload_allowlists_editable_fields() {
        let mut payload: Map<String, Value> = serde_json::from_value(json!({
            // editable — kept
            "name": "Trip",
            "color": "#FFC107",
            "conceal": true,
            // construction controls — kept
            "id": "client-123",
            "is_shared": true,
            // plaintext marker — kept
            "encryption": "plaintext",
            // managed / dangerous — dropped
            "participants": [{"id": "1"}],
            "owner_id": "999",
            "state": "active",
        }))
        .unwrap();

        filter_create_payload(&mut payload);

        for kept in ["name", "color", "conceal", "id", "is_shared", "encryption"] {
            assert!(payload.contains_key(kept), "{kept} should be kept");
        }
        for dropped in ["participants", "owner_id", "state"] {
            assert!(
                !payload.contains_key(dropped),
                "{dropped} should be dropped"
            );
        }
    }

    #[test]
    fn filter_create_payload_drops_encryption_vault_object() {
        let mut payload: Map<String, Value> =
            serde_json::from_value(json!({"name": "X", "encryption": {"vault": {}}})).unwrap();
        filter_create_payload(&mut payload);
        assert!(!payload.contains_key("encryption"));
        assert!(payload.contains_key("name"));
    }

    #[test]
    fn payload_plaintext_request_detects_plaintext_marker() {
        let mut payload = Map::new();
        payload.insert(
            "encryption".to_owned(),
            Value::String("plaintext".to_owned()),
        );
        assert!(payload_has_plaintext_request(&payload));

        payload.insert(
            "encryption".to_owned(),
            Value::String("PLAINTEXT".to_owned()),
        );
        assert!(payload_has_plaintext_request(&payload));

        payload.insert("encryption".to_owned(), json!({"vault": {}}));
        assert!(!payload_has_plaintext_request(&payload));
    }

    #[test]
    fn plaintext_detection_treats_missing_or_non_object_encryption_as_plaintext() {
        let missing = json!({
            "id": "journal-1"
        });
        assert!(is_plaintext_journal(&missing));

        let marker = json!({
            "id": "journal-2",
            "encryption": "plaintext"
        });
        assert!(is_plaintext_journal(&marker));

        let e2e = json!({
            "id": "journal-3",
            "encryption": {
                "vault": {}
            }
        });
        assert!(!is_plaintext_journal(&e2e));
    }

    #[tokio::test]
    async fn create_defaults_to_e2e_when_profile_has_key() {
        let base_url = "https://stg.dayone.me";
        let (_temp_dir, store, profile_id) = setup_store(base_url);
        store
            .set_encryption_key_for_profile(profile_id, TEST_MASTER_KEY)
            .expect("encryption key should save");

        let err = execute(&store, base_url, default_args("Encrypted by default"))
            .await
            .expect_err("without synced user keys, default E2E should fail");
        assert!(
            err.to_string().contains(
                "missing synced user key; run `dayone sync` before creating an E2E journal"
            )
        );
    }

    #[tokio::test]
    async fn create_plaintext_opt_out_overrides_profile_key_default() {
        let base_url = "https://stg.dayone.me";
        let (_temp_dir, store, profile_id) = setup_store(base_url);
        store
            .set_encryption_key_for_profile(profile_id, TEST_MASTER_KEY)
            .expect("encryption key should save");
        let mut args = default_args("Explicit plaintext");
        args.plaintext = true;

        let out = execute(&store, base_url, args)
            .await
            .expect("explicit plaintext should bypass E2E defaults");
        assert_eq!(
            out.journal.get("name").and_then(Value::as_str),
            Some("Explicit plaintext")
        );
        assert!(
            out.journal.get("encryption").is_none(),
            "plaintext opt-out should not auto-generate E2E encryption payload"
        );
    }

    #[tokio::test]
    async fn create_defaults_to_plaintext_without_profile_key() {
        let base_url = "https://stg.dayone.me";
        let (_temp_dir, store, _profile_id) = setup_store(base_url);
        let out = execute(&store, base_url, default_args("Plaintext default"))
            .await
            .expect("without an encryption key, create should remain plaintext");
        assert_eq!(
            out.journal.get("name").and_then(Value::as_str),
            Some("Plaintext default")
        );
        assert!(out.journal.get("encryption").is_none());
        assert!(
            !out.encrypted,
            "plaintext journals should report encrypted=false"
        );
    }

    #[tokio::test]
    async fn create_silently_drops_payload_supplied_encryption_vault() {
        // The E2E vault is generated internally from `--e2e`; a hand-authored
        // vault passed via --json is silently stripped (not used, not an error),
        // so the journal is created without it.
        let base_url = "https://stg.dayone.me";
        let (_temp_dir, store, _profile_id) = setup_store(base_url);
        let mut args = default_args("Payload E2E");
        args.json = Some(
            json!({
                "name": "Payload E2E",
                "encryption": { "vault": {} }
            })
            .to_string(),
        );

        let out = execute(&store, base_url, args)
            .await
            .expect("create should succeed with the vault stripped");
        assert!(
            out.journal.get("encryption").is_none(),
            "hand-authored encryption vault must be dropped"
        );
        assert!(
            !out.encrypted,
            "without --e2e or a profile key, it's plaintext"
        );
    }

    #[tokio::test]
    async fn create_rejects_conflicting_e2e_and_plaintext_flags() {
        let base_url = "https://stg.dayone.me";
        let (_temp_dir, store, _profile_id) = setup_store(base_url);
        let mut args = default_args("Conflicting flags");
        args.e2e = true;
        args.plaintext = true;

        let err = execute(&store, base_url, args)
            .await
            .expect_err("conflicting flags should fail");
        assert!(err.to_string().contains("conflicting encryption options"));
    }

    #[tokio::test]
    async fn create_drops_payload_encryption_vault_with_plaintext_flag() {
        // Stripping the vault means there's no longer a phantom e2e/plaintext
        // conflict — the journal is created as plaintext.
        let base_url = "https://stg.dayone.me";
        let (_temp_dir, store, _profile_id) = setup_store(base_url);
        let mut args = default_args("Plaintext + payload vault");
        args.plaintext = true;
        args.json = Some(
            json!({
                "name": "Plaintext + payload vault",
                "encryption": { "vault": {} }
            })
            .to_string(),
        );

        let out = execute(&store, base_url, args)
            .await
            .expect("create should succeed as plaintext");
        assert!(out.journal.get("encryption").is_none());
        assert!(!out.encrypted);
    }

    #[tokio::test]
    async fn create_rejects_conflicting_e2e_flag_and_payload_plaintext_request() {
        let base_url = "https://stg.dayone.me";
        let (_temp_dir, store, _profile_id) = setup_store(base_url);
        let mut args = default_args("Conflicting e2e payload-plaintext");
        args.e2e = true;
        args.json = Some(
            json!({
                "name": "Conflicting e2e payload-plaintext",
                "encryption": "plaintext"
            })
            .to_string(),
        );

        let err = execute(&store, base_url, args)
            .await
            .expect_err("e2e flag + payload plaintext should fail");
        assert!(err.to_string().contains("conflicting encryption options"));
    }

    #[tokio::test]
    async fn create_shared_without_key_gives_neutral_e2e_guidance() {
        let base_url = "https://stg.dayone.me";
        let (_temp_dir, store, _profile_id) = setup_store(base_url);
        let mut args = default_args("Shared journal");
        args.shared = true;

        let err = execute(&store, base_url, args)
            .await
            .expect_err("shared journals should require E2E");
        assert!(err.to_string().contains(
            "shared journal creation requires E2E encryption; either provide encrypted payload JSON or configure an encryption key with `dayone auth key-set` (run `dayone sync` too if synced user keys are not yet available) before retrying"
        ));
    }

    #[tokio::test]
    async fn create_shared_with_plaintext_opt_out_mentions_opt_out() {
        let base_url = "https://stg.dayone.me";
        let (_temp_dir, store, _profile_id) = setup_store(base_url);
        let mut args = default_args("Shared plaintext");
        args.shared = true;
        args.plaintext = true;

        let err = execute(&store, base_url, args)
            .await
            .expect_err("shared journals should reject plaintext opt-out");
        assert!(err.to_string().contains("remove plaintext opt-out"));
        assert!(err.to_string().contains("run `dayone sync` too"));
    }
}
