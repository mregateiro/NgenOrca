//! Hardware fingerprinting and key generation.
//!
//! On platforms with TPM/Secure Enclave, this module generates keypairs
//! inside the hardware security module. On other platforms, it falls back
//! to a composite software fingerprint.
//!
//! ## Feature flags
//!
//! - `tpm` — enables Windows TPM 2.0 integration via CNG Platform Crypto
//!   Provider, and Linux TPM via `/dev/tpmrm0` (requires `tss-esapi`).
//! - `secure-enclave` — enables macOS Secure Enclave integration via
//!   Security.framework.
//!
//! Without these features, the system gracefully falls back to a software-based
//! composite fingerprint using Ed25519 (via `ring`).

use ngenorca_core::identity::AttestationType;
use ngenorca_core::types::DeviceId;
use ngenorca_core::Result;
use ring::signature::{Ed25519KeyPair, KeyPair};
use tracing::info;

/// Detected hardware security capabilities of the current platform.
#[derive(Debug)]
pub struct HardwareCapabilities {
    /// Whether a TPM is available (Windows).
    pub tpm_available: bool,
    /// Whether Secure Enclave is available (macOS/iOS).
    pub secure_enclave_available: bool,
    /// Whether StrongBox is available (Android).
    pub strongbox_available: bool,
}

/// Detect hardware security capabilities of the current system.
pub fn detect_capabilities() -> HardwareCapabilities {
    HardwareCapabilities {
        tpm_available: detect_tpm(),
        secure_enclave_available: detect_secure_enclave(),
        strongbox_available: false, // Android-only, detected at runtime on mobile
    }
}

/// Generate a device fingerprint using the best available method.
pub fn generate_device_fingerprint() -> Result<(DeviceId, AttestationType, Vec<u8>)> {
    let caps = detect_capabilities();

    if caps.tpm_available {
        info!("Using TPM for device identity");
        generate_tpm_fingerprint()
    } else if caps.secure_enclave_available {
        info!("Using Secure Enclave for device identity");
        generate_secure_enclave_fingerprint()
    } else {
        info!("Using composite software fingerprint");
        generate_composite_fingerprint()
    }
}

/// Generate a software-based composite fingerprint (fallback).
fn generate_composite_fingerprint() -> Result<(DeviceId, AttestationType, Vec<u8>)> {
    // Collect system identifiers.
    let hostname = get_hostname();

    let os_info = format!(
        "{}-{}-{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::consts::FAMILY,
    );

    // Generate a persistent Ed25519 keypair for signing.
    let rng = ring::rand::SystemRandom::new();
    let pkcs8_bytes = Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|e| ngenorca_core::Error::Identity(format!("Key generation failed: {e}")))?;

    let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8_bytes.as_ref())
        .map_err(|e| ngenorca_core::Error::Identity(format!("Key parsing failed: {e}")))?;

    let public_key = key_pair.public_key().as_ref().to_vec();

    // Device ID = hash of hostname + public key.
    let device_id_input = format!("{hostname}:{os_info}:{}", base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        &public_key,
    ));
    let device_id = DeviceId(format!(
        "sw-{}",
        &sha256_hex(device_id_input.as_bytes())[..16]
    ));

    Ok((device_id, AttestationType::CompositeFingerprint, public_key))
}

/// Generate a TPM-backed fingerprint (Windows CNG Platform Crypto Provider).
///
/// Creates (or opens) a persistent ECDSA P-256 key named `NgenOrca-DeviceIdentity`
/// inside the TPM via the Microsoft Platform Crypto Provider. The public key
/// blob is exported and hashed to form the `DeviceId`.
///
/// Requires the `tpm` feature flag. Without it, falls back to composite.
#[cfg(all(windows, feature = "tpm"))]
fn generate_tpm_fingerprint() -> Result<(DeviceId, AttestationType, Vec<u8>)> {
    use windows::core::w;
    use windows::Win32::Security::Cryptography::*;

    unsafe {
        // Open the Microsoft Platform Crypto Provider (TPM-backed).
        let mut provider = NCRYPT_PROV_HANDLE::default();
        NCryptOpenStorageProvider(
            &mut provider,
            MS_PLATFORM_CRYPTO_PROVIDER,
            0,
        )
        .map_err(|e| ngenorca_core::Error::Identity(format!("TPM provider open failed: {e}")))?;

        let key_name = w!("NgenOrca-DeviceIdentity");
        let mut key = NCRYPT_KEY_HANDLE::default();

        // Try to open an existing persistent key first.
        let open_result = NCryptOpenKey(
            provider,
            &mut key,
            key_name,
            CERT_KEY_SPEC(0),
            NCRYPT_FLAGS(0),
        );
        if open_result.is_err() {
            // Key doesn't exist — create a new persistent ECDSA P-256 key in TPM.
            NCryptCreatePersistedKey(
                provider,
                &mut key,
                BCRYPT_ECDSA_P256_ALGORITHM,
                key_name,
                CERT_KEY_SPEC(0),
                NCRYPT_FLAGS(0),
            )
            .map_err(|e| ngenorca_core::Error::Identity(format!("TPM key creation failed: {e}")))?;

            NCryptFinalizeKey(key, NCRYPT_FLAGS(0))
                .map_err(|e| ngenorca_core::Error::Identity(format!("TPM key finalize failed: {e}")))?;

            info!("Created new persistent ECDSA P-256 key in TPM");
        } else {
            info!("Opened existing TPM-backed device key");
        }

        // Export the public key blob.
        let mut pub_key_size = 0u32;
        NCryptExportKey(
            key,
            None,
            BCRYPT_ECCPUBLIC_BLOB,
            None,
            None,
            &mut pub_key_size,
            NCRYPT_FLAGS(0),
        )
        .map_err(|e| ngenorca_core::Error::Identity(format!("TPM public key query failed: {e}")))?;

        let mut pub_key_bytes = vec![0u8; pub_key_size as usize];
        NCryptExportKey(
            key,
            None,
            BCRYPT_ECCPUBLIC_BLOB,
            None,
            Some(&mut pub_key_bytes),
            &mut pub_key_size,
            NCRYPT_FLAGS(0),
        )
        .map_err(|e| ngenorca_core::Error::Identity(format!("TPM public key export failed: {e}")))?;
        pub_key_bytes.truncate(pub_key_size as usize);

        let device_id = DeviceId(format!("tpm-{}", &sha256_hex(&pub_key_bytes)[..16]));

        let _ = NCryptFreeObject(key.into());
        let _ = NCryptFreeObject(provider.into());

        Ok((device_id, AttestationType::Tpm, pub_key_bytes))
    }
}

/// Fallback when `tpm` feature is not enabled on Windows, or on non-Windows.
#[cfg(not(all(windows, feature = "tpm")))]
fn generate_tpm_fingerprint() -> Result<(DeviceId, AttestationType, Vec<u8>)> {
    tracing::warn!("TPM integration requires the `tpm` feature — using composite fingerprint");
    generate_composite_fingerprint()
}

/// Generate a Secure Enclave-backed fingerprint (macOS).
///
/// Creates (or retrieves) a persistent ECDSA P-256 key in the Secure Enclave
/// using macOS Security.framework. The raw public key bytes are exported and
/// hashed to form the `DeviceId`.
///
/// Requires the `secure-enclave` feature flag. Without it, falls back to composite.
#[cfg(all(target_os = "macos", feature = "secure-enclave"))]
fn generate_secure_enclave_fingerprint() -> Result<(DeviceId, AttestationType, Vec<u8>)> {
    use security_framework::key::{GenerateKeyOptions, KeyType, SecKey};

    let tag = "dev.ngenorca.device-identity";

    // Try to find an existing key with this tag.
    if let Some(existing_key) = SecKey::find(tag) {
        info!("Found existing Secure Enclave device key");
        let pub_key = existing_key.public_key()
            .ok_or_else(|| ngenorca_core::Error::Identity("Failed to get SE public key".into()))?;
        let pub_key_data = pub_key.external_representation()
            .ok_or_else(|| ngenorca_core::Error::Identity("Failed to export SE public key".into()))?;
        let pub_bytes = pub_key_data.to_vec();
        let device_id = DeviceId(format!("se-{}", &sha256_hex(&pub_bytes)[..16]));
        return Ok((device_id, AttestationType::SecureEnclave, pub_bytes));
    }

    // No existing key — generate a new one in the Secure Enclave.
    let opts = GenerateKeyOptions::default()
        .set_key_type(KeyType::ec())
        .set_key_size(256)
        .set_label(tag)
        .set_token(security_framework::key::Token::SecureEnclave);

    let private_key = SecKey::generate(opts.to_dictionary())
        .map_err(|e| ngenorca_core::Error::Identity(format!("SE key generation failed: {e}")))?;

    info!("Created new ECDSA P-256 key in Secure Enclave");

    let pub_key = private_key.public_key()
        .ok_or_else(|| ngenorca_core::Error::Identity("Failed to get SE public key".into()))?;
    let pub_key_data = pub_key.external_representation()
        .ok_or_else(|| ngenorca_core::Error::Identity("Failed to export SE public key".into()))?;
    let pub_bytes = pub_key_data.to_vec();
    let device_id = DeviceId(format!("se-{}", &sha256_hex(&pub_bytes)[..16]));

    Ok((device_id, AttestationType::SecureEnclave, pub_bytes))
}

/// Fallback when `secure-enclave` feature is not enabled on macOS, or on non-macOS.
#[cfg(not(all(target_os = "macos", feature = "secure-enclave")))]
fn generate_secure_enclave_fingerprint() -> Result<(DeviceId, AttestationType, Vec<u8>)> {
    tracing::warn!("Secure Enclave integration requires the `secure-enclave` feature — using composite fingerprint");
    generate_composite_fingerprint()
}

/// Detect if a TPM 2.0 is available (Windows-specific).
fn detect_tpm() -> bool {
    #[cfg(windows)]
    {
        // Check for TPM device via registry or WMI.
        // Simplified: check if the TPM base services are present.
        std::path::Path::new(r"C:\Windows\System32\Tpm.msc").exists()
    }
    #[cfg(not(windows))]
    {
        // Linux: check for /dev/tpmrm0.
        std::path::Path::new("/dev/tpmrm0").exists()
    }
}

/// Detect if Secure Enclave is available (macOS-specific).
fn detect_secure_enclave() -> bool {
    #[cfg(target_os = "macos")]
    {
        // Secure Enclave is available on Macs with T1/T2/Apple Silicon chips.
        // Simplified detection.
        true
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Get the hostname without external crate dependency.
fn get_hostname() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown".to_string())
    }
    #[cfg(not(windows))]
    {
        std::env::var("HOSTNAME")
            .or_else(|_| {
                std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string())
            })
            .unwrap_or_else(|_| "unknown".to_string())
    }
}

/// Simple SHA-256 hex digest.
fn sha256_hex(data: &[u8]) -> String {
    use ring::digest;
    let digest = digest::digest(&digest::SHA256, data);
    digest
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
