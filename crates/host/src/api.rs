//! Client API: JSON-RPC 2.0 over any line/frame transport.
//!
//! The same handler serves the local Unix socket (desktop, CLI) and the
//! encrypted relay tunnel (phone). Only the [`Principal`] differs: local
//! connections get every capability, paired devices get what they were
//! granted at pairing.

use crate::bus::BusMsg;
use crate::host::Host;
use crate::util::now_ms;
use agenthost_proto::jsonrpc::{codes, Message, Notification, Request, Response, RpcError};
use agenthost_proto::methods::{self as m, Capability};
use agenthost_proto::*;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

/// Who is on the other end of a connection.
#[derive(Debug, Clone)]
pub struct Principal {
    /// `None` for the local socket.
    pub device_id: Option<String>,
    pub capabilities: Vec<Capability>,
    /// For logs and the `device` field of prompts.
    pub label: String,
}

impl Principal {
    pub fn local() -> Self {
        Self {
            device_id: None,
            capabilities: Capability::desktop(),
            label: "local".into(),
        }
    }
}

/// Serve the Unix socket until the listener fails.
pub async fn serve_unix(host: Arc<Host>, path: &Path) -> anyhow::Result<JoinHandle<()>> {
    if path.exists() {
        // A stale socket from a previous run; refuse if another host answers.
        if UnixStream::connect(path).await.is_ok() {
            anyhow::bail!("another host is already listening on {}", path.display());
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            // Owner-only: the socket mode already restricts this; double-check the peer.
            if let Ok(cred) = stream.peer_cred() {
                if cred.uid() != unsafe { libc::getuid() } {
                    host.ctx
                        .audit
                        .log("socket.refused", json!({"uid": cred.uid()}));
                    continue;
                }
            }
            let host = host.clone();
            tokio::spawn(async move { serve_stream(host, stream).await });
        }
    });
    Ok(task)
}

async fn serve_stream(host: Arc<Host>, stream: UnixStream) {
    let (r, mut w) = stream.into_split();
    let (in_tx, in_rx) = mpsc::channel::<String>(256);
    let (out_tx, mut out_rx) = mpsc::channel::<String>(1024);
    let writer = tokio::spawn(async move {
        while let Some(line) = out_rx.recv().await {
            if w.write_all(line.as_bytes()).await.is_err() || w.write_all(b"\n").await.is_err() {
                break;
            }
        }
    });
    let reader = tokio::spawn(async move {
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if in_tx.send(line).await.is_err() {
                break;
            }
        }
    });
    serve_connection(host, Principal::local(), in_rx, out_tx).await;
    reader.abort();
    let _ = writer.await;
}

struct Conn {
    host: Arc<Host>,
    principal: Principal,
    out: mpsc::Sender<String>,
    subs: HashMap<String, JoinHandle<()>>,
    watchers: Vec<JoinHandle<()>>,
    greeted: bool,
    pending_after: Option<After>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        for (_, t) in self.subs.drain() {
            t.abort();
        }
        for t in self.watchers.drain(..) {
            t.abort();
        }
    }
}

/// Serve one connection: messages in on `inbound`, out on `outbound`.
/// Returns when the client goes away or the device is revoked.
pub async fn serve_connection(
    host: Arc<Host>,
    principal: Principal,
    mut inbound: mpsc::Receiver<String>,
    outbound: mpsc::Sender<String>,
) {
    let mut bus = host.ctx.bus.subscribe();
    let mut conn = Conn {
        host,
        principal,
        out: outbound,
        subs: HashMap::new(),
        watchers: vec![],
        greeted: false,
        pending_after: None,
    };
    loop {
        tokio::select! {
            line = inbound.recv() => {
                let Some(line) = line else { break };
                if line.trim().is_empty() { continue; }
                match Message::parse(&line) {
                    Ok(Message::Request(req)) => {
                        let id = req.id.clone();
                        let resp = match conn.handle(req).await {
                            Ok(v) => Response::ok(id, v),
                            Err(e) => Response::err(id, e),
                        };
                        if conn.out.send(Message::Response(resp).to_json()).await.is_err() { break; }
                        conn.after_response().await;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        let _ = conn.out.send(Message::Response(Response::err(Value::Null, e)).to_json()).await;
                    }
                }
            }
            msg = bus.recv() => {
                match msg {
                    Ok(BusMsg::DeviceRevoked(id)) if conn.principal.device_id.as_deref() == Some(id.as_str()) => {
                        let n = Notification::new(m::N_DEVICE_REVOKED, json!({"device_id": id, "message": "This device was removed"}));
                        let _ = conn.out.send(Message::Notification(n).to_json()).await;
                        break;
                    }
                    Ok(BusMsg::PairingPending(p)) if conn.principal.capabilities.contains(&Capability::Admin) && conn.greeted => {
                        conn.notify(m::N_PAIRING_PENDING, serde_json::to_value(p).unwrap()).await;
                    }
                    Ok(BusMsg::PairingDone { pairing_id, result }) if conn.principal.capabilities.contains(&Capability::Admin) && conn.greeted => {
                        let mut v = result;
                        v["pairing_id"] = json!(pairing_id);
                        conn.notify(m::N_PAIRING_DONE, v).await;
                    }
                    Ok(BusMsg::ConnectorChanged(id)) if conn.principal.capabilities.contains(&Capability::Admin) && conn.greeted => {
                        conn.notify(m::N_CONNECTOR_CHANGED, json!({"connector_id": id})).await;
                    }
                    Ok(BusMsg::VoiceChanged(v)) if conn.greeted => {
                        conn.notify(m::N_VOICE_CHANGED, serde_json::to_value(v).unwrap()).await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    _ => {}
                }
            }
        }
    }
}

fn params<T: DeserializeOwned>(v: Value) -> Result<T, RpcError> {
    let v = if v.is_null() { json!({}) } else { v };
    serde_json::from_value(v).map_err(|e| RpcError::invalid_params(e.to_string()))
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::internal(e.to_string())
}

fn to_value<T: serde::Serialize>(v: T) -> Result<Value, RpcError> {
    serde_json::to_value(v).map_err(internal)
}

/// Work to do right after a response is sent (replay after `events.subscribe`).
enum After {
    Replay {
        session_id: String,
        after_seq: i64,
        sub_id: String,
    },
}

impl Conn {
    async fn notify(&self, method: &str, params: Value) {
        let _ = self
            .out
            .send(Message::Notification(Notification::new(method, params)).to_json())
            .await;
    }

    async fn handle(&mut self, req: Request) -> Result<Value, RpcError> {
        let method = req.method.as_str();
        if !self.greeted && method != m::HELLO {
            return Err(RpcError::new(
                codes::HELLO_REQUIRED,
                "the first message must be hello",
            ));
        }
        let Some(cap) = m::required_capability(method) else {
            return Err(RpcError::new(
                codes::METHOD_NOT_FOUND,
                format!("unknown method {method}"),
            ));
        };
        if !self.principal.capabilities.contains(&cap) {
            self.host.ctx.audit.log(
                "capability.denied",
                json!({"method": method, "device_id": self.principal.device_id, "label": self.principal.label}),
            );
            return Err(RpcError::new(
                codes::CAPABILITY_DENIED,
                format!("this device may not call {method}"),
            )
            .with_data(json!({"required": cap})));
        }
        if let Some(d) = &self.principal.device_id {
            let _ = self.host.ctx.store.device_seen(d);
        }
        let h = self.host.clone();
        let p = req.params;
        match method {
            m::HELLO => {
                let hp: HelloParams = params(p)?;
                if hp.protocol_version < MIN_PROTOCOL_VERSION {
                    return Err(RpcError::new(
                        codes::UPDATE_REQUIRED,
                        format!(
                            "update required: this host needs client protocol {MIN_PROTOCOL_VERSION} or newer, the client speaks {}",
                            hp.protocol_version
                        ),
                    )
                    .with_data(json!({"min_protocol_version": MIN_PROTOCOL_VERSION, "protocol_version": PROTOCOL_VERSION})));
                }
                self.greeted = true;
                to_value(HelloResult {
                    host_id: h.ctx.host_id.clone(),
                    host_version: env!("CARGO_PKG_VERSION").into(),
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: self.principal.capabilities.clone(),
                    device_id: self.principal.device_id.clone(),
                })
            }
            m::HOST_INFO | m::HOST_CHECK => to_value(h.info()),

            m::PROJECT_BROWSE => to_value(crate::projects::browse(&h, params(p)?)?),
            m::PROJECT_CREATE => to_value(crate::projects::create(&h, params(p)?)?),
            m::PROJECT_SET_ROOTS => to_value(crate::projects::set_roots(&h, params(p)?)?),

            m::AGENT_PROFILE_LIST => to_value(h.ctx.store.profile_list().map_err(internal)?),
            m::AGENT_PROFILE_ADD => to_value(crate::profiles::add(&h, params(p)?)?),
            m::AGENT_PROFILE_REMOVE => {
                let id: IdParams = params(p)?;
                crate::profiles::remove(&h, &id.id)?;
                Ok(json!({"removed": true}))
            }

            m::WORKSPACE_LIST => to_value(h.ctx.store.workspace_list().map_err(internal)?),
            m::WORKSPACE_ADD => to_value(workspace_add(&h, params(p)?)?),
            m::WORKSPACE_UPDATE => to_value(workspace_update(&h, params(p)?)?),
            m::WORKSPACE_REMOVE => {
                let id: IdParams = params(p)?;
                if id.id == QUICK_WORKSPACE_ID {
                    return Err(RpcError::invalid_params(
                        "Quick tasks is built in; turn its open web or connectors off instead",
                    ));
                }
                let live = h
                    .ctx
                    .store
                    .session_list(Some(&id.id))
                    .map_err(internal)?
                    .into_iter()
                    .filter(|s| !s.state.is_terminal())
                    .count();
                if live > 0 {
                    return Err(RpcError::invalid_state(format!(
                        "end the {live} live session(s) in this workspace first"
                    )));
                }
                if !h.ctx.store.workspace_remove(&id.id).map_err(internal)? {
                    return Err(RpcError::not_found("no such workspace"));
                }
                Ok(json!({"removed": true}))
            }

            m::SESSION_LIST => {
                let lp: SessionListParams = params(p)?;
                let list: Vec<Session> = h
                    .ctx
                    .store
                    .session_list(lp.workspace_id.as_deref())
                    .map_err(internal)?
                    .into_iter()
                    .filter(|s| s.archived_at.is_some() == lp.archived)
                    .collect();
                if lp.watch {
                    self.watch_sessions(lp.workspace_id.clone());
                }
                to_value(list)
            }
            m::SESSION_GET => {
                let r: SessionRef = params(p)?;
                let s = h
                    .ctx
                    .store
                    .session_get(&r.session_id)
                    .map_err(internal)?
                    .ok_or_else(|| RpcError::not_found("no such session"))?;
                to_value(s)
            }
            m::SESSION_CREATE => {
                let cp: SessionCreateParams = params(p)?;
                to_value(h.sessions.create(cp, Some(&self.principal.label)).await?)
            }
            m::SESSION_SET_CONFIG => {
                let sp: SessionSetConfigParams = params(p)?;
                to_value(h.sessions.set_config(sp).await?)
            }
            m::SESSION_SIDE_ASK => {
                let sp: SessionSideAskParams = params(p)?;
                to_value(h.sessions.side_ask(sp).await?)
            }
            m::SESSION_PROMPT => {
                let pp: SessionPromptParams = params(p)?;
                to_value(h.sessions.prompt(pp, &self.principal.label)?)
            }
            m::SESSION_CANCEL => {
                let r: SessionRef = params(p)?;
                h.sessions.cancel(&r.session_id)?;
                Ok(json!({"ok": true}))
            }
            m::SESSION_END => {
                let r: SessionRef = params(p)?;
                h.sessions.end(&r.session_id).await?;
                Ok(json!({"ok": true}))
            }
            m::SESSION_ARCHIVE => {
                let r: SessionRef = params(p)?;
                to_value(h.sessions.set_archived(&r.session_id, true).await?)
            }
            m::SESSION_RENAME => {
                let r: agenthost_proto::SessionRenameParams = params(p)?;
                to_value(h.sessions.rename(&r.session_id, r.title.as_deref())?)
            }
            m::SESSION_UNARCHIVE => {
                let r: SessionRef = params(p)?;
                to_value(h.sessions.set_archived(&r.session_id, false).await?)
            }
            m::WORKSPACE_SET_DEFAULT_AGENT => {
                let dp: WorkspaceDefaultAgentParams = params(p)?;
                to_value(workspace_set_default_agent(&h, dp)?)
            }

            m::EVENTS_SUBSCRIBE => {
                let sp: EventsSubscribeParams = params(p)?;
                h.ctx
                    .store
                    .session_get(&sp.session_id)
                    .map_err(internal)?
                    .ok_or_else(|| RpcError::not_found("no such session"))?;
                let sub_id = crate::util::new_id("sub");
                // Count what will be replayed; the replay itself starts after the response.
                let pending = h
                    .ctx
                    .store
                    .events_after(&sp.session_id, sp.after_seq, usize::MAX >> 1)
                    .map_err(internal)?
                    .len();
                self.pending_after = Some(After::Replay {
                    session_id: sp.session_id,
                    after_seq: sp.after_seq,
                    sub_id: sub_id.clone(),
                });
                to_value(EventsSubscribeResult {
                    subscription_id: sub_id,
                    replayed: pending,
                })
            }
            m::EVENTS_UNSUBSCRIBE => {
                #[derive(serde::Deserialize)]
                struct U {
                    subscription_id: String,
                }
                let u: U = params(p)?;
                if let Some(t) = self.subs.remove(&u.subscription_id) {
                    t.abort();
                }
                Ok(json!({"ok": true}))
            }

            m::APPROVAL_LIST => to_value(h.gateway_pending()),
            m::BUILTIN_LIST | m::BUILTIN_SET_POLICY => h.connector_call(method, p).await,
            m::APPROVAL_RESPOND => {
                let ap: ApprovalRespondParams = params(p)?;
                h.approval_respond(&ap, &self.principal)?;
                Ok(json!({"ok": true}))
            }

            m::CONNECTOR_LIST
            | m::CONNECTOR_ADD
            | m::CONNECTOR_UPDATE
            | m::CONNECTOR_REMOVE
            | m::CONNECTOR_SIGN_IN
            | m::CONNECTOR_REFRESH
            | m::CONNECTOR_SEARCH => h.connector_call(method, p).await,

            m::UPLOAD_BEGIN => to_value(h.ctx.uploads.begin(params(p)?)?),
            m::UPLOAD_APPEND => to_value(h.ctx.uploads.append(params(p)?)?),
            m::UPLOAD_FINISH => {
                let r: UploadRef = params(p)?;
                to_value(h.ctx.uploads.finish(&r.upload_id)?)
            }

            m::VOICE_STATUS => to_value(h.ctx.voice.status()),
            m::VOICE_DOWNLOAD => to_value(h.ctx.voice.download()),
            m::VOICE_TRANSCRIBE => {
                let vp: VoiceTranscribeParams = params(p)?;
                let (_, path) = h.ctx.uploads.get(&vp.attachment_id)?;
                let r = h.ctx.voice.transcribe(&path, vp.language).await;
                h.ctx.uploads.remove(&vp.attachment_id);
                to_value(VoiceTranscribeResult { text: r? })
            }

            m::DEVICE_LIST
            | m::DEVICE_PAIR_START
            | m::DEVICE_PAIR_CONFIRM
            | m::DEVICE_RENAME
            | m::DEVICE_REVOKE => h.device_call(method, p).await,

            _ => Err(RpcError::new(
                codes::METHOD_NOT_FOUND,
                format!("unknown method {method}"),
            )),
        }
    }

    async fn after_response(&mut self) {
        if let Some(After::Replay {
            session_id,
            after_seq,
            sub_id,
        }) = self.pending_after.take()
        {
            let t = spawn_subscription(self.host.clone(), self.out.clone(), session_id, after_seq);
            self.subs.insert(sub_id, t);
        }
    }

    fn watch_sessions(&mut self, workspace_id: Option<String>) {
        let mut rx = self.host.ctx.bus.subscribe();
        let out = self.out.clone();
        self.watchers.push(tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(BusMsg::SessionChanged(s)) => {
                        if workspace_id
                            .as_ref()
                            .map(|w| *w == s.workspace_id)
                            .unwrap_or(true)
                        {
                            let n = Notification::new(
                                m::N_SESSION_CHANGED,
                                serde_json::to_value(&s).unwrap(),
                            );
                            if out.send(Message::Notification(n).to_json()).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    _ => {}
                }
            }
        }));
    }
}

/// Replay stored events after `after_seq`, then stream live ones, with no
/// gaps and no duplicates: subscribe to the bus first, read the store, then
/// forward only events newer than the last one sent, re-reading the store
/// whenever a gap appears or the bus overflows.
fn spawn_subscription(
    host: Arc<Host>,
    out: mpsc::Sender<String>,
    session_id: String,
    after_seq: i64,
) -> JoinHandle<()> {
    let mut rx = host.ctx.bus.subscribe();
    tokio::spawn(async move {
        let send_event = |e: &Event| {
            Message::Notification(Notification::new(
                m::N_EVENT,
                serde_json::to_value(e).unwrap(),
            ))
            .to_json()
        };
        let mut last = after_seq;
        // Replay in pages.
        macro_rules! catch_up {
            () => {
                loop {
                    let page = match host.ctx.store.events_after(&session_id, last, 500) {
                        Ok(p) => p,
                        Err(_) => break,
                    };
                    let n = page.len();
                    for e in &page {
                        if out.send(send_event(e)).await.is_err() {
                            return;
                        }
                        last = e.seq;
                    }
                    if n < 500 {
                        break;
                    }
                }
            };
        }
        catch_up!();
        loop {
            match rx.recv().await {
                Ok(BusMsg::Event(e)) if e.session_id == session_id => {
                    if e.seq == last + 1 {
                        if out.send(send_event(&e)).await.is_err() {
                            return;
                        }
                        last = e.seq;
                    } else if e.seq > last + 1 {
                        catch_up!();
                    }
                }
                Ok(BusMsg::Delta(d)) if d.session_id == session_id => {
                    let n =
                        Notification::new(m::N_SESSION_DELTA, serde_json::to_value(&d).unwrap());
                    if out.send(Message::Notification(n).to_json()).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => catch_up!(),
                Err(broadcast::error::RecvError::Closed) => return,
                _ => {}
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Workspaces
// ---------------------------------------------------------------------------

fn validate_hosts(hosts: &[String]) -> Result<(), RpcError> {
    for h in hosts {
        let h = h.trim();
        let ok = !h.is_empty()
            && h.len() < 254
            && h.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '*' | ':' | '_'))
            && (!h.contains('*') || (h.starts_with("*.") && h.matches('*').count() == 1))
            && h != "*";
        if !ok {
            return Err(RpcError::invalid_params(format!("invalid host {h:?}")));
        }
    }
    Ok(())
}

/// Add a folder as a workspace, after every check (also used by `project.create`).
pub fn workspace_add(h: &Host, p: WorkspaceAddParams) -> Result<Workspace, RpcError> {
    let raw = PathBuf::from(&p.path);
    if !raw.is_absolute() {
        return Err(RpcError::invalid_params("path must be absolute"));
    }
    let path = std::fs::canonicalize(&raw)
        .map_err(|e| RpcError::invalid_params(format!("{}: {e}", raw.display())))?;
    if !path.is_dir() {
        return Err(RpcError::invalid_params("path is not a folder"));
    }
    let home = std::fs::canonicalize(&h.ctx.cfg.home_dir).unwrap_or(h.ctx.cfg.home_dir.clone());
    if path == Path::new("/") || path == home {
        return Err(RpcError::invalid_params(
            "refusing to use / or your home folder as a workspace; pick a project folder",
        ));
    }
    for protected in [
        &h.ctx.cfg.data_dir,
        &h.ctx.cfg.runtime_dir,
        &h.ctx.cfg.scratch_dir(),
        &h.ctx.cfg.session_tools_dir(),
        &h.ctx.cfg.worktrees_dir(),
    ] {
        let prot = std::fs::canonicalize(protected).unwrap_or(protected.clone());
        if path.starts_with(&prot) || prot.starts_with(&path) {
            return Err(RpcError::invalid_params(
                "a workspace may not contain or be inside the host's own data",
            ));
        }
    }
    for secret in crate::sandbox::SECRET_DIRS {
        if path.starts_with(home.join(secret)) {
            return Err(RpcError::invalid_params(format!(
                "refusing to use a secrets folder (~/{secret}) as a workspace"
            )));
        }
    }
    for dir in crate::profiles::folders(h) {
        if crate::profiles::overlaps(&path, &dir) {
            return Err(RpcError::invalid_params(format!(
                "a workspace may not contain or be inside an agent profile's config folder ({})",
                dir.display()
            )));
        }
    }
    let path_s = path.display().to_string();
    if h.ctx
        .store
        .workspace_by_path(&path_s)
        .map_err(internal)?
        .is_some()
    {
        return Err(RpcError::invalid_state(
            "this folder is already a workspace",
        ));
    }
    let network = p.network.unwrap_or_default();
    validate_hosts(&network.custom_hosts)?;
    let kind = p.kind.unwrap_or(if path.join(".git").exists() {
        WorkspaceKind::Repo
    } else {
        WorkspaceKind::Folder
    });
    let w = Workspace {
        workspace_id: crate::util::new_id("ws"),
        name: p.name.unwrap_or_else(|| {
            path.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or(path_s.clone())
        }),
        path: path_s,
        kind,
        network,
        // Every connector starts on; switch off the ones a project shouldn't use.
        connectors: h
            .ctx
            .store
            .connector_list()
            .map_err(internal)?
            .into_iter()
            .map(|c| c.connector_id)
            .collect(),
        created_at: now_ms(),
        default_agent: None,
        default_profile_id: None,
    };
    h.ctx.store.workspace_insert(&w).map_err(internal)?;
    Ok(w)
}

fn workspace_set_default_agent(
    h: &Host,
    p: WorkspaceDefaultAgentParams,
) -> Result<Workspace, RpcError> {
    h.ctx
        .store
        .workspace_get(&p.workspace_id)
        .map_err(internal)?
        .ok_or_else(|| RpcError::not_found("no such workspace"))?;
    let profile = match (p.agent, p.profile_id.as_deref()) {
        (None, _) => None,
        (Some(a), Some(id)) if id != a.as_str() => {
            let prof = h
                .ctx
                .store
                .profile_get(id)
                .map_err(internal)?
                .ok_or_else(|| RpcError::not_found(format!("no agent profile {id}")))?;
            if prof.agent != a {
                return Err(RpcError::invalid_params(format!(
                    "profile {} is not a {} profile",
                    prof.name,
                    a.display_name()
                )));
            }
            Some(prof.profile_id)
        }
        _ => None,
    };
    h.ctx
        .store
        .workspace_set_default_agent(&p.workspace_id, p.agent, profile.as_deref())
        .map_err(internal)?;
    h.ctx
        .store
        .workspace_get(&p.workspace_id)
        .map_err(internal)?
        .ok_or_else(|| RpcError::not_found("no such workspace"))
}

fn workspace_update(h: &Host, p: WorkspaceUpdateParams) -> Result<Workspace, RpcError> {
    let mut w = h
        .ctx
        .store
        .workspace_get(&p.workspace_id)
        .map_err(internal)?
        .ok_or_else(|| RpcError::not_found("no such workspace"))?;
    if let Some(n) = p.name {
        w.name = n;
    }
    if let Some(net) = p.network {
        validate_hosts(&net.custom_hosts)?;
        w.network = net;
    }
    if let Some(host) = p.add_custom_host {
        let host = host.trim().to_ascii_lowercase();
        validate_hosts(std::slice::from_ref(&host))?;
        if !w.network.custom_hosts.contains(&host) {
            w.network.custom_hosts.push(host);
        }
    }
    if let Some(c) = p.connectors {
        for id in &c {
            if h.ctx.store.connector_get(id).map_err(internal)?.is_none() {
                return Err(RpcError::not_found(format!("no connector {id}")));
            }
        }
        w.connectors = c;
    }
    h.ctx.store.workspace_update(&w).map_err(internal)?;
    Ok(w)
}
