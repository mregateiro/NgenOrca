//! Typed event subscribers with filtering.

use ngenorca_core::event::{Event, EventPayload};
use tokio::sync::broadcast;

/// A filtered subscriber that only receives events matching a predicate.
pub struct FilteredSubscriber {
    rx: broadcast::Receiver<Event>,
    filter: Box<dyn Fn(&Event) -> bool + Send>,
}

impl FilteredSubscriber {
    /// Create a subscriber that receives all events.
    pub fn all(rx: broadcast::Receiver<Event>) -> Self {
        Self {
            rx,
            filter: Box::new(|_| true),
        }
    }

    /// Create a subscriber that only receives message events.
    pub fn messages_only(rx: broadcast::Receiver<Event>) -> Self {
        Self {
            rx,
            filter: Box::new(|e| matches!(e.payload, EventPayload::Message(_))),
        }
    }

    /// Create a subscriber that only receives events for a specific session.
    pub fn for_session(
        rx: broadcast::Receiver<Event>,
        session_id: ngenorca_core::SessionId,
    ) -> Self {
        Self {
            rx,
            filter: Box::new(move |e| e.session_id.as_ref() == Some(&session_id)),
        }
    }

    /// Receive the next matching event.
    pub async fn recv(&mut self) -> Option<Event> {
        loop {
            match self.rx.recv().await {
                Ok(event) if (self.filter)(&event) => return Some(event),
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Subscriber lagged by {} events", n);
                    continue;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ngenorca_core::event::{Event, EventPayload};
    use ngenorca_core::message::{Content, Direction, Message};
    use ngenorca_core::types::*;
    use tokio::sync::broadcast;

    fn make_event(payload: EventPayload) -> Event {
        Event {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            session_id: Some(SessionId::new()),
            user_id: None,
            payload,
        }
    }

    fn make_message_event() -> Event {
        let msg = Message {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            user_id: None,
            trust: TrustLevel::Unknown,
            session_id: SessionId::new(),
            channel: ChannelId("test".into()),
            channel_kind: ChannelKind::WebChat,
            direction: Direction::Inbound,
            content: Content::Text("hello".into()),
            metadata: serde_json::Value::Null,
        };
        make_event(EventPayload::Message(msg))
    }

    #[tokio::test]
    async fn all_subscriber_receives_everything() {
        let (tx, rx) = broadcast::channel(16);
        let mut sub = FilteredSubscriber::all(rx);

        let event = make_event(EventPayload::SessionCreated {
            session_id: SessionId::new(),
            user_id: None,
        });
        tx.send(event).unwrap();

        let received = sub.recv().await;
        assert!(received.is_some());
    }

    #[tokio::test]
    async fn messages_only_filters_non_messages() {
        let (tx, rx) = broadcast::channel(16);
        let mut sub = FilteredSubscriber::messages_only(rx);

        // Send a non-message event
        let non_msg = make_event(EventPayload::SessionCreated {
            session_id: SessionId::new(),
            user_id: None,
        });
        tx.send(non_msg).unwrap();

        // Send a message event
        let msg_event = make_message_event();
        let msg_id = msg_event.id.clone();
        tx.send(msg_event).unwrap();

        let received = sub.recv().await.unwrap();
        assert_eq!(received.id, msg_id);
    }

    #[tokio::test]
    async fn for_session_filters_by_session() {
        let (tx, rx) = broadcast::channel(16);
        let target_session = SessionId::new();
        let mut sub = FilteredSubscriber::for_session(rx, target_session.clone());

        // Send event for different session
        let other = make_event(EventPayload::SessionCreated {
            session_id: SessionId::new(),
            user_id: None,
        });
        tx.send(other).unwrap();

        // Send event for target session
        let mut target = make_event(EventPayload::SessionCreated {
            session_id: target_session.clone(),
            user_id: None,
        });
        target.session_id = Some(target_session.clone());
        let target_id = target.id.clone();
        tx.send(target).unwrap();

        let received = sub.recv().await.unwrap();
        assert_eq!(received.id, target_id);
    }
}
