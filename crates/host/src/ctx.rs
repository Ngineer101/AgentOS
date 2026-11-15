//! Shared host state handed to every component.

use crate::bus::{Bus, BusMsg};
use crate::config::HostConfig;
use crate::store::Store;
use agenthost_proto::{Event, EventType, Session, SessionState};
use anyhow::Result;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// Implemented by the MCP gateway; sessions register with it.
pub trait GatewayApi: Send + Sync {
    /// Register a session; returns the URL the agent should use.
    fn register_session(&self, session_id: &str, workspace_id: &str) -> Option<String>;
    fn unregister_session(&self, session_id: &str);
    /// Unix socket the egress proxy forwards the internal gateway name to.
    fn socket_path(&self) -> PathBuf;
}

pub struct Ctx {
    pub cfg: HostConfig,
    pub store: Arc<Store>,
    pub bus: Bus,
    pub host_id: String,
    pub gateway: OnceLock<Arc<dyn GatewayApi>>,
    pub audit: crate::audit::Audit,
}

impl Ctx {
    /// Append an event and publish it.
    pub fn append(&self, session_id: &str, kind: EventType, payload: Value) -> Result<Event> {
        let ev = self.store.append_event(session_id, kind, payload)?;
        self.bus.publish(BusMsg::Event(ev.clone()));
        Ok(ev)
    }

    /// Change state, record it, publish both the event and the session.
    pub fn set_state(
        &self,
        session_id: &str,
        state: SessionState,
        reason: Option<&str>,
        extra: Option<Value>,
    ) -> Result<Session> {
        let (s, ev) = self.store.set_state(session_id, state, reason, extra)?;
        self.bus.publish(BusMsg::Event(ev));
        self.bus.publish(BusMsg::SessionChanged(s.clone()));
        Ok(s)
    }

    pub fn publish_session(&self, session_id: &str) {
        if let Ok(Some(s)) = self.store.session_get(session_id) {
            self.bus.publish(BusMsg::SessionChanged(s));
        }
    }
}
