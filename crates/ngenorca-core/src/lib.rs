//! # NgenOrca Core
//!
//! Shared types, traits, and error definitions used across all NgenOrca crates.
//! This crate contains no logic — only data structures and contracts.

pub mod error;
pub mod event;
pub mod identity;
pub mod message;
pub mod orchestration;
pub mod plugin;
pub mod session;
pub mod types;

pub use error::{Error, Result};
pub use types::*;
