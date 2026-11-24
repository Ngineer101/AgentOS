//! In-process fan-out of events to subscribed clients.
//!
//! Persisted events always go to SQLite first and are published here after.
//! A subscriber that falls behind re-reads from SQLite, so a slow client never
//! loses events.

use agenthost_proto::{AutomationChanged, Event, NoteChanged, PairingPending, ScreenStatus, Session, SessionDelta, VoiceStatus};
use serde_json::Value;
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub enum BusMsg {
    Event(Event),
    Delta(SessionDelta),
    SessionChanged(Session),
    PairingPending(PairingPending),
    PairingDone {
        pairing_id: String,
        result: Value,
    },
    /// A device was revoked; its connections must close.
    DeviceRevoked(String),
    /// A connector's status or tools changed (by id).
    ConnectorChanged(String),
    /// The speech model's status changed.
    VoiceChanged(VoiceStatus),
    /// Screen sharing started, stopped, or its policy changed.
    ScreenChanged(ScreenStatus),
    /// An automation changed, or a run of it started or finished.
    AutomationChanged(AutomationChanged),
    /// A note was created, changed or deleted.
    NoteChanged(NoteChanged),
}

#[derive(Clone)]
pub struct Bus {
    tx: broadcast::Sender<BusMsg>,
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(4096);
        Self { tx }
    }
    pub fn publish(&self, msg: BusMsg) {
        let _ = self.tx.send(msg);
    }
    pub fn subscribe(&self) -> broadcast::Receiver<BusMsg> {
        self.tx.subscribe()
    }
}
