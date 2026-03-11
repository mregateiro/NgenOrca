//! Pure-Rust WhatsApp Web multi-device client.
//!
//! Implements the WhatsApp Web protocol natively — no external runtime
//! dependencies required.  Connects via persistent WebSocket and uses the
//! Noise Protocol (XX pattern) for transport encryption plus the Signal
//! Protocol (Double Ratchet) for end-to-end message encryption.
//!
//! ## Architecture
//!
//! ```text
//!  ┌──────────────────────────────────────────────────┐
//!  │                 WhatsAppClient                   │
//!  │  (high-level API: connect, send, receive, auth)  │
//!  └──────┬────────────┬───────────────┬──────────────┘
//!         │            │               │
//!    ┌────▼────┐  ┌────▼────┐   ┌──────▼──────┐
//!    │  Auth   │  │ Signal  │   │  Transport  │
//!    │  (QR +  │  │  (E2E   │   │  (WebSocket │
//!    │  pair)  │  │  crypto)│   │  + Noise)   │
//!    └────┬────┘  └────┬────┘   └──────┬──────┘
//!         │            │               │
//!    ┌────▼────┐  ┌────▼────┐   ┌──────▼──────┐
//!    │  Store  │  │  Proto  │   │   Binary    │
//!    │  (cred  │  │  (proto │   │  (WABinary  │
//!    │  persist│  │  bufs)  │   │   codec)    │
//!    └─────────┘  └─────────┘   └─────────────┘
//! ```
//!
//! ## Protocol Overview
//!
//! 1. Open WebSocket to `web.whatsapp.com/ws/chat`
//! 2. Noise XX handshake (X25519 + AES-256-GCM + SHA-256)
//! 3. If new device: QR code pairing (scan with phone)
//! 4. If returning: resume session with stored credentials
//! 5. Messages arrive as WABinary nodes → Signal-decrypt → protobuf → text
//! 6. Outgoing text → protobuf → Signal-encrypt → WABinary → send

pub mod auth;
pub mod binary;
pub mod client;
pub mod crypto;
pub mod noise;
pub mod proto;
pub mod signal;
pub mod store;
pub mod transport;

// Re-export the main public types.
pub use client::{WhatsAppClient, WhatsAppEvent};
pub use store::FileStore;

/// Errors produced by the WhatsApp Web client.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("WebSocket error: {0}")]
    WebSocket(String),

    #[error("Noise handshake failed: {0}")]
    Noise(String),

    #[error("Authentication failed: {0}")]
    Auth(String),

    #[error("Signal Protocol error: {0}")]
    Signal(String),

    #[error("Binary codec error: {0}")]
    Binary(String),

    #[error("Protobuf error: {0}")]
    Proto(String),

    #[error("Store error: {0}")]
    Store(String),

    #[error("Connection closed")]
    Closed,

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
