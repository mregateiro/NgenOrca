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
use tokio::sync::{Mutex, Notify, mpsc, oneshot};
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
    /// Pending IQ request waiters keyed by IQ `id` attribute.
    iq_waiters: Arc<Mutex<HashMap<String, oneshot::Sender<WaNode>>>>,
}

// ─── Pre-Key Bundle ─────────────────────────────────────────────

/// A pre-key bundle fetched from the WhatsApp server for a remote contact.
///
/// Carries the key material required to perform an X3DH session bootstrap
/// when sending the first Signal-encrypted message to a new contact.
#[derive(Debug)]
struct PreKeyBundle {
    /// Remote device's long-term identity public key (32 bytes, Curve25519).
    identity_pub: Vec<u8>,
    /// Signed pre-key ID (used for server-side rotation tracking).
    #[allow(dead_code)]
    signed_prekey_id: u32,
    /// Signed pre-key public key (32 bytes, Curve25519).
    signed_prekey_pub: Vec<u8>,
    /// Signature over the signed pre-key (for optional verification).
    #[allow(dead_code)]
    signed_prekey_sig: Vec<u8>,
    /// One-time pre-key ID (optional).
    #[allow(dead_code)]
    one_time_prekey_id: Option<u32>,
    /// One-time pre-key public key (optional, 32 bytes, Curve25519).
    one_time_prekey_pub: Option<Vec<u8>>,
}

/// Walk a `WaNode` tree depth-first and return the first `<user>` node found.
fn find_user_node(node: &WaNode) -> Option<&WaNode> {
    if node.tag == "user" {
        return Some(node);
    }
    if let WaNodeContent::List(ref children) = node.content {
        for child in children {
            if let Some(found) = find_user_node(child) {
                return Some(found);
            }
        }
    }
    None
}

/// Parse a pre-key bundle from an IQ result node.
///
/// Expected shape (WhatsApp multi-device protocol):
/// ```xml
/// <iq type="result" id="...">
///   <list>
///     <user jid="...@s.whatsapp.net">
///       <identity><!-- 32-byte identity pub --></identity>
///       <skey id="1">
///         <value><!-- 32-byte signed-prekey pub --></value>
///         <signature><!-- 64-byte XEdDSA sig --></signature>
///       </skey>
///       <key id="1">                <!-- optional one-time pre-key -->
///         <value><!-- 32-byte OPK pub --></value>
///       </key>
///     </user>
///   </list>
/// </iq>
/// ```
fn parse_prekey_bundle_response(iq: &WaNode) -> crate::Result<PreKeyBundle> {
    let err = |msg: &str| crate::Error::Signal(msg.to_string());

    if iq.attr("type") == Some("error") {
        let code = iq.attr("code").unwrap_or("unknown");
        return Err(crate::Error::Signal(format!(
            "server returned IQ error (code={code}) for pre-key bundle request"
        )));
    }

    let user_node =
        find_user_node(iq).ok_or_else(|| err("no <user> node in pre-key bundle response"))?;

    // <identity>
    let identity_pub = user_node
        .child("identity")
        .and_then(|n| n.content_bytes())
        .ok_or_else(|| err("missing <identity> in pre-key bundle"))?
        .to_vec();

    // <skey id="…">
    let skey_node = user_node
        .child("skey")
        .ok_or_else(|| err("missing <skey> in pre-key bundle"))?;
    let signed_prekey_id: u32 = skey_node
        .attr("id")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let signed_prekey_pub = skey_node
        .child("value")
        .and_then(|n| n.content_bytes())
        .ok_or_else(|| err("missing <skey><value> in pre-key bundle"))?
        .to_vec();
    let signed_prekey_sig = skey_node
        .child("signature")
        .and_then(|n| n.content_bytes())
        .unwrap_or(&[])
        .to_vec();

    // <key id="…">  (optional one-time pre-key)
    let (one_time_prekey_id, one_time_prekey_pub) = if let Some(key_node) = user_node.child("key") {
        let id = key_node.attr("id").and_then(|s| s.parse().ok());
        let pub_key = key_node
            .child("value")
            .and_then(|n| n.content_bytes())
            .map(|b| b.to_vec());
        (id, pub_key)
    } else {
        (None, None)
    };

    Ok(PreKeyBundle {
        identity_pub,
        signed_prekey_id,
        signed_prekey_pub,
        signed_prekey_sig,
        one_time_prekey_id,
        one_time_prekey_pub,
    })
}

// ─── Client impl ────────────────────────────────────────────────

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
            iq_waiters: Arc::new(Mutex::new(HashMap::new())),
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
        let (noise_pub, noise_priv) = {
            let t = transport.lock().await;
            (t.noise_public_key(), t.noise_private_key())
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
                    noise_private: noise_priv.clone(),
                    noise_public: noise_pub.clone(),
                    signal_keys: keys.clone(),
                    adv_secret: vec![],
                    paired_at: chrono::Utc::now().timestamp() as u64,
                };
                self.store.save_credentials(&creds).await?;
                *self.credentials.lock().await = Some(creds);

                self.connected
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                let _ = self
                    .event_tx
                    .send(WhatsAppEvent::Connected { jid: pair_data.jid });

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
        let iq_waiters = self.iq_waiters.clone();

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
                                Self::handle_incoming_node(&event_tx, &store, &iq_waiters, &node).await;
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
        store: &FileStore,
        iq_waiters: &Arc<Mutex<HashMap<String, oneshot::Sender<WaNode>>>>,
        node: &WaNode,
    ) {
        match node.tag.as_str() {
            "message" => {
                Self::handle_message_node(event_tx, store, node).await;
            }
            "receipt" | "ack" | "notification" => {
                debug!(tag = %node.tag, "Received control node");
            }
            "ib" => {
                debug!(tag = %node.tag, "Received IB node");
            }
            "iq" => {
                // Route IQ result/error responses to any registered waiter.
                let node_type = node.attr("type").unwrap_or("");
                if (node_type == "result" || node_type == "error")
                    && let Some(id) = node.attr("id")
                {
                    let waiter = {
                        let mut waiters = iq_waiters.lock().await;
                        waiters.remove(id)
                    };
                    if let Some(tx) = waiter {
                        let _ = tx.send(node.clone());
                        return;
                    }
                }
                debug!(tag = %node.tag, "Received unmatched IQ node");
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
    async fn handle_message_node(
        event_tx: &mpsc::UnboundedSender<WhatsAppEvent>,
        store: &FileStore,
        node: &WaNode,
    ) {
        let from = node
            .attr("from")
            .or_else(|| node.attr("participant"))
            .unwrap_or("unknown")
            .to_string();
        let message_id = node.attr("id").unwrap_or("").to_string();
        let timestamp: u64 = node.attr("t").and_then(|s| s.parse().ok()).unwrap_or(0);

        let text = Self::try_decrypt_or_extract_text(store, &from, node).await;

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
                        // Encrypted nodes are handled by try_decrypt_or_extract_text.
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

    /// Try to decrypt a Signal-encrypted `<enc>` node, falling back to plain-text
    /// extraction for unencrypted message nodes.
    async fn try_decrypt_or_extract_text(
        store: &FileStore,
        from: &str,
        node: &WaNode,
    ) -> Option<String> {
        // 1. Fast path: cleartext extraction (handles legacy/unencrypted nodes).
        if let Some(t) = Self::extract_text_from_node(node) {
            return Some(t);
        }

        // 2. Look for <enc> children and attempt Signal decryption.
        let children = match &node.content {
            WaNodeContent::List(c) => c,
            _ => return None,
        };

        for child in children {
            if child.tag != "enc" {
                continue;
            }
            let bytes = match &child.content {
                WaNodeContent::Binary(b) => b,
                _ => continue,
            };

            // Decode the Signal message from the enc payload bytes.
            let enc_msg = match crate::proto::decode_signal_message(bytes) {
                Ok(m) => m,
                Err(e) => {
                    debug!(from, error = %e, "Failed to decode enc node payload");
                    continue;
                }
            };

            // Load the Signal session for this sender.
            let mut session = match store.load_session(from).await {
                Ok(Some(s)) => s,
                Ok(None) => {
                    debug!(from, "No Signal session — cannot decrypt enc node");
                    continue;
                }
                Err(e) => {
                    warn!(from, error = %e, "Error loading Signal session");
                    continue;
                }
            };

            // Decrypt and advance the ratchet.
            match session.decrypt(&enc_msg) {
                Ok(plaintext) => {
                    // Persist the updated ratchet state.
                    if let Err(e) = store.save_session(from, &session).await {
                        warn!(from, error = %e, "Failed to persist updated Signal session");
                    }
                    // Decode the WaMessage protobuf envelope and return the text.
                    return crate::proto::decode_wa_message(&plaintext);
                }
                Err(e) => {
                    warn!(from, error = %e, "Signal decryption failed for enc node");
                }
            }
        }

        None
    }

    // ─── X3DH session bootstrap ─────────────────────────────────

    /// Fetch the pre-key bundle for `jid` via an `<iq type="get" xmlns="encrypt">` exchange.
    ///
    /// Registers a one-shot waiter in `iq_waiters` before sending so the
    /// background receive loop can route the server's response straight here
    /// without racing against normal message traffic.
    async fn fetch_prekey_bundle(
        &self,
        transport: &Arc<Mutex<Transport>>,
        jid: &str,
    ) -> crate::Result<PreKeyBundle> {
        let req_id = {
            let raw = uuid::Uuid::new_v4().to_string().replace('-', "");
            raw[..20].to_string()
        };

        // Build the IQ "get" stanza.
        let mut iq_attrs = HashMap::new();
        iq_attrs.insert("id".into(), req_id.clone());
        iq_attrs.insert("xmlns".into(), "encrypt".into());
        iq_attrs.insert("type".into(), "get".into());
        iq_attrs.insert("to".into(), "s.whatsapp.net".into());

        let mut key_attrs = HashMap::new();
        key_attrs.insert("jid".into(), jid.to_string());

        let iq_node = WaNode {
            tag: "iq".into(),
            attrs: iq_attrs,
            content: WaNodeContent::List(vec![WaNode {
                tag: "key".into(),
                attrs: key_attrs,
                content: WaNodeContent::None,
            }]),
        };

        // Register the waiter *before* sending to avoid a response race.
        let (tx, rx) = oneshot::channel::<WaNode>();
        {
            let mut waiters = self.iq_waiters.lock().await;
            waiters.insert(req_id.clone(), tx);
        }

        // Send the IQ.
        {
            let t = transport.lock().await;
            if let Err(e) = t.send_node(&iq_node).await {
                // Remove the waiter on send failure so it doesn't leak.
                self.iq_waiters.lock().await.remove(&req_id);
                return Err(e);
            }
        }
        debug!(to = jid, iq_id = %req_id, "Sent pre-key bundle IQ request");

        // Wait up to 10 s for the server response routed by the recv loop.
        let response = tokio::time::timeout(std::time::Duration::from_secs(10), rx)
            .await
            .map_err(|_| crate::Error::Timeout(format!("pre-key bundle response for {jid}")))?
            .map_err(|_| crate::Error::Other("IQ waiter sender dropped unexpectedly".into()))?;

        parse_prekey_bundle_response(&response)
    }

    /// Perform a full X3DH session bootstrap for `jid` and return the new session.
    ///
    /// Fetches the remote pre-key bundle, derives the shared secret, and
    /// returns a ready-to-use `Session`; the caller is responsible for
    /// persisting it via `store.save_session`.
    async fn bootstrap_signal_session(&self, jid: &str) -> crate::Result<crate::signal::Session> {
        let transport = self.transport.as_ref().ok_or(crate::Error::Closed)?;

        let bundle = self.fetch_prekey_bundle(transport, jid).await?;

        let our_identity = {
            let guard = self.signal_keys.lock().await;
            guard
                .as_ref()
                .ok_or_else(|| {
                    crate::Error::Signal("Signal keys not loaded — call connect() first".into())
                })?
                .identity
                .clone()
        };

        let session = crate::signal::Session::from_prekey_bundle(
            &our_identity,
            &bundle.identity_pub,
            &bundle.signed_prekey_pub,
            bundle.one_time_prekey_pub.as_deref(),
        )?;

        debug!(to = jid, "X3DH session bootstrap complete");
        Ok(session)
    }

    /// Send a text message to a JID.
    pub async fn send_text(&self, jid: &str, text: &str) -> crate::Result<()> {
        if !self.connected.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(crate::Error::Closed);
        }

        let transport = self.transport.as_ref().ok_or(crate::Error::Closed)?;

        let msg_id = uuid::Uuid::new_v4().to_string().replace('-', "")[..20].to_string();
        let timestamp = chrono::Utc::now().timestamp().to_string();

        let mut attrs = HashMap::new();
        attrs.insert("to".into(), jid.to_string());
        attrs.insert("type".into(), "text".into());
        attrs.insert("id".into(), msg_id.clone());
        attrs.insert("t".into(), timestamp);

        let body_node = WaNode {
            tag: "body".into(),
            attrs: HashMap::new(),
            content: WaNodeContent::Text(text.to_string()),
        };

        // If a Signal session exists for the recipient, encrypt the body as an
        // <enc type="msg"> node.  Otherwise fall back to a plaintext <body> node
        // (server will reject this for E2E-enrolled contacts once X3DH session
        // bootstrap is implemented).
        let final_node = match self.store.load_session(jid).await {
            Ok(Some(mut session)) => {
                let proto_bytes = crate::proto::encode_wa_message(text);
                match session.encrypt(&proto_bytes) {
                    Ok(encrypted) => {
                        if let Err(e) = self.store.save_session(jid, &session).await {
                            warn!(to = jid, error = %e, "Failed to persist Signal session");
                        }
                        let ciphertext = crate::proto::encode_signal_message(&encrypted);
                        let mut enc_attrs = HashMap::new();
                        enc_attrs.insert("v".into(), "2".into());
                        enc_attrs.insert("type".into(), "msg".into());
                        let enc_child = WaNode {
                            tag: "enc".into(),
                            attrs: enc_attrs,
                            content: WaNodeContent::Binary(ciphertext),
                        };
                        WaNode {
                            tag: "message".into(),
                            attrs,
                            content: WaNodeContent::List(vec![enc_child]),
                        }
                    }
                    Err(e) => {
                        warn!(to = jid, error = %e, "Signal encrypt failed — falling back to plaintext");
                        WaNode {
                            tag: "message".into(),
                            attrs,
                            content: WaNodeContent::List(vec![body_node]),
                        }
                    }
                }
            }
            _ => {
                // No existing session — attempt X3DH session bootstrap.
                match self.bootstrap_signal_session(jid).await {
                    Ok(mut session) => {
                        let proto_bytes = crate::proto::encode_wa_message(text);
                        match session.encrypt(&proto_bytes) {
                            Ok(encrypted) => {
                                if let Err(e) = self.store.save_session(jid, &session).await {
                                    warn!(to = jid, error = %e, "Failed to persist bootstrapped Signal session");
                                }
                                let ciphertext = crate::proto::encode_signal_message(&encrypted);
                                let mut enc_attrs = HashMap::new();
                                enc_attrs.insert("v".into(), "2".into());
                                enc_attrs.insert("type".into(), "msg".into());
                                let enc_child = WaNode {
                                    tag: "enc".into(),
                                    attrs: enc_attrs,
                                    content: WaNodeContent::Binary(ciphertext),
                                };
                                WaNode {
                                    tag: "message".into(),
                                    attrs,
                                    content: WaNodeContent::List(vec![enc_child]),
                                }
                            }
                            Err(e) => {
                                warn!(to = jid, error = %e, "Signal encrypt failed after X3DH bootstrap — falling back to plaintext");
                                WaNode {
                                    tag: "message".into(),
                                    attrs,
                                    content: WaNodeContent::List(vec![body_node]),
                                }
                            }
                        }
                    }
                    Err(e) => {
                        debug!(to = jid, error = %e, "X3DH bootstrap failed — sending plaintext body");
                        WaNode {
                            tag: "message".into(),
                            attrs,
                            content: WaNodeContent::List(vec![body_node]),
                        }
                    }
                }
            }
        };

        {
            let t = transport.lock().await;
            t.send_node(&final_node).await?;
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
