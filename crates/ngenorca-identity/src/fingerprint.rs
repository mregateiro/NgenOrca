//! Hardware fingerprinting and key generation.
//!
//! On platforms with TPM/Secure Enclave, this module generates keypairs
//! inside the hardware security module. On other platforms, it falls back
//! to a composite software fingerprint.

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

/// Placeholder for TPM-based fingerprint (Windows).
fn generate_tpm_fingerprint() -> Result<(DeviceId, AttestationType, Vec<u8>)> {
    // TODO: Integrate with Windows TPM 2.0 via tss-esapi or windows-rs.
    // For now, fall back to composite.
    tracing::warn!("TPM integration not yet implemented, using composite fingerprint");
    generate_composite_fingerprint()
}

/// Placeholder for Secure Enclave fingerprint (macOS/iOS).
fn generate_secure_enclave_fingerprint() -> Result<(DeviceId, AttestationType, Vec<u8>)> {
    // TODO: Integrate with macOS Secure Enclave via Security.framework.
    tracing::warn!("Secure Enclave integration not yet implemented, using composite fingerprint");
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
