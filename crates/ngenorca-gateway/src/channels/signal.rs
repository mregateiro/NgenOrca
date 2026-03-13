//! Signal adapter using signal-cli as the backend process.
//!
//! ## Receive path
//! Spawns `signal-cli -u {phone} daemon --json` as a child process.
//! Reads newline-delimited JSON envelopes from stdout.  Each envelope
//! with a `dataMessage` containing a `message` field is converted to
//! an NgenOrca `Message` and emitted on the event bus.
//!
//! ## Send path
//! Writes a JSON-RPC `send` call to the daemon's stdin, or (in CLI
//! mode) spawns a one-shot `signal-cli send` command.
//!
//! ## Modes
//! - `"daemon"` (default) — long-running child process
//! - `"json-rpc"` — connects to an already-running signal-cli JSON-RPC
//!   socket (same child read/write pattern, just over TCP/Unix socket)

use async_trait::async_trait;
use ngenorca_core::ChannelKind;
use ngenorca_core::event::{Event, EventPayload};
use ngenorca_core::message::{Content, Direction, Message};
use ngenorca_core::plugin::{API_VERSION, Permission, PluginKind, PluginManifest};
use ngenorca_core::types::{ChannelId, EventId, PluginId, SessionId, TrustLevel, UserId};
use ngenorca_plugin_sdk::{ChannelAdapter, Plugin, PluginContext, flume_like};
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// Signal adapter using signal-cli backend.
pub struct SignalAdapter {
    phone_number: String,
    signal_cli_path: String,
    data_path: String,
    mode: String,
    sender: Option<flume_like::Sender>,
    running: Arc<AtomicBool>,
    /// Handle to the daemon's stdin for sending messages in daemon mode.
    daemon_stdin: Arc<Mutex<Option<tokio::process::ChildStdin>>>,
}

impl SignalAdapter {
    pub fn new(
        phone_number: String,
        signal_cli_path: String,
        data_path: String,
        mode: String,
    ) -> Self {
        Self {
            phone_number,
            signal_cli_path,
            data_path,
            mode,
            sender: None,
            running: Arc::new(AtomicBool::new(false)),
            daemon_stdin: Arc::new(Mutex::new(None)),
        }
    }

    /// Send a text message via a one-shot signal-cli invocation.
    async fn send_text_cli(&self, recipient: &str, text: &str) -> ngenorca_core::Result<()> {
        let output = Command::new(&self.signal_cli_path)
            .args([
                "--config",
                &self.data_path,
                "-u",
                &self.phone_number,
                "send",
                "-m",
                text,
                recipient,
            ])
            .output()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("signal-cli send: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ngenorca_core::Error::Other(format!(
                "signal-cli send failed: {stderr}"
            )));
        }

        debug!(recipient = recipient, "Signal message sent via CLI");
        Ok(())
    }

    /// Send a text message via the daemon's stdin (JSON-RPC).
    async fn send_text_daemon(&self, recipient: &str, text: &str) -> ngenorca_core::Result<()> {
        let mut stdin_guard = self.daemon_stdin.lock().await;
        let stdin = stdin_guard.as_mut().ok_or_else(|| {
            ngenorca_core::Error::Other("Signal daemon stdin not available".into())
        })?;

        let rpc = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "send",
            "id": uuid::Uuid::new_v4().to_string(),
            "params": {
                "recipient": [recipient],
                "message": text,
            }
        });

        let mut line = serde_json::to_string(&rpc)
            .map_err(|e| ngenorca_core::Error::Other(format!("Signal RPC serialize: {e}")))?;
        line.push('\n');

        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Signal daemon write: {e}")))?;

        debug!(recipient = recipient, "Signal message sent via daemon");
        Ok(())
    }

    /// Convert a signal-cli JSON envelope into an NgenOrca `Message`.
    fn envelope_to_message(envelope: &SignalEnvelope, own_phone: &str) -> Option<Message> {
        // Skip our own messages.
        let source = envelope.source.as_deref()?;
        if source == own_phone {
            return None;
        }

        let data = envelope.data_message.as_ref()?;
        let text = data.message.as_deref()?;
        if text.is_empty() {
            return None;
        }

        // Use groupId as channel when present, else sender phone.
        let channel = data
            .group_info
            .as_ref()
            .and_then(|g| g.group_id.clone())
            .unwrap_or_else(|| source.to_string());

        Some(Message {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            user_id: Some(UserId(format!("signal:{source}"))),
            trust: TrustLevel::Channel,
            session_id: SessionId::new(),
            channel: ChannelId(channel),
            channel_kind: ChannelKind::Signal,
            direction: Direction::Inbound,
            content: Content::Text(text.to_string()),
            metadata: serde_json::json!({
                "signal_source": source,
                "signal_timestamp": envelope.timestamp,
                "signal_group_id": data.group_info.as_ref().and_then(|g| g.group_id.as_deref()),
            }),
        })
    }
}

#[async_trait]
impl Plugin for SignalAdapter {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PluginId("ngenorca-signal".into()),
            name: "Signal".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: PluginKind::ChannelAdapter,
            permissions: vec![
                Permission::ReadMessages,
                Permission::WriteMessages,
                Permission::Network,
            ],
            api_version: API_VERSION,
            description: "Signal adapter via signal-cli".into(),
        }
    }

    async fn init(&mut self, ctx: PluginContext) -> ngenorca_core::Result<()> {
        self.sender = Some(ctx.sender);
        info!(
            phone = %self.phone_number,
            mode = %self.mode,
            cli_path = %self.signal_cli_path,
            "Signal adapter initialized"
        );
        Ok(())
    }

    async fn handle_message(&self, message: &Message) -> ngenorca_core::Result<Option<Message>> {
        if message.direction == Direction::Outbound {
            let recipient = &message.channel.0;
            let text = match &message.content {
                Content::Text(t) => t.clone(),
                other => format!("{other:?}"),
            };

            if self.mode == "daemon" || self.mode == "json-rpc" {
                self.send_text_daemon(recipient, &text).await?;
            } else {
                self.send_text_cli(recipient, &text).await?;
            }
        }
        Ok(None)
    }

    async fn health_check(&self) -> ngenorca_core::Result<()> {
        // Verify that signal-cli binary exists and is executable.
        let output = Command::new(&self.signal_cli_path)
            .arg("--version")
            .output()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("signal-cli health check: {e}")))?;

        if output.status.success() {
            Ok(())
        } else {
            Err(ngenorca_core::Error::Other(
                "signal-cli health check failed".into(),
            ))
        }
    }

    async fn shutdown(&self) -> ngenorca_core::Result<()> {
        self.running.store(false, Ordering::SeqCst);
        // Drop the daemon's stdin to signal it to exit.
        let mut stdin = self.daemon_stdin.lock().await;
        *stdin = None;
        info!("Signal adapter shutting down");
        Ok(())
    }
}

#[async_trait]
impl ChannelAdapter for SignalAdapter {
    async fn start_listening(&self) -> ngenorca_core::Result<()> {
        let sender = self
            .sender
            .clone()
            .ok_or_else(|| ngenorca_core::Error::Other("Signal: not initialized".into()))?;

        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let phone = self.phone_number.clone();
        let cli_path = self.signal_cli_path.clone();
        let data_path = self.data_path.clone();
        let daemon_stdin = self.daemon_stdin.clone();

        tokio::spawn(async move {
            while running.load(Ordering::SeqCst) {
                info!("Signal: spawning signal-cli daemon …");

                let child = Command::new(&cli_path)
                    .args(["--config", &data_path, "-u", &phone, "daemon", "--json"])
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn();

                match child {
                    Ok(mut child) => {
                        // Store stdin handle for sending.
                        if let Some(stdin) = child.stdin.take() {
                            let mut guard = daemon_stdin.lock().await;
                            *guard = Some(stdin);
                        }

                        // Read newline-delimited JSON from stdout.
                        if let Some(stdout) = child.stdout.take() {
                            let reader = BufReader::new(stdout);
                            let mut lines = reader.lines();

                            loop {
                                if !running.load(Ordering::SeqCst) {
                                    break;
                                }

                                match lines.next_line().await {
                                    Ok(Some(line)) => {
                                        if line.trim().is_empty() {
                                            continue;
                                        }

                                        match serde_json::from_str::<SignalCliOutput>(&line) {
                                            Ok(output) => {
                                                if let Some(ref envelope) = output.envelope
                                                    && let Some(ngen_msg) =
                                                        SignalAdapter::envelope_to_message(
                                                            envelope, &phone,
                                                        )
                                                {
                                                    let event = Event {
                                                        id: EventId::new(),
                                                        timestamp: chrono::Utc::now(),
                                                        session_id: Some(
                                                            ngen_msg.session_id.clone(),
                                                        ),
                                                        user_id: ngen_msg.user_id.clone(),
                                                        payload: EventPayload::Message(ngen_msg),
                                                    };
                                                    if let Err(e) = sender.send(event) {
                                                        error!(
                                                            error = %e,
                                                            "Signal: failed to send event"
                                                        );
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                debug!(
                                                    error = %e,
                                                    line = %line,
                                                    "Signal: failed to parse output line"
                                                );
                                            }
                                        }
                                    }
                                    Ok(None) => {
                                        info!("Signal: daemon stdout closed");
                                        break;
                                    }
                                    Err(e) => {
                                        error!(error = %e, "Signal: read error");
                                        break;
                                    }
                                }
                            }
                        }

                        // Clean up stdin handle.
                        {
                            let mut guard = daemon_stdin.lock().await;
                            *guard = None;
                        }

                        // Wait for child to finish.
                        match child.wait().await {
                            Ok(status) => info!(status = %status, "Signal: daemon exited"),
                            Err(e) => warn!(error = %e, "Signal: daemon wait error"),
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "Signal: failed to spawn daemon");
                    }
                }

                if running.load(Ordering::SeqCst) {
                    info!("Signal: restarting daemon in 5 s …");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        });

        Ok(())
    }

    async fn send_message(&self, message: &Message) -> ngenorca_core::Result<()> {
        self.handle_message(message).await?;
        Ok(())
    }

    fn channel_kind(&self) -> ChannelKind {
        ChannelKind::Signal
    }
}

// ─── signal-cli JSON Types ──────────────────────────────────────

/// Top-level JSON object from `signal-cli daemon --json`.
#[derive(Debug, Deserialize)]
struct SignalCliOutput {
    #[serde(default)]
    envelope: Option<SignalEnvelope>,
}

#[derive(Debug, Deserialize)]
struct SignalEnvelope {
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    timestamp: Option<u64>,
    #[serde(default, rename = "dataMessage")]
    data_message: Option<SignalDataMessage>,
}

#[derive(Debug, Deserialize)]
struct SignalDataMessage {
    #[serde(default)]
    message: Option<String>,
    #[serde(default, rename = "groupInfo")]
    group_info: Option<SignalGroupInfo>,
}

#[derive(Debug, Deserialize)]
struct SignalGroupInfo {
    #[serde(default, rename = "groupId")]
    group_id: Option<String>,
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_adapter() -> SignalAdapter {
        SignalAdapter::new(
            "+1234567890".into(),
            "/usr/bin/signal-cli".into(),
            "/var/lib/signal-cli".into(),
            "daemon".into(),
        )
    }

    fn make_envelope(source: &str, text: Option<&str>, group: Option<&str>) -> SignalEnvelope {
        SignalEnvelope {
            source: Some(source.into()),
            timestamp: Some(1234567890),
            data_message: Some(SignalDataMessage {
                message: text.map(String::from),
                group_info: group.map(|g| SignalGroupInfo {
                    group_id: Some(g.into()),
                }),
            }),
        }
    }

    #[test]
    fn signal_manifest() {
        let adapter = make_adapter();
        let manifest = adapter.manifest();
        assert_eq!(manifest.name, "Signal");
        assert_eq!(manifest.kind, PluginKind::ChannelAdapter);
    }

    #[test]
    fn signal_channel_kind() {
        let adapter = make_adapter();
        assert_eq!(adapter.channel_kind(), ChannelKind::Signal);
    }

    #[test]
    fn envelope_converts_to_message() {
        let env = make_envelope("+15559998888", Some("Hello from Signal"), None);
        let msg = SignalAdapter::envelope_to_message(&env, "+1234567890").unwrap();
        assert_eq!(msg.user_id, Some(UserId("signal:+15559998888".into())));
        assert_eq!(msg.channel_kind, ChannelKind::Signal);
        match &msg.content {
            Content::Text(t) => assert_eq!(t, "Hello from Signal"),
            _ => panic!("Expected text"),
        }
        // Channel should be the sender phone (no group).
        assert_eq!(msg.channel.0, "+15559998888");
    }

    #[test]
    fn group_message_uses_group_as_channel() {
        let env = make_envelope("+15559998888", Some("Group msg"), Some("group123"));
        let msg = SignalAdapter::envelope_to_message(&env, "+1234567890").unwrap();
        assert_eq!(msg.channel.0, "group123");
    }

    #[test]
    fn own_messages_skipped() {
        let env = make_envelope("+1234567890", Some("Echo"), None);
        assert!(SignalAdapter::envelope_to_message(&env, "+1234567890").is_none());
    }

    #[test]
    fn empty_text_skipped() {
        let env = make_envelope("+15559998888", Some(""), None);
        assert!(SignalAdapter::envelope_to_message(&env, "+1234567890").is_none());
    }

    #[test]
    fn no_data_message_skipped() {
        let env = SignalEnvelope {
            source: Some("+15559998888".into()),
            timestamp: None,
            data_message: None,
        };
        assert!(SignalAdapter::envelope_to_message(&env, "+1234567890").is_none());
    }
}
