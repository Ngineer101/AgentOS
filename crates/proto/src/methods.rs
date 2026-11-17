//! Method names, notification names and the capability gate.
//!
//! The gate lives here, in the shared crate, so the host and every client
//! agree on which methods a device may call.

use serde::{Deserialize, Serialize};

// ---- Methods (client -> host) ----
pub const HELLO: &str = "hello";
pub const HOST_INFO: &str = "host.info";
pub const HOST_CHECK: &str = "host.check";

pub const WORKSPACE_LIST: &str = "workspace.list";
pub const WORKSPACE_ADD: &str = "workspace.add";
pub const WORKSPACE_UPDATE: &str = "workspace.update";
pub const WORKSPACE_REMOVE: &str = "workspace.remove";

pub const SESSION_LIST: &str = "session.list";
pub const SESSION_GET: &str = "session.get";
pub const SESSION_CREATE: &str = "session.create";
pub const SESSION_PROMPT: &str = "session.prompt";
pub const SESSION_CANCEL: &str = "session.cancel";
pub const SESSION_END: &str = "session.end";

pub const EVENTS_SUBSCRIBE: &str = "events.subscribe";
pub const EVENTS_UNSUBSCRIBE: &str = "events.unsubscribe";

pub const APPROVAL_RESPOND: &str = "approval.respond";
pub const APPROVAL_LIST: &str = "approval.list";

pub const CONNECTOR_LIST: &str = "connector.list";
pub const CONNECTOR_ADD: &str = "connector.add";
pub const CONNECTOR_UPDATE: &str = "connector.update";
pub const CONNECTOR_REMOVE: &str = "connector.remove";
pub const CONNECTOR_SIGN_IN: &str = "connector.sign_in";
pub const CONNECTOR_REFRESH: &str = "connector.refresh";

pub const DEVICE_LIST: &str = "device.list";
pub const DEVICE_PAIR_START: &str = "device.pair_start";
pub const DEVICE_PAIR_CONFIRM: &str = "device.pair_confirm";
pub const DEVICE_RENAME: &str = "device.rename";
pub const DEVICE_REVOKE: &str = "device.revoke";

// ---- Notifications (host -> client) ----
/// A persisted event; params are an [`crate::Event`].
pub const N_EVENT: &str = "event";
/// An ephemeral streaming chunk that will be coalesced into a later event.
pub const N_SESSION_DELTA: &str = "session.delta";
/// A session was created or changed; params are a [`crate::Session`].
pub const N_SESSION_CHANGED: &str = "session.changed";
/// A pairing handshake finished and waits for confirmation on the desktop.
pub const N_PAIRING_PENDING: &str = "pairing.pending";
/// A pairing finished (confirmed, refused or expired).
pub const N_PAIRING_DONE: &str = "pairing.done";
/// Sent to a device right before the host closes its connection after revoking it.
pub const N_DEVICE_REVOKED: &str = "device.revoked";
/// A connector's status or tools changed on its own (sign-in finished or
/// failed). Params: `{"connector_id"}`. Admin connections only.
pub const N_CONNECTOR_CHANGED: &str = "connector.changed";

/// One permission a device holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Read host status, workspaces, connectors, devices.
    Read,
    /// Create, prompt, cancel and end sessions; subscribe to events.
    Sessions,
    /// Answer MCP approval requests.
    Approvals,
    /// Change workspaces, connectors, policy and devices. Desktop only.
    Admin,
}

impl Capability {
    /// Everything: the local desktop and command-line client.
    pub fn desktop() -> Vec<Capability> {
        vec![
            Capability::Read,
            Capability::Sessions,
            Capability::Approvals,
            Capability::Admin,
        ]
    }
    /// A paired phone: sessions and approvals, never policy.
    pub fn phone() -> Vec<Capability> {
        vec![
            Capability::Read,
            Capability::Sessions,
            Capability::Approvals,
        ]
    }
}

/// The capability a method requires, or `None` for an unknown method.
pub fn required_capability(method: &str) -> Option<Capability> {
    use Capability::*;
    Some(match method {
        HELLO | HOST_INFO | WORKSPACE_LIST | CONNECTOR_LIST | DEVICE_LIST | SESSION_LIST
        | SESSION_GET | APPROVAL_LIST => Read,
        SESSION_CREATE | SESSION_PROMPT | SESSION_CANCEL | SESSION_END | EVENTS_SUBSCRIBE
        | EVENTS_UNSUBSCRIBE => Sessions,
        APPROVAL_RESPOND => Approvals,
        HOST_CHECK | WORKSPACE_ADD | WORKSPACE_UPDATE | WORKSPACE_REMOVE | CONNECTOR_ADD
        | CONNECTOR_UPDATE | CONNECTOR_REMOVE | CONNECTOR_SIGN_IN | CONNECTOR_REFRESH
        | DEVICE_PAIR_START | DEVICE_PAIR_CONFIRM | DEVICE_RENAME | DEVICE_REVOKE => Admin,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_cannot_widen_policy() {
        let phone = Capability::phone();
        for m in [
            WORKSPACE_ADD,
            WORKSPACE_UPDATE,
            WORKSPACE_REMOVE,
            CONNECTOR_ADD,
            CONNECTOR_UPDATE,
            CONNECTOR_REMOVE,
            DEVICE_PAIR_START,
            DEVICE_REVOKE,
        ] {
            assert!(!phone.contains(&required_capability(m).unwrap()), "{m}");
        }
        for m in [
            SESSION_CREATE,
            SESSION_PROMPT,
            APPROVAL_RESPOND,
            EVENTS_SUBSCRIBE,
            HOST_INFO,
        ] {
            assert!(phone.contains(&required_capability(m).unwrap()), "{m}");
        }
    }
}
