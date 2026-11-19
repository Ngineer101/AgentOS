//! agenthost daemon library.
//!
//! One daemon owns agents, sandboxes, credentials and data. Clients (desktop,
//! CLI, phone) speak one JSON-RPC protocol to it; see `agenthost-proto`.

pub mod acp;
pub mod agents;
pub mod api;
pub mod audit;
pub mod bus;
pub mod config;
pub mod ctx;
pub mod host;
pub mod gitremote;
pub mod installer;
pub mod mcp;
pub mod profiles;
pub mod projects;
pub mod proxy;
pub mod relay_dev;
pub mod remote;
pub mod sandbox;
pub mod screen;
pub mod secrets;
pub mod session;
pub mod store;
pub mod uploads;
pub mod util;
pub mod voice;
pub mod worktree;
pub mod integrations;

pub use config::HostConfig;
pub use host::Host;

use std::sync::Arc;

/// Start serving: resume live sessions and listen on the Unix socket.
pub async fn start(host: Arc<Host>) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let socket = host.ctx.cfg.socket_path.clone();
    let gw = mcp::Gateway::start(host.ctx.clone(), host.secrets.clone(), &host.sessions).await?;
    let _ = host.ctx.gateway.set(gw.clone());
    let _ = host.connectors.set(Arc::new(gw));
    let remote = remote::Remote::new(&host)?;
    let _ = host.devices.set(remote.clone());
    remote.spawn();
    let task = api::serve_unix(host.clone(), &socket).await?;
    let resumed = host.sessions.resume_all()?;
    tracing::info!(socket = %socket.display(), resumed, "host listening");
    Ok(task)
}
