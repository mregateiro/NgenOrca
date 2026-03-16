//! Noise Protocol XX handler for WhatsApp Web.
//!
//! WhatsApp uses `Noise_XX_25519_AESGCM_SHA256`:
//! - **XX** pattern (both sides transmit static keys)
//! - **X25519** for Diffie-Hellman
//! - **AES-256-GCM** for symmetric encryption
//! - **SHA-256** for hashing
//!
//! The handshake proceeds as:
//! 1. Client → Server: ephemeral public key
//! 2. Server → Client: ephemeral + static keys, encrypted payload
//! 3. Client → Server: static key + encrypted payload (ClientPayload)
//!
//! After the handshake, a pair of CipherStates is used for transport
//! encryption of all subsequent WABinary frames.
//!
//! ## Framing
//!
//! Before the handshake, the client sends a 4-byte prologue: `WA\x06\x03`
//! (WA magic + protocol version major.minor).
//!
//! After the handshake, each message is framed as:
//!   `[3-byte big-endian length][noise-encrypted payload]`

use crate::crypto;
use sha2::{Digest, Sha256};
use tracing::{debug, trace};
use x25519_dalek::{PublicKey as X25519Public, StaticSecret as X25519Secret};

/// WhatsApp protocol version embedded in the Noise prologue.
pub const WA_MAGIC: &[u8] = b"WA";
pub const WA_VERSION_MAJOR: u8 = 6;
pub const WA_VERSION_MINOR: u8 = 3;

/// Maximum Noise message size (64 KiB per spec).
#[allow(dead_code)]
const MAX_MSG_SIZE: usize = 65535;

/// Noise handshake hash initial value (protocol name hashed).
const PROTOCOL_NAME: &[u8] = b"Noise_XX_25519_AESGCM_SHA256\0\0\0\0";

/// Noise handler managing the XX handshake and transport encryption.
pub struct NoiseHandler {
    /// Handshake hash — evolves during the handshake.
    h: [u8; 32],
    /// Chaining key — used to derive new keys.
    ck: [u8; 32],
    /// Encryption key during handshake.
    enc_key: [u8; 32],
    /// Our static key pair.
    static_secret: X25519Secret,
    static_public: X25519Public,
    /// Our ephemeral key pair (generated per handshake).
    ephemeral_secret: Option<X25519Secret>,
    ephemeral_public: Option<X25519Public>,
    /// Server's ephemeral public key — captured during `process_server_hello`,
    /// consumed during `build_client_finish` for the Noise XX `se` DH step.
    server_ephemeral: Option<X25519Public>,
    /// Post-handshake send/receive cipher states.
    send_key: Option<[u8; 32]>,
    recv_key: Option<[u8; 32]>,
    send_counter: u32,
    recv_counter: u32,
    /// Whether the handshake is complete.
    pub handshake_complete: bool,
}

impl NoiseHandler {
    /// Create a new Noise handler with a fresh static key pair.
    pub fn new() -> Self {
        let (static_secret, static_public) = crypto::generate_x25519_keypair();
        Self::with_static_key(static_secret, static_public)
    }

    /// Create a handler with an existing static key pair (for session resumption).
    pub fn with_static_key(secret: X25519Secret, public: X25519Public) -> Self {
        // Initialize h = SHA-256(protocol_name)
        let h = Sha256::digest(PROTOCOL_NAME);
        let mut h_arr = [0u8; 32];
        h_arr.copy_from_slice(&h);

        // ck = h (initially)
        let ck = h_arr;

        Self {
            h: h_arr,
            ck,
            enc_key: [0u8; 32],
            static_secret: secret,
            static_public: public,
            ephemeral_secret: None,
            ephemeral_public: None,
            server_ephemeral: None,
            send_key: None,
            recv_key: None,
            send_counter: 0,
            recv_counter: 0,
            handshake_complete: false,
        }
    }

    /// Get the WA prologue bytes.
    pub fn prologue() -> Vec<u8> {
        let mut p = Vec::with_capacity(4);
        p.extend_from_slice(WA_MAGIC);
        p.push(WA_VERSION_MAJOR);
        p.push(WA_VERSION_MINOR);
        p
    }

    /// Our static public key.
    pub fn static_public_key(&self) -> &X25519Public {
        &self.static_public
    }

    /// Our static private key bytes — for credential persistence only.
    pub fn static_private_key_bytes(&self) -> [u8; 32] {
        self.static_secret.to_bytes()
    }

    // ── Handshake Phase ─────────────────────────────────────────

    /// Initialize the handshake: mix in the prologue, generate ephemeral keys,
    /// and return the client hello message (our ephemeral public key, wrapped
    /// in a HandshakeMessage proto).
    pub fn init_handshake(&mut self) -> crate::Result<Vec<u8>> {
        // Mix prologue into h.
        let prologue = Self::prologue();
        self.mix_hash(&prologue);

        // Generate ephemeral key pair.
        let (eph_secret, eph_public) = crypto::generate_x25519_keypair();
        self.ephemeral_secret = Some(eph_secret);
        self.ephemeral_public = Some(eph_public);

        // Mix ephemeral public key into h.
        self.mix_hash(eph_public.as_bytes());

        debug!("Noise: sending client hello (ephemeral key)");
        Ok(eph_public.as_bytes().to_vec())
    }

    /// Process the server hello response.
    ///
    /// Returns the decrypted server payload (typically contains the server's
    /// certificate / static key).
    pub fn process_server_hello(&mut self, server_hello: &[u8]) -> crate::Result<Vec<u8>> {
        if server_hello.len() < 32 {
            return Err(crate::Error::Noise("server hello too short".into()));
        }

        // Server's ephemeral public key is the first 32 bytes.
        let mut server_eph_bytes = [0u8; 32];
        server_eph_bytes.copy_from_slice(&server_hello[..32]);
        let server_ephemeral = X25519Public::from(server_eph_bytes);

        // Mix server ephemeral into h.
        self.mix_hash(server_ephemeral.as_bytes());
        // Stash for the `se` DH in build_client_finish (Noise XX third step).
        self.server_ephemeral = Some(server_ephemeral);

        // DH: ee (our ephemeral × server ephemeral).
        let eph_secret = self
            .ephemeral_secret
            .clone()
            .ok_or_else(|| crate::Error::Noise("no ephemeral key".into()))?;
        let shared_ee = eph_secret.diffie_hellman(&server_ephemeral);
        self.mix_key(shared_ee.as_bytes());

        // Remaining bytes are: encrypted(server_static) + encrypted(payload).
        let rest = &server_hello[32..];

        // Decrypt server's static key (48 bytes: 32 key + 16 GCM tag).
        if rest.len() < 48 {
            return Err(crate::Error::Noise(
                "server hello: missing static key".into(),
            ));
        }
        let server_static_enc = &rest[..48];
        let server_static_dec = self.decrypt_and_hash(server_static_enc)?;
        let mut server_static_bytes = [0u8; 32];
        server_static_bytes.copy_from_slice(&server_static_dec);
        let server_static = X25519Public::from(server_static_bytes);

        // DH: es (our ephemeral × server static).
        let shared_es = eph_secret.diffie_hellman(&server_static);
        self.mix_key(shared_es.as_bytes());

        // Decrypt the payload.
        let payload_enc = &rest[48..];
        let payload = self.decrypt_and_hash(payload_enc)?;

        debug!(
            "Noise: server hello processed, payload len={}",
            payload.len()
        );
        Ok(payload)
    }

    /// Build the client finish message.
    ///
    /// Encrypts our static key and the given payload (ClientPayload proto)
    /// into the final handshake message.
    pub fn build_client_finish(&mut self, payload: &[u8]) -> crate::Result<Vec<u8>> {
        // Encrypt our static public key.
        let pub_bytes = self.static_public.as_bytes().to_vec();
        let static_enc = self.encrypt_and_hash(&pub_bytes)?;

        // DH: se (our static × server ephemeral) — third step of Noise XX.
        // `X25519Public` is `Copy`, so the `if let` copies it out of the Option
        // without conflicting with the `&self.static_secret` borrow that follows.
        if let Some(server_eph) = self.server_ephemeral {
            let shared_se = self.static_secret.diffie_hellman(&server_eph);
            self.mix_key(shared_se.as_bytes());
        }

        // Encrypt the payload.
        let payload_enc = self.encrypt_and_hash(payload)?;

        let mut msg = Vec::with_capacity(static_enc.len() + payload_enc.len());
        msg.extend_from_slice(&static_enc);
        msg.extend_from_slice(&payload_enc);

        // Split into transport keys.
        self.split_keys()?;
        self.handshake_complete = true;

        debug!("Noise: handshake complete, transport keys derived");
        Ok(msg)
    }

    // ── Transport Phase ─────────────────────────────────────────

    /// Encrypt a message for sending after the handshake is complete.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> crate::Result<Vec<u8>> {
        let key = self
            .send_key
            .as_ref()
            .ok_or_else(|| crate::Error::Noise("handshake not complete".into()))?;

        let nonce = self.counter_to_nonce(self.send_counter);
        self.send_counter += 1;

        crypto::aes256_gcm_encrypt(key, &nonce, plaintext, &[])
    }

    /// Decrypt a received message after the handshake is complete.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> crate::Result<Vec<u8>> {
        let key = self
            .recv_key
            .as_ref()
            .ok_or_else(|| crate::Error::Noise("handshake not complete".into()))?;

        let nonce = self.counter_to_nonce(self.recv_counter);
        self.recv_counter += 1;

        crypto::aes256_gcm_decrypt(key, &nonce, ciphertext, &[])
    }

    // ── Internal helpers ────────────────────────────────────────

    /// Mix data into the handshake hash.
    fn mix_hash(&mut self, data: &[u8]) {
        let mut hasher = Sha256::new();
        hasher.update(self.h);
        hasher.update(data);
        self.h.copy_from_slice(&hasher.finalize());
        trace!("Noise: mix_hash, new h = {:02x?}", &self.h[..8]);
    }

    /// Mix DH output into the chaining key and derive a new encryption key.
    fn mix_key(&mut self, dh_output: &[u8]) {
        let derived = crypto::hkdf_sha256(&self.ck, dh_output, &[], 64).expect("HKDF for mix_key");
        self.ck.copy_from_slice(&derived[..32]);
        self.enc_key.copy_from_slice(&derived[32..64]);
        trace!("Noise: mix_key, new ck = {:02x?}", &self.ck[..8]);
    }

    /// Encrypt data and mix the ciphertext into the hash.
    fn encrypt_and_hash(&mut self, plaintext: &[u8]) -> crate::Result<Vec<u8>> {
        let nonce = [0u8; 12]; // During handshake, nonce is zero (single-use per key).
        let ct = crypto::aes256_gcm_encrypt(&self.enc_key, &nonce, plaintext, &self.h)?;
        self.mix_hash(&ct);
        Ok(ct)
    }

    /// Decrypt data and mix the ciphertext into the hash.
    fn decrypt_and_hash(&mut self, ciphertext: &[u8]) -> crate::Result<Vec<u8>> {
        let nonce = [0u8; 12];
        let ct_copy = ciphertext.to_vec(); // Need to mix the original ciphertext.
        let pt = crypto::aes256_gcm_decrypt(&self.enc_key, &nonce, ciphertext, &self.h)?;
        self.mix_hash(&ct_copy);
        Ok(pt)
    }

    /// Split the chaining key into send and receive transport keys.
    fn split_keys(&mut self) -> crate::Result<()> {
        let derived = crypto::hkdf_sha256(&self.ck, &[], &[], 64).expect("HKDF for split");
        let mut send = [0u8; 32];
        let mut recv = [0u8; 32];
        send.copy_from_slice(&derived[..32]);
        recv.copy_from_slice(&derived[32..64]);
        self.send_key = Some(send);
        self.recv_key = Some(recv);
        self.send_counter = 0;
        self.recv_counter = 0;
        Ok(())
    }

    /// Convert a counter to a 12-byte GCM nonce (big-endian in last 4 bytes).
    fn counter_to_nonce(&self, counter: u32) -> [u8; 12] {
        let mut nonce = [0u8; 12];
        nonce[8..12].copy_from_slice(&counter.to_be_bytes());
        nonce
    }
}

impl Default for NoiseHandler {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prologue_bytes() {
        let p = NoiseHandler::prologue();
        assert_eq!(&p[..2], b"WA");
        assert_eq!(p[2], 6);
        assert_eq!(p[3], 3);
    }

    #[test]
    fn new_handler_has_keys() {
        let handler = NoiseHandler::new();
        assert!(!handler.handshake_complete);
        assert!(handler.send_key.is_none());
    }

    #[test]
    fn init_handshake_returns_32_bytes() {
        let mut handler = NoiseHandler::new();
        let hello = handler.init_handshake().unwrap();
        assert_eq!(hello.len(), 32); // X25519 public key is 32 bytes
    }

    #[test]
    fn counter_to_nonce_is_big_endian() {
        let handler = NoiseHandler::new();
        let nonce = handler.counter_to_nonce(1);
        assert_eq!(&nonce[8..12], &[0, 0, 0, 1]);
        let nonce = handler.counter_to_nonce(256);
        assert_eq!(&nonce[8..12], &[0, 0, 1, 0]);
    }

    #[test]
    fn transport_encrypt_decrypt_after_manual_split() {
        // Simulate a completed handshake by manually setting transport keys.
        let mut handler = NoiseHandler::new();
        handler.send_key = Some([0x42; 32]);
        handler.recv_key = Some([0x42; 32]);
        handler.handshake_complete = true;

        let plaintext = b"Hello WhatsApp";
        let ct = handler.encrypt(plaintext).unwrap();

        // Reset counters and swap keys to simulate the other side.
        handler.recv_counter = 0;
        handler.send_counter = 0;
        std::mem::swap(&mut handler.send_key, &mut handler.recv_key);
        let pt = handler.decrypt(&ct).unwrap();

        assert_eq!(pt, plaintext);
    }
}
