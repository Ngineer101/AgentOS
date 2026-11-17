//! Shared protocol for agenthost.
//!
//! Both the desktop (over the host's Unix socket) and the phone (over the
//! encrypted relay tunnel) speak exactly this protocol. It is JSON-RPC 2.0
//! with one message per line on stream transports.

pub mod jsonrpc;
pub mod methods;
pub mod tunnel;
pub mod types;

pub use jsonrpc::{Message, Notification, Request, Response, RpcError};
pub use types::*;

/// Version of the client protocol. `hello` refuses clients on an older version.
///
/// 2: attachments (`upload.*`, `session.prompt` `attachments`) and voice input
/// (`voice.*`). Clients show those controls only for hosts on 2 or newer.
pub const PROTOCOL_VERSION: u32 = 2;

/// Oldest client protocol version the host still accepts.
pub const MIN_PROTOCOL_VERSION: u32 = 1;
