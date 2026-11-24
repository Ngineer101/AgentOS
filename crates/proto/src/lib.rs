//! Shared protocol for AgentOS.
//!
//! Both the desktop (over the host's Unix socket) and the phone (over the
//! encrypted relay tunnel) speak exactly this protocol. It is JSON-RPC 2.0
//! with one message per line on stream transports.

pub mod automation;
pub mod hookseal;
pub mod jsonrpc;
pub mod methods;
pub mod screen;
pub mod tunnel;
pub mod types;

pub use jsonrpc::{Message, Notification, Request, Response, RpcError};
pub use automation::*;
pub use types::*;

/// Version of the client protocol. `hello` refuses clients on an older version.
///
/// 2: attachments (`upload.*`, `session.prompt` `attachments`) and voice input
/// (`voice.*`). Clients show those controls only for hosts on 2 or newer.
/// 3: screen sharing (`screen.*`).
/// 4: push notifications (`push.*`).
/// 5: automations (`automation.*`), `Session.automation_id`.
/// 6: notifications through a relay (`push.register` `gateway`), so the
/// host needs no APNs key.
/// 7: images sent with prompts can be read back (`attachment.read`).
/// 8: pinned sessions (`session.pin`, `Session.pinned_at`).
pub const PROTOCOL_VERSION: u32 = 8;

/// Oldest client protocol version the host still accepts.
pub const MIN_PROTOCOL_VERSION: u32 = 1;
