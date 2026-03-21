## Summary

Implements the last remaining Signal Protocol workstream: automatic X3DH session bootstrap when `send_text` encounters a recipient with no existing Signal session on disk.

## What changed

**`crates/ngenorca-whatsapp-web/src/client.rs`**

- **`iq_waiters` field** — `Arc<Mutex<HashMap<String, oneshot::Sender<WaNode>>>>` correlates in-flight IQ requests with their server response without racing against the background recv loop.
- **`spawn_recv_loop`** — threads `iq_waiters` into the recv loop closure.
- **`handle_incoming_node`** — new `iq` arm routes `type=result/error` nodes to the matching waiter; unmatched IQs are logged and discarded.
- **`PreKeyBundle` struct + `parse_prekey_bundle_response` + `find_user_node`** — parse the server's IQ response into typed key material (`identity`, `skey`, optional one-time `key`).
- **`fetch_prekey_bundle`** — registers waiter → sends `<iq type="get" xmlns="encrypt" to="s.whatsapp.net"><key jid="…"/></iq>` → waits up to 10 s for the routed response.
- **`bootstrap_signal_session`** — calls `fetch_prekey_bundle`, loads `our_identity`, calls `Session::from_prekey_bundle` (X3DH initiator).
- **`send_text` `_ =>` arm** — replaced the plaintext fallback with the full X3DH bootstrap → encrypt flow; plaintext remains as last-resort fallback only.

## Validation

- 45/45 unit tests pass
- `cargo fmt --check` clean
- `cargo clippy -D warnings` clean
