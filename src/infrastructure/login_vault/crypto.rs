use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    aead::{Aead, Payload},
    KeyInit, XChaCha20Poly1305, XNonce,
};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub(super) const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Sealed {
    pub nonce: [u8; 24],
    pub bytes: Vec<u8>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Envelope {
    pub version: u32,
    pub kind: String,
    pub id: String,
    pub salt: [u8; 16],
    pub key: Sealed,
    pub data: Sealed,
}
pub(super) fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    OsRng.fill_bytes(&mut bytes);
    bytes
}
pub(super) fn password_key(password: &str, salt: &[u8; 16]) -> Result<Zeroizing<[u8; 32]>, String> {
    if password.len() > 1024 {
        return Err("Passphrase is too long".into());
    }
    let params =
        Params::new(65536, 3, 1, Some(32)).map_err(|_| "Invalid key derivation parameters")?;
    let mut key = Zeroizing::new([0; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, key.as_mut())
        .map_err(|_| "Passphrase verification failed")?;
    Ok(key)
}
pub(super) fn validate_new_password(password: &str) -> Result<(), String> {
    if password.chars().count() < 12 || password.len() > 1024 {
        return Err("Use a passphrase with at least 12 characters".into());
    }
    Ok(())
}
pub(super) fn seal(key: &[u8; 32], bytes: &[u8], aad: &[u8]) -> Result<Sealed, String> {
    let nonce = random();
    let cipher = XChaCha20Poly1305::new(key.into());
    let bytes = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: bytes, aad })
        .map_err(|_| "Encryption failed")?;
    Ok(Sealed { nonce, bytes })
}
pub(super) fn open(
    key: &[u8; 32],
    sealed: &Sealed,
    aad: &[u8],
) -> Result<Zeroizing<Vec<u8>>, String> {
    if sealed.bytes.len() > MAX_FILE_BYTES as usize {
        return Err("Login file exceeds the size limit".into());
    }
    XChaCha20Poly1305::new(key.into())
        .decrypt(
            XNonce::from_slice(&sealed.nonce),
            Payload {
                msg: &sealed.bytes,
                aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| "Incorrect passphrase or damaged encrypted file".into())
}
impl Envelope {
    pub fn aad(&self, part: &str) -> Vec<u8> {
        format!(
            "ArcRelayLogin:{}:{}:{}:{}",
            self.version, self.kind, self.id, part
        )
        .into_bytes()
    }
    pub fn create(
        kind: &str,
        password: &str,
        payload: &[u8],
    ) -> Result<(Self, Zeroizing<[u8; 32]>), String> {
        validate_new_password(password)?;
        let salt = random();
        let kek = password_key(password, &salt)?;
        let dek = Zeroizing::new(random());
        let mut e = Self {
            version: 1,
            kind: kind.into(),
            id: uuid::Uuid::new_v4().to_string(),
            salt,
            key: Sealed {
                nonce: [0; 24],
                bytes: vec![],
            },
            data: Sealed {
                nonce: [0; 24],
                bytes: vec![],
            },
        };
        e.key = seal(&kek, dek.as_ref(), &e.aad("key"))?;
        e.data = seal(&dek, payload, &e.aad("data"))?;
        Ok((e, dek))
    }
    pub fn unlock(&self, password: &str, kind: &str) -> Result<Zeroizing<[u8; 32]>, String> {
        if self.version != 1 || self.kind != kind || uuid::Uuid::parse_str(&self.id).is_err() {
            return Err("Unsupported login file format or version".into());
        }
        let kek = password_key(password, &self.salt)?;
        let bytes = open(&kek, &self.key, &self.aad("key"))?;
        if bytes.len() != 32 {
            return Err("Invalid login key".into());
        }
        let mut key = Zeroizing::new([0; 32]);
        key.copy_from_slice(&bytes);
        Ok(key)
    }
}
