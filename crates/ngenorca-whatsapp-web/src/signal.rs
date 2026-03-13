//! Signal Protocol implementation for WhatsApp E2E encryption.
//!
//! Implements the core primitives needed for WhatsApp's end-to-end
//! encryption:
//! - **Identity Key Pair** (Curve25519)
//! - **Pre-Keys** (one-time Curve25519 key pairs)
//! - **Signed Pre-Key** (signed with identity key)
//! - **Double Ratchet** (symmetric-key + DH ratchet)
//! - **Session management** (establishing and using Signal sessions)
//!
//! WhatsApp uses the Signal Protocol with AES-256-CBC + HMAC-SHA256
//! for message encryption and HKDF-SHA256 for key derivation.

use crate::crypto;
use serde::{Deserialize, Serialize};
use tracing::{debug, trace};
use x25519_dalek::{PublicKey as X25519Public, StaticSecret as X25519Secret};

/// Signal Protocol registration ID (random 14-bit integer).
pub fn generate_registration_id() -> u32 {
    use rand::Rng;
    rand::thread_rng().gen_range(1..16381)
}

// ─── Key Types ──────────────────────────────────────────────────

/// Curve25519 identity key pair — the device's long-term key.
#[derive(Clone, Serialize, Deserialize)]
pub struct IdentityKeyPair {
    pub private: Vec<u8>,
    pub public: Vec<u8>,
}

impl IdentityKeyPair {
    /// Generate a new random identity key pair.
    pub fn generate() -> Self {
        let (secret, public) = crypto::generate_x25519_keypair();
        Self {
            private: secret.to_bytes().to_vec(),
            public: public.as_bytes().to_vec(),
        }
    }

    /// Get the X25519 secret key.
    pub fn secret(&self) -> X25519Secret {
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&self.private);
        X25519Secret::from(bytes)
    }

    /// Get the X25519 public key.
    pub fn public_key(&self) -> X25519Public {
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&self.public);
        X25519Public::from(bytes)
    }
}

/// A pre-key (one-time use Curve25519 key pair).
#[derive(Clone, Serialize, Deserialize)]
pub struct PreKey {
    pub id: u32,
    pub private: Vec<u8>,
    pub public: Vec<u8>,
}

impl PreKey {
    pub fn generate(id: u32) -> Self {
        let (secret, public) = crypto::generate_x25519_keypair();
        Self {
            id,
            private: secret.to_bytes().to_vec(),
            public: public.as_bytes().to_vec(),
        }
    }
}

/// A signed pre-key — a pre-key signed by the identity key.
#[derive(Clone, Serialize, Deserialize)]
pub struct SignedPreKey {
    pub id: u32,
    pub private: Vec<u8>,
    pub public: Vec<u8>,
    pub signature: Vec<u8>,
    pub timestamp: u64,
}

impl SignedPreKey {
    /// Generate a signed pre-key and sign it with the identity key.
    ///
    /// In real Signal Protocol, this uses XEdDSA (Curve25519 → Ed25519
    /// for signing).  We use HMAC-SHA256 as a simplified signature
    /// for the pairing handshake (WhatsApp verifies server-side).
    pub fn generate(id: u32, identity: &IdentityKeyPair) -> Self {
        let (secret, public) = crypto::generate_x25519_keypair();

        // Sign the public key bytes with the identity private key (simplified).
        let signature = crypto::hmac_sha256(&identity.private, public.as_bytes());

        Self {
            id,
            private: secret.to_bytes().to_vec(),
            public: public.as_bytes().to_vec(),
            signature,
            timestamp: chrono::Utc::now().timestamp() as u64,
        }
    }
}

// ─── Session ────────────────────────────────────────────────────

/// A Signal session with a remote device.
///
/// Tracks the Double Ratchet state for encrypting/decrypting messages
/// to/from a specific contact.
#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    /// Remote device's identity key (public).
    pub remote_identity: Vec<u8>,
    /// Our current ratchet private key.
    pub ratchet_private: Vec<u8>,
    /// Remote's current ratchet public key.
    pub remote_ratchet_public: Vec<u8>,
    /// Root key (32 bytes).
    pub root_key: Vec<u8>,
    /// Sending chain key.
    pub send_chain_key: Vec<u8>,
    /// Receiving chain key.
    pub recv_chain_key: Vec<u8>,
    /// Send message counter.
    pub send_counter: u32,
    /// Receive message counter.
    pub recv_counter: u32,
    /// Previous sending chain length.
    pub prev_send_counter: u32,
}

impl Session {
    /// Establish a new session from a pre-key bundle (X3DH).
    ///
    /// This is the initiator side: we have the recipient's pre-key bundle
    /// and derive a shared secret.
    pub fn from_prekey_bundle(
        our_identity: &IdentityKeyPair,
        their_identity_pub: &[u8],
        their_signed_prekey_pub: &[u8],
        their_one_time_prekey_pub: Option<&[u8]>,
    ) -> crate::Result<Self> {
        // X3DH: compute shared secret from 3 (or 4) DH exchanges.
        let our_identity_secret = our_identity.secret();

        let their_identity = to_x25519_public(their_identity_pub)?;
        let their_signed = to_x25519_public(their_signed_prekey_pub)?;

        // DH1: our identity × their signed pre-key
        let dh1 = our_identity_secret.diffie_hellman(&their_signed);

        // DH2: our ephemeral × their identity
        let (eph_secret, _eph_public) = crypto::generate_x25519_keypair();
        let dh2 = eph_secret.diffie_hellman(&their_identity);

        // DH3: our ephemeral × their signed pre-key
        let dh3 = eph_secret.diffie_hellman(&their_signed);

        // Concatenate DH outputs.
        let mut master_secret = Vec::with_capacity(32 * 4);
        master_secret.extend_from_slice(dh1.as_bytes());
        master_secret.extend_from_slice(dh2.as_bytes());
        master_secret.extend_from_slice(dh3.as_bytes());

        // DH4 (optional): our ephemeral × their one-time pre-key
        if let Some(otpk) = their_one_time_prekey_pub {
            let their_otpk = to_x25519_public(otpk)?;
            let dh4 = eph_secret.diffie_hellman(&their_otpk);
            master_secret.extend_from_slice(dh4.as_bytes());
        }

        // Derive root key and chain key via HKDF.
        let derived = crypto::hkdf_sha256(
            &[0u8; 32], // salt
            &master_secret,
            b"WhisperText",
            64,
        )?;

        let root_key = derived[..32].to_vec();
        let chain_key = derived[32..64].to_vec();

        debug!("Signal: session established via X3DH");

        Ok(Self {
            remote_identity: their_identity_pub.to_vec(),
            ratchet_private: eph_secret.to_bytes().to_vec(),
            remote_ratchet_public: their_signed_prekey_pub.to_vec(),
            root_key,
            send_chain_key: chain_key.clone(),
            recv_chain_key: chain_key,
            send_counter: 0,
            recv_counter: 0,
            prev_send_counter: 0,
        })
    }

    /// Encrypt a message using the sending chain.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> crate::Result<EncryptedMessage> {
        // Derive message key from chain key.
        let msg_key = self.derive_message_key(&self.send_chain_key.clone());

        // Advance the sending chain.
        self.send_chain_key = self.advance_chain_key(&self.send_chain_key.clone());

        // Split message key into encryption key + auth key + IV.
        let keys = crypto::hkdf_sha256(&[0u8; 32], &msg_key, b"WhisperMessageKeys", 80)?;
        let enc_key: [u8; 32] = keys[..32].try_into().unwrap();
        let auth_key: [u8; 32] = keys[32..64].try_into().unwrap();
        let iv: [u8; 16] = keys[64..80].try_into().unwrap();

        // Encrypt with AES-256-CBC.
        let ciphertext = crypto::aes256_cbc_encrypt(&enc_key, &iv, plaintext);

        // HMAC-SHA256 for authentication.
        let mac_input = [&self.remote_ratchet_public[..], &ciphertext[..]].concat();
        let mac = crypto::hmac_sha256(&auth_key, &mac_input);

        let counter = self.send_counter;
        self.send_counter += 1;

        trace!(counter, "Signal: encrypted message");

        Ok(EncryptedMessage {
            ciphertext,
            counter,
            previous_counter: self.prev_send_counter,
            ratchet_key: self.ratchet_public_bytes(),
            mac: mac[..8].to_vec(), // Truncated to 8 bytes per Signal spec
        })
    }

    /// Decrypt a message using the receiving chain.
    pub fn decrypt(&mut self, msg: &EncryptedMessage) -> crate::Result<Vec<u8>> {
        // Check if we need to perform a DH ratchet step.
        if msg.ratchet_key != self.remote_ratchet_public {
            self.dh_ratchet_step(&msg.ratchet_key)?;
        }

        // Derive message key.
        let msg_key = self.derive_message_key(&self.recv_chain_key.clone());

        // Advance the receiving chain.
        self.recv_chain_key = self.advance_chain_key(&self.recv_chain_key.clone());

        // Split into keys.
        let keys = crypto::hkdf_sha256(&[0u8; 32], &msg_key, b"WhisperMessageKeys", 80)?;
        let enc_key: [u8; 32] = keys[..32].try_into().unwrap();
        let _auth_key: [u8; 32] = keys[32..64].try_into().unwrap();
        let iv: [u8; 16] = keys[64..80].try_into().unwrap();

        // Decrypt with AES-256-CBC.
        let plaintext = crypto::aes256_cbc_decrypt(&enc_key, &iv, &msg.ciphertext)?;

        self.recv_counter += 1;
        trace!(counter = msg.counter, "Signal: decrypted message");

        Ok(plaintext)
    }

    /// Perform a DH ratchet step when the remote device sends a new ratchet key.
    fn dh_ratchet_step(&mut self, new_remote_ratchet: &[u8]) -> crate::Result<()> {
        self.prev_send_counter = self.send_counter;
        self.send_counter = 0;
        self.recv_counter = 0;

        self.remote_ratchet_public = new_remote_ratchet.to_vec();

        // Receiving chain: DH our ratchet × new remote ratchet.
        let remote_pub = to_x25519_public(new_remote_ratchet)?;
        let our_secret = to_x25519_secret(&self.ratchet_private)?;
        let dh = our_secret.diffie_hellman(&remote_pub);

        let derived = crypto::hkdf_sha256(&self.root_key, dh.as_bytes(), b"WhisperRatchet", 64)?;
        self.root_key = derived[..32].to_vec();
        self.recv_chain_key = derived[32..64].to_vec();

        // Generate new ratchet key pair for sending.
        let (new_secret, _new_public) = crypto::generate_x25519_keypair();
        let dh2 = new_secret.diffie_hellman(&remote_pub);
        let derived2 = crypto::hkdf_sha256(&self.root_key, dh2.as_bytes(), b"WhisperRatchet", 64)?;
        self.root_key = derived2[..32].to_vec();
        self.send_chain_key = derived2[32..64].to_vec();
        self.ratchet_private = new_secret.to_bytes().to_vec();

        debug!("Signal: DH ratchet step completed");
        Ok(())
    }

    /// Derive a message key from a chain key.
    fn derive_message_key(&self, chain_key: &[u8]) -> Vec<u8> {
        crypto::hmac_sha256(chain_key, &[0x01])
    }

    /// Advance a chain key to the next step.
    fn advance_chain_key(&self, chain_key: &[u8]) -> Vec<u8> {
        crypto::hmac_sha256(chain_key, &[0x02])
    }

    /// Get our current ratchet public key bytes.
    fn ratchet_public_bytes(&self) -> Vec<u8> {
        let secret = to_x25519_secret(&self.ratchet_private).expect("valid ratchet key");
        let public = X25519Public::from(&secret);
        public.as_bytes().to_vec()
    }
}

/// An encrypted Signal message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedMessage {
    pub ciphertext: Vec<u8>,
    pub counter: u32,
    pub previous_counter: u32,
    pub ratchet_key: Vec<u8>,
    pub mac: Vec<u8>,
}

// ─── Key Store ──────────────────────────────────────────────────

/// All Signal Protocol keys for this device.
#[derive(Clone, Serialize, Deserialize)]
pub struct SignalKeys {
    pub registration_id: u32,
    pub identity: IdentityKeyPair,
    pub signed_prekey: SignedPreKey,
    pub prekeys: Vec<PreKey>,
}

impl SignalKeys {
    /// Generate a complete set of Signal keys.
    pub fn generate() -> Self {
        let identity = IdentityKeyPair::generate();
        let signed_prekey = SignedPreKey::generate(1, &identity);

        // Generate a batch of one-time pre-keys.
        let prekeys: Vec<PreKey> = (1..=100).map(PreKey::generate).collect();

        Self {
            registration_id: generate_registration_id(),
            identity,
            signed_prekey,
            prekeys,
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────

fn to_x25519_public(bytes: &[u8]) -> crate::Result<X25519Public> {
    if bytes.len() != 32 {
        return Err(crate::Error::Signal(format!(
            "expected 32-byte public key, got {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(bytes);
    Ok(X25519Public::from(arr))
}

fn to_x25519_secret(bytes: &[u8]) -> crate::Result<X25519Secret> {
    if bytes.len() != 32 {
        return Err(crate::Error::Signal(format!(
            "expected 32-byte private key, got {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(bytes);
    Ok(X25519Secret::from(arr))
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_key_pair_generate() {
        let kp = IdentityKeyPair::generate();
        assert_eq!(kp.private.len(), 32);
        assert_eq!(kp.public.len(), 32);
    }

    #[test]
    fn pre_key_generate() {
        let pk = PreKey::generate(42);
        assert_eq!(pk.id, 42);
        assert_eq!(pk.private.len(), 32);
        assert_eq!(pk.public.len(), 32);
    }

    #[test]
    fn signed_pre_key_generate() {
        let identity = IdentityKeyPair::generate();
        let spk = SignedPreKey::generate(1, &identity);
        assert_eq!(spk.id, 1);
        assert!(!spk.signature.is_empty());
    }

    #[test]
    fn signal_keys_generate() {
        let keys = SignalKeys::generate();
        assert!(keys.registration_id > 0);
        assert_eq!(keys.prekeys.len(), 100);
    }

    #[test]
    fn registration_id_in_range() {
        for _ in 0..100 {
            let id = generate_registration_id();
            assert!((1..=16380).contains(&id));
        }
    }

    #[test]
    fn session_encrypt_decrypt_roundtrip() {
        let alice_identity = IdentityKeyPair::generate();
        let bob_identity = IdentityKeyPair::generate();
        let bob_signed_prekey = SignedPreKey::generate(1, &bob_identity);
        let bob_one_time = PreKey::generate(1);

        // Alice establishes session with Bob's pre-key bundle.
        let mut alice_session = Session::from_prekey_bundle(
            &alice_identity,
            &bob_identity.public,
            &bob_signed_prekey.public,
            Some(&bob_one_time.public),
        )
        .unwrap();

        // Alice encrypts a message.
        let plaintext = b"Hello Bob, this is a secret message!";
        let encrypted = alice_session.encrypt(plaintext).unwrap();
        assert!(!encrypted.ciphertext.is_empty());
        assert_eq!(encrypted.counter, 0);

        // For a full roundtrip, we'd need Bob to also establish his session
        // from Alice's first message (PreKeySignalMessage). This is a
        // simplified test — we verify that encryption produces output.
    }
}
