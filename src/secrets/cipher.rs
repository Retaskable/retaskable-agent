//! One secret, sealed (agent-settings.md section 3.3): XChaCha20-Poly1305 under the data key, a fresh random 24-byte
//! nonce for every seal, and the secret's own NAME as associated data, so a ciphertext copied under another name
//! doesn't open. Stored as `v1:<base64 of nonce then ciphertext and tag>`; the prefix lets a later cipher live beside
//! this one.
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use rand::RngExt;
use zeroize::{Zeroize, Zeroizing};

const PREFIX: &str = "v1:";
const NONCE_LEN: usize = 24;

/// The 256-bit key every secret in one data folder is sealed under. Its `Debug` says nothing of the bytes, it is
/// compared in constant time, and its memory is wiped when it is dropped.
#[derive(Clone)]
pub struct DataKey([u8; 32]);

impl Drop for DataKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl PartialEq for DataKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.iter().zip(&other.0).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
    }
}

impl Eq for DataKey {}

impl DataKey {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rng().fill(&mut bytes);
        Self(bytes)
    }

    /// From base64 text (surrounding whitespace ignored), or `None` unless it holds exactly 32 bytes.
    pub fn from_base64(text: &str) -> Option<Self> {
        let bytes = Zeroizing::new(STANDARD.decode(text.trim()).ok()?);
        Some(Self(<[u8; 32]>::try_from(bytes.as_slice()).ok()?))
    }

    pub fn to_base64(&self) -> Zeroizing<String> {
        Zeroizing::new(STANDARD.encode(self.0))
    }
}

impl std::fmt::Debug for DataKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DataKey(..)")
    }
}

/// A stored value that doesn't open: the wrong key, a changed or truncated ciphertext, another name's, or a format
/// this build doesn't know. The caller shows "unreadable"; it never falls back to anything else.
#[derive(Debug, PartialEq, Eq)]
pub struct Unreadable;

fn cipher(key: &DataKey) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(&key.0.into())
}

/// `plaintext` sealed as secret `name`.
pub fn seal(key: &DataKey, name: &str, plaintext: &str) -> String {
    let mut nonce = [0u8; NONCE_LEN];
    rand::rng().fill(&mut nonce);
    let payload = Payload {
        msg: plaintext.as_bytes(),
        aad: name.as_bytes(),
    };
    let sealed = cipher(key)
        .encrypt(&XNonce::from(nonce), payload)
        .expect("XChaCha20-Poly1305 seals any message this size");
    let mut out = nonce.to_vec();
    out.extend_from_slice(&sealed);
    format!("{PREFIX}{}", STANDARD.encode(out))
}

/// The secret `name`'s value from `stored`.
pub fn open(key: &DataKey, name: &str, stored: &str) -> Result<String, Unreadable> {
    let body = stored.strip_prefix(PREFIX).ok_or(Unreadable)?;
    let bytes = STANDARD.decode(body).map_err(|_| Unreadable)?;
    if bytes.len() <= NONCE_LEN {
        return Err(Unreadable);
    }
    let (nonce, sealed) = bytes.split_at(NONCE_LEN);
    let nonce = <[u8; NONCE_LEN]>::try_from(nonce).map_err(|_| Unreadable)?;
    let payload = Payload {
        msg: sealed,
        aad: name.as_bytes(),
    };
    let plain = cipher(key)
        .decrypt(&XNonce::from(nonce), payload)
        .map_err(|_| Unreadable)?;
    // A value that isn't text is wiped, not dropped as it is.
    String::from_utf8(plain).map_err(|e| {
        e.into_bytes().zeroize();
        Unreadable
    })
}
