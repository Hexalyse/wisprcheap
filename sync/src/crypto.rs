//! End-to-end encryption of the synced data (SPEC.md section 3).
//!
//! - The **data key** (DK, 32 random bytes) encrypts everything; it's created by the first device.
//! - The **passphrase key** (PK) = Argon2id(NFC(passphrase), salt) wraps DK for the server (keyring).
//! - Records are sealed with AES-256-GCM under `enc = HKDF(DK, "wisprcheap/enc/v1")`; ids that would
//!   reveal content are blinded with HMAC-SHA256 under `ids = HKDF(DK, "wisprcheap/ids/v1")`.

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use unicode_normalization::UnicodeNormalization;

pub const ENVELOPE_PREFIX: &str = "e1.";
pub const DATA_KEY_PREFIX: &str = "wck_";
const NONCE_LEN: usize = 12;
const PAD_TO: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("wrong passphrase")]
    WrongPassphrase,
    #[error("could not decrypt the data (wrong key or damaged data)")]
    Decrypt,
    #[error("invalid encoding: {0}")]
    Encoding(String),
    #[error("key derivation failed: {0}")]
    Kdf(String),
}

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn unb64(text: &str) -> Result<Vec<u8>, CryptoError> {
    URL_SAFE_NO_PAD.decode(text.trim()).map_err(|e| CryptoError::Encoding(e.to_string()))
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).expect("the operating system's random generator failed");
    out
}

/// Argon2id parameters, stored with the keyring so they can change later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub alg: String,
    /// Memory in KiB.
    pub m: u32,
    pub t: u32,
    pub p: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        Self { alg: "argon2id".into(), m: 65_536, t: 3, p: 1 }
    }
}

/// PK = Argon2id(NFC(passphrase), salt) → 32 bytes.
pub fn derive_passphrase_key(passphrase: &str, salt: &[u8], params: &KdfParams) -> Result<[u8; 32], CryptoError> {
    if params.alg != "argon2id" {
        return Err(CryptoError::Kdf(format!("unsupported algorithm {}", params.alg)));
    }
    let normalized: String = passphrase.nfc().collect();
    let p = argon2::Params::new(params.m, params.t, params.p, Some(32)).map_err(|e| CryptoError::Kdf(e.to_string()))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, p);
    let mut out = [0u8; 32];
    argon
        .hash_password_into(normalized.as_bytes(), salt, &mut out)
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(out)
}

fn hkdf32(ikm: &[u8], info: &str) -> [u8; 32] {
    let hk = hkdf::Hkdf::<Sha256>::new(None, ikm);
    let mut out = [0u8; 32];
    hk.expand(info.as_bytes(), &mut out).expect("32 bytes is a valid HKDF length");
    out
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message);
    mac.finalize().into_bytes().into()
}

/// `e1.` + base64url(nonce ‖ ciphertext ‖ tag), AES-256-GCM.
pub fn seal_with_nonce(key: &[u8; 32], nonce: &[u8; NONCE_LEN], aad: &[u8], plaintext: &[u8]) -> String {
    let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte key");
    let ct = cipher
        .encrypt(nonce.into(), Payload { msg: plaintext, aad })
        .expect("AES-GCM encryption can't fail for valid sizes");
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(nonce);
    out.extend_from_slice(&ct);
    format!("{ENVELOPE_PREFIX}{}", b64(&out))
}

pub fn seal(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> String {
    seal_with_nonce(key, &random_bytes::<NONCE_LEN>(), aad, plaintext)
}

pub fn open(key: &[u8; 32], aad: &[u8], envelope: &str) -> Result<Vec<u8>, CryptoError> {
    let body = envelope
        .strip_prefix(ENVELOPE_PREFIX)
        .ok_or_else(|| CryptoError::Encoding("unknown envelope version".into()))?;
    let bytes = unb64(body)?;
    if bytes.len() < NONCE_LEN + 16 {
        return Err(CryptoError::Encoding("envelope too short".into()));
    }
    let (nonce, ct) = bytes.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce.try_into().expect("12 bytes");
    let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte key");
    cipher
        .decrypt((&nonce).into(), Payload { msg: ct, aad })
        .map_err(|_| CryptoError::Decrypt)
}

fn wrap_aad(user_id: &str) -> Vec<u8> {
    format!("wisprcheap/keywrap/v1|{user_id}").into_bytes()
}

fn record_aad(user_id: &str, kind: &str, id: &str) -> Vec<u8> {
    format!("wisprcheap/rec/v1|{user_id}|{kind}|{id}").into_bytes()
}

/// JSON `{"v":1,"value":…}` padded with spaces to a multiple of 64 bytes (hides exact lengths).
fn record_plaintext(value: &serde_json::Value) -> Vec<u8> {
    let mut text = serde_json::to_vec(&serde_json::json!({ "v": 1, "value": value })).expect("JSON value");
    let padded = text.len().div_ceil(PAD_TO) * PAD_TO;
    text.resize(padded, b' ');
    text
}

/// The data key and its derived subkeys.
#[derive(Clone)]
pub struct DataKey {
    dk: [u8; 32],
    enc: [u8; 32],
    ids: [u8; 32],
}

impl std::fmt::Debug for DataKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DataKey({})", self.key_id())
    }
}

impl DataKey {
    pub fn generate() -> Self {
        Self::from_bytes(random_bytes::<32>())
    }

    pub fn from_bytes(dk: [u8; 32]) -> Self {
        Self { dk, enc: hkdf32(&dk, "wisprcheap/enc/v1"), ids: hkdf32(&dk, "wisprcheap/ids/v1") }
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.dk
    }

    /// `wck_…`: how a device stores its copy of the data key (desktop `sync.key`).
    pub fn export(&self) -> String {
        format!("{DATA_KEY_PREFIX}{}", b64(&self.dk))
    }

    pub fn import(text: &str) -> Result<Self, CryptoError> {
        let body = text
            .trim()
            .strip_prefix(DATA_KEY_PREFIX)
            .ok_or_else(|| CryptoError::Encoding("a data key starts with wck_".into()))?;
        let bytes: [u8; 32] = unb64(body)?
            .try_into()
            .map_err(|_| CryptoError::Encoding("a data key is 32 bytes".into()))?;
        Ok(Self::from_bytes(bytes))
    }

    /// Public fingerprint of the key (stored in the keyring) so devices notice an encryption reset.
    pub fn key_id(&self) -> String {
        b64(&hmac_sha256(&self.dk, b"wisprcheap/keyid/v1")[..12])
    }

    /// Blinded record id for ids that would reveal content (dictionary terms, pair ids, price models).
    pub fn blind_id(&self, kind: &str, key: &str) -> String {
        let mac = hmac_sha256(&self.ids, format!("{kind}:{key}").as_bytes());
        let mut id = b64(&mac);
        id.truncate(22);
        id
    }

    pub fn encrypt_record(&self, user_id: &str, kind: &str, id: &str, value: &serde_json::Value) -> String {
        seal(&self.enc, &record_aad(user_id, kind, id), &record_plaintext(value))
    }

    /// Deterministic variant for test vectors only.
    pub fn encrypt_record_with_nonce(&self, user_id: &str, kind: &str, id: &str, value: &serde_json::Value, nonce: &[u8; 12]) -> String {
        seal_with_nonce(&self.enc, nonce, &record_aad(user_id, kind, id), &record_plaintext(value))
    }

    pub fn decrypt_record(&self, user_id: &str, kind: &str, id: &str, envelope: &str) -> Result<serde_json::Value, CryptoError> {
        let plain = open(&self.enc, &record_aad(user_id, kind, id), envelope)?;
        let doc: serde_json::Value =
            serde_json::from_slice(&plain).map_err(|e| CryptoError::Encoding(format!("record JSON: {e}")))?;
        if doc.get("v").and_then(|v| v.as_i64()) != Some(1) {
            return Err(CryptoError::Encoding("unknown record version".into()));
        }
        Ok(doc.get("value").cloned().unwrap_or(serde_json::Value::Null))
    }
}

/// Wraps the data key with the passphrase key for the server keyring.
pub fn wrap_key(pk: &[u8; 32], user_id: &str, dk: &DataKey) -> String {
    seal(pk, &wrap_aad(user_id), dk.as_bytes())
}

pub fn wrap_key_with_nonce(pk: &[u8; 32], user_id: &str, dk: &DataKey, nonce: &[u8; 12]) -> String {
    seal_with_nonce(pk, nonce, &wrap_aad(user_id), dk.as_bytes())
}

/// Unwraps the keyring's data key; a wrong passphrase fails here.
pub fn unwrap_key(pk: &[u8; 32], user_id: &str, wrapped: &str) -> Result<DataKey, CryptoError> {
    let bytes = open(pk, &wrap_aad(user_id), wrapped).map_err(|e| match e {
        CryptoError::Decrypt => CryptoError::WrongPassphrase,
        other => other,
    })?;
    let dk: [u8; 32] = bytes.try_into().map_err(|_| CryptoError::Encoding("wrapped key is not 32 bytes".into()))?;
    Ok(DataKey::from_bytes(dk))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast() -> KdfParams {
        KdfParams { alg: "argon2id".into(), m: 1024, t: 1, p: 1 }
    }

    #[test]
    fn record_round_trip_and_binding() {
        let dk = DataKey::generate();
        let value = serde_json::json!({ "term": "Kubernetes", "soundsLike": ["cube"] });
        let env = dk.encrypt_record("usr_a", "dict", "abc", &value);
        assert!(env.starts_with("e1."));
        assert_eq!(dk.decrypt_record("usr_a", "dict", "abc", &env).unwrap(), value);
        // Bound to the user, kind and id.
        assert!(dk.decrypt_record("usr_b", "dict", "abc", &env).is_err());
        assert!(dk.decrypt_record("usr_a", "pair", "abc", &env).is_err());
        assert!(dk.decrypt_record("usr_a", "dict", "abd", &env).is_err());
        // Another key can't open it.
        assert!(DataKey::generate().decrypt_record("usr_a", "dict", "abc", &env).is_err());
    }

    #[test]
    fn padding_hides_lengths() {
        let dk = DataKey::generate();
        let a = dk.encrypt_record("u", "secret", "openai", &serde_json::json!("sk-1"));
        let b = dk.encrypt_record("u", "secret", "openai", &serde_json::json!("sk-12345678901234567890"));
        assert_eq!(a.len(), b.len());
    }

    #[test]
    fn wrap_unwrap_and_wrong_passphrase() {
        let salt = [7u8; 16];
        let pk = derive_passphrase_key("correct horse", &salt, &fast()).unwrap();
        let dk = DataKey::generate();
        let wrapped = wrap_key(&pk, "usr_a", &dk);
        assert_eq!(unwrap_key(&pk, "usr_a", &wrapped).unwrap().as_bytes(), dk.as_bytes());
        let wrong = derive_passphrase_key("wrong horse", &salt, &fast()).unwrap();
        assert!(matches!(unwrap_key(&wrong, "usr_a", &wrapped), Err(CryptoError::WrongPassphrase)));
        assert!(matches!(unwrap_key(&pk, "usr_b", &wrapped), Err(CryptoError::WrongPassphrase)));
    }

    #[test]
    fn nfc_normalisation() {
        let salt = [1u8; 16];
        let composed = derive_passphrase_key("caf\u{e9}", &salt, &fast()).unwrap();
        let decomposed = derive_passphrase_key("cafe\u{301}", &salt, &fast()).unwrap();
        assert_eq!(composed, decomposed);
    }

    #[test]
    fn export_import_and_ids() {
        let dk = DataKey::generate();
        let again = DataKey::import(&dk.export()).unwrap();
        assert_eq!(again.as_bytes(), dk.as_bytes());
        assert_eq!(dk.key_id().len(), 16);
        assert_eq!(dk.blind_id("dict", "kubernetes").len(), 22);
        assert_eq!(dk.blind_id("dict", "kubernetes"), again.blind_id("dict", "kubernetes"));
        assert_ne!(dk.blind_id("dict", "kubernetes"), dk.blind_id("pair", "kubernetes"));
        assert!(DataKey::import("nope").is_err());
    }
}
