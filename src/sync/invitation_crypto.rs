use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use hmac::{Hmac, Mac};
use rsa::pkcs8::{DecodePublicKey, EncodePublicKey, LineEnding};
use rsa::{Pkcs1v15Sign, RsaPublicKey};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::crypto::{
    UserKeysPayload, derive_d1_symmetric_key_from_master_key, unlock_user_identity_key,
};

type HmacSha256 = Hmac<Sha256>;

pub struct InvitationProof {
    pub participant_public_key_hmac: String,
    pub owner_public_key_signature_by_participant: String,
}

#[derive(Debug, thiserror::Error)]
pub enum InvitationCryptoError {
    #[error("invitation key verification failed; use the complete original link")]
    OwnerVerification,
    #[error("invalid invitation owner public key")]
    OwnerKey,
    #[error("cannot unlock user identity key; check `dayone auth key-set` and run `dayone sync`")]
    ParticipantKey,
}

pub fn verify_owner_key(
    invite_key: &[u8; 32],
    owner_key: &str,
    owner_hmac: &str,
) -> Result<(), InvitationCryptoError> {
    let signature = BASE64
        .decode(owner_hmac)
        .map_err(|_| InvitationCryptoError::OwnerVerification)?;
    let mut mac = HmacSha256::new_from_slice(invite_key)
        .map_err(|_| InvitationCryptoError::OwnerVerification)?;
    mac.update(owner_key.as_bytes());
    mac.verify_slice(&signature)
        .map_err(|_| InvitationCryptoError::OwnerVerification)?;
    RsaPublicKey::from_public_key_pem(owner_key).map_err(|_| InvitationCryptoError::OwnerKey)?;
    Ok(())
}

pub fn build_proof(
    master_key: &str,
    user_keys: &str,
    invite_key: &[u8; 32],
    owner_key: &str,
) -> Result<InvitationProof, InvitationCryptoError> {
    let invalid = |_| InvitationCryptoError::ParticipantKey;
    let raw: Value = serde_json::from_str(user_keys).map_err(invalid)?;
    let keys = UserKeysPayload::from_raw(&raw).ok_or(InvitationCryptoError::ParticipantKey)?;
    let symmetric = derive_d1_symmetric_key_from_master_key(master_key)
        .ok_or(InvitationCryptoError::ParticipantKey)?;
    let private = unlock_user_identity_key(&keys, &symmetric)
        .map_err(|_| InvitationCryptoError::ParticipantKey)?
        .ok_or(InvitationCryptoError::ParticipantKey)?;
    // WebCrypto re-exports SPKI PEM with 64-character lines, LF, and a trailing newline.
    let public = private
        .to_public_key()
        .to_public_key_pem(LineEnding::LF)
        .map_err(|_| InvitationCryptoError::ParticipantKey)?;
    let mut mac = HmacSha256::new_from_slice(invite_key)
        .map_err(|_| InvitationCryptoError::ParticipantKey)?;
    mac.update(public.as_bytes());
    let signature = private
        .sign_with_rng(
            &mut rsa::rand_core::OsRng,
            Pkcs1v15Sign::new::<Sha256>(),
            &Sha256::digest(owner_key.as_bytes()),
        )
        .map_err(|_| InvitationCryptoError::ParticipantKey)?;
    Ok(InvitationProof {
        participant_public_key_hmac: BASE64.encode(mac.finalize().into_bytes()),
        owner_public_key_signature_by_participant: BASE64.encode(signature),
    })
}
