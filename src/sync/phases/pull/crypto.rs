use super::*;

pub(crate) fn normalize_known_encrypted_journal_name(journal: &mut Value) {
    let Some(obj) = journal.as_object_mut() else {
        return;
    };
    let is_name_encrypted = obj
        .get("name")
        .and_then(Value::as_str)
        .map(|name| name.starts_with("RDE"))
        .unwrap_or(false);
    if !is_name_encrypted {
        return;
    }
    let is_strava_journal = obj
        .get("connected_services")
        .and_then(Value::as_array)
        .map(|services| {
            services.iter().any(|v| {
                v.as_str()
                    .map(|s| s.eq_ignore_ascii_case("strava"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if is_strava_journal {
        obj.insert("name".to_owned(), Value::String("Strava".to_owned()));
    }
}

pub(crate) fn build_journal_decryptor(
    store: &Store,
    master_key: &str,
    user_id: Option<&str>,
) -> Result<Option<JournalDecryptor>> {
    let Some(user_identity_key) = store.get_user_identity_key_json()? else {
        return Ok(None);
    };
    let payload: Value = serde_json::from_str(&user_identity_key)?;
    let Some(user_keys_payload) = UserKeysPayload::from_raw(&payload) else {
        return Ok(None);
    };
    let derived_master_key = derive_d1_symmetric_key_from_master_key(master_key)
        .ok_or_else(|| anyhow!("invalid master key format"))?;
    let mut user_verify_keys = HashMap::new();
    let Some(user_private_key) = add_unlocked_user_verify_key(
        &user_keys_payload,
        &derived_master_key,
        &mut user_verify_keys,
    )?
    else {
        return Ok(None);
    };
    add_alternative_user_verify_keys(
        &user_keys_payload,
        &derived_master_key,
        &mut user_verify_keys,
    );

    if let Some(bundle_json) = store.get_json_row_by_id("user_keys", "1")?
        && bundle_json != user_identity_key
        && let Ok(bundle) = serde_json::from_str::<Value>(&bundle_json)
        && let Some(bundle) = UserKeysPayload::from_raw(&bundle)
    {
        if let Err(err) =
            add_unlocked_user_verify_key(&bundle, &derived_master_key, &mut user_verify_keys)
        {
            log_sync(format!("supplementary user verify key skipped: {err}"));
        }
        add_alternative_user_verify_keys(&bundle, &derived_master_key, &mut user_verify_keys);
    }

    let user_id = user_id.map(ToOwned::to_owned);
    Ok(Some(JournalDecryptor {
        user_id,
        user_private_key,
        user_verify_keys,
    }))
}

fn add_alternative_user_verify_keys(
    payload: &UserKeysPayload,
    derived_master_key: &[u8; 32],
    verify_keys: &mut HashMap<String, RsaPublicKey>,
) {
    for key in payload.alternative_user_keys() {
        if let Err(err) = add_unlocked_user_verify_key(key, derived_master_key, verify_keys) {
            log_sync(format!("alternative user verify key skipped: {err}"));
        }
    }
}

fn add_unlocked_user_verify_key(
    payload: &UserKeysPayload,
    derived_master_key: &[u8; 32],
    verify_keys: &mut HashMap<String, RsaPublicKey>,
) -> Result<Option<RsaPrivateKey>> {
    let Some(encrypted_private_key) = payload.encrypted_private_key() else {
        return Ok(None);
    };
    let private_key = parse_private_key_pem(&String::from_utf8(decrypt_d1_mode0_with_key(
        encrypted_private_key,
        derived_master_key,
    )?)?)?;
    let public_key = private_key.to_public_key();
    if public_key.n().bits() != 2048 {
        bail!("RSA user key must be 2048 bits");
    }
    if let Some(expected_public_key) = payload.user_public_key()
        && parse_public_key_pem(expected_public_key)? != public_key
    {
        bail!("user public key does not match unlocked private key");
    }

    let mut fingerprints = vec![
        bytes_to_hex_lower(&Sha256::digest(public_key.to_public_key_der()?.as_bytes())),
        bytes_to_hex_lower(&Sha256::digest(private_key.to_pkcs8_der()?.as_bytes())),
        bytes_to_hex_lower(&Sha256::digest(private_key.to_pkcs1_der()?.as_bytes())),
    ];
    if let Some(fingerprint) = payload.user_key_fingerprint() {
        fingerprints.push(fingerprint.to_ascii_lowercase());
    }
    for fingerprint in fingerprints {
        verify_keys
            .entry(fingerprint)
            .or_insert_with(|| public_key.clone());
    }
    Ok(Some(private_key))
}

#[derive(Clone, Copy)]
pub(crate) enum JournalFieldDecryptOutcome {
    Decrypted,
    Skipped,
    Failed,
}

pub(crate) fn journal_vault_key(journal: &Value, decryptor: &JournalDecryptor) -> Option<Vec<u8>> {
    journal
        .get("encryption")
        .and_then(|value| value.get("vault"))
        .and_then(Value::as_object)
        .and_then(|vault| decrypt_journal_vault_key(vault, &decryptor.user_private_key))
}

pub(crate) fn inspect_journal_metadata(
    journal: &Value,
    vault_key: Option<&[u8]>,
) -> Vec<(&'static str, &'static str)> {
    if journal
        .get("encryption")
        .and_then(|value| value.get("vault"))
        .and_then(Value::as_object)
        .is_none()
    {
        return Vec::new();
    }
    let mut diagnostics = Vec::new();
    for field in ["name", "description_v2"] {
        let value = match journal.get(field) {
            None | Some(Value::Null) => continue,
            Some(Value::String(value)) if value.is_empty() => continue,
            Some(Value::String(value)) => value,
            Some(_) => {
                diagnostics.push((field, "malformed_d1"));
                continue;
            }
        };
        let status = match d1_format_0_shape(value) {
            D1Format0Shape::Plaintext => Some("plaintext"),
            D1Format0Shape::Malformed => Some("malformed_d1"),
            D1Format0Shape::Valid => match vault_key {
                None => Some("unverified_d1"),
                Some(key) => match decrypt_d1_mode0_with_key(value, key)
                    .and_then(|plaintext| String::from_utf8(plaintext).map_err(Into::into))
                {
                    Ok(_) => None,
                    Err(_) => Some("undecryptable_d1"),
                },
            },
        };
        if let Some(status) = status {
            diagnostics.push((field, status));
        }
    }
    diagnostics
}

enum D1Format0Shape {
    Plaintext,
    Malformed,
    Valid,
}

fn d1_format_0_shape(value: &str) -> D1Format0Shape {
    let decoded = match BASE64_STANDARD.decode(value) {
        Ok(decoded) => decoded,
        Err(_) if value.starts_with("RDE") => return D1Format0Shape::Malformed,
        Err(_) => return D1Format0Shape::Plaintext,
    };
    if !decoded.starts_with(b"D1") {
        return if value.starts_with("RDE") {
            D1Format0Shape::Malformed
        } else {
            D1Format0Shape::Plaintext
        };
    }
    if decoded.len() < 4 + 12 + 16 + 16 || decoded[2] != 1 || decoded[3] != 0 {
        return D1Format0Shape::Malformed;
    }
    let checksum_offset = decoded.len() - 16;
    if Md5::digest(&decoded[..checksum_offset]).as_slice() != &decoded[checksum_offset..] {
        return D1Format0Shape::Malformed;
    }
    D1Format0Shape::Valid
}

pub(crate) fn try_decrypt_journal_fields(
    journal: &mut Value,
    decryptor: &JournalDecryptor,
) -> Result<JournalFieldDecryptOutcome> {
    let vault_key = journal_vault_key(journal, decryptor);
    try_decrypt_journal_fields_with_key(journal, vault_key.as_deref())
}

pub(crate) fn try_decrypt_journal_fields_with_key(
    journal: &mut Value,
    vault_key: Option<&[u8]>,
) -> Result<JournalFieldDecryptOutcome> {
    let Some(obj) = journal.as_object_mut() else {
        return Ok(JournalFieldDecryptOutcome::Skipped);
    };
    if obj
        .get("encryption")
        .and_then(|value| value.get("vault"))
        .and_then(Value::as_object)
        .is_none()
    {
        return Ok(JournalFieldDecryptOutcome::Skipped);
    }
    let encrypted_metadata = ["name", "description_v2"].into_iter().any(|field| {
        obj.get(field)
            .and_then(Value::as_str)
            .is_some_and(|value| value.starts_with("RDE"))
    });
    if !encrypted_metadata {
        return Ok(JournalFieldDecryptOutcome::Skipped);
    }
    let Some(vault_key) = vault_key else {
        return Ok(JournalFieldDecryptOutcome::Failed);
    };

    let mut decrypted_any = false;
    let mut failed = false;
    if let Some(name) = obj.get("name").and_then(Value::as_str)
        && name.starts_with("RDE")
    {
        match decrypt_d1_mode0_with_key(name, vault_key) {
            Ok(decrypted) => {
                obj.insert(
                    "name".to_owned(),
                    Value::String(String::from_utf8_lossy(&decrypted).into_owned()),
                );
                decrypted_any = true;
            }
            Err(_) => failed = true,
        }
    }
    if let Some(description) = obj.get("description_v2").and_then(Value::as_str)
        && description.starts_with("RDE")
    {
        match decrypt_d1_mode0_with_key(description, vault_key) {
            Ok(decrypted) => {
                obj.insert(
                    "description_v2".to_owned(),
                    Value::String(String::from_utf8_lossy(&decrypted).into_owned()),
                );
                decrypted_any = true;
            }
            Err(_) => failed = true,
        }
    }
    Ok(if failed {
        JournalFieldDecryptOutcome::Failed
    } else if decrypted_any {
        JournalFieldDecryptOutcome::Decrypted
    } else {
        JournalFieldDecryptOutcome::Skipped
    })
}

pub(crate) fn build_entries_decryptor(
    store: &Store,
    decryptor: &JournalDecryptor,
) -> Result<EntriesDecryptor> {
    let journals = store.list_json_rows("journals")?;
    let mut journal_keys_by_fingerprint: HashMap<String, RsaPrivateKey> = HashMap::new();
    for journal_row in journals {
        let journal: Journal = match serde_json::from_str(&journal_row) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let Some(vault) = journal
            .extra
            .get("encryption")
            .and_then(|v| v.get("vault"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        let Some(vault_key) = decrypt_journal_vault_key(vault, &decryptor.user_private_key) else {
            continue;
        };
        let Some(keys) = vault.get("keys").and_then(Value::as_array) else {
            continue;
        };
        for key in keys {
            // Deliberately no unsigned legacy fallback: iOS JournalKeyChecker
            // rejects missing update signatures, and DAYONE-1016 requires the
            // CLI to fail closed rather than trust merely decryptable material.
            let signature_started = std::time::Instant::now();
            if let Err(err) = verify_journal_key_signature(key, &journal, vault, decryptor) {
                crate::diagnostics::record_crypto(
                    "journal_key_signature_verify",
                    "journal",
                    "error",
                    signature_started.elapsed(),
                    Some(err.as_ref()),
                );
                log_sync(format!("journal key rejected: {err}"));
                continue;
            }
            crate::diagnostics::record_crypto(
                "journal_key_signature_verify",
                "journal",
                "success",
                signature_started.elapsed(),
                None,
            );
            let Some(fingerprint) = key.get("fingerprint").and_then(Value::as_str) else {
                log_sync("journal key rejected: missing fingerprint");
                continue;
            };
            let Some(public_key_pem) = key.get("public_key").and_then(Value::as_str) else {
                continue; // already checked by verify_journal_key_signature
            };
            let Ok(public_key) = parse_public_key_pem(public_key_pem) else {
                continue;
            };
            let Some(encrypted_private_key) =
                key.get("encrypted_private_key").and_then(Value::as_str)
            else {
                continue;
            };
            let decrypted_private_key =
                match decrypt_d1_mode0_with_key(encrypted_private_key, &vault_key) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
            let pem = match String::from_utf8(decrypted_private_key) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let private_key = match parse_private_key_pem(&pem) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if private_key.to_public_key() != public_key {
                log_sync("journal key rejected: public and private keys do not match");
                continue;
            }
            // Journal-key fingerprints have a documented canonical format:
            // Web's Fingerprint.forPEM hashes the decoded SPKI DER, matching
            // journal create, doctor repair, and Apple. Validate after unlock
            // so the signed public key is also bound to the private key.
            let Ok(public_key_der) = public_key.to_public_key_der() else {
                continue;
            };
            if !bytes_to_hex_lower(&Sha256::digest(public_key_der.as_bytes()))
                .eq_ignore_ascii_case(fingerprint)
            {
                log_sync("journal key rejected: fingerprint does not match public_key");
                continue;
            }
            journal_keys_by_fingerprint.insert(fingerprint.to_ascii_lowercase(), private_key);
        }
    }
    Ok(EntriesDecryptor {
        journal_keys_by_fingerprint,
    })
}

fn verify_journal_key_signature(
    key: &Value,
    journal: &Journal,
    vault: &serde_json::Map<String, Value>,
    decryptor: &JournalDecryptor,
) -> Result<()> {
    let public_key_pem = key
        .get("public_key")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing public_key"))?;
    let encrypted_private_key = key
        .get("encrypted_private_key")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing encrypted_private_key"))?;
    let signature = key
        .get("updated")
        .and_then(|updated| updated.get("signature"))
        .and_then(Value::as_str)
        .filter(|signature| !signature.is_empty())
        .ok_or_else(|| anyhow!("missing updated.signature"))?;
    let encrypted_private_key = BASE64_STANDARD
        .decode(encrypted_private_key)
        .map_err(|_| anyhow!("invalid encrypted_private_key encoding"))?;
    let mut signed_data = public_key_pem.as_bytes().to_vec();
    signed_data.extend_from_slice(&encrypted_private_key);

    if signature_valid_via_shared_journal(journal, signature, &signed_data, decryptor)
        || signature_valid_via_user_key(key, vault, signature, &signed_data, decryptor)
    {
        Ok(())
    } else {
        bail!("invalid updated.signature")
    }
}

fn signature_valid_via_user_key(
    key: &Value,
    vault: &serde_json::Map<String, Value>,
    signature: &str,
    signed_data: &[u8],
    decryptor: &JournalDecryptor,
) -> bool {
    let grant_fingerprint = vault
        .get("grants")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|grant| {
            decryptor.user_id.as_deref().is_some_and(|user_id| {
                grant.get("user_id").and_then(Value::as_str) == Some(user_id)
            })
        })
        .and_then(|grant| grant.get("user_fingerprint"))
        .and_then(Value::as_str);
    let update_fingerprint = key
        .get("updated")
        .and_then(|updated| updated.get("fingerprint"))
        .and_then(Value::as_str);

    [grant_fingerprint, update_fingerprint]
        .into_iter()
        .flatten()
        .filter_map(|fingerprint| {
            decryptor
                .user_verify_keys
                .get(&fingerprint.to_ascii_lowercase())
        })
        .any(|public_key| verify_rsa_signature(public_key, signature, signed_data))
}

fn signature_valid_via_shared_journal(
    journal: &Journal,
    signature: &str,
    signed_data: &[u8],
    decryptor: &JournalDecryptor,
) -> bool {
    let Some(invitation) = journal
        .extra
        .get("participants")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|participant| {
            decryptor.user_id.as_deref().is_some_and(|user_id| {
                participant.get("id").and_then(Value::as_str) == Some(user_id)
            })
        })
        .and_then(|participant| participant.get("invitation"))
    else {
        return false;
    };
    let Some(participant_key_pem) = invitation
        .get("participant_public_key")
        .and_then(Value::as_str)
    else {
        return false;
    };
    let Ok(participant_key) = parse_public_key_pem(participant_key_pem) else {
        return false;
    };
    if !decryptor
        .user_verify_keys
        .values()
        .any(|known_key| known_key == &participant_key)
    {
        return false;
    }
    let Some(owner_key_pem) = invitation.get("owner_public_key").and_then(Value::as_str) else {
        return false;
    };
    let Some(owner_key_signature) = invitation
        .get("owner_public_key_signature_by_participant")
        .and_then(Value::as_str)
    else {
        return false;
    };
    if !verify_rsa_signature(
        &participant_key,
        owner_key_signature,
        owner_key_pem.as_bytes(),
    ) {
        return false;
    }
    let Ok(owner_key) = parse_public_key_pem(owner_key_pem) else {
        return false;
    };
    if verify_rsa_signature(&owner_key, signature, signed_data) {
        return true;
    }

    let Some(inviting_owner_id) = invitation.get("owner_id").and_then(Value::as_str) else {
        return false;
    };
    let transfers = journal
        .extra
        .get("ownership_transfers")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let split = transfers
        .iter()
        .position(|transfer| {
            transfer.get("new_owner_id").and_then(Value::as_str) == Some(inviting_owner_id)
        })
        .unwrap_or(transfers.len());

    let mut trusted_key = owner_key.clone();
    for transfer in transfers[..split].iter().rev() {
        let Some(next_key) = verify_transfer(
            &trusted_key,
            transfer,
            "previous_owner_public_key",
            "new_owner_public_key",
            "new_owner_public_key_signature_by_previous_owner",
        ) else {
            break;
        };
        trusted_key = next_key;
        if verify_rsa_signature(&trusted_key, signature, signed_data) {
            return true;
        }
    }

    trusted_key = owner_key;
    for transfer in &transfers[split..] {
        let Some(previous_key) = verify_transfer(
            &trusted_key,
            transfer,
            "new_owner_public_key",
            "previous_owner_public_key",
            "previous_owner_public_key_signature_by_new_owner",
        ) else {
            break;
        };
        trusted_key = previous_key;
        if verify_rsa_signature(&trusted_key, signature, signed_data) {
            return true;
        }
    }
    false
}

fn verify_transfer(
    trusted_signer: &RsaPublicKey,
    transfer: &Value,
    signer_field: &str,
    next_key_field: &str,
    signature_field: &str,
) -> Option<RsaPublicKey> {
    let signer = parse_public_key_pem(transfer.get(signer_field)?.as_str()?).ok()?;
    if &signer != trusted_signer {
        return None;
    }
    let next_key_pem = transfer.get(next_key_field)?.as_str()?;
    let signature = transfer.get(signature_field)?.as_str()?;
    verify_rsa_signature(trusted_signer, signature, next_key_pem.as_bytes())
        .then(|| parse_public_key_pem(next_key_pem).ok())
        .flatten()
}

fn verify_rsa_signature(public_key: &RsaPublicKey, signature: &str, data: &[u8]) -> bool {
    let Ok(signature) = BASE64_STANDARD.decode(signature) else {
        return false;
    };
    public_key
        .verify(
            rsa::pkcs1v15::Pkcs1v15Sign::new::<Sha256>(),
            Sha256::digest(data).as_slice(),
            &signature,
        )
        .is_ok()
}

fn parse_public_key_pem(pem: &str) -> Result<RsaPublicKey> {
    if pem.len() > 1024 {
        bail!("RSA public key is too large");
    }
    let key = if let Ok(key) = RsaPublicKey::from_public_key_pem(pem) {
        key
    } else {
        RsaPublicKey::from_pkcs1_pem(pem)?
    };
    if key.n().bits() != 2048 {
        bail!("RSA public key must be 2048 bits");
    }
    Ok(key)
}

pub(crate) fn decrypt_journal_vault_key(
    vault: &serde_json::Map<String, Value>,
    user_private_key: &RsaPrivateKey,
) -> Option<Vec<u8>> {
    let grants = vault.get("grants")?.as_array()?;
    for grant in grants {
        let Some(encrypted_vault_key) = grant.get("encrypted_vault_key").and_then(Value::as_str)
        else {
            continue;
        };
        if let Ok(key) = decrypt_grant_vault_key(encrypted_vault_key, user_private_key) {
            return Some(key);
        }
    }
    None
}

pub(crate) fn decrypt_entry_d1_payload(
    d1_payload: &[u8],
    decryptor: &EntriesDecryptor,
) -> Result<Vec<u8>> {
    if d1_payload.len() < 4 + 12 + 16 + 16 {
        bail!("entry d1 payload too short");
    }
    if &d1_payload[0..2] != b"D1" {
        bail!("invalid entry d1 header");
    }
    let schema = d1_payload[2];
    if schema != 1 {
        bail!("unsupported entry d1 schema {}", schema);
    }
    let version = d1_payload[3];
    if version != 1 && version != 2 {
        bail!("unsupported entry d1 format {}", version);
    }

    let mut offset = 4usize;
    if d1_payload.len() < offset + 32 + 2 + 256 + 12 + 16 + 16 {
        bail!("entry d1 payload missing locked-key fields");
    }
    let fingerprint_hex = bytes_to_hex_lower(&d1_payload[offset..offset + 32]);
    offset += 32;
    let sig_len = ((d1_payload[offset] as usize) << 8) | (d1_payload[offset + 1] as usize);
    offset += 2;
    if d1_payload.len() < offset + sig_len + 256 + 12 + 16 + 16 {
        bail!("entry d1 payload truncated in signature");
    }
    offset += sig_len; // signature (unused)
    let locked_key = &d1_payload[offset..offset + 256];
    offset += 256;
    let iv = &d1_payload[offset..offset + 12];
    offset += 12;

    let body_end = d1_payload.len() - 16; // strip checksum
    if body_end < offset {
        bail!("entry d1 payload body underflow");
    }
    let ciphertext_and_tag = &d1_payload[offset..body_end];

    let private_key = decryptor
        .journal_keys_by_fingerprint
        .get(&fingerprint_hex)
        .ok_or_else(|| {
            anyhow!("journal private key not found for fingerprint {fingerprint_hex}")
        })?;
    let content_key = private_key.decrypt(Oaep::new::<Sha1>(), locked_key)?;
    if content_key.len() != 32 {
        bail!("unexpected content key size {}", content_key.len());
    }
    let key = aes_gcm::Key::<Aes256Gcm>::try_from(content_key.as_slice())?;
    let cipher = Aes256Gcm::new(&key);
    let mut decrypted = cipher
        .decrypt(iv.try_into()?, ciphertext_and_tag)
        .map_err(|_| anyhow!("failed to decrypt entry d1 payload"))?;

    if version == 2 {
        let mut attempts = 0u8;
        while attempts < 3 && decrypted.starts_with(&[0x1f, 0x8b]) {
            let decoder = GzDecoder::new(decrypted.as_slice());
            let mut out = Vec::new();
            decoder
                .take(MAX_DECRYPTED_ENTRY_BYTES + 1)
                .read_to_end(&mut out)?;
            if out.len() as u64 > MAX_DECRYPTED_ENTRY_BYTES {
                bail!("decompressed entry exceeds 128 MiB limit");
            }
            decrypted = out;
            attempts += 1;
        }
    }
    Ok(decrypted)
}

/// Decrypts a client-side-encrypted original media blob (a D1 "locked key"
/// blob, format 1) pulled for an E2E journal back to its original bytes.
///
/// This is the pull-side counterpart to
/// `crate::sync::crypto::encrypt_original_media_for_journal`. Media blobs share
/// the same on-disk D1 container as encrypted entry content (the per-object
/// content key is wrapped to the journal content key), so the journal keys held
/// by [`EntriesDecryptor`] are sufficient to decrypt them. Unlike entry
/// content, media is never gzip-compressed (format 1, not format 2).
#[allow(dead_code)]
pub(crate) fn decrypt_original_media(
    d1_payload: &[u8],
    decryptor: &EntriesDecryptor,
) -> Result<Vec<u8>> {
    use anyhow::Context as _;
    // Wrap with a media-specific top-level message so failures don't surface as
    // "entry d1 payload..." while still preserving the underlying cause.
    decrypt_entry_d1_payload(d1_payload, decryptor)
        .context("failed to decrypt original media D1 payload")
}

pub(crate) fn bytes_to_hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{:02x}", b));
    }
    out
}

pub(crate) fn decrypt_grant_vault_key(
    encrypted_vault_key_b64: &str,
    user_private_key: &RsaPrivateKey,
) -> Result<Vec<u8>> {
    let encrypted_vault_key = BASE64_STANDARD.decode(encrypted_vault_key_b64)?;
    let vault_key = user_private_key.decrypt(Oaep::new::<Sha1>(), &encrypted_vault_key)?;
    Ok(vault_key)
}

pub(crate) fn decrypt_d1_mode0_with_key(input_b64: &str, key_bytes: &[u8]) -> Result<Vec<u8>> {
    let d1 = BASE64_STANDARD.decode(input_b64)?;
    if d1.len() < 4 + 12 + 16 + 16 {
        bail!("d1 payload too short");
    }
    if &d1[0..2] != b"D1" {
        bail!("invalid d1 header");
    }
    let schema = d1[2];
    if schema != 1 {
        bail!("unsupported d1 schema {}", schema);
    }
    let version = d1[3];
    if version != 0 && version != 2 {
        bail!("unsupported d1 format {} for symmetric decrypt", version);
    }

    let offset = 4;
    let iv = &d1[offset..offset + 12];
    let body_end = d1.len() - 16; // strip checksum
    let ciphertext_and_tag = &d1[offset + 12..body_end];
    let key = <&aes_gcm::Key<Aes256Gcm>>::try_from(key_bytes)?;
    let cipher = Aes256Gcm::new(key);
    let decrypted = cipher
        .decrypt(iv.try_into()?, ciphertext_and_tag)
        .map_err(|_| anyhow!("failed to decrypt d1 payload"))?;

    if version == 2 && decrypted.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = GzDecoder::new(decrypted.as_slice());
        let mut out = Vec::new();
        decoder.read_to_end(&mut out)?;
        return Ok(out);
    }
    Ok(decrypted)
}

pub(crate) fn parse_private_key_pem(pem: &str) -> Result<RsaPrivateKey> {
    if let Ok(pkcs8_key) = RsaPrivateKey::from_pkcs8_pem(pem) {
        return Ok(pkcs8_key);
    }
    Ok(RsaPrivateKey::from_pkcs1_pem(pem)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Key};
    use md5::{Digest, Md5};
    use rsa::rand_core::{OsRng, RngCore};
    use rsa::{Oaep, RsaPrivateKey, RsaPublicKey};
    use sha1::Sha1;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn decode_hex(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    fn test_decryptor(user_id: &str, private_key: RsaPrivateKey) -> JournalDecryptor {
        let public_key = private_key.to_public_key();
        JournalDecryptor {
            user_id: Some(user_id.to_owned()),
            user_private_key: private_key,
            user_verify_keys: HashMap::from([("user-fingerprint".to_owned(), public_key)]),
        }
    }

    fn metadata_statuses(
        journal: &Value,
        decryptor: Option<&JournalDecryptor>,
    ) -> Vec<(&'static str, &'static str)> {
        let vault_key = decryptor.and_then(|decryptor| journal_vault_key(journal, decryptor));
        inspect_journal_metadata(journal, vault_key.as_deref())
    }

    #[test]
    fn journal_metadata_classification_covers_wire_states() {
        let mut rng = OsRng;
        let user_private =
            RsaPrivateKey::new(&mut rng, 2048).expect("user private key should generate");
        let user_public = user_private.to_public_key();
        let decryptor = test_decryptor("user-1", user_private);
        let vault_key = [7_u8; 32];
        let grant = BASE64_STANDARD.encode(
            user_public
                .encrypt(&mut rng, Oaep::new::<Sha1>(), &vault_key)
                .expect("vault key should wrap"),
        );
        let encrypted_name = crate::sync::crypto::encrypt_d1_mode0_with_key(b"Private", &vault_key)
            .expect("name should encrypt");
        let wrong_key_name =
            crate::sync::crypto::encrypt_d1_mode0_with_key(b"Private", &[8_u8; 32])
                .expect("wrong-key name should encrypt");
        let mut bad_checksum = BASE64_STANDARD
            .decode(&encrypted_name)
            .expect("encrypted name should decode");
        *bad_checksum.last_mut().expect("checksum should exist") ^= 1;
        let bad_checksum = BASE64_STANDARD.encode(bad_checksum);
        let mut wrong_format = BASE64_STANDARD
            .decode(&encrypted_name)
            .expect("encrypted name should decode");
        wrong_format[3] = 2;
        let checksum_offset = wrong_format.len() - 16;
        let checksum = Md5::digest(&wrong_format[..checksum_offset]);
        wrong_format[checksum_offset..].copy_from_slice(&checksum);
        let wrong_format = BASE64_STANDARD.encode(wrong_format);
        let journal = |name: &str| {
            serde_json::json!({
                "name": name,
                "description_v2": "",
                "encryption": { "vault": { "grants": [{ "encrypted_vault_key": grant }] } }
            })
        };

        assert!(metadata_statuses(&journal(&encrypted_name), Some(&decryptor)).is_empty());
        assert_eq!(
            metadata_statuses(
                &serde_json::json!({
                    "name": encrypted_name,
                    "description_v2": "Private description",
                    "encryption": { "vault": { "grants": [{ "encrypted_vault_key": grant }] } }
                }),
                Some(&decryptor)
            ),
            vec![("description_v2", "plaintext")]
        );
        assert_eq!(
            metadata_statuses(&journal(&encrypted_name), None),
            vec![("name", "unverified_d1")]
        );
        assert_eq!(
            metadata_statuses(&journal("Private"), Some(&decryptor)),
            vec![("name", "plaintext")]
        );
        assert_eq!(
            metadata_statuses(&journal("RDEmalformed"), Some(&decryptor)),
            vec![("name", "malformed_d1")]
        );
        assert_eq!(
            metadata_statuses(&journal(&bad_checksum), Some(&decryptor)),
            vec![("name", "malformed_d1")]
        );
        assert_eq!(
            metadata_statuses(&journal(&wrong_format), Some(&decryptor)),
            vec![("name", "malformed_d1")],
            "journal metadata accepts only D1 format 0"
        );
        assert_eq!(
            metadata_statuses(
                &serde_json::json!({
                    "name": 123,
                    "encryption": { "vault": { "grants": [] } }
                }),
                Some(&decryptor)
            ),
            vec![("name", "malformed_d1")]
        );
        assert_eq!(
            metadata_statuses(&journal(&wrong_key_name), Some(&decryptor)),
            vec![("name", "undecryptable_d1")]
        );
        assert!(metadata_statuses(&serde_json::json!({"name": "Plain"}), None).is_empty());
    }

    #[test]
    fn journal_metadata_rejects_valid_ciphertext_with_invalid_utf8() {
        let mut rng = OsRng;
        let user_private =
            RsaPrivateKey::new(&mut rng, 2048).expect("user private key should generate");
        let user_public = user_private.to_public_key();
        let decryptor = test_decryptor("user-1", user_private);
        let vault_key = [7_u8; 32];
        let grant = BASE64_STANDARD.encode(
            user_public
                .encrypt(&mut rng, Oaep::new::<Sha1>(), &vault_key)
                .expect("vault key should wrap"),
        );
        let encrypted = crate::sync::crypto::encrypt_d1_mode0_with_key(&[0xff], &vault_key)
            .expect("metadata should encrypt");
        let journal = serde_json::json!({
            "name": encrypted,
            "encryption": { "vault": { "grants": [{ "encrypted_vault_key": grant }] } }
        });

        assert_eq!(
            metadata_statuses(&journal, Some(&decryptor)),
            vec![("name", "undecryptable_d1")]
        );
    }

    fn public_key_pem(private_key: &RsaPrivateKey) -> String {
        private_key
            .to_public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .expect("public key should encode")
    }

    fn locked_user_key(private_key: &RsaPrivateKey, fingerprint: &str) -> Value {
        let derived = crate::sync::crypto::derive_d1_symmetric_key_from_master_key(
            crate::sync::crypto::test_support::TEST_MASTER_KEY,
        )
        .expect("master key should derive");
        let private_key_pem = private_key
            .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .expect("user key should encode");
        let encrypted_private_key =
            crate::sync::crypto::encrypt_d1_mode0_with_key(private_key_pem.as_bytes(), &derived)
                .expect("user key should encrypt");
        serde_json::json!({
            "fingerprint": fingerprint,
            "publicKey": public_key_pem(private_key),
            "encryptedPrivateKey": encrypted_private_key
        })
    }

    fn sign(private_key: &RsaPrivateKey, data: &[u8]) -> String {
        BASE64_STANDARD.encode(
            private_key
                .sign(
                    rsa::pkcs1v15::Pkcs1v15Sign::new::<Sha256>(),
                    Sha256::digest(data).as_slice(),
                )
                .expect("signature should generate"),
        )
    }

    fn signed_journal_key(signer: &RsaPrivateKey) -> Value {
        let mut rng = OsRng;
        let journal_private =
            RsaPrivateKey::new(&mut rng, 2048).expect("journal key should generate");
        let public_key = public_key_pem(&journal_private);
        let fingerprint = bytes_to_hex_lower(&Sha256::digest(
            journal_private
                .to_public_key()
                .to_public_key_der()
                .expect("public key should encode")
                .as_bytes(),
        ));
        let encrypted_private_key = BASE64_STANDARD.encode(b"locked-journal-private-key");
        let mut signed_data = public_key.as_bytes().to_vec();
        signed_data.extend_from_slice(b"locked-journal-private-key");
        serde_json::json!({
            "fingerprint": fingerprint,
            "public_key": public_key,
            "encrypted_private_key": encrypted_private_key,
            "updated": {
                "fingerprint": "signer-fingerprint",
                "signature": sign(signer, &signed_data)
            }
        })
    }

    #[test]
    fn store_backed_import_accepts_signed_key_and_rejects_forged_key() {
        let mut rng = OsRng;
        let user_private = RsaPrivateKey::new(&mut rng, 2048).expect("user key should generate");
        let journal_private =
            RsaPrivateKey::new(&mut rng, 2048).expect("journal key should generate");
        let vault_key = [7_u8; 32];
        let public_key = public_key_pem(&journal_private);
        let fingerprint = bytes_to_hex_lower(&Sha256::digest(
            journal_private
                .to_public_key()
                .to_public_key_der()
                .expect("public key should encode")
                .as_bytes(),
        ));
        let journal_private_pem = journal_private
            .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .expect("journal key should encode");
        let encrypted_private_key = crate::sync::crypto::encrypt_d1_mode0_with_key(
            journal_private_pem.as_bytes(),
            &vault_key,
        )
        .expect("journal key should encrypt");
        let raw_locked_key = BASE64_STANDARD
            .decode(&encrypted_private_key)
            .expect("locked key should decode");
        let mut signed_data = public_key.as_bytes().to_vec();
        signed_data.extend_from_slice(&raw_locked_key);
        let encrypted_vault_key = user_private
            .to_public_key()
            .encrypt(&mut rng, Oaep::new::<Sha1>(), &vault_key)
            .expect("vault key should wrap");
        let journal = serde_json::json!({
            "id": "journal-1",
            "encryption": {"vault": {
                "grants": [{
                    "user_id": "user-1",
                    "user_fingerprint": "user-fingerprint",
                    "encrypted_vault_key": BASE64_STANDARD.encode(encrypted_vault_key)
                }],
                "keys": [{
                    "fingerprint": fingerprint,
                    "public_key": public_key,
                    "encrypted_private_key": encrypted_private_key,
                    "updated": {"signature": sign(&user_private, &signed_data)}
                }]
            }}
        });
        let user_payload = locked_user_key(&user_private, "user-fingerprint");
        let dir = TempDir::new().expect("temp dir should create");
        let store = Store::open_at(dir.path().join("store.db")).expect("store should open");
        store
            .upsert_singleton_json_row("user_identity_key", 1, &user_payload.to_string(), None)
            .expect("user key should save");
        store
            .upsert_json_row("journals", "journal-1", None, None, &journal.to_string())
            .expect("journal should save");
        let decryptor = build_journal_decryptor(
            &store,
            crate::sync::crypto::test_support::TEST_MASTER_KEY,
            Some("user-1"),
        )
        .expect("journal decryptor should build")
        .expect("journal decryptor should exist");

        let accepted =
            build_entries_decryptor(&store, &decryptor).expect("entries decryptor should build");
        assert!(
            accepted
                .journal_keys_by_fingerprint
                .contains_key(&fingerprint)
        );
        let entry_blob = encrypt_media_blob(
            &journal_private.to_public_key(),
            &fingerprint,
            b"verified entry content",
        );
        assert_eq!(
            decrypt_entry_d1_payload(&entry_blob, &accepted)
                .expect("imported key should decrypt entry content"),
            b"verified entry content"
        );

        let mut forged = journal;
        forged["encryption"]["vault"]["keys"][0]["updated"]["signature"] =
            Value::String("AAAA".to_owned());
        store
            .upsert_json_row("journals", "journal-1", None, None, &forged.to_string())
            .expect("forged journal should save");
        let rejected =
            build_entries_decryptor(&store, &decryptor).expect("entries decryptor should build");
        assert!(rejected.journal_keys_by_fingerprint.is_empty());
    }

    #[test]
    fn accepts_user_signed_journal_key_via_grant_or_update_fingerprint() {
        let mut rng = OsRng;
        let user_private = RsaPrivateKey::new(&mut rng, 2048).expect("user key should generate");
        let decryptor = test_decryptor("user-1", user_private.clone());
        let mut key = signed_journal_key(&user_private);
        let journal: Journal = serde_json::from_value(serde_json::json!({"id": "journal-1"}))
            .expect("journal should parse");
        let grant_vault = serde_json::json!({
            "grants": [{"user_id": "user-1", "user_fingerprint": "USER-FINGERPRINT"}]
        });

        verify_journal_key_signature(
            &key,
            &journal,
            grant_vault
                .as_object()
                .expect("grant vault should be an object"),
            &decryptor,
        )
        .expect("grant fingerprint should resolve the signer");

        key["updated"]["fingerprint"] = Value::String("USER-FINGERPRINT".to_owned());
        let update_vault = serde_json::json!({"grants": []});
        verify_journal_key_signature(
            &key,
            &journal,
            update_vault
                .as_object()
                .expect("update vault should be an object"),
            &decryptor,
        )
        .expect("update fingerprint should resolve the signer");
    }

    #[test]
    fn accepts_journal_key_signed_by_store_backed_rotated_user_key() {
        let mut rng = OsRng;
        let current = RsaPrivateKey::new(&mut rng, 2048).expect("current key should generate");
        let rotated = RsaPrivateKey::new(&mut rng, 2048).expect("rotated key should generate");
        let current_payload = locked_user_key(&current, "current-fingerprint");
        let rotated_payload = locked_user_key(&rotated, "rotated-fingerprint");
        let dir = TempDir::new().expect("temp dir should create");
        let store = Store::open_at(dir.path().join("store.db")).expect("store should open");
        store
            .upsert_singleton_json_row("user_identity_key", 1, &current_payload.to_string(), None)
            .expect("current key should save");
        store
            .upsert_singleton_json_row(
                "user_keys",
                1,
                &serde_json::json!({
                    "userKey": current_payload,
                    "otherUserKeys": [rotated_payload]
                })
                .to_string(),
                None,
            )
            .expect("key bundle should save");
        let decryptor = build_journal_decryptor(
            &store,
            crate::sync::crypto::test_support::TEST_MASTER_KEY,
            Some("user-1"),
        )
        .expect("journal decryptor should build")
        .expect("journal decryptor should exist");
        let mut key = signed_journal_key(&rotated);
        key["updated"]["fingerprint"] = Value::String("rotated-fingerprint".to_owned());
        let journal: Journal = serde_json::from_value(serde_json::json!({"id": "journal-1"}))
            .expect("journal should parse");
        let vault = serde_json::json!({"grants": []});

        verify_journal_key_signature(
            &key,
            &journal,
            vault.as_object().expect("vault should be an object"),
            &decryptor,
        )
        .expect("a store-backed rotated signer should verify");
    }

    #[test]
    fn rejects_unsigned_legacy_journal_key_without_compatibility_fallback() {
        let mut rng = OsRng;
        let user_private = RsaPrivateKey::new(&mut rng, 2048).expect("user key should generate");
        let decryptor = test_decryptor("user-1", user_private.clone());
        let mut key = signed_journal_key(&user_private);
        key["updated"]
            .as_object_mut()
            .expect("updated object")
            .remove("signature");
        let journal: Journal = serde_json::from_value(serde_json::json!({"id": "journal-1"}))
            .expect("journal should parse");
        let vault = serde_json::json!({
            "grants": [{"user_id": "user-1", "user_fingerprint": "user-fingerprint"}]
        });

        assert!(
            verify_journal_key_signature(
                &key,
                &journal,
                vault.as_object().expect("vault should be an object"),
                &decryptor,
            )
            .is_err(),
            "missing trust material must fail closed instead of taking the legacy unlock path"
        );
    }

    #[test]
    fn rejects_missing_and_forged_journal_key_signatures() {
        let mut rng = OsRng;
        let user_private = RsaPrivateKey::new(&mut rng, 2048).expect("user key should generate");
        let attacker_private =
            RsaPrivateKey::new(&mut rng, 2048).expect("attacker key should generate");
        let decryptor = test_decryptor("user-1", user_private);
        let journal: Journal = serde_json::from_value(serde_json::json!({"id": "journal-1"}))
            .expect("journal should parse");
        let vault = serde_json::json!({
            "grants": [{"user_id": "user-1", "user_fingerprint": "user-fingerprint"}]
        });
        let mut missing = signed_journal_key(&attacker_private);
        missing["updated"]
            .as_object_mut()
            .expect("updated object")
            .remove("signature");
        let forged = signed_journal_key(&attacker_private);

        for key in [&missing, &forged] {
            assert!(
                verify_journal_key_signature(
                    key,
                    &journal,
                    vault.as_object().expect("vault should be an object"),
                    &decryptor,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn accepts_shared_journal_key_through_contiguous_ownership_transfer() {
        let mut rng = OsRng;
        let participant =
            RsaPrivateKey::new(&mut rng, 2048).expect("participant key should generate");
        let inviting_owner = RsaPrivateKey::new(&mut rng, 2048).expect("owner key should generate");
        let intermediate_owner =
            RsaPrivateKey::new(&mut rng, 2048).expect("intermediate key should generate");
        let current_owner =
            RsaPrivateKey::new(&mut rng, 2048).expect("new owner key should generate");
        let participant_pem = public_key_pem(&participant);
        let inviting_owner_pem = public_key_pem(&inviting_owner);
        let intermediate_owner_pem = public_key_pem(&intermediate_owner);
        let current_owner_pem = public_key_pem(&current_owner);
        let decryptor = test_decryptor("user-1", participant.clone());
        let key = signed_journal_key(&current_owner);
        let journal: Journal = serde_json::from_value(serde_json::json!({
            "id": "shared-journal",
            "participants": [{
                "id": "user-1",
                "invitation": {
                    "owner_id": "owner-1",
                    "participant_public_key": participant_pem,
                    "owner_public_key": inviting_owner_pem,
                    "owner_public_key_signature_by_participant": sign(&participant, inviting_owner_pem.as_bytes())
                }
            }],
            // The API contract is newest-first; verification walks this chain
            // chronologically from the inviting owner.
            "ownership_transfers": [{
                "previous_owner_id": "owner-2",
                "new_owner_id": "owner-3",
                "previous_owner_public_key": intermediate_owner_pem,
                "new_owner_public_key": current_owner_pem,
                "new_owner_public_key_signature_by_previous_owner": sign(&intermediate_owner, current_owner_pem.as_bytes())
            }, {
                "previous_owner_id": "owner-1",
                "new_owner_id": "owner-2",
                "previous_owner_public_key": inviting_owner_pem,
                "new_owner_public_key": intermediate_owner_pem,
                "new_owner_public_key_signature_by_previous_owner": sign(&inviting_owner, intermediate_owner_pem.as_bytes())
            }]
        }))
        .expect("journal should parse");
        let vault = serde_json::json!({"grants": []});

        verify_journal_key_signature(
            &key,
            &journal,
            vault.as_object().expect("vault should be an object"),
            &decryptor,
        )
        .expect("trusted ownership transfer should verify");
    }

    #[test]
    fn rejects_disconnected_shared_journal_trust_link() {
        let mut rng = OsRng;
        let participant =
            RsaPrivateKey::new(&mut rng, 2048).expect("participant key should generate");
        let inviting_owner = RsaPrivateKey::new(&mut rng, 2048).expect("owner key should generate");
        let attacker = RsaPrivateKey::new(&mut rng, 2048).expect("attacker key should generate");
        let forged_owner =
            RsaPrivateKey::new(&mut rng, 2048).expect("forged owner key should generate");
        let participant_pem = public_key_pem(&participant);
        let inviting_owner_pem = public_key_pem(&inviting_owner);
        let attacker_pem = public_key_pem(&attacker);
        let forged_owner_pem = public_key_pem(&forged_owner);
        let decryptor = test_decryptor("user-1", participant.clone());
        let key = signed_journal_key(&forged_owner);
        let journal: Journal = serde_json::from_value(serde_json::json!({
            "id": "shared-journal",
            "participants": [{
                "id": "user-1",
                "invitation": {
                    "owner_id": "owner-1",
                    "participant_public_key": participant_pem,
                    "owner_public_key": inviting_owner_pem,
                    "owner_public_key_signature_by_participant": sign(&participant, inviting_owner_pem.as_bytes())
                }
            }],
            "ownership_transfers": [{
                "previous_owner_id": "attacker",
                "new_owner_id": "owner-2",
                "previous_owner_public_key": attacker_pem,
                "new_owner_public_key": forged_owner_pem,
                "new_owner_public_key_signature_by_previous_owner": sign(&attacker, forged_owner_pem.as_bytes())
            }]
        }))
        .expect("journal should parse");
        let vault = serde_json::json!({"grants": []});

        assert!(
            verify_journal_key_signature(
                &key,
                &journal,
                vault.as_object().expect("vault should be an object"),
                &decryptor,
            )
            .is_err(),
            "an independently valid but disconnected transfer must not become trusted"
        );
    }

    /// Builds a D1 format-1 (locked-key) media blob exactly like the push-side
    /// `encrypt_original_media_for_journal` does, so we can prove the pull path
    /// decrypts it.
    fn encrypt_media_blob(
        public: &RsaPublicKey,
        fingerprint_hex: &str,
        plaintext: &[u8],
    ) -> Vec<u8> {
        let mut rng = OsRng;
        let mut content_key = [0_u8; 32];
        rng.fill_bytes(&mut content_key);
        let mut iv = [0_u8; 12];
        rng.fill_bytes(&mut iv);

        let key = Key::<Aes256Gcm>::from(content_key);
        let cipher = Aes256Gcm::new(&key);
        let ciphertext_and_tag = cipher
            .encrypt((&iv).into(), plaintext)
            .expect("media should encrypt");
        let wrapped_key = public
            .encrypt(&mut rng, Oaep::new::<Sha1>(), &content_key)
            .expect("content key should wrap");

        let mut out = Vec::new();
        out.extend_from_slice(b"D1");
        out.push(1); // schema
        out.push(1); // format 1 (no gzip)
        out.extend_from_slice(&decode_hex(fingerprint_hex));
        out.extend_from_slice(&[0_u8, 0_u8]); // signature length = 0
        out.extend_from_slice(&wrapped_key);
        out.extend_from_slice(&iv);
        out.extend_from_slice(&ciphertext_and_tag);
        let md5 = Md5::digest(&out);
        out.extend_from_slice(&md5);
        out
    }

    fn gzip_bytes(input: &[u8]) -> Vec<u8> {
        use std::io::Write;

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(input).expect("gzip input should write");
        encoder.finish().expect("gzip should finish")
    }

    #[test]
    fn encrypted_entry_rejects_decompression_over_128_mib() {
        let mut rng = OsRng;
        let journal_private = RsaPrivateKey::new(&mut rng, 2048).expect("journal key generates");
        let fingerprint = Sha256::digest(
            journal_private
                .to_public_key()
                .to_public_key_der()
                .expect("public key should encode")
                .as_bytes(),
        );
        let content_key = [9_u8; 32];
        let iv = [7_u8; 12];
        let compressed = gzip_bytes(&vec![0_u8; MAX_DECRYPTED_ENTRY_BYTES as usize + 1]);
        let key = Key::<Aes256Gcm>::from(content_key);
        let ciphertext = Aes256Gcm::new(&key)
            .encrypt((&iv).into(), compressed.as_slice())
            .expect("payload should encrypt");
        let locked_key = journal_private
            .to_public_key()
            .encrypt(&mut rng, Oaep::new::<Sha1>(), &content_key)
            .expect("content key should lock");
        let mut payload = b"D1\x01\x02".to_vec();
        payload.extend_from_slice(&fingerprint);
        payload.extend_from_slice(&[0, 0]);
        payload.extend_from_slice(&locked_key);
        payload.extend_from_slice(&iv);
        payload.extend_from_slice(&ciphertext);
        payload.extend_from_slice(&[0_u8; 16]);
        let decryptor = EntriesDecryptor {
            journal_keys_by_fingerprint: HashMap::from([(
                bytes_to_hex_lower(&fingerprint),
                journal_private,
            )]),
        };

        let error = decrypt_entry_d1_payload(&payload, &decryptor)
            .expect_err("oversized plaintext must be rejected");
        assert!(error.to_string().contains("128 MiB"));
    }

    #[test]
    fn encrypted_metadata_without_a_vault_is_skipped() {
        let mut journal = serde_json::json!({
            "name": "RDEencrypted-looking",
            "encryption": {}
        });

        assert!(matches!(
            try_decrypt_journal_fields_with_key(&mut journal, None).expect("diagnostic outcome"),
            JournalFieldDecryptOutcome::Skipped
        ));
    }

    #[test]
    fn malformed_grant_for_encrypted_metadata_is_a_failed_decryption() {
        let mut rng = OsRng;
        let user_private_key =
            RsaPrivateKey::new(&mut rng, 2048).expect("user private key generates");
        let decryptor = JournalDecryptor {
            user_id: Some("user-1".to_owned()),
            user_private_key,
            user_verify_keys: HashMap::new(),
        };
        let mut journal = serde_json::json!({
            "name": "RDEmalformed",
            "encryption": { "vault": { "grants": [{}] } }
        });

        assert!(matches!(
            try_decrypt_journal_fields(&mut journal, &decryptor).expect("diagnostic outcome"),
            JournalFieldDecryptOutcome::Failed
        ));
    }

    #[test]
    fn pull_decryptor_round_trips_encrypted_media() {
        let mut rng = OsRng;
        let journal_private = RsaPrivateKey::new(&mut rng, 2048).expect("journal key generates");
        let public = journal_private.to_public_key();
        let fingerprint_hex =
            "0011223344556677889900aabbccddeeff00112233445566778899aabbccddee".to_owned();

        let media = b"\x00\x01\x02\xff\xfe binary media \x80\x90 not utf8 \xff";
        let blob = encrypt_media_blob(&public, &fingerprint_hex, media);

        let mut journal_keys_by_fingerprint = HashMap::new();
        journal_keys_by_fingerprint.insert(fingerprint_hex.clone(), journal_private);
        let decryptor = EntriesDecryptor {
            journal_keys_by_fingerprint,
        };

        let decrypted =
            decrypt_original_media(&blob, &decryptor).expect("pull should decrypt media");
        assert_eq!(decrypted, media);
    }
}
