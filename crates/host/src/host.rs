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
    pub screen: Arc<crate::screen::Screen>,
    pub push: Arc<crate::push::Push>,
    pub automations: Arc<crate::automations::Automations>,
}

pub fn os_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// The built-in "Quick tasks" workspace: open web on, every connector on, at
/// first; both can be changed like any workspace's.
fn ensure_quick_workspace(store: &Store, cfg: &HostConfig) -> Result<()> {
    let connectors = store
        .connector_list()?
        .into_iter()
        .map(|c| c.connector_id)
        .collect();
    store.workspace_ensure(&Workspace {
        workspace_id: QUICK_WORKSPACE_ID.into(),
        name: "Quick tasks".into(),
        path: cfg.scratch_dir().display().to_string(),
        kind: WorkspaceKind::Scratch,
        network: NetworkProfile {
            presets: vec![NetworkPreset::Web],
            custom_hosts: vec![],
        },
        connectors,
        created_at: 0,
        default_agent: None,
        default_profile_id: None,
    })
}

impl Host {
    /// Open storage and build the host. Does not start listening.
    pub fn open(cfg: HostConfig) -> Result<Arc<Host>> {
        cfg.prepare()?;
        let store = Arc::new(Store::open(&cfg.db_path())?);
        let host_id = store.ensure_host(os_name(), env!("CARGO_PKG_VERSION"))?;
        // Sessions from before the host titled them are named from their first message.
        match store.backfill_prompt_titles() {
            Ok(0) => {}
            Ok(n) => tracing::info!("titled {n} sessions from their first message"),
            Err(e) => tracing::warn!("could not title older sessions: {e:#}"),
        }
        ensure_quick_workspace(&store, &cfg)?;
        let secrets: Arc<dyn SecretStore> = Arc::from(crate::secrets::open(
            &store,
            &cfg.data_dir,
            &host_id,
            cfg.file_secrets,
        )?);
        let audit = crate::audit::Audit::new(Some(cfg.logs_dir().join("audit.jsonl")));
        let uploads = crate::uploads::Uploads::open(&cfg.data_dir)?;
        let bus = Bus::new();
        let voice = crate::voice::Voice::new(&cfg.data_dir, bus.clone());
        let ctx = Arc::new(Ctx {
            cfg,
            store,
            bus,
            host_id,
            gateway: OnceLock::new(),
            audit,
            uploads,
            voice,
        });
        let sessions = SessionManager::new(ctx.clone());
        let screen = crate::screen::Screen::new(ctx.clone());
        let push = crate::push::Push::new(ctx.clone(), secrets.clone());
        let automations = crate::automations::Automations::new(ctx.clone(), sessions.clone(), secrets.clone())?;
        Ok(Arc::new(Host {
            ctx,
            sessions,
            secrets,
            connectors: OnceLock::new(),
            devices: OnceLock::new(),
            screen,
            push,
            automations,
        }))
    }

    pub fn agent_kinds(&self) -> Vec<AgentKind> {
        let mut v = AgentKind::REAL.to_vec();
        if self.ctx.cfg.enable_fake_agent {
            v.push(AgentKind::Fake);
        }
        v
    }

    /// One entry per login: each agent's default login, then its profiles.
    pub fn agent_statuses(&self) -> Vec<AgentStatus> {
        let profiles = self.ctx.store.profile_list().unwrap_or_default();
        let mut out = vec![];
        for kind in self.agent_kinds() {
            out.push(crate::agents::status(kind, &self.ctx.cfg));
            for p in profiles.iter().filter(|p| p.agent == kind) {
                out.push(crate::agents::status_for(kind, Some(p), &self.ctx.cfg));
            }
        }
        out
    }

    pub fn info(&self) -> HostInfo {
        HostInfo {
            host_id: self.ctx.host_id.clone(),
            version: env!("CARGO_PKG_VERSION").into(),
            os: os_name().into(),
            protocol_version: PROTOCOL_VERSION,
            agents: self.agent_statuses(),
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
