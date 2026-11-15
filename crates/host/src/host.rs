//! The host: wires the store, sessions, gateway and remote access together.

use crate::api::Principal;
use crate::bus::Bus;
use crate::config::HostConfig;
use crate::ctx::Ctx;
use crate::secrets::SecretStore;
use crate::session::SessionManager;
use crate::store::Store;
use agenthost_proto::jsonrpc::{codes, RpcError};
use agenthost_proto::*;
use anyhow::Result;
use serde_json::Value;
use std::sync::{Arc, OnceLock};

/// Connector management, implemented by the MCP gateway.
#[async_trait::async_trait]
pub trait ConnectorAdmin: Send + Sync {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError>;
    fn pending(&self) -> Vec<PendingApproval>;
    fn respond(&self, p: &ApprovalRespondParams, who: &Principal) -> Result<(), RpcError>;
}

/// Device pairing and revocation, implemented by remote access.
#[async_trait::async_trait]
pub trait DeviceAdmin: Send + Sync {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError>;
    fn relay_status(&self) -> RelayStatus;
    /// Disconnect from the relay and stop reconnecting.
    fn shutdown(&self) {}
}

pub struct Host {
    pub ctx: Arc<Ctx>,
    pub sessions: Arc<SessionManager>,
    pub secrets: Arc<dyn SecretStore>,
    pub connectors: OnceLock<Arc<dyn ConnectorAdmin>>,
    pub devices: OnceLock<Arc<dyn DeviceAdmin>>,
}

pub fn os_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

impl Host {
    /// Open storage and build the host. Does not start listening.
    pub fn open(cfg: HostConfig) -> Result<Arc<Host>> {
        cfg.prepare()?;
        let store = Arc::new(Store::open(&cfg.db_path())?);
        let host_id = store.ensure_host(os_name(), env!("CARGO_PKG_VERSION"))?;
        let secrets: Arc<dyn SecretStore> = Arc::from(crate::secrets::open(
            &store,
            &cfg.data_dir,
            &host_id,
            cfg.file_secrets,
        )?);
        let audit = crate::audit::Audit::new(Some(cfg.logs_dir().join("audit.jsonl")));
        let ctx = Arc::new(Ctx {
            cfg,
            store,
            bus: Bus::new(),
            host_id,
            gateway: OnceLock::new(),
            audit,
        });
        let sessions = SessionManager::new(ctx.clone());
        Ok(Arc::new(Host {
            ctx,
            sessions,
            secrets,
            connectors: OnceLock::new(),
            devices: OnceLock::new(),
        }))
    }

    pub fn agent_kinds(&self) -> Vec<AgentKind> {
        let mut v = AgentKind::REAL.to_vec();
        if self.ctx.cfg.enable_fake_agent {
            v.push(AgentKind::Fake);
        }
        v
    }

    pub fn info(&self) -> HostInfo {
        HostInfo {
            host_id: self.ctx.host_id.clone(),
            version: env!("CARGO_PKG_VERSION").into(),
            os: os_name().into(),
            protocol_version: PROTOCOL_VERSION,
            agents: self
                .agent_kinds()
                .into_iter()
                .map(|k| crate::agents::status(k, &self.ctx.cfg))
                .collect(),
            sandbox: crate::sandbox::health(&self.ctx.cfg),
            relay: self
                .devices
                .get()
                .map(|d| d.relay_status())
                .unwrap_or_default(),
        }
    }

    pub fn gateway_pending(&self) -> Vec<PendingApproval> {
        self.connectors
            .get()
            .map(|c| c.pending())
            .unwrap_or_default()
    }

    pub fn approval_respond(
        &self,
        p: &ApprovalRespondParams,
        who: &Principal,
    ) -> Result<(), RpcError> {
        match self.connectors.get() {
            Some(c) => c.respond(p, who),
            None => Err(RpcError::not_found("no such approval")),
        }
    }

    pub async fn connector_call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        match self.connectors.get() {
            Some(c) => c.call(method, params).await,
            None if method == agenthost_proto::methods::CONNECTOR_LIST => Ok(Value::Array(vec![])),
            None => Err(RpcError::new(
                codes::UNAVAILABLE,
                "the MCP gateway is not running",
            )),
        }
    }

    pub async fn device_call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        match self.devices.get() {
            Some(d) => d.call(method, params).await,
            None if method == agenthost_proto::methods::DEVICE_LIST => {
                let list: Vec<Device> = self
                    .ctx
                    .store
                    .device_list(false)
                    .map_err(|e| RpcError::internal(e.to_string()))?
                    .into_iter()
                    .map(|d| d.device)
                    .collect();
                Ok(serde_json::to_value(list).unwrap())
            }
            None => Err(RpcError::new(
                codes::UNAVAILABLE,
                "remote access is not configured (start the host with --relay-url)",
            )),
        }
    }
}
