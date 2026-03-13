//! SQLite-backed identity store.

use ngenorca_core::identity::{
    AttestationType, ChannelBinding, DeviceBinding, UserIdentity, UserRole,
};
use ngenorca_core::types::{ChannelId, ChannelKind, DeviceId, TrustLevel, UserId};
use ngenorca_core::{Error, Result};
use rusqlite::{Connection, params};
use std::sync::Mutex;

pub struct IdentityStore {
    conn: Mutex<Connection>,
}

impl IdentityStore {
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path).map_err(|e| Error::Database(e.to_string()))?;

        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| Error::Database(e.to_string()))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS users (
                user_id      TEXT PRIMARY KEY,
                display_name TEXT NOT NULL,
                role         TEXT NOT NULL,
                created_at   TEXT NOT NULL,
                last_seen    TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS device_bindings (
                device_id       TEXT PRIMARY KEY,
                user_id         TEXT NOT NULL REFERENCES users(user_id),
                device_name     TEXT NOT NULL,
                attestation     TEXT NOT NULL,
                public_key_hash TEXT NOT NULL,
                trust           TEXT NOT NULL,
                paired_at       TEXT NOT NULL,
                last_used       TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS channel_bindings (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id      TEXT NOT NULL REFERENCES users(user_id),
                channel_id   TEXT NOT NULL,
                channel_kind TEXT NOT NULL,
                handle       TEXT NOT NULL,
                trust        TEXT NOT NULL,
                linked_at    TEXT NOT NULL,
                UNIQUE(channel_kind, handle)
            );

            CREATE INDEX IF NOT EXISTS idx_device_user ON device_bindings(user_id);
            CREATE INDEX IF NOT EXISTS idx_channel_user ON channel_bindings(user_id);
            CREATE INDEX IF NOT EXISTS idx_channel_lookup ON channel_bindings(channel_kind, handle);",
        )
        .map_err(|e| Error::Database(e.to_string()))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn save_user(&self, identity: &UserIdentity) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO users (user_id, display_name, role, created_at, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                identity.user_id.0,
                identity.display_name,
                serde_json::to_string(&identity.role).unwrap(),
                identity.created_at.to_rfc3339(),
                identity.last_seen.to_rfc3339(),
            ],
        )
        .map_err(|e| Error::Database(e.to_string()))?;
        Ok(())
    }

    pub fn get_user(&self, user_id: &UserId) -> Result<Option<UserIdentity>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;

        let mut stmt = conn
            .prepare(
                "SELECT display_name, role, created_at, last_seen FROM users WHERE user_id = ?1",
            )
            .map_err(|e| Error::Database(e.to_string()))?;

        let result = stmt
            .query_row(params![user_id.0], |row| {
                let display_name: String = row.get(0)?;
                let role_str: String = row.get(1)?;
                let created_at: String = row.get(2)?;
                let last_seen: String = row.get(3)?;
                Ok((display_name, role_str, created_at, last_seen))
            })
            .optional()
            .map_err(|e| Error::Database(e.to_string()))?;

        match result {
            Some((display_name, role_str, created_at, last_seen)) => {
                let devices = self.get_devices_for_user_inner(&conn, user_id)?;
                let channels = self.get_channels_for_user_inner(&conn, user_id)?;

                Ok(Some(UserIdentity {
                    user_id: user_id.clone(),
                    display_name,
                    role: serde_json::from_str(&role_str).unwrap_or(UserRole::Guest),
                    devices,
                    channels,
                    created_at: chrono::DateTime::parse_from_rfc3339(&created_at)
                        .unwrap_or_default()
                        .with_timezone(&chrono::Utc),
                    last_seen: chrono::DateTime::parse_from_rfc3339(&last_seen)
                        .unwrap_or_default()
                        .with_timezone(&chrono::Utc),
                }))
            }
            None => Ok(None),
        }
    }

    pub fn list_users(&self) -> Result<Vec<UserIdentity>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;

        let mut stmt = conn
            .prepare("SELECT user_id FROM users ORDER BY created_at")
            .map_err(|e| Error::Database(e.to_string()))?;

        let user_ids: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| Error::Database(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        drop(stmt);
        drop(conn);

        let mut users = Vec::new();
        for uid in user_ids {
            if let Some(user) = self.get_user(&UserId(uid))? {
                users.push(user);
            }
        }
        Ok(users)
    }

    pub fn find_by_device(&self, device_id: &DeviceId) -> Result<Option<UserIdentity>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;

        let user_id: Option<String> = conn
            .query_row(
                "SELECT user_id FROM device_bindings WHERE device_id = ?1",
                params![device_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| Error::Database(e.to_string()))?;

        drop(conn);

        match user_id {
            Some(uid) => self.get_user(&UserId(uid)),
            None => Ok(None),
        }
    }

    pub fn find_by_channel(
        &self,
        channel_kind: &ChannelKind,
        handle: &str,
    ) -> Result<Option<UserIdentity>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;

        let kind_str = serde_json::to_string(channel_kind).unwrap();

        let user_id: Option<String> = conn
            .query_row(
                "SELECT user_id FROM channel_bindings WHERE channel_kind = ?1 AND handle = ?2",
                params![kind_str, handle],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| Error::Database(e.to_string()))?;

        drop(conn);

        match user_id {
            Some(uid) => self.get_user(&UserId(uid)),
            None => Ok(None),
        }
    }

    pub fn add_device(&self, user_id: &UserId, device: &DeviceBinding) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO device_bindings
             (device_id, user_id, device_name, attestation, public_key_hash, trust, paired_at, last_used)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                device.device_id.0,
                user_id.0,
                device.device_name,
                serde_json::to_string(&device.attestation).unwrap(),
                device.public_key_hash,
                serde_json::to_string(&device.trust).unwrap(),
                device.paired_at.to_rfc3339(),
                device.last_used.to_rfc3339(),
            ],
        )
        .map_err(|e| Error::Database(e.to_string()))?;
        Ok(())
    }

    pub fn add_channel(&self, user_id: &UserId, binding: &ChannelBinding) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO channel_bindings
             (user_id, channel_id, channel_kind, handle, trust, linked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                user_id.0,
                binding.channel_id.0,
                serde_json::to_string(&binding.channel_kind).unwrap(),
                binding.handle,
                serde_json::to_string(&binding.trust).unwrap(),
                binding.linked_at.to_rfc3339(),
            ],
        )
        .map_err(|e| Error::Database(e.to_string()))?;
        Ok(())
    }

    pub fn remove_device(&self, user_id: &UserId, device_id: &DeviceId) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;
        conn.execute(
            "DELETE FROM device_bindings WHERE user_id = ?1 AND device_id = ?2",
            params![user_id.0, device_id.0],
        )
        .map_err(|e| Error::Database(e.to_string()))?;
        Ok(())
    }

    fn get_devices_for_user_inner(
        &self,
        conn: &Connection,
        user_id: &UserId,
    ) -> Result<Vec<DeviceBinding>> {
        let mut stmt = conn
            .prepare(
                "SELECT device_id, device_name, attestation, public_key_hash, trust, paired_at, last_used
                 FROM device_bindings WHERE user_id = ?1",
            )
            .map_err(|e| Error::Database(e.to_string()))?;

        let devices = stmt
            .query_map(params![user_id.0], |row| {
                let device_id: String = row.get(0)?;
                let device_name: String = row.get(1)?;
                let attestation: String = row.get(2)?;
                let public_key_hash: String = row.get(3)?;
                let trust: String = row.get(4)?;
                let paired_at: String = row.get(5)?;
                let last_used: String = row.get(6)?;
                Ok((
                    device_id,
                    device_name,
                    attestation,
                    public_key_hash,
                    trust,
                    paired_at,
                    last_used,
                ))
            })
            .map_err(|e| Error::Database(e.to_string()))?
            .filter_map(|r| r.ok())
            .map(
                |(
                    device_id,
                    device_name,
                    attestation,
                    public_key_hash,
                    trust,
                    paired_at,
                    last_used,
                )| {
                    DeviceBinding {
                        device_id: DeviceId(device_id),
                        device_name,
                        attestation: serde_json::from_str(&attestation)
                            .unwrap_or(AttestationType::CompositeFingerprint),
                        public_key_hash,
                        trust: serde_json::from_str(&trust).unwrap_or(TrustLevel::Channel),
                        paired_at: chrono::DateTime::parse_from_rfc3339(&paired_at)
                            .unwrap_or_default()
                            .with_timezone(&chrono::Utc),
                        last_used: chrono::DateTime::parse_from_rfc3339(&last_used)
                            .unwrap_or_default()
                            .with_timezone(&chrono::Utc),
                    }
                },
            )
            .collect();

        Ok(devices)
    }

    fn get_channels_for_user_inner(
        &self,
        conn: &Connection,
        user_id: &UserId,
    ) -> Result<Vec<ChannelBinding>> {
        let mut stmt = conn
            .prepare(
                "SELECT channel_id, channel_kind, handle, trust, linked_at
                 FROM channel_bindings WHERE user_id = ?1",
            )
            .map_err(|e| Error::Database(e.to_string()))?;

        let channels = stmt
            .query_map(params![user_id.0], |row| {
                let channel_id: String = row.get(0)?;
                let channel_kind: String = row.get(1)?;
                let handle: String = row.get(2)?;
                let trust: String = row.get(3)?;
                let linked_at: String = row.get(4)?;
                Ok((channel_id, channel_kind, handle, trust, linked_at))
            })
            .map_err(|e| Error::Database(e.to_string()))?
            .filter_map(|r| r.ok())
            .map(
                |(channel_id, channel_kind, handle, trust, linked_at)| ChannelBinding {
                    channel_id: ChannelId(channel_id),
                    channel_kind: serde_json::from_str(&channel_kind)
                        .unwrap_or(ChannelKind::Custom("unknown".into())),
                    handle,
                    trust: serde_json::from_str(&trust).unwrap_or(TrustLevel::Channel),
                    linked_at: chrono::DateTime::parse_from_rfc3339(&linked_at)
                        .unwrap_or_default()
                        .with_timezone(&chrono::Utc),
                },
            )
            .collect();

        Ok(channels)
    }
}

/// Extension trait for optional query results.
trait OptionalExt<T> {
    fn optional(self) -> std::result::Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for std::result::Result<T, rusqlite::Error> {
    fn optional(self) -> std::result::Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
