//! Filesystem-based persistence for WhatsApp credentials and Signal sessions.
//!
//! Stores:
//! - **Authentication credentials** (noise keys, identity, JID, device ID)
//! - **Signal sessions** (per-contact ratchet state)
//! - **Signal keys** (identity, signed pre-key, pre-keys)
//!
//! All data is stored as JSON files inside a configurable data directory.

use crate::auth::Credentials;
use crate::signal::{Session, SignalKeys};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs;
use tracing::{debug, info, warn};

/// Filesystem-backed store for WhatsApp state.
#[derive(Clone)]
pub struct FileStore {
    /// Root directory for all stored data.
    data_dir: PathBuf,
}

impl FileStore {
    /// Create a new store backed by the given directory.
    ///
    /// The directory is created if it doesn't exist.
    pub async fn new(data_dir: impl Into<PathBuf>) -> crate::Result<Self> {
        let data_dir = data_dir.into();
        fs::create_dir_all(&data_dir).await.map_err(|e| {
            crate::Error::Store(format!("cannot create data dir {}: {e}", data_dir.display()))
        })?;
        debug!(path = %data_dir.display(), "FileStore initialized");
        Ok(Self { data_dir })
    }

    // ─── Credentials ────────────────────────────────────────────

    fn creds_path(&self) -> PathBuf {
        self.data_dir.join("credentials.json")
    }

    /// Load saved credentials, if any.
    pub async fn load_credentials(&self) -> crate::Result<Option<Credentials>> {
        let path = self.creds_path();
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path)
            .await
            .map_err(|e| crate::Error::Store(format!("read credentials: {e}")))?;
        let creds: Credentials = serde_json::from_str(&data)
            .map_err(|e| crate::Error::Store(format!("parse credentials: {e}")))?;
        info!(jid = %creds.jid, "Loaded saved credentials");
        Ok(Some(creds))
    }

    /// Persist credentials to disk.
    pub async fn save_credentials(&self, creds: &Credentials) -> crate::Result<()> {
        let json = serde_json::to_string_pretty(creds)
            .map_err(|e| crate::Error::Store(format!("serialize credentials: {e}")))?;
        fs::write(self.creds_path(), json)
            .await
            .map_err(|e| crate::Error::Store(format!("write credentials: {e}")))?;
        debug!("Credentials saved");
        Ok(())
    }

    /// Delete credentials (e.g., after logout).
    pub async fn clear_credentials(&self) -> crate::Result<()> {
        let path = self.creds_path();
        if path.exists() {
            fs::remove_file(&path)
                .await
                .map_err(|e| crate::Error::Store(format!("remove credentials: {e}")))?;
        }
        info!("Credentials cleared");
        Ok(())
    }

    // ─── Signal Keys ────────────────────────────────────────────

    fn signal_keys_path(&self) -> PathBuf {
        self.data_dir.join("signal_keys.json")
    }

    /// Load or generate Signal keys.
    ///
    /// If keys exist on disk, they are loaded.  Otherwise, a fresh set is
    /// generated and saved.
    pub async fn load_or_generate_signal_keys(&self) -> crate::Result<SignalKeys> {
        let path = self.signal_keys_path();
        if path.exists() {
            let data = fs::read_to_string(&path)
                .await
                .map_err(|e| crate::Error::Store(format!("read signal keys: {e}")))?;
            let keys: SignalKeys = serde_json::from_str(&data)
                .map_err(|e| crate::Error::Store(format!("parse signal keys: {e}")))?;
            debug!("Loaded existing Signal keys");
            return Ok(keys);
        }

        // Generate new keys.
        let keys = SignalKeys::generate();
        self.save_signal_keys(&keys).await?;
        info!("Generated new Signal keys");
        Ok(keys)
    }

    /// Save Signal keys.
    pub async fn save_signal_keys(&self, keys: &SignalKeys) -> crate::Result<()> {
        let json = serde_json::to_string_pretty(keys)
            .map_err(|e| crate::Error::Store(format!("serialize signal keys: {e}")))?;
        fs::write(self.signal_keys_path(), json)
            .await
            .map_err(|e| crate::Error::Store(format!("write signal keys: {e}")))?;
        Ok(())
    }

    // ─── Signal Sessions ────────────────────────────────────────

    fn sessions_dir(&self) -> PathBuf {
        self.data_dir.join("sessions")
    }

    fn session_path(&self, jid: &str) -> PathBuf {
        // Sanitize JID for filename.
        let safe_name: String = jid
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
            .collect();
        self.sessions_dir().join(format!("{safe_name}.json"))
    }

    /// Load a Signal session for a specific JID.
    pub async fn load_session(&self, jid: &str) -> crate::Result<Option<Session>> {
        let path = self.session_path(jid);
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path)
            .await
            .map_err(|e| crate::Error::Store(format!("read session {jid}: {e}")))?;
        let session: Session = serde_json::from_str(&data)
            .map_err(|e| crate::Error::Store(format!("parse session {jid}: {e}")))?;
        Ok(Some(session))
    }

    /// Save a Signal session.
    pub async fn save_session(&self, jid: &str, session: &Session) -> crate::Result<()> {
        let dir = self.sessions_dir();
        fs::create_dir_all(&dir)
            .await
            .map_err(|e| crate::Error::Store(format!("create sessions dir: {e}")))?;

        let json = serde_json::to_string_pretty(session)
            .map_err(|e| crate::Error::Store(format!("serialize session: {e}")))?;
        fs::write(self.session_path(jid), json)
            .await
            .map_err(|e| crate::Error::Store(format!("write session {jid}: {e}")))?;
        Ok(())
    }

    /// Delete a specific session.
    pub async fn delete_session(&self, jid: &str) -> crate::Result<()> {
        let path = self.session_path(jid);
        if path.exists() {
            fs::remove_file(&path)
                .await
                .map_err(|e| crate::Error::Store(format!("remove session {jid}: {e}")))?;
        }
        Ok(())
    }

    /// List all stored session JIDs.
    pub async fn list_sessions(&self) -> crate::Result<Vec<String>> {
        let dir = self.sessions_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut entries = fs::read_dir(&dir)
            .await
            .map_err(|e| crate::Error::Store(format!("read sessions dir: {e}")))?;
        let mut jids = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| crate::Error::Store(format!("iterate sessions: {e}")))?
        {
            if let Some(name) = entry.file_name().to_str()
                && let Some(jid) = name.strip_suffix(".json")
            {
                jids.push(jid.replace('_', ".").replace("_..", "@"));
            }
        }
        Ok(jids)
    }

    // ─── App State (generic key-value) ──────────────────────────

    fn state_path(&self) -> PathBuf {
        self.data_dir.join("state.json")
    }

    /// Load arbitrary app state.
    pub async fn load_state(&self) -> crate::Result<HashMap<String, serde_json::Value>> {
        let path = self.state_path();
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let data = fs::read_to_string(&path)
            .await
            .map_err(|e| crate::Error::Store(format!("read state: {e}")))?;
        let state: HashMap<String, serde_json::Value> = serde_json::from_str(&data)
            .map_err(|e| crate::Error::Store(format!("parse state: {e}")))?;
        Ok(state)
    }

    /// Save arbitrary app state.
    pub async fn save_state(
        &self,
        state: &HashMap<String, serde_json::Value>,
    ) -> crate::Result<()> {
        let json = serde_json::to_string_pretty(state)
            .map_err(|e| crate::Error::Store(format!("serialize state: {e}")))?;
        fs::write(self.state_path(), json)
            .await
            .map_err(|e| crate::Error::Store(format!("write state: {e}")))?;
        Ok(())
    }

    /// Wipe all stored data (factory reset).
    pub async fn wipe(&self) -> crate::Result<()> {
        if self.data_dir.exists() {
            fs::remove_dir_all(&self.data_dir)
                .await
                .map_err(|e| crate::Error::Store(format!("wipe data dir: {e}")))?;
            fs::create_dir_all(&self.data_dir)
                .await
                .map_err(|e| crate::Error::Store(format!("recreate data dir: {e}")))?;
        }
        warn!("All stored data wiped");
        Ok(())
    }
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn store_create_dir() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("new_sub");
        let store = FileStore::new(&sub).await.unwrap();
        assert!(sub.exists());
        drop(store);
    }

    #[tokio::test]
    async fn credentials_roundtrip() {
        let dir = tempdir().unwrap();
        let store = FileStore::new(dir.path()).await.unwrap();

        // Initially none.
        assert!(store.load_credentials().await.unwrap().is_none());

        // Save.
        let creds = Credentials {
            jid: "1234567890@s.whatsapp.net".into(),
            device_id: 1,
            client_id: vec![0u8; 16],
            noise_private: vec![1u8; 32],
            noise_public: vec![2u8; 32],
            signal_keys: crate::signal::SignalKeys::generate(),
            adv_secret: vec![3u8; 32],
            paired_at: 1700000000,
        };
        store.save_credentials(&creds).await.unwrap();

        // Load.
        let loaded = store.load_credentials().await.unwrap().unwrap();
        assert_eq!(loaded.jid, "1234567890@s.whatsapp.net");
        assert_eq!(loaded.device_id, 1);

        // Clear.
        store.clear_credentials().await.unwrap();
        assert!(store.load_credentials().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn signal_keys_generate_and_load() {
        let dir = tempdir().unwrap();
        let store = FileStore::new(dir.path()).await.unwrap();

        let keys = store.load_or_generate_signal_keys().await.unwrap();
        assert!(keys.registration_id > 0);
        assert_eq!(keys.prekeys.len(), 100);

        // Loading again should return the same keys.
        let keys2 = store.load_or_generate_signal_keys().await.unwrap();
        assert_eq!(keys.registration_id, keys2.registration_id);
        assert_eq!(keys.identity.public, keys2.identity.public);
    }

    #[tokio::test]
    async fn session_roundtrip() {
        let dir = tempdir().unwrap();
        let store = FileStore::new(dir.path()).await.unwrap();

        let jid = "9876543210@s.whatsapp.net";

        // No session initially.
        assert!(store.load_session(jid).await.unwrap().is_none());

        // Save a dummy session.
        let session = crate::signal::Session {
            remote_identity: vec![0u8; 32],
            ratchet_private: vec![1u8; 32],
            remote_ratchet_public: vec![2u8; 32],
            root_key: vec![3u8; 32],
            send_chain_key: vec![4u8; 32],
            recv_chain_key: vec![5u8; 32],
            send_counter: 10,
            recv_counter: 5,
            prev_send_counter: 0,
        };
        store.save_session(jid, &session).await.unwrap();

        // Load it back.
        let loaded = store.load_session(jid).await.unwrap().unwrap();
        assert_eq!(loaded.send_counter, 10);
        assert_eq!(loaded.recv_counter, 5);

        // Delete.
        store.delete_session(jid).await.unwrap();
        assert!(store.load_session(jid).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn state_roundtrip() {
        let dir = tempdir().unwrap();
        let store = FileStore::new(dir.path()).await.unwrap();

        let state = store.load_state().await.unwrap();
        assert!(state.is_empty());

        let mut state = HashMap::new();
        state.insert("last_seen".into(), serde_json::json!(1700000000));
        store.save_state(&state).await.unwrap();

        let loaded = store.load_state().await.unwrap();
        assert_eq!(loaded["last_seen"], 1700000000);
    }

    #[tokio::test]
    async fn wipe_clears_everything() {
        let dir = tempdir().unwrap();
        let store = FileStore::new(dir.path()).await.unwrap();

        // Create some data.
        let _keys = store.load_or_generate_signal_keys().await.unwrap();
        assert!(dir.path().join("signal_keys.json").exists());

        // Wipe.
        store.wipe().await.unwrap();
        assert!(!dir.path().join("signal_keys.json").exists());
        assert!(dir.path().exists()); // Directory itself still exists.
    }
}
