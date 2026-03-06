//! Core error types for NgenOrca.

use thiserror::Error;

/// Top-level error type used across all NgenOrca crates.
#[derive(Error, Debug)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Identity error: {0}")]
    Identity(String),

    #[error("Memory error: {0}")]
    Memory(String),

    #[error("Plugin error: {0}")]
    Plugin(String),

    #[error("Bus error: {0}")]
    Bus(String),

    #[error("Gateway error: {0}")]
    Gateway(String),

    #[error("Sandbox error: {0}")]
    Sandbox(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Channel closed")]
    ChannelClosed,

    #[error("Timeout after {0:?}")]
    Timeout(std::time::Duration),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("{0}")]
    Other(String),
}

/// Convenience Result alias.
pub type Result<T> = std::result::Result<T, Error>;

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Serialization(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_config() {
        let e = Error::Config("bad value".into());
        assert_eq!(e.to_string(), "Configuration error: bad value");
    }

    #[test]
    fn error_display_identity() {
        let e = Error::Identity("no key".into());
        assert_eq!(e.to_string(), "Identity error: no key");
    }

    #[test]
    fn error_display_memory() {
        let e = Error::Memory("OOM".into());
        assert_eq!(e.to_string(), "Memory error: OOM");
    }

    #[test]
    fn error_display_channel_closed() {
        let e = Error::ChannelClosed;
        assert_eq!(e.to_string(), "Channel closed");
    }

    #[test]
    fn error_display_timeout() {
        let e = Error::Timeout(std::time::Duration::from_secs(5));
        assert_eq!(e.to_string(), "Timeout after 5s");
    }

    #[test]
    fn error_display_unauthorized() {
        let e = Error::Unauthorized("bad token".into());
        assert_eq!(e.to_string(), "Unauthorized: bad token");
    }

    #[test]
    fn error_display_not_found() {
        let e = Error::NotFound("user 42".into());
        assert_eq!(e.to_string(), "Not found: user 42");
    }

    #[test]
    fn error_display_other() {
        let e = Error::Other("something".into());
        assert_eq!(e.to_string(), "something");
    }

    #[test]
    fn error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file gone");
        let e: Error = io_err.into();
        assert!(e.to_string().contains("file gone"));
    }

    #[test]
    fn error_from_serde_json() {
        let json_err = serde_json::from_str::<String>("not json!").unwrap_err();
        let e: Error = json_err.into();
        assert!(matches!(e, Error::Serialization(_)));
    }

    #[test]
    fn result_type_alias_works() {
        fn returns_ok() -> Result<i32> {
            Ok(42)
        }
        fn returns_err() -> Result<i32> {
            Err(Error::Other("oops".into()))
        }
        assert_eq!(returns_ok().unwrap(), 42);
        assert!(returns_err().is_err());
    }
}
