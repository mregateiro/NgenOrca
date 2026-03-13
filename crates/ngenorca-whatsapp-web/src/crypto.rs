//! Shared cryptographic utilities.
//!
//! Provides HKDF, HMAC, AES-CBC, AES-GCM, and key-pair generation
//! used by both the Noise handshake and Signal Protocol layers.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce as GcmNonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use x25519_dalek::{PublicKey as X25519Public, StaticSecret as X25519Secret};

type HmacSha256 = Hmac<Sha256>;

/// Generate a new X25519 key pair.
pub fn generate_x25519_keypair() -> (X25519Secret, X25519Public) {
    let secret = X25519Secret::random_from_rng(rand::thread_rng());
    let public = X25519Public::from(&secret);
    (secret, public)
}

/// Generate random bytes.
pub fn random_bytes(len: usize) -> Vec<u8> {
    use rand::RngCore;
    let mut buf = vec![0u8; len];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

/// HKDF-SHA256 key derivation.
///
/// Derives `length` bytes from the input key material.
pub fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8], length: usize) -> crate::Result<Vec<u8>> {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut okm = vec![0u8; length];
    hk.expand(info, &mut okm)
        .map_err(|e| crate::Error::Other(format!("HKDF expand: {e}")))?;
    Ok(okm)
}

/// HMAC-SHA256.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC can take key of any size");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// Verify HMAC-SHA256.
pub fn hmac_sha256_verify(key: &[u8], data: &[u8], expected: &[u8]) -> bool {
    let computed = hmac_sha256(key, data);
    constant_time_eq(&computed, expected)
}

/// AES-256-GCM encrypt.
pub fn aes256_gcm_encrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    plaintext: &[u8],
    aad: &[u8],
) -> crate::Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(key.into());
    let nonce = GcmNonce::from_slice(nonce);
    let payload = aes_gcm::aead::Payload {
        msg: plaintext,
        aad,
    };
    cipher
        .encrypt(nonce, payload)
        .map_err(|e| crate::Error::Other(format!("AES-GCM encrypt: {e}")))
}

/// AES-256-GCM decrypt.
pub fn aes256_gcm_decrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
    aad: &[u8],
) -> crate::Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(key.into());
    let nonce = GcmNonce::from_slice(nonce);
    let payload = aes_gcm::aead::Payload {
        msg: ciphertext,
        aad,
    };
    cipher
        .decrypt(nonce, payload)
        .map_err(|e| crate::Error::Other(format!("AES-GCM decrypt: {e}")))
}

/// AES-256-CBC encrypt (used by Signal Protocol for message encryption).
pub fn aes256_cbc_encrypt(key: &[u8; 32], iv: &[u8; 16], plaintext: &[u8]) -> Vec<u8> {
    use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
    type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;

    let encryptor = Aes256CbcEnc::new(key.into(), iv.into());
    encryptor.encrypt_padded_vec_mut::<Pkcs7>(plaintext)
}

/// AES-256-CBC decrypt (used by Signal Protocol for message encryption).
pub fn aes256_cbc_decrypt(
    key: &[u8; 32],
    iv: &[u8; 16],
    ciphertext: &[u8],
) -> crate::Result<Vec<u8>> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
    type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

    let decryptor = Aes256CbcDec::new(key.into(), iv.into());
    decryptor
        .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
        .map_err(|e| crate::Error::Signal(format!("AES-CBC decrypt: {e}")))
}

/// Constant-time byte comparison.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x25519_keypair_generates_different_keys() {
        let (s1, p1) = generate_x25519_keypair();
        let (s2, p2) = generate_x25519_keypair();
        assert_ne!(p1.as_bytes(), p2.as_bytes());
        // DH should be commutative.
        let shared1 = s1.diffie_hellman(&p2);
        let shared2 = s2.diffie_hellman(&p1);
        assert_eq!(shared1.as_bytes(), shared2.as_bytes());
    }

    #[test]
    fn hkdf_produces_expected_length() {
        let result = hkdf_sha256(b"salt", b"ikm", b"info", 64).unwrap();
        assert_eq!(result.len(), 64);
    }

    #[test]
    fn hmac_roundtrip() {
        let key = b"my-secret-key";
        let data = b"hello world";
        let mac = hmac_sha256(key, data);
        assert!(hmac_sha256_verify(key, data, &mac));
        assert!(!hmac_sha256_verify(key, b"wrong data", &mac));
    }

    #[test]
    fn aes_gcm_roundtrip() {
        let key = [0x42u8; 32];
        let nonce = [0x01u8; 12];
        let plaintext = b"Hello WhatsApp";
        let aad = b"additional-data";

        let ct = aes256_gcm_encrypt(&key, &nonce, plaintext, aad).unwrap();
        let pt = aes256_gcm_decrypt(&key, &nonce, &ct, aad).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn aes_cbc_roundtrip() {
        let key = [0x42u8; 32];
        let iv = [0x01u8; 16];
        let plaintext = b"Signal Protocol message";

        let ct = aes256_cbc_encrypt(&key, &iv, plaintext);
        let pt = aes256_cbc_decrypt(&key, &iv, &ct).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn random_bytes_nonzero_length() {
        let a = random_bytes(32);
        let b = random_bytes(32);
        assert_eq!(a.len(), 32);
        assert_ne!(a, b); // Overwhelmingly unlikely to be equal
    }
}
