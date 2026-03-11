//! High-level WhatsApp Web client.
//!
//! `WhatsAppClient` provides a simple API:
//! - `connect(data_dir)` — connect, authenticate, resume or pair.
//! - `send_text(jid, text)` — send a text message.
//! - Events are delivered via a channel (`WhatsAppEvent`).
//!
//! Internally it orchestrates Transport, Noise, Signal, Auth, and Store.

use crate::auth::{self, Credentials};
use crate::binary::{WaNode, WaNodeContent};
use crate::noise::NoiseHandler;
use crate::signal::SignalKeys;
use crate::store::FileStore;
use crate::transport::Transport;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, Notify};
use tracing::{debug, error, info, warn};

/// Events emitted by the client.
#[derive(Debug, Clone)]
pub enum WhatsAppEvent {
    /// QR code to display for pairing.
    QrCode(String),
    /// Successfully connected (possibly after pairing).
    Connected { jid: String },
    /// Incoming text message.
    Message {
        from: String,
        text: String,
        timestamp: u64,
        message_id: String,
    },
    /// Disconnected from WhatsApp.
    Disconnected { reason: String },
}

/// Configuration for the WhatsApp client.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Directory to store credentials and session data.
    pub data_dir: PathBuf,
    /// Display name for this companion device.
    pub push_name: Option<String>,
}

/// High-level WhatsApp Web client.
pub struct WhatsAppClient {
    #[allow(dead_code)]
    config: ClientConfig,
    store: FileStore,
    transport: Option<Arc<Mutex<Transport>>>,
    event_tx: mpsc::UnboundedSender<WhatsAppEvent>,
    event_rx: Option<mpsc::UnboundedReceiver<WhatsAppEvent>>,
    credentials: Arc<Mutex<Option<Credentials>>>,
    signal_keys: Arc<Mutex<Option<SignalKeys>>>,
    shutdown: Arc<Notify>,
    connected: Arc<std::sync::atomic::AtomicBool>,
}

impl WhatsAppClient {
    /// Create a new client.  Does NOT connect — call `connect()` next.
    pub async fn new(config: ClientConfig) -> crate::Result<Self> {
        let store = FileStore::new(&config.data_dir).await?;
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        Ok(Self {
            config,
            store,
            transport: None,
            event_tx,
            event_rx: Some(event_rx),
            credentials: Arc::new(Mutex::new(None)),
            signal_keys: Arc::new(Mutex::new(None)),
            shutdown: Arc::new(Notify::new()),
            connected: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    /// Take the event receiver.  Can only be called once.
    pub fn take_event_receiver(&mut self) -> Option<mpsc::UnboundedReceiver<WhatsAppEvent>> {
        self.event_rx.take()
    }

    /// Connect to WhatsApp Web.
    ///
    /// If credentials exist on disk, attempts a resume/login.
    /// Otherwise, initiates a new-device pairing flow (emits QR code events).
    pub async fn connect(&mut self) -> crate::Result<()> {
        info!("WhatsApp native client connecting…");

        // Load or generate Signal keys.
        let keys = self.store.load_or_generate_signal_keys().await?;
        *self.signal_keys.lock().await = Some(keys.clone());

        // Check for existing credentials.
        let existing_creds = self.store.load_credentials().await?;
        let is_new_device = existing_creds.is_none();

        // Prepare the Noise handler.
        let noise = if let Some(ref creds) = existing_creds {
            // Resume with known static key.
            let mut secret_bytes = [0u8; 32];
            secret_bytes.copy_from_slice(&creds.noise_private);
            let secret = x25519_dalek::StaticSecret::from(secret_bytes);
            let public = x25519_dalek::PublicKey::from(&secret);
            NoiseHandler::with_static_key(secret, public)
        } else {
            NoiseHandler::new()
        };

        // Build client payload.
        let payload = if let Some(ref creds) = existing_creds {
            auth::build_login_payload(&creds.jid)
        } else {
            auth::build_pairing_payload(&keys)
        };

        // Connect WebSocket and perform Noise handshake.
        let (transport, _server_payload) = Transport::connect(noise, &payload).await?;
        let transport = Arc::new(Mutex::new(transport));
        self.transport = Some(transport.clone());

        if is_new_device {
            // New device: expect QR code flow.
            self.run_pairing_flow(transport.clone(), &keys).await?;
        } else {
            // Existing device: we should be connected.
            let creds = existing_creds.unwrap();
            *self.credentials.lock().await = Some(creds.clone());
            self.connected
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = self.event_tx.send(WhatsAppEvent::Connected {
                jid: creds.jid.clone(),
            });
            info!(jid = %creds.jid, "Reconnected with saved credentials");
        }

        // Start the receive loop.
        self.spawn_recv_loop(transport);

        Ok(())
    }

    /// Run the QR code pairing flow for a new device.
    async fn run_pairing_flow(
        &self,
        transport: Arc<Mutex<Transport>>,
        keys: &SignalKeys,
    ) -> crate::Result<()> {
        info!("Starting QR code pairing flow…");

        // Upload pre-keys.
        let prekey_node = auth::build_prekey_upload_node(keys);
        {
            let t = transport.lock().await;
            t.send_node(&prekey_node).await?;
        }

        // Wait for QR ref from server.
        let qr_ref = {
            let mut attempts = 0;
            let mut found_ref = None;
            while attempts < 10 {
                let node = {
                    let t = transport.lock().await;
                    t.recv_node().await?
                };
                if let Some(r) = auth::extract_qr_ref(&node) {
                    found_ref = Some(r);
                    break;
                }
                attempts += 1;
            }
            found_ref.ok_or_else(|| crate::Error::Auth("no QR ref received".into()))?
        };

        // Build and emit QR code.
        let noise_pub = {
            let t = transport.lock().await;
            t.noise_public_key()
        };
        let qr = auth::build_qr_data(&qr_ref, &noise_pub, &keys.identity.public, &[0u8; 16]);
        let _ = self.event_tx.send(WhatsAppEvent::QrCode(qr.data));
        info!("QR code emitted — scan with your phone");

        // Wait for pair-success.
        let mut paired = false;
        for _ in 0..60 {
            let node = {
                let t = transport.lock().await;
                match tokio::time::timeout(std::time::Duration::from_secs(2), t.recv_node()).await {
                    Ok(Ok(n)) => n,
                    Ok(Err(e)) => {
                        warn!("recv error during pairing: {e}");
                        continue;
                    }
                    Err(_) => continue, // timeout, keep waiting
                }
            };

            if let Some(pair_data) = auth::parse_pair_success(&node) {
                // Save credentials.
                let creds = Credentials {
                    jid: pair_data.jid.clone(),
                    device_id: pair_data.device_id,
                    client_id: vec![0u8; 16],
                    noise_private: vec![0u8; 32], // TODO: extract from noise handler
                    noise_public: noise_pub.clone(),
                    signal_keys: keys.clone(),
                    adv_secret: vec![],
                    paired_at: chrono::Utc::now().timestamp() as u64,
                };
                self.store.save_credentials(&creds).await?;
                *self.credentials.lock().await = Some(creds);

                self.connected
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                let _ = self.event_tx.send(WhatsAppEvent::Connected {
                    jid: pair_data.jid,
                });

                paired = true;
                break;
            }
        }

        if !paired {
            return Err(crate::Error::Auth("pairing timed out".into()));
        }

        Ok(())
    }

    /// Spawn the background receive loop.
    fn spawn_recv_loop(&self, transport: Arc<Mutex<Transport>>) {
        let event_tx = self.event_tx.clone();
        let shutdown = self.shutdown.clone();
        let connected = self.connected.clone();
        let store = self.store.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.notified() => {
                        debug!("Receive loop shutting down");
                        break;
                    }
                    result = async {
                        let t = transport.lock().await;
                        t.recv_node().await
                    } => {
                        match result {
                            Ok(node) => {
                                Self::handle_incoming_node(&event_tx, &store, &node).await;
                            }
                            Err(e) => {
                                error!("WebSocket receive error: {e}");
                                connected.store(false, std::sync::atomic::Ordering::SeqCst);
                                let _ = event_tx.send(WhatsAppEvent::Disconnected {
                                    reason: e.to_string(),
                                });
                                break;
                            }
                        }
                    }
                }
            }
        });
    }

    /// Handle an incoming WABinary node.
    async fn handle_incoming_node(
        event_tx: &mpsc::UnboundedSender<WhatsAppEvent>,
        _store: &FileStore,
        node: &WaNode,
    ) {
        match node.tag.as_str() {
            "message" => {
                Self::handle_message_node(event_tx, node);
            }
            "receipt" | "ack" | "notification" => {
                debug!(tag = %node.tag, "Received control node");
            }
            "ib" | "iq" => {
                debug!(tag = %node.tag, "Received IQ node");
            }
            "stream:error" => {
                warn!("Stream error from server");
                let _ = event_tx.send(WhatsAppEvent::Disconnected {
                    reason: "stream:error".into(),
                });
            }
            _ => {
                debug!(tag = %node.tag, "Unhandled node");
            }
        }
    }

    /// Extract a text message from a message node.
    fn handle_message_node(
        event_tx: &mpsc::UnboundedSender<WhatsAppEvent>,
        node: &WaNode,
    ) {
        let from = node
            .attr("from")
            .or_else(|| node.attr("participant"))
            .unwrap_or("unknown")
            .to_string();
        let message_id = node.attr("id").unwrap_or("").to_string();
        let timestamp: u64 = node
            .attr("t")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // The message body is typically in a child node.
        // In the E2E flow, it would be an encrypted Signal blob
        // that we decrypt. For now, try to find plaintext content.
        let text = Self::extract_text_from_node(node);

        if let Some(text) = text {
            info!(from = %from, len = text.len(), "Received message");
            let _ = event_tx.send(WhatsAppEvent::Message {
                from,
                text,
                timestamp,
                message_id,
            });
        } else {
            debug!(from = %from, "Received non-text or encrypted message");
        }
    }

    /// Try to extract text content from a message node tree.
    fn extract_text_from_node(node: &WaNode) -> Option<String> {
        // Direct text content.
        if let WaNodeContent::Text(ref t) = node.content {
            return Some(t.clone());
        }

        // Search children.
        if let WaNodeContent::List(ref children) = node.content {
            for child in children {
                match child.tag.as_str() {
                    "body" | "conversation" => {
                        if let WaNodeContent::Text(ref t) = child.content {
                            return Some(t.clone());
                        }
                    }
                    "enc" => {
                        // Encrypted message — needs Signal decryption.
                        // TODO: implement Signal decryption pipeline.
                        debug!("Encrypted message node (enc) — decryption not yet implemented");
                    }
                    _ => {}
                }
                // Recurse.
                if let Some(t) = Self::extract_text_from_node(child) {
                    return Some(t);
                }
            }
        }

        None
    }

    /// Send a text message to a JID.
    pub async fn send_text(&self, jid: &str, text: &str) -> crate::Result<()> {
        if !self.connected.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(crate::Error::Closed);
        }

        let transport = self
            .transport
            .as_ref()
            .ok_or(crate::Error::Closed)?;

        let msg_id = uuid::Uuid::new_v4().to_string().replace('-', "")[..20].to_string();
        let timestamp = chrono::Utc::now().timestamp().to_string();

        // Build the message node.
        // In full implementation this would be Signal-encrypted.
        // The WABinary message structure:
        //   <message to="jid" type="text" id="..." t="timestamp">
        //     <body>text</body>
        //   </message>
        let body_node = WaNode {
            tag: "body".into(),
            attrs: HashMap::new(),
            content: WaNodeContent::Text(text.to_string()),
        };

        let mut attrs = HashMap::new();
        attrs.insert("to".into(), jid.to_string());
        attrs.insert("type".into(), "text".into());
        attrs.insert("id".into(), msg_id.clone());
        attrs.insert("t".into(), timestamp);

        let msg_node = WaNode {
            tag: "message".into(),
            attrs,
            content: WaNodeContent::List(vec![body_node]),
        };

        // TODO: encrypt message body with Signal session for the recipient.
        // For now we send the WABinary node; in a full implementation the
        // body would be replaced with an <enc> node containing the encrypted
        // Signal message.

        {
            let t = transport.lock().await;
            t.send_node(&msg_node).await?;
        }

        debug!(to = jid, msg_id = %msg_id, "Sent text message");
        Ok(())
    }

    /// Check if the client is connected.
    pub fn is_connected(&self) -> bool {
        self.connected.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Disconnect and clean up.
    pub async fn disconnect(&self) -> crate::Result<()> {
        self.shutdown.notify_one();
        self.connected
            .store(false, std::sync::atomic::Ordering::SeqCst);

        if let Some(ref transport) = self.transport {
            let t = transport.lock().await;
            let _ = t.close().await;
        }

        info!("WhatsApp client disconnected");
        Ok(())
    }

    /// Logout: disconnect AND clear stored credentials.
    pub async fn logout(&mut self) -> crate::Result<()> {
        self.disconnect().await?;
        self.store.clear_credentials().await?;
        self.store.wipe().await?;
        info!("Logged out — credentials cleared");
        Ok(())
    }
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn client_create() {
        let dir = tempdir().unwrap();
        let config = ClientConfig {
            data_dir: dir.path().to_path_buf(),
            push_name: Some("Test".into()),
        };
        let client = WhatsAppClient::new(config).await.unwrap();
        assert!(!client.is_connected());
    }

    #[tokio::test]
    async fn event_receiver_taken_once() {
        let dir = tempdir().unwrap();
        let config = ClientConfig {
            data_dir: dir.path().to_path_buf(),
            push_name: None,
        };
        let mut client = WhatsAppClient::new(config).await.unwrap();
        assert!(client.take_event_receiver().is_some());
        assert!(client.take_event_receiver().is_none());
    }

    #[test]
    fn extract_text_from_body_child() {
        let body = WaNode {
            tag: "body".into(),
            attrs: HashMap::new(),
            content: WaNodeContent::Text("Hello world".into()),
        };
        let msg = WaNode {
            tag: "message".into(),
            attrs: HashMap::new(),
            content: WaNodeContent::List(vec![body]),
        };
        let text = WhatsAppClient::extract_text_from_node(&msg);
        assert_eq!(text, Some("Hello world".into()));
    }

    #[test]
    fn extract_text_from_direct_text() {
        let node = WaNode {
            tag: "message".into(),
            attrs: HashMap::new(),
            content: WaNodeContent::Text("Direct text".into()),
        };
        let text = WhatsAppClient::extract_text_from_node(&node);
        assert_eq!(text, Some("Direct text".into()));
    }

    #[test]
    fn extract_text_none_for_empty() {
        let node = WaNode {
            tag: "message".into(),
            attrs: HashMap::new(),
            content: WaNodeContent::None,
        };
        assert!(WhatsAppClient::extract_text_from_node(&node).is_none());
    }
}
