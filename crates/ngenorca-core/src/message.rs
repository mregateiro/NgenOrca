//! Message types flowing through NgenOrca.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{ChannelId, ChannelKind, SessionId, TrustLevel, UserId};

/// A message flowing through the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Unique message ID (ULID for ordering).
    pub id: crate::EventId,

    /// Timestamp when the message was created.
    pub timestamp: DateTime<Utc>,

    /// Resolved user identity (if known).
    pub user_id: Option<UserId>,

    /// Trust level of the sender's identity.
    pub trust: TrustLevel,

    /// Session this message belongs to.
    pub session_id: SessionId,

    /// Source channel.
    pub channel: ChannelId,

    /// Kind of channel.
    pub channel_kind: ChannelKind,

    /// Message direction.
    pub direction: Direction,

    /// Message content.
    pub content: Content,

    /// Optional metadata (channel-specific headers, etc.).
    pub metadata: serde_json::Value,
}

/// Whether a message is inbound (from user) or outbound (from agent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    /// User → Agent
    Inbound,
    /// Agent → User
    Outbound,
    /// System event (internal)
    System,
}

/// Message content variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Content {
    /// Plain text message.
    Text(String),

    /// Image with optional caption.
    Image {
        url: String,
        caption: Option<String>,
        mime_type: String,
    },

    /// Audio message.
    Audio {
        url: String,
        duration_secs: Option<f64>,
        transcript: Option<String>,
    },

    /// Video message.
    Video {
        url: String,
        duration_secs: Option<f64>,
        caption: Option<String>,
    },

    /// File/document.
    File {
        url: String,
        filename: String,
        mime_type: String,
    },

    /// Tool call request (agent → tool).
    ToolCall {
        tool_name: String,
        arguments: serde_json::Value,
        call_id: String,
    },

    /// Tool call result (tool → agent).
    ToolResult {
        call_id: String,
        result: serde_json::Value,
        is_error: bool,
    },

    /// Structured data (for plugin communication).
    Structured {
        kind: String,
        data: serde_json::Value,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn sample_message() -> Message {
        Message {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            user_id: Some(UserId("alice".into())),
            trust: TrustLevel::Channel,
            session_id: SessionId::new(),
            channel: ChannelId("webchat".into()),
            channel_kind: ChannelKind::WebChat,
            direction: Direction::Inbound,
            content: Content::Text("hello world".into()),
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn message_serde_roundtrip() {
        let msg = sample_message();
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg.id, back.id);
        assert_eq!(msg.user_id, back.user_id);
        assert_eq!(msg.trust, back.trust);
    }

    #[test]
    fn content_text_serde() {
        let c = Content::Text("hi".into());
        let json = serde_json::to_string(&c).unwrap();
        let back: Content = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Content::Text(s) if s == "hi"));
    }

    #[test]
    fn content_image_serde() {
        let c = Content::Image {
            url: "https://example.com/img.png".into(),
            caption: Some("logo".into()),
            mime_type: "image/png".into(),
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: Content = serde_json::from_str(&json).unwrap();
        if let Content::Image { url, caption, .. } = back {
            assert_eq!(url, "https://example.com/img.png");
            assert_eq!(caption, Some("logo".into()));
        } else {
            panic!("Expected Image variant");
        }
    }

    #[test]
    fn content_tool_call_serde() {
        let c = Content::ToolCall {
            call_id: "call-1".into(),
            tool_name: "search".into(),
            arguments: serde_json::json!({"q": "rust"}),
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: Content = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Content::ToolCall { .. }));
    }

    #[test]
    fn content_tool_result_serde() {
        let c = Content::ToolResult {
            call_id: "call-1".into(),
            result: serde_json::json!({"answer": 42}),
            is_error: false,
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: Content = serde_json::from_str(&json).unwrap();
        if let Content::ToolResult { is_error, .. } = back {
            assert!(!is_error);
        } else {
            panic!("Expected ToolResult");
        }
    }

    #[test]
    fn direction_serde_roundtrip() {
        for dir in &[Direction::Inbound, Direction::Outbound, Direction::System] {
            let json = serde_json::to_string(dir).unwrap();
            let back: Direction = serde_json::from_str(&json).unwrap();
            assert_eq!(*dir, back);
        }
    }

    #[test]
    fn message_without_optional_fields() {
        let msg = Message {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            user_id: None,
            trust: TrustLevel::Unknown,
            session_id: SessionId::new(),
            channel: ChannelId("system".into()),
            channel_kind: ChannelKind::WebChat,
            direction: Direction::System,
            content: Content::Text("system boot".into()),
            metadata: serde_json::Value::Null,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert!(back.user_id.is_none());
    }

    #[test]
    fn message_metadata_stores_arbitrary_data() {
        let mut msg = sample_message();
        msg.metadata = serde_json::json!({"custom_key": "custom_value"});
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.metadata.get("custom_key").unwrap(),
            &serde_json::json!("custom_value")
        );
    }
}
