//! WebSocket transport with Noise framing.
//!
//! Manages the raw WebSocket connection to WhatsApp's servers and
//! wraps it with the Noise Protocol for encrypted transport.
//!
//! ## Connection flow
//! 1. Open WSS to `wss://web.whatsapp.com/ws/chat`
//! 2. Send 4-byte prologue: `WA\x06\x03`
//! 3. Noise XX handshake (3 round-trips)
//! 4. Post-handshake: all frames are `[3-byte BE len][noise-encrypted WABinary]`

use crate::binary::{self, WaNode};
use crate::noise::NoiseHandler;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tracing::{debug, trace, warn};

/// WhatsApp WebSocket endpoint.
const WA_WS_URL: &str = "wss://web.whatsapp.com/ws/chat";

type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, WsMessage>;
type WsStream = SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;

/// Transport layer — WebSocket + Noise encryption.
pub struct Transport {
    sink: Arc<Mutex<WsSink>>,
    stream: Arc<Mutex<WsStream>>,
    noise: Arc<Mutex<NoiseHandler>>,
}

impl Transport {
    /// Connect to WhatsApp, send prologue, and perform the Noise handshake.
    ///
    /// `client_payload` is the protobuf-encoded ClientPayload sent inside
    /// the final handshake message.
    ///
    /// Returns `(Transport, server_hello_payload)` where
    /// `server_hello_payload` is the decrypted certificate/payload from the
    /// server hello.
    pub async fn connect(
        noise: NoiseHandler,
        client_payload: &[u8],
    ) -> crate::Result<(Self, Vec<u8>)> {
        debug!("Transport: connecting to {WA_WS_URL}");

        let (ws, _resp) = connect_async(WA_WS_URL)
            .await
            .map_err(|e| crate::Error::WebSocket(format!("connect: {e}")))?;

        let (mut sink, mut stream) = ws.split();

        // 1. Send prologue.
        let prologue = NoiseHandler::prologue();
        sink.send(WsMessage::Binary(prologue.into()))
            .await
            .map_err(|e| crate::Error::WebSocket(format!("send prologue: {e}")))?;
        debug!("Transport: prologue sent");

        let mut noise = noise;

        // 2. Client Hello: send our ephemeral key in a HandshakeMessage.
        let eph_key = noise.init_handshake()?;
        let client_hello = crate::proto::encode_handshake_client_hello(&eph_key);
        sink.send(WsMessage::Binary(client_hello.into()))
            .await
            .map_err(|e| crate::Error::WebSocket(format!("send client hello: {e}")))?;
        debug!("Transport: client hello sent");

        // 3. Receive Server Hello.
        let server_hello_raw = Self::read_binary(&mut stream).await?;
        let (server_ephemeral, server_encrypted) =
            crate::proto::decode_handshake_server_hello(&server_hello_raw)?;

        // Assemble the full server hello payload for noise processing.
        let mut full_server = Vec::with_capacity(32 + server_encrypted.len());
        full_server.extend_from_slice(&server_ephemeral);
        full_server.extend_from_slice(&server_encrypted);

        let server_payload = noise.process_server_hello(&full_server)?;
        debug!(
            "Transport: server hello received, payload={}B",
            server_payload.len()
        );

        // 4. Client Finish: send our static key + encrypted ClientPayload.
        let client_finish = noise.build_client_finish(client_payload)?;
        let finish_msg = crate::proto::encode_handshake_client_finish(&client_finish);
        sink.send(WsMessage::Binary(finish_msg.into()))
            .await
            .map_err(|e| crate::Error::WebSocket(format!("send client finish: {e}")))?;
        debug!("Transport: handshake complete");

        let noise = Arc::new(Mutex::new(noise));
        let transport = Self {
            sink: Arc::new(Mutex::new(sink)),
            stream: Arc::new(Mutex::new(stream)),
            noise,
        };

        Ok((transport, server_payload))
    }

    /// Send a WABinary node (encrypted by Noise).
    pub async fn send_node(&self, node: &WaNode) -> crate::Result<()> {
        let raw = binary::encode(node)?;
        let encrypted = {
            let mut noise = self.noise.lock().await;
            noise.encrypt(&raw)?
        };

        // Frame: 3-byte big-endian length + encrypted data.
        let len = encrypted.len();
        let mut frame = Vec::with_capacity(3 + len);
        frame.push(((len >> 16) & 0xFF) as u8);
        frame.push(((len >> 8) & 0xFF) as u8);
        frame.push((len & 0xFF) as u8);
        frame.extend_from_slice(&encrypted);

        let mut sink = self.sink.lock().await;
        sink.send(WsMessage::Binary(frame.into()))
            .await
            .map_err(|e| crate::Error::WebSocket(format!("send: {e}")))?;

        trace!(tag = %node.tag, "Transport: sent node");
        Ok(())
    }

    /// Receive the next WABinary node (decrypted).
    pub async fn recv_node(&self) -> crate::Result<WaNode> {
        let raw = {
            let mut stream = self.stream.lock().await;
            Self::read_binary(&mut stream).await?
        };

        // The frame may be: 3-byte length header + encrypted data,
        // or just encrypted data if the WebSocket frame IS the message.
        let encrypted = if raw.len() > 3 {
            let len = ((raw[0] as usize) << 16) | ((raw[1] as usize) << 8) | (raw[2] as usize);
            if len + 3 == raw.len() {
                // Has a length header.
                raw[3..].to_vec()
            } else {
                // No header — entire frame is the encrypted payload.
                raw
            }
        } else {
            raw
        };

        let decrypted = {
            let mut noise = self.noise.lock().await;
            noise.decrypt(&encrypted)?
        };

        let node = binary::decode(&decrypted)?;
        trace!(tag = %node.tag, "Transport: received node");
        Ok(node)
    }

    /// Send a raw binary frame (used during handshake).
    pub async fn send_raw(&self, data: &[u8]) -> crate::Result<()> {
        let mut sink = self.sink.lock().await;
        sink.send(WsMessage::Binary(data.to_vec().into()))
            .await
            .map_err(|e| crate::Error::WebSocket(format!("send raw: {e}")))?;
        Ok(())
    }

    /// Close the WebSocket cleanly.
    pub async fn close(&self) {
        let mut sink = self.sink.lock().await;
        let _ = sink.send(WsMessage::Close(None)).await;
    }

    /// Get the Noise static public key bytes (for QR code generation).
    pub fn noise_public_key(&self) -> Vec<u8> {
        // We need a sync version since this is called from sync context too.
        // The noise handler's static key is set at construction time, so
        // we can safely access it via a blocking lock.
        let noise = self.noise.blocking_lock();
        noise.static_public_key().as_bytes().to_vec()
    }

    /// Read the next binary WebSocket message.
    async fn read_binary(stream: &mut WsStream) -> crate::Result<Vec<u8>> {
        loop {
            match stream.next().await {
                Some(Ok(WsMessage::Binary(data))) => return Ok(data.to_vec()),
                Some(Ok(WsMessage::Ping(_payload))) => {
                    trace!("Transport: received ping");
                    // tokio-tungstenite auto-responds to pings.
                    continue;
                }
                Some(Ok(WsMessage::Pong(_))) => continue,
                Some(Ok(WsMessage::Close(_))) => return Err(crate::Error::Closed),
                Some(Ok(other)) => {
                    warn!("Transport: unexpected message type: {other:?}");
                    continue;
                }
                Some(Err(e)) => {
                    return Err(crate::Error::WebSocket(format!("read: {e}")));
                }
                None => return Err(crate::Error::Closed),
            }
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_is_correct() {
        assert!(WA_WS_URL.starts_with("wss://"));
        assert!(WA_WS_URL.contains("web.whatsapp.com"));
    }
}
