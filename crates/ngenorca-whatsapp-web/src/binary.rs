//! WABinary codec — WhatsApp's custom binary serialization format.
//!
//! Every message exchanged after the Noise handshake is a "WABinary node":
//! a tree structure with a tag (string), attributes (key-value map), and
//! optional content (string, bytes, or child nodes).
//!
//! Strings are compressed using a token dictionary — common protocol strings
//! (like `"s.whatsapp.net"`, `"type"`, `"from"`) are encoded as single-byte
//! indices instead of full UTF-8.

use std::collections::HashMap;
use tracing::trace;

// ─── Token Dictionary ───────────────────────────────────────────
// These are the 256 single-byte tokens from the WhatsApp binary protocol.
// Index 0 is unused; indices 1–255 map to common protocol strings.

const SINGLE_BYTE_TOKENS: &[&str] = &[
    "",                  // 0 — unused / list-empty sentinel
    "xmlstreamend",      // 1
    "xmlstreamstart",    // 2
    "s.whatsapp.net",    // 3
    "type",              // 4
    "participant",       // 5
    "from",              // 6
    "to",                // 7
    "fallback_hostname", // 8
    "media",             // 9
    "notification",      // 10
    "0",                 // 11
    "1",                 // 12
    "ag",                // 13
    "message",           // 14
    "body",              // 15
    "response",          // 16
    "action",            // 17
    "list",              // 18
    "set",               // 19
    "delete",            // 20
    "urn:xmpp:whatsapp:push",  // 21
    "result",            // 22
    "groups_v2",         // 23
    "image",             // 24
    "count",             // 25
    "jid",               // 26
    "id",                // 27
    "g.us",              // 28
    "status",            // 29
    "subject",           // 30
    "broadcast",         // 31
    "success",           // 32
    "receipt",           // 33
    "error",             // 34
    "text",              // 35
    "ack",               // 36
    "category",          // 37
    "reason",            // 38
    "creation",          // 39
    "epoch",             // 40
    "relay",             // 41
    "value",             // 42
    "read",              // 43
    "call",              // 44
    "code",              // 45
    "query",             // 46
    "picture",           // 47
    "audio",             // 48
    "played",            // 49
    "last",              // 50
    "offer",             // 51
    "user",              // 52
    "enable",            // 53
    "critical_unblock_low",  // 54
    "accept",            // 55
    "ib",                // 56
    "timeout",           // 57
    "devices",           // 58
    "after",             // 59
    "props",             // 60
    "true",              // 61
    "false",             // 62
    "contact",           // 63
    "get",               // 64
    "order",             // 65
    "linked_devices_ts", // 66
    "encrypt",           // 67
    "key",               // 68
    "none",              // 69
    "identity",          // 70
    "all",               // 71
    "group",             // 72
    "item",              // 73
    "item_count",        // 74
    "video",             // 75
    "priority",          // 76
    "server-error",      // 77
    "w:gp2",             // 78
    "available",         // 79
    "admin",             // 80
    "owner",             // 81
    "mute",              // 82
    "create",            // 83
    "revoke",            // 84
    "encoding",          // 85
    "chatstate",         // 86
    "paused",            // 87
    "composing",         // 88
    "recording",         // 89
    "hash",              // 90
    "stanza",            // 91
    "lidJid",            // 92
    "pn",                // 93
    "dirty",             // 94
    "w:stats",           // 95
    "device_hash",       // 96
    "hostname",          // 97
    "edit",              // 98
    "2",                 // 99
    "3",                 // 100
    "subscribe",         // 101
    "w:web",             // 102
    "offline",           // 103
    "preview",           // 104
    "w:profile:picture", // 105
    "add",               // 106
    "remove",            // 107
    "demote",            // 108
    "promote",           // 109
    "ts",                // 110
    "reg_push",          // 111
    "config",            // 112
    "features",          // 113
    "web",               // 114
    "primary",           // 115
    "w:m",               // 116
    "voip",              // 117
    "display",           // 118
    "dc",                // 119
    "t",                 // 120
    "resource",          // 121
    "batch",             // 122
    "update",            // 123
    "msg",               // 124
    "device-list",       // 125
    "resume",            // 126
    "description",       // 127
    "business_hours",    // 128
    "categories",        // 129
    "delivery",          // 130
    "modify",            // 131
    "disappearing_mode", // 132
    "usync",             // 133
    "notice",            // 134
    "protocol",          // 135
    "v",                 // 136
    "lid",               // 137
    "not-found",         // 138
    "dns",               // 139
    "verified_name",     // 140
    "contact_remove",    // 141
    "profile",           // 142
    "side_list",         // 143
    "active",            // 144
    "passive",           // 145
    "terminate",         // 146
    "member_add_mode",   // 147
    "membership_approval_mode", // 148
    "locale",            // 149
    "default_membership_approval_mode", // 150
    "chat",              // 151
    "presence",          // 152
    "tag",               // 153
    "link_code_companion_reg", // 154
    "companion_identity_class", // 155
    "edge_routing",      // 156
    "routing_info",      // 157
    "hi",                // 158
    "4",                 // 159
    "5",                 // 160
    "6",                 // 161
    "7",                 // 162
    "8",                 // 163
    "9",                 // 164
    "10",                // 165
];

// Build a reverse lookup: string → token index.
fn build_token_map() -> HashMap<&'static str, u8> {
    let mut map = HashMap::new();
    for (i, &tok) in SINGLE_BYTE_TOKENS.iter().enumerate() {
        if !tok.is_empty() {
            map.insert(tok, i as u8);
        }
    }
    map
}

// ─── WaNode ─────────────────────────────────────────────────────

/// A node in WhatsApp's binary tree protocol.
#[derive(Debug, Clone, PartialEq)]
pub struct WaNode {
    /// Tag name (e.g. "message", "iq", "ack").
    pub tag: String,
    /// Key-value attributes.
    pub attrs: HashMap<String, String>,
    /// Node content.
    pub content: WaNodeContent,
}

/// Content of a WABinary node.
#[derive(Debug, Clone, PartialEq)]
pub enum WaNodeContent {
    /// No content.
    None,
    /// Raw binary data.
    Binary(Vec<u8>),
    /// UTF-8 text.
    Text(String),
    /// Child nodes.
    List(Vec<WaNode>),
}

impl WaNode {
    /// Create a new node with no content.
    pub fn new(tag: impl Into<String>, attrs: HashMap<String, String>) -> Self {
        Self {
            tag: tag.into(),
            attrs,
            content: WaNodeContent::None,
        }
    }

    /// Create a node with binary content.
    pub fn with_binary(
        tag: impl Into<String>,
        attrs: HashMap<String, String>,
        data: Vec<u8>,
    ) -> Self {
        Self {
            tag: tag.into(),
            attrs,
            content: WaNodeContent::Binary(data),
        }
    }

    /// Create a node with child nodes.
    pub fn with_children(
        tag: impl Into<String>,
        attrs: HashMap<String, String>,
        children: Vec<WaNode>,
    ) -> Self {
        Self {
            tag: tag.into(),
            attrs,
            content: WaNodeContent::List(children),
        }
    }

    /// Get an attribute value.
    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs.get(key).map(|s| s.as_str())
    }

    /// Find first child node with the given tag.
    pub fn child(&self, tag: &str) -> Option<&WaNode> {
        if let WaNodeContent::List(children) = &self.content {
            children.iter().find(|c| c.tag == tag)
        } else {
            None
        }
    }

    /// Get content as bytes.
    pub fn content_bytes(&self) -> Option<&[u8]> {
        match &self.content {
            WaNodeContent::Binary(b) => Some(b),
            WaNodeContent::Text(t) => Some(t.as_bytes()),
            _ => None,
        }
    }
}

// ─── Encoder ────────────────────────────────────────────────────

/// Encode a `WaNode` to WhatsApp's binary format.
pub fn encode(node: &WaNode) -> crate::Result<Vec<u8>> {
    let token_map = build_token_map();
    let mut buf = Vec::new();
    encode_node(&mut buf, node, &token_map)?;
    // Frame: 0 (flags) + big-endian u24 length + data.
    let len = buf.len();
    let mut framed = Vec::with_capacity(1 + 3 + len);
    framed.push(0); // flags
    framed.push(((len >> 16) & 0xFF) as u8);
    framed.push(((len >> 8) & 0xFF) as u8);
    framed.push((len & 0xFF) as u8);
    framed.extend_from_slice(&buf);
    Ok(framed)
}

fn encode_node(
    buf: &mut Vec<u8>,
    node: &WaNode,
    tokens: &HashMap<&str, u8>,
) -> crate::Result<()> {
    // A node is encoded as a list: [tag, attrs..., content]
    // List size = 1 (tag) + 2*num_attrs + has_content
    let num_attrs = node.attrs.len();
    let has_content = !matches!(node.content, WaNodeContent::None);
    let list_size = 1 + 2 * num_attrs + if has_content { 1 } else { 0 };

    write_list_start(buf, list_size);

    // Tag
    write_string(buf, &node.tag, tokens)?;

    // Attributes (key-value pairs)
    for (k, v) in &node.attrs {
        write_string(buf, k, tokens)?;
        write_string(buf, v, tokens)?;
    }

    // Content
    match &node.content {
        WaNodeContent::None => {}
        WaNodeContent::Binary(data) => {
            write_byte_length(buf, data.len());
            buf.extend_from_slice(data);
        }
        WaNodeContent::Text(text) => {
            write_string(buf, text, tokens)?;
        }
        WaNodeContent::List(children) => {
            write_list_start(buf, children.len());
            for child in children {
                encode_node(buf, child, tokens)?;
            }
        }
    }

    Ok(())
}

fn write_list_start(buf: &mut Vec<u8>, size: usize) {
    if size == 0 {
        buf.push(0x00); // LIST_EMPTY
    } else if size < 256 {
        buf.push(0xF8); // LIST_8
        buf.push(size as u8);
    } else {
        buf.push(0xF9); // LIST_16
        buf.push(((size >> 8) & 0xFF) as u8);
        buf.push((size & 0xFF) as u8);
    }
}

fn write_string(
    buf: &mut Vec<u8>,
    s: &str,
    tokens: &HashMap<&str, u8>,
) -> crate::Result<()> {
    // Check if the string is a known token.
    if let Some(&idx) = tokens.get(s) {
        buf.push(idx);
        return Ok(());
    }

    // Check if it's a JID (user@server).
    if let Some(at_pos) = s.find('@') {
        let user = &s[..at_pos];
        let server = &s[at_pos + 1..];
        if let Some(&server_token) = tokens.get(server) {
            buf.push(0xFC); // JID_PAIR
            write_string(buf, user, tokens)?;
            buf.push(server_token);
            return Ok(());
        }
    }

    // Fallback: write as a raw string with length prefix.
    let bytes = s.as_bytes();
    write_byte_length(buf, bytes.len());
    buf.extend_from_slice(bytes);
    Ok(())
}

fn write_byte_length(buf: &mut Vec<u8>, len: usize) {
    if len < 256 {
        buf.push(0xFE); // BINARY_8
        buf.push(len as u8);
    } else if len < 1 << 20 {
        buf.push(0xFF); // BINARY_20
        buf.push(((len >> 16) & 0x0F) as u8);
        buf.push(((len >> 8) & 0xFF) as u8);
        buf.push((len & 0xFF) as u8);
    } else {
        buf.push(0xFD); // BINARY_32
        buf.push(((len >> 24) & 0xFF) as u8);
        buf.push(((len >> 16) & 0xFF) as u8);
        buf.push(((len >> 8) & 0xFF) as u8);
        buf.push((len & 0xFF) as u8);
    }
}

// ─── Decoder ────────────────────────────────────────────────────

/// Decode a WABinary frame into a `WaNode`.
///
/// The input should start with the 1-byte flags + 3-byte length header.
pub fn decode(data: &[u8]) -> crate::Result<WaNode> {
    if data.len() < 4 {
        return Err(crate::Error::Binary("frame too short".into()));
    }
    let _flags = data[0];
    let len =
        ((data[1] as usize) << 16) | ((data[2] as usize) << 8) | (data[3] as usize);
    if data.len() < 4 + len {
        return Err(crate::Error::Binary(format!(
            "frame truncated: expected {} bytes, got {}",
            4 + len,
            data.len()
        )));
    }
    let mut cursor = Cursor::new(&data[4..4 + len]);
    decode_node(&mut cursor)
}

/// Decode from raw bytes (no frame header).
pub fn decode_raw(data: &[u8]) -> crate::Result<WaNode> {
    let mut cursor = Cursor::new(data);
    decode_node(&mut cursor)
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn read_u8(&mut self) -> crate::Result<u8> {
        if self.pos >= self.data.len() {
            return Err(crate::Error::Binary("unexpected end of data".into()));
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn read_u16(&mut self) -> crate::Result<u16> {
        let hi = self.read_u8()? as u16;
        let lo = self.read_u8()? as u16;
        Ok((hi << 8) | lo)
    }

    fn read_bytes(&mut self, n: usize) -> crate::Result<&'a [u8]> {
        if self.pos + n > self.data.len() {
            return Err(crate::Error::Binary("unexpected end of data".into()));
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }
}

fn decode_node(c: &mut Cursor<'_>) -> crate::Result<WaNode> {
    let list_size = read_list_size(c)?;
    if list_size == 0 {
        return Err(crate::Error::Binary("empty node list".into()));
    }

    let tag = read_string(c)?;
    let num_attrs = (list_size - 1) >> 1; // (list_size - 1 - has_content) / 2
    let has_content = (list_size - 1) % 2 == 1;

    let mut attrs = HashMap::new();
    for _ in 0..num_attrs {
        let key = read_string(c)?;
        let val = read_string(c)?;
        attrs.insert(key, val);
    }

    let content = if has_content {
        read_content(c)?
    } else {
        WaNodeContent::None
    };

    trace!(tag = %tag, attrs = ?attrs, "decoded WABinary node");

    Ok(WaNode {
        tag,
        attrs,
        content,
    })
}

fn read_list_size(c: &mut Cursor<'_>) -> crate::Result<usize> {
    let tag = c.read_u8()?;
    match tag {
        0x00 => Ok(0),       // LIST_EMPTY
        0xF8 => {            // LIST_8
            let size = c.read_u8()? as usize;
            Ok(size)
        }
        0xF9 => {            // LIST_16
            let size = c.read_u16()? as usize;
            Ok(size)
        }
        _ => Err(crate::Error::Binary(format!(
            "expected list tag, got 0x{tag:02X}"
        ))),
    }
}

fn read_string(c: &mut Cursor<'_>) -> crate::Result<String> {
    let tag = c.read_u8()?;
    match tag {
        // Token index (single byte) — look up in dictionary.
        1..=165 => {
            let s = SINGLE_BYTE_TOKENS
                .get(tag as usize)
                .copied()
                .unwrap_or("");
            Ok(s.to_string())
        }
        // BINARY_8: 1-byte length + raw bytes.
        0xFE => {
            let len = c.read_u8()? as usize;
            let bytes = c.read_bytes(len)?;
            String::from_utf8(bytes.to_vec())
                .map_err(|e| crate::Error::Binary(format!("invalid UTF-8: {e}")))
        }
        // BINARY_20: 20-bit length + raw bytes.
        0xFF => {
            let b0 = c.read_u8()? as usize;
            let b1 = c.read_u8()? as usize;
            let b2 = c.read_u8()? as usize;
            let len = ((b0 & 0x0F) << 16) | (b1 << 8) | b2;
            let bytes = c.read_bytes(len)?;
            String::from_utf8(bytes.to_vec())
                .map_err(|e| crate::Error::Binary(format!("invalid UTF-8: {e}")))
        }
        // BINARY_32: 4-byte length + raw bytes.
        0xFD => {
            let len = ((c.read_u8()? as usize) << 24)
                | ((c.read_u8()? as usize) << 16)
                | ((c.read_u8()? as usize) << 8)
                | (c.read_u8()? as usize);
            let bytes = c.read_bytes(len)?;
            String::from_utf8(bytes.to_vec())
                .map_err(|e| crate::Error::Binary(format!("invalid UTF-8: {e}")))
        }
        // JID_PAIR: user + server token.
        0xFC => {
            let user = read_string(c)?;
            let server_tag = c.read_u8()?;
            let server = SINGLE_BYTE_TOKENS
                .get(server_tag as usize)
                .copied()
                .unwrap_or("unknown");
            Ok(format!("{user}@{server}"))
        }
        // Empty / nil.
        0x00 => Ok(String::new()),
        _ => Err(crate::Error::Binary(format!(
            "unknown string tag 0x{tag:02X}"
        ))),
    }
}

fn read_content(c: &mut Cursor<'_>) -> crate::Result<WaNodeContent> {
    if c.remaining() == 0 {
        return Ok(WaNodeContent::None);
    }

    let peek = c.data[c.pos];
    match peek {
        // List of child nodes.
        0xF8 | 0xF9 => {
            let list_size = read_list_size(c)?;
            let mut children = Vec::with_capacity(list_size);
            for _ in 0..list_size {
                children.push(decode_node(c)?);
            }
            Ok(WaNodeContent::List(children))
        }
        // Binary data (8-bit length).
        0xFE => {
            c.pos += 1;
            let len = c.read_u8()? as usize;
            let data = c.read_bytes(len)?.to_vec();
            Ok(WaNodeContent::Binary(data))
        }
        // Binary data (20-bit length).
        0xFF => {
            c.pos += 1;
            let b0 = c.read_u8()? as usize;
            let b1 = c.read_u8()? as usize;
            let b2 = c.read_u8()? as usize;
            let len = ((b0 & 0x0F) << 16) | (b1 << 8) | b2;
            let data = c.read_bytes(len)?.to_vec();
            Ok(WaNodeContent::Binary(data))
        }
        // Binary data (32-bit length).
        0xFD => {
            c.pos += 1;
            let len = ((c.read_u8()? as usize) << 24)
                | ((c.read_u8()? as usize) << 16)
                | ((c.read_u8()? as usize) << 8)
                | (c.read_u8()? as usize);
            let data = c.read_bytes(len)?.to_vec();
            Ok(WaNodeContent::Binary(data))
        }
        // Token or raw string — treat as text content.
        _ => {
            let s = read_string(c)?;
            if s.is_empty() {
                Ok(WaNodeContent::None)
            } else {
                Ok(WaNodeContent::Text(s))
            }
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_map_has_expected_entries() {
        let map = build_token_map();
        assert_eq!(map.get("s.whatsapp.net"), Some(&3));
        assert_eq!(map.get("type"), Some(&4));
        assert_eq!(map.get("from"), Some(&6));
        assert_eq!(map.get("message"), Some(&14));
    }

    #[test]
    fn node_attr_getter() {
        let mut attrs = HashMap::new();
        attrs.insert("type".into(), "text".into());
        let node = WaNode::new("message", attrs);
        assert_eq!(node.attr("type"), Some("text"));
        assert_eq!(node.attr("missing"), None);
    }

    #[test]
    fn encode_decode_simple_node() {
        let mut attrs = HashMap::new();
        attrs.insert("type".into(), "text".into());
        let node = WaNode::with_binary("message", attrs, b"hello".to_vec());

        let encoded = encode(&node).unwrap();
        let decoded = decode(&encoded).unwrap();

        assert_eq!(decoded.tag, "message");
        assert_eq!(decoded.attr("type"), Some("text"));
        assert_eq!(decoded.content_bytes(), Some(b"hello".as_slice()));
    }

    #[test]
    fn encode_decode_node_with_children() {
        let child1 = WaNode::new("item", {
            let mut a = HashMap::new();
            a.insert("id".into(), "1".into());
            a
        });
        let child2 = WaNode::new("item", {
            let mut a = HashMap::new();
            a.insert("id".into(), "2".into());
            a
        });

        let parent = WaNode::with_children("list", HashMap::new(), vec![child1, child2]);
        let encoded = encode(&parent).unwrap();
        let decoded = decode(&encoded).unwrap();

        assert_eq!(decoded.tag, "list");
        if let WaNodeContent::List(children) = &decoded.content {
            assert_eq!(children.len(), 2);
            assert_eq!(children[0].tag, "item");
        } else {
            panic!("expected list content");
        }
    }

    #[test]
    fn decode_raw_empty_node() {
        // LIST_8(1) + token(14="message") → node with tag "message", no attrs, no content
        let data = [0xF8, 0x01, 14];
        let node = decode_raw(&data).unwrap();
        assert_eq!(node.tag, "message");
        assert!(node.attrs.is_empty());
    }
}
