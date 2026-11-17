//! Session manager and the per-session actor.
//!
//! Each live session has one actor task that owns its agent process, its
//! sandbox and its egress proxy, and is the only writer of its state. All
//! agent output becomes events through the actor.

use crate::acp::{self, AcpConn, Incoming};
use crate::agents::{self, AgentSpec};
use crate::bus::BusMsg;
use crate::ctx::Ctx;
use crate::proxy::{EgressProxy, ProxyHooks};
use crate::sandbox::{self, SandboxPolicy};
use crate::util::{now_ms, truncate};
use agenthost_proto::jsonrpc::RpcError;
use agenthost_proto::*;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Child;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// Tool output above this is truncated in the event log.
pub const TOOL_OUTPUT_LIMIT: usize = 64 * 1024;
/// The same failing tool call this many times in one turn cancels the turn.
pub const REPEAT_FAILURE_LIMIT: usize = 3;
/// How long an agent gets to honour `session/cancel` before it is restarted.
pub const CANCEL_GRACE: Duration = Duration::from_secs(3);
const INIT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub enum Cmd {
    Prompt {
        seq: i64,
        text: String,
    },
    Cancel,
    End,
    /// The MCP gateway is holding (true) or released (false) a call for approval.
    Approval {
        waiting: bool,
    },
    /// Stop the actor without changing the session's state (host shutdown).
    Shutdown,
}

#[derive(Clone)]
pub struct SessionHandle {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl SessionHandle {
    pub fn send(&self, cmd: Cmd) -> bool {
        self.tx.send(cmd).is_ok()
    }
}

pub struct SessionManager {
    ctx: Arc<Ctx>,
    live: Mutex<HashMap<String, SessionHandle>>,
}

impl SessionManager {
    pub fn new(ctx: Arc<Ctx>) -> Arc<Self> {
        Arc::new(Self {
            ctx,
            live: Mutex::new(HashMap::new()),
        })
    }

    pub fn handle(&self, session_id: &str) -> Option<SessionHandle> {
        self.live.lock().unwrap().get(session_id).cloned()
    }

    fn spawn_actor(self: &Arc<Self>, session: Session, workspace: Workspace, resume: bool) {
        let (tx, rx) = mpsc::unbounded_channel();
        self.live
            .lock()
            .unwrap()
            .insert(session.session_id.clone(), SessionHandle { tx });
        let mgr = self.clone();
        let sid = session.session_id.clone();
        tokio::spawn(async move {
            let actor = Actor::new(mgr.ctx.clone(), session, workspace, rx);
            actor.run(resume).await;
            mgr.live.lock().unwrap().remove(&sid);
        });
    }

    /// Create and start a session.
    pub fn create(
        self: &Arc<Self>,
        p: SessionCreateParams,
        origin: Option<&str>,
    ) -> Result<Session, RpcError> {
        let ws = self
            .ctx
            .store
            .workspace_get(&p.workspace_id)
            .map_err(|e| RpcError::internal(e.to_string()))?
            .ok_or_else(|| RpcError::not_found(format!("no workspace {}", p.workspace_id)))?;
        // One live session per workspace: two sandboxes in one folder would
        // remove each other's protective mount points.
        let busy = self
            .ctx
            .store
            .session_list(Some(&ws.workspace_id))
            .map_err(|e| RpcError::internal(e.to_string()))?
            .into_iter()
            .find(|s| !s.state.is_terminal());
        if let Some(b) = busy {
            return Err(RpcError::invalid_state(format!(
                "workspace {} already has a live session ({}, {}); end it first",
                ws.name, b.session_id, b.state
            ))
            .with_data(json!({"session_id": b.session_id})));
        }
        if p.agent == AgentKind::Fake && !self.ctx.cfg.enable_fake_agent {
            return Err(RpcError::new(
                jsonrpc::codes::UNAVAILABLE,
                "the fake agent is not enabled on this host",
            ));
        }
        let now = now_ms();
        let s = Session {
            session_id: crate::util::new_id("ses"),
            workspace_id: ws.workspace_id.clone(),
            agent: p.agent,
            state: SessionState::Created,
            state_reason: None,
            acp_session_id: None,
            title: p.title,
            created_at: now,
            updated_at: now,
            last_seq: 0,
        };
        self.ctx
            .store
            .session_insert(&s)
            .map_err(|e| RpcError::internal(e.to_string()))?;
        // Creation is the first `session.state` event.
        self.ctx
            .append(&s.session_id, EventType::SessionState, json!({"state": "created", "agent": p.agent, "workspace_id": ws.workspace_id, "by": origin}))
            .map_err(|e| RpcError::internal(e.to_string()))?;
        let s = self
            .ctx
            .store
            .session_get(&s.session_id)
            .ok()
            .flatten()
            .unwrap_or(s);
        self.ctx.bus.publish(BusMsg::SessionChanged(s.clone()));
        self.spawn_actor(s.clone(), ws, false);
        Ok(s)
    }

    /// Record a prompt (once per prompt id) and hand it to the actor.
    pub fn prompt(
        &self,
        p: SessionPromptParams,
        origin: &str,
    ) -> Result<SessionPromptResult, RpcError> {
        let s = self.get(&p.session_id)?;
        if s.state.is_terminal() {
            return Err(RpcError::invalid_state(format!("session is {}", s.state)));
        }
        if p.text.trim().is_empty() {
            return Err(RpcError::invalid_params("empty prompt"));
        }
        let (ev, seq) = self
            .ctx
            .store
            .append_prompt(
                &p.session_id,
                &p.prompt_id,
                json!({"text": p.text, "prompt_id": p.prompt_id, "device": origin}),
            )
            .map_err(|e| RpcError::internal(e.to_string()))?;
        let Some(ev) = ev else {
            return Ok(SessionPromptResult {
                seq,
                accepted: false,
            });
        };
        self.ctx.bus.publish(BusMsg::Event(ev));
        let h = self
            .handle(&p.session_id)
            .ok_or_else(|| RpcError::invalid_state("session is not running"))?;
        h.send(Cmd::Prompt { seq, text: p.text });
        Ok(SessionPromptResult {
            seq,
            accepted: true,
        })
    }

    pub fn cancel(&self, session_id: &str) -> Result<(), RpcError> {
        self.get(session_id)?;
        let h = self
            .handle(session_id)
            .ok_or_else(|| RpcError::invalid_state("session is not running"))?;
        h.send(Cmd::Cancel);
        Ok(())
    }

    /// End a session and wait (up to 15s) until its agent has stopped.
    pub async fn end(&self, session_id: &str) -> Result<(), RpcError> {
        let s = self.get(session_id)?;
        if s.state.is_terminal() {
            return Ok(());
        }
        match self.handle(session_id) {
            Some(h) => {
                h.send(Cmd::End);
                let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
                while self.handle(session_id).is_some() && tokio::time::Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
            None => {
                self.ctx
                    .set_state(session_id, SessionState::Ended, Some("ended"), None)
                    .map_err(|e| RpcError::internal(e.to_string()))?;
            }
        }
        Ok(())
    }

    pub fn notify_approval(&self, session_id: &str, waiting: bool) {
        if let Some(h) = self.handle(session_id) {
            h.send(Cmd::Approval { waiting });
        }
    }

    fn get(&self, session_id: &str) -> Result<Session, RpcError> {
        self.ctx
            .store
            .session_get(session_id)
            .map_err(|e| RpcError::internal(e.to_string()))?
            .ok_or_else(|| RpcError::not_found(format!("no session {session_id}")))
    }

    /// After a host restart: resume every session that was live.
    pub fn resume_all(self: &Arc<Self>) -> Result<usize> {
        let mut n = 0;
        for s in self.ctx.store.session_list(None)? {
            if s.state.is_terminal() {
                continue;
            }
            let ws = self.ctx.store.workspace_get(&s.workspace_id)?;
            match (ws, &s.acp_session_id) {
                (Some(ws), Some(_)) => {
                    if matches!(
                        s.state,
                        SessionState::Running | SessionState::WaitingApproval
                    ) {
                        let _ = self.ctx.append(
                            &s.session_id,
                            EventType::TurnEnd,
                            json!({"stop_reason": "interrupted", "host_reason": "the host restarted during the turn"}),
                        );
                    }
                    self.spawn_actor(s, ws, true);
                    n += 1;
                }
                (None, _) => {
                    let _ = self.ctx.set_state(
                        &s.session_id,
                        SessionState::Failed,
                        Some("workspace was removed"),
                        None,
                    );
                }
                (Some(_), None) => {
                    let _ = self.ctx.set_state(
                        &s.session_id,
                        SessionState::Failed,
                        Some("the host restarted before the agent session was created"),
                        None,
                    );
                }
            }
        }
        Ok(n)
    }

    /// Stop every actor, leaving state as is so the next start resumes.
    pub fn shutdown(&self) {
        for h in self.live.lock().unwrap().values() {
            h.send(Cmd::Shutdown);
        }
    }

    pub fn live_count(&self) -> usize {
        self.live.lock().unwrap().len()
    }
}

// ---------------------------------------------------------------------------
// Network hooks for this session's egress proxy
// ---------------------------------------------------------------------------

struct SessionNet {
    ctx: Arc<Ctx>,
    session_id: String,
    workspace_id: String,
    agent_hosts: Vec<String>,
}

impl ProxyHooks for SessionNet {
    fn allowed_hosts(&self) -> Vec<String> {
        let mut hosts = self.agent_hosts.clone();
        if let Ok(Some(ws)) = self.ctx.store.workspace_get(&self.workspace_id) {
            hosts.extend(sandbox::profile_hosts(&ws.network));
        }
        hosts
    }
    fn on_denied(&self, host: &str, port: u16, reason: &str) {
        let _ = self.ctx.append(
            &self.session_id,
            EventType::NetDenied,
            json!({"host": host, "port": port, "reason": reason, "workspace_id": self.workspace_id}),
        );
    }
    fn gateway_socket(&self) -> Option<PathBuf> {
        self.ctx.gateway.get().map(|g| g.socket_path())
    }
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

type TurnFuture = Pin<Box<dyn Future<Output = Result<Value, RpcError>> + Send>>;

struct ToolInfo {
    signature: String,
    is_mcp: bool,
    title: String,
    finished: bool,
    /// Output streamed outside `content` (Codex: `_meta.terminal_output_delta`).
    streamed: String,
    /// The `tool.call` event is written once the arguments are known.
    recorded: bool,
    kind: Value,
    raw_input: Value,
    locations: Value,
}

struct Running {
    child: Child,
    conn: Arc<AcpConn>,
    incoming: mpsc::Receiver<Incoming>,
    _proxy: EgressProxy,
}

struct Actor {
    ctx: Arc<Ctx>,
    session: Session,
    workspace: Workspace,
    spec: AgentSpec,
    rx: mpsc::UnboundedReceiver<Cmd>,
    state: SessionState,
    temp_dir: PathBuf,
    agent: Option<Running>,
    queue: VecDeque<(i64, String)>,
    turn: Option<TurnFuture>,
    turn_prompt_seq: i64,
    cancel_deadline: Option<Instant>,
    cancel_requested: bool,
    host_cancel_reason: Option<String>,
    /// Text being streamed: (event kind, accumulated text).
    buf: Option<(EventType, String)>,
    tools: HashMap<String, ToolInfo>,
    failures: HashMap<String, usize>,
    /// Ignore `session/update` while the agent replays history on load.
    loading: bool,
    /// When this actor started the agent (for the mount-point sweep).
    started_at: std::time::SystemTime,
}

enum Exit {
    Ended,
    Failed(String, Value),
    Shutdown,
}

impl Actor {
    fn new(
        ctx: Arc<Ctx>,
        session: Session,
        workspace: Workspace,
        rx: mpsc::UnboundedReceiver<Cmd>,
    ) -> Self {
        let spec = agents::spec(session.agent, &ctx.cfg);
        let short = session
            .session_id
            .trim_start_matches("ses_")
            .chars()
            .take(10)
            .collect::<String>();
        let temp_dir = ctx.cfg.runtime_dir.join(format!("s{short}"));
        let state = session.state;
        Actor {
            ctx,
            session,
            workspace,
            spec,
            rx,
            state,
            temp_dir,
            agent: None,
            queue: VecDeque::new(),
            turn: None,
            turn_prompt_seq: 0,
            cancel_deadline: None,
            cancel_requested: false,
            host_cancel_reason: None,
            buf: None,
            tools: HashMap::new(),
            failures: HashMap::new(),
            loading: false,
            started_at: std::time::SystemTime::now(),
        }
    }

    fn sid(&self) -> String {
        self.session.session_id.clone()
    }

    fn set_state(&mut self, state: SessionState, reason: Option<&str>, extra: Option<Value>) {
        if self.state == state {
            return;
        }
        match self
            .ctx
            .set_state(&self.session.session_id, state, reason, extra)
        {
            Ok(s) => {
                self.state = s.state;
                self.session = s;
            }
            Err(e) => {
                tracing::error!(session = %self.session.session_id, "state change refused: {e}")
            }
        }
    }

    fn append(&self, kind: EventType, payload: Value) {
        if let Err(e) = self.ctx.append(&self.session.session_id, kind, payload) {
            tracing::error!(session = %self.session.session_id, "append event: {e}");
        }
    }

    async fn run(mut self, resume: bool) {
        let exit = match self.start(resume).await {
            Ok(()) => self.main_loop().await,
            Err(e) => match e.downcast::<StartFailure>() {
                Ok(f) => Exit::Failed(f.reason, f.detail),
                Err(e) => Exit::Failed(format!("{e:#}"), Value::Null),
            },
        };
        self.stop_agent().await;
        if let Some(g) = self.ctx.gateway.get() {
            g.unregister_session(&self.session.session_id);
        }
        match exit {
            Exit::Ended => {
                let _ = std::fs::remove_dir_all(&self.temp_dir);
                let _ = std::fs::remove_file(self.temp_dir.with_extension("codex"));
                let _ = std::fs::remove_dir_all(self.temp_dir.with_extension("cfg"));
                self.set_state(SessionState::Ended, Some("ended"), None);
            }
            Exit::Failed(reason, detail) => {
                let _ = std::fs::remove_dir_all(&self.temp_dir);
                let extra = if detail.is_null() { None } else { Some(detail) };
                self.set_state(SessionState::Failed, Some(&reason), extra);
            }
            Exit::Shutdown => {}
        }
    }

    /// `starting`: sandbox, agent launch and the ACP handshake.
    async fn start(&mut self, resume: bool) -> Result<()> {
        self.set_state(
            SessionState::Starting,
            Some(if resume { "resuming" } else { "starting" }),
            None,
        );

        let status = agents::status(self.session.agent, &self.ctx.cfg);
        if !status.installed || (status.problem.is_some() && status.logged_in) {
            return Err(StartFailure::new(
                status
                    .problem
                    .clone()
                    .unwrap_or_else(|| "agent is not installed".into()),
                json!({"fix": status.fix}),
            ));
        }
        if !status.logged_in {
            return Err(StartFailure::new(
                "not logged in",
                json!({"fix": status.fix}),
            ));
        }

        crate::config::create_private_dir(&self.temp_dir)?;
        if let Some((rel, content)) = self.spec.host_xdg_config {
            // Readable inside the sandbox, not writable: a sibling of the temp folder.
            let dir = self.temp_dir.with_extension("cfg");
            let file = dir.join(rel);
            std::fs::create_dir_all(file.parent().unwrap())?;
            std::fs::write(&file, content)?;
            self.spec
                .env
                .push(("XDG_CONFIG_HOME".into(), dir.display().to_string()));
        }
        if let Some((var, rel, content)) = self.spec.host_config_file {
            let file = self.temp_dir.with_extension("cfg").join(rel);
            std::fs::create_dir_all(file.parent().unwrap())?;
            std::fs::write(&file, content)?;
            self.spec.env.push((var.into(), file.display().to_string()));
        }
        if self.spec.kind == AgentKind::Codex {
            let wrapper = self.temp_dir.with_extension("codex");
            agents::write_codex_wrapper(
                &self.ctx.cfg,
                &wrapper,
                std::path::Path::new(&self.workspace.path),
            )?;
            self.spec
                .env
                .retain(|(k, _)| k != "CODEX_PATH" && k != "CODEX_CONFIG");
            self.spec
                .env
                .push(("CODEX_PATH".into(), wrapper.display().to_string()));
            // codex-acp starts each thread with this config map; Codex reloads
            // your config.toml for a thread, so the same overrides go here too.
            let thread_cfg = agents::codex_thread_config(
                &self.ctx.cfg,
                std::path::Path::new(&self.workspace.path),
            );
            self.spec
                .env
                .push(("CODEX_CONFIG".into(), thread_cfg.to_string()));
        }
        let workspace_path = PathBuf::from(&self.workspace.path);
        if !workspace_path.is_dir() {
            return Err(StartFailure::new(
                format!("workspace folder {} does not exist", self.workspace.path),
                Value::Null,
            ));
        }
        let policy: SandboxPolicy = sandbox::policy_for(
            &self.ctx.cfg,
            &self.spec,
            &workspace_path,
            &self.temp_dir,
            &self.workspace.network,
        );

        let proxy = EgressProxy::start(Arc::new(SessionNet {
            ctx: self.ctx.clone(),
            session_id: self.sid(),
            workspace_id: self.workspace.workspace_id.clone(),
            agent_hosts: self.spec.hosts.clone(),
        }))
        .await?;

        let launch = if self.spec.kind == AgentKind::Fake && self.ctx.cfg.fake_agent_unsandboxed {
            sandbox::launch_unsandboxed_fake(&self.ctx.cfg, &self.spec, &policy, proxy.http_port)
        } else {
            sandbox::launch_sandboxed(
                &self.ctx.cfg,
                &self.spec,
                &policy,
                proxy.http_port,
                proxy.socks_port,
            )
        }
        .map_err(|e| StartFailure::new(format!("sandbox failed to start: {e:#}"), Value::Null))?;

        let mut child = launch.child;
        let log = self
            .ctx
            .cfg
            .logs_dir()
            .join(format!("{}.agent.log", self.session.session_id));
        let (conn, incoming) = AcpConn::attach(&mut child, Some(log))?;
        self.agent = Some(Running {
            child,
            conn: conn.clone(),
            incoming,
            _proxy: proxy,
        });

        // initialize
        let init = self
            .call_during_start("initialize", acp::initialize_params())
            .await?;
        let version = init["protocolVersion"].as_i64().unwrap_or(-1);
        if version != acp::ACP_PROTOCOL_VERSION {
            return Err(StartFailure::new(
                format!(
                    "ACP protocol version mismatch: agent speaks {version}, host speaks {}",
                    acp::ACP_PROTOCOL_VERSION
                ),
                Value::Null,
            ));
        }
        let can_load = init["agentCapabilities"]["loadSession"]
            .as_bool()
            .unwrap_or(false);
        // Agents that need to be told which of their logins to use.
        if let Some(method) = self.spec.auth_method {
            let offered = init["authMethods"]
                .as_array()
                .map(|m| m.iter().any(|x| x["id"] == method))
                .unwrap_or(false);
            if offered {
                self.call_during_start("authenticate", json!({"methodId": method}))
                    .await
                    .map_err(|e| {
                        StartFailure::new(
                            format!("{} sign-in failed: {e:#}", self.spec.kind.display_name()),
                            json!({"fix": self.spec.login_fix}),
                        )
                    })?;
            }
        }

        let mcp_servers = self.mcp_servers();
        let cwd = self.workspace.path.clone();
        if resume {
            let acp_id = self
                .session
                .acp_session_id
                .clone()
                .ok_or_else(|| anyhow!("no ACP session id to resume"))?;
            if !can_load {
                return Err(StartFailure::new(
                    "agent cannot resume sessions (no loadSession capability)",
                    Value::Null,
                ));
            }
            self.loading = true;
            let mut params = json!({"sessionId": acp_id, "cwd": cwd, "mcpServers": mcp_servers});
            if let Some(m) = &self.spec.session_meta {
                params["_meta"] = m.clone();
            }
            let r = self.call_during_start("session/load", params).await;
            self.loading = false;
            r.map_err(|e| prefix_failure(e, "resume failed"))?;
        } else {
            let mut params = json!({"cwd": cwd, "mcpServers": mcp_servers});
            if let Some(m) = &self.spec.session_meta {
                params["_meta"] = m.clone();
            }
            let r = self.call_during_start("session/new", params).await?;
            let acp_id = r["sessionId"]
                .as_str()
                .ok_or_else(|| anyhow!("session/new returned no sessionId"))?
                .to_string();
            self.ctx
                .store
                .session_set_acp_id(&self.session.session_id, &acp_id)?;
            self.session.acp_session_id = Some(acp_id);
        }
        self.apply_session_mode().await?;
        self.set_state(
            SessionState::Idle,
            None,
            Some(json!({"agent_info": init.get("agentInfo")})),
        );
        Ok(())
    }

    /// Switch to the agent's no-prompt, no-inner-sandbox mode, if it has one.
    async fn apply_session_mode(&mut self) -> Result<()> {
        let Some(mode) = self.spec.session_mode.clone() else {
            return Ok(());
        };
        let acp_id = self.session.acp_session_id.clone().unwrap_or_default();
        self.call_during_start(
            "session/set_mode",
            json!({"sessionId": acp_id, "modeId": mode}),
        )
        .await
        .map_err(|e| prefix_failure(e, &format!("could not switch the agent to mode {mode}")))?;
        Ok(())
    }

    fn mcp_servers(&self) -> Value {
        match self.ctx.gateway.get().and_then(|g| {
            g.register_session(&self.session.session_id, &self.workspace.workspace_id)
        }) {
            Some(mut url) => {
                if self.spec.direct_gateway {
                    if let Some(a) = &self.agent {
                        let base = crate::proxy::direct_gateway_base(a._proxy.http_port);
                        url = url.replacen(
                            &format!("http://{}", crate::proxy::GATEWAY_HOST),
                            &base,
                            1,
                        );
                    }
                }
                json!([{"type": "http", "name": "agenthost", "url": url, "headers": []}])
            }
            None => json!([]),
        }
    }

    /// A request during `starting`, while still serving agent requests and
    /// noticing if the agent dies.
    async fn call_during_start(&mut self, method: &str, params: Value) -> Result<Value> {
        let conn = self.agent.as_ref().unwrap().conn.clone();
        let mut fut = Box::pin(async move { conn.call(method, params).await });
        let deadline = tokio::time::sleep(INIT_TIMEOUT);
        tokio::pin!(deadline);
        enum Step {
            Done(Result<Value, RpcError>),
            Msg(Option<Incoming>),
            Timeout,
        }
        loop {
            let step = {
                let running = self.agent.as_mut().unwrap();
                tokio::select! {
                    r = &mut fut => Step::Done(r),
                    m = running.incoming.recv() => Step::Msg(m),
                    _ = &mut deadline => Step::Timeout,
                }
            };
            match step {
                Step::Done(r) => {
                    return r.map_err(|e| {
                        if acp::is_auth_error(&e) {
                            StartFailure::new(
                                "not logged in",
                                json!({"fix": self.spec.login_fix, "agent_error": e.message}),
                            )
                        } else {
                            let tail = self
                                .agent
                                .as_ref()
                                .map(|a| a.conn.stderr.text())
                                .unwrap_or_default();
                            StartFailure::new(
                                format!("{method} failed: {}", e.message),
                                json!({"stderr": last_lines(&tail, 15)}),
                            )
                        }
                    });
                }
                Step::Msg(Some(Incoming::Closed(_))) | Step::Msg(None) => {
                    // Give stderr a moment to drain so the reason is complete.
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    let running = self.agent.as_mut().unwrap();
                    let tail = running.conn.stderr.text();
                    let exit =
                        match tokio::time::timeout(Duration::from_secs(2), running.child.wait())
                            .await
                        {
                            Ok(Ok(s)) => format!(" ({s})"),
                            _ => String::new(),
                        };
                    return Err(StartFailure::new(
                        format!("agent exited during {method}{exit}"),
                        json!({"stderr": last_lines(&tail, 20)}),
                    ));
                }
                Step::Msg(Some(m)) => self.on_incoming(m).await,
                Step::Timeout => {
                    let tail = self.agent.as_ref().unwrap().conn.stderr.text();
                    return Err(StartFailure::new(
                        format!("{method} timed out after {}s", INIT_TIMEOUT.as_secs()),
                        json!({"stderr": last_lines(&tail, 15)}),
                    ));
                }
            }
        }
    }

    async fn main_loop(&mut self) -> Exit {
        loop {
            if self.turn.is_none() && self.state == SessionState::Idle {
                if let Some((seq, text)) = self.queue.pop_front() {
                    if let Err(e) = self.start_turn(seq, text).await {
                        return Exit::Failed(format!("could not send prompt: {e}"), Value::Null);
                    }
                }
            }
            enum Step {
                Cmd(Option<Cmd>),
                Msg(Option<Incoming>),
                Turn(Result<Value, RpcError>),
                CancelTimeout,
            }
            let cancel_deadline = self.cancel_deadline;
            let step = {
                let agent = self.agent.as_mut().expect("agent running in main loop");
                let turn = &mut self.turn;
                let has_turn = turn.is_some();
                tokio::select! {
                    c = self.rx.recv() => Step::Cmd(c),
                    m = agent.incoming.recv() => Step::Msg(m),
                    r = async { turn.as_mut().unwrap().await }, if has_turn => Step::Turn(r),
                    _ = async { tokio::time::sleep_until(cancel_deadline.unwrap()).await }, if cancel_deadline.is_some() && has_turn => Step::CancelTimeout,
                }
            };
            match step {
                Step::Cmd(None) | Step::Cmd(Some(Cmd::Shutdown)) => return Exit::Shutdown,
                Step::Cmd(Some(Cmd::End)) => return Exit::Ended,
                Step::Cmd(Some(Cmd::Prompt { seq, text })) => self.queue.push_back((seq, text)),
                Step::Cmd(Some(Cmd::Cancel)) => self.request_cancel(None).await,
                Step::Cmd(Some(Cmd::Approval { waiting })) => {
                    if waiting && self.state == SessionState::Running {
                        self.set_state(
                            SessionState::WaitingApproval,
                            Some("an MCP write call waits for approval"),
                            None,
                        );
                    } else if !waiting && self.state == SessionState::WaitingApproval {
                        self.set_state(SessionState::Running, None, None);
                    }
                }
                Step::Msg(Some(Incoming::Closed(_))) | Step::Msg(None) => {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    let agent = self.agent.as_mut().unwrap();
                    let tail = agent.conn.stderr.text();
                    let status = match tokio::time::timeout(
                        Duration::from_secs(2),
                        agent.child.wait(),
                    )
                    .await
                    {
                        Ok(Ok(s)) => s.to_string(),
                        _ => "still running".into(),
                    };
                    if self.turn.is_some() {
                        self.flush();
                        self.append(EventType::TurnEnd, json!({"stop_reason": "error", "host_reason": "the agent exited", "prompt_seq": self.turn_prompt_seq}));
                        self.turn = None;
                    }
                    return Exit::Failed(
                        format!("agent exited unexpectedly ({status})"),
                        json!({"stderr": last_lines(&tail, 20)}),
                    );
                }
                Step::Msg(Some(m)) => self.on_incoming(m).await,
                Step::Turn(r) => {
                    self.turn = None;
                    self.finish_turn(r);
                }
                Step::CancelTimeout => {
                    // The agent ignored session/cancel: restart it and resume.
                    self.cancel_deadline = None;
                    self.turn = None;
                    self.flush();
                    let reason = self.host_cancel_reason.take();
                    self.append(EventType::TurnEnd, json!({
                        "stop_reason": "cancelled",
                        "host_reason": reason.unwrap_or_else(|| "the agent did not stop in time; it was restarted".into()),
                        "prompt_seq": self.turn_prompt_seq,
                        "forced": true
                    }));
                    self.cancel_requested = false;
                    self.stop_agent().await;
                    if let Err(e) = self.start(true).await {
                        let (reason, detail) = match e.downcast::<StartFailure>() {
                            Ok(f) => (f.reason, f.detail),
                            Err(e) => (e.to_string(), Value::Null),
                        };
                        return Exit::Failed(
                            format!("restart after cancel failed: {reason}"),
                            detail,
                        );
                    }
                }
            }
        }
    }

    async fn start_turn(&mut self, seq: i64, text: String) -> Result<()> {
        let acp_id = self.session.acp_session_id.clone().unwrap_or_default();
        let conn = self.agent.as_ref().unwrap().conn.clone();
        let rx = conn
            .request(
                "session/prompt",
                json!({"sessionId": acp_id, "prompt": [{"type": "text", "text": text}]}),
            )
            .await?;
        self.turn = Some(Box::pin(async move {
            rx.await
                .unwrap_or_else(|_| Err(RpcError::internal("agent exited")))
        }));
        self.turn_prompt_seq = seq;
        self.failures.clear();
        self.cancel_requested = false;
        self.host_cancel_reason = None;
        self.set_state(
            SessionState::Running,
            None,
            Some(json!({"prompt_seq": seq})),
        );
        Ok(())
    }

    async fn request_cancel(&mut self, host_reason: Option<String>) {
        if self.turn.is_none() {
            // Nothing running; drop queued prompts.
            self.queue.clear();
            return;
        }
        self.cancel_requested = true;
        if host_reason.is_some() {
            self.host_cancel_reason = host_reason;
        }
        self.queue.clear();
        if let Some(a) = &self.agent {
            let acp_id = self.session.acp_session_id.clone().unwrap_or_default();
            let _ = a
                .conn
                .notify("session/cancel", json!({"sessionId": acp_id}))
                .await;
        }
        if self.cancel_deadline.is_none() {
            self.cancel_deadline = Some(Instant::now() + CANCEL_GRACE);
        }
    }

    fn finish_turn(&mut self, r: Result<Value, RpcError>) {
        self.record_pending_calls();
        self.flush();
        self.cancel_deadline = None;
        let mut payload = match &r {
            Ok(v) => json!({
                "stop_reason": v["stopReason"].as_str().unwrap_or("end_turn"),
                "usage": v.get("usage").cloned().unwrap_or(Value::Null),
            }),
            Err(e) => json!({"stop_reason": "error", "error": e.message}),
        };
        if self.cancel_requested && payload["stop_reason"] != "error" {
            payload["stop_reason"] = json!("cancelled");
        }
        if let Some(reason) = self.host_cancel_reason.take() {
            payload["host_reason"] = json!(reason);
        }
        payload["prompt_seq"] = json!(self.turn_prompt_seq);
        self.append(EventType::TurnEnd, payload);
        self.cancel_requested = false;
        self.tools.retain(|_, t| !t.finished);
        if matches!(
            self.state,
            SessionState::Running | SessionState::WaitingApproval
        ) {
            self.set_state(SessionState::Idle, None, None);
        }
    }

    /// Write out streamed text as one coalesced event.
    fn flush(&mut self) {
        if let Some((kind, text)) = self.buf.take() {
            if !text.is_empty() {
                self.append(kind, json!({"text": text}));
            }
        }
    }

    fn stream(&mut self, kind: EventType, text: &str) {
        if text.is_empty() {
            return;
        }
        self.record_pending_calls();
        match &mut self.buf {
            Some((k, buf)) if *k == kind => buf.push_str(text),
            _ => {
                self.flush();
                self.buf = Some((kind, text.to_string()));
            }
        }
        self.ctx.bus.publish(BusMsg::Delta(SessionDelta {
            session_id: self.sid(),
            kind,
            text: text.to_string(),
        }));
    }

    async fn on_incoming(&mut self, msg: Incoming) {
        match msg {
            Incoming::Notification(n) if n.method == "session/update" => {
                if !self.loading {
                    self.on_update(&n.params["update"]).await;
                }
            }
            Incoming::Notification(_) => {}
            Incoming::Request(r) => {
                let conn = self.agent.as_ref().unwrap().conn.clone();
                let result = match r.method.as_str() {
                    // Sandbox-local tools are approved automatically: the
                    // sandbox is the boundary.
                    "session/request_permission" => Ok(acp::auto_allow_outcome(&r.params)),
                    m => Err(RpcError::new(
                        jsonrpc::codes::METHOD_NOT_FOUND,
                        format!("host does not provide {m}"),
                    )),
                };
                let _ = conn.respond(r.id, result).await;
            }
            Incoming::Response(r) => {
                if let Some(a) = &self.agent {
                    a.conn.resolve(r);
                }
            }
            Incoming::Closed(_) => {}
        }
    }

    async fn on_update(&mut self, u: &Value) {
        let kind = u["sessionUpdate"].as_str().unwrap_or_default();
        match kind {
            "agent_message_chunk" => {
                self.stream(EventType::AgentMessage, &acp::content_text(&u["content"]))
            }
            "agent_thought_chunk" => {
                self.stream(EventType::AgentThought, &acp::content_text(&u["content"]))
            }
            "tool_call" | "tool_call_update" => self.on_tool(kind == "tool_call", u).await,
            _ => {}
        }
    }

    /// Write the `tool.call` event for a call whose arguments are now known.
    fn record_call(&mut self, id: &str) {
        let Some(t) = self.tools.get_mut(id) else {
            return;
        };
        if t.recorded || t.is_mcp {
            t.recorded = true;
            return;
        }
        t.recorded = true;
        let payload = json!({
            "tool_call_id": id,
            "name": t.title,
            "kind": t.kind,
            "arguments": t.raw_input,
            "source": "sandbox",
            "locations": t.locations,
        });
        self.append(EventType::ToolCall, payload);
    }

    /// Record calls still waiting for arguments (before other output).
    fn record_pending_calls(&mut self) {
        let ids: Vec<String> = self
            .tools
            .iter()
            .filter(|(_, t)| !t.recorded)
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            self.record_call(&id);
        }
    }

    async fn on_tool(&mut self, is_new: bool, u: &Value) {
        let id = u["toolCallId"].as_str().unwrap_or_default().to_string();
        if id.is_empty() {
            return;
        }
        let has_input = |v: &Value| match v {
            Value::Null => false,
            Value::Object(o) => !o.is_empty(),
            _ => true,
        };
        if is_new || !self.tools.contains_key(&id) {
            self.flush();
            let title = u["title"].as_str().unwrap_or("tool").to_string();
            let raw_input = u.get("rawInput").cloned().unwrap_or(Value::Null);
            // Calls to our own MCP gateway are recorded by the gateway itself.
            let is_mcp = is_gateway_tool(&title, u);
            let signature = format!("{title}\u{0}{raw_input}");
            self.tools.insert(
                id.clone(),
                ToolInfo {
                    signature,
                    is_mcp,
                    title,
                    finished: false,
                    streamed: String::new(),
                    recorded: false,
                    kind: u.get("kind").cloned().unwrap_or(Value::Null),
                    raw_input: raw_input.clone(),
                    locations: u.get("locations").cloned().unwrap_or(Value::Null),
                },
            );
            if has_input(&raw_input) {
                self.record_call(&id);
            }
        } else if let Some(t) = self.tools.get_mut(&id) {
            if let Some(title) = u["title"].as_str() {
                t.title = title.to_string();
                if !t.is_mcp {
                    t.is_mcp = is_gateway_tool(title, u);
                }
            }
            if let Some(ri) = u.get("rawInput").filter(|v| has_input(v)) {
                t.raw_input = ri.clone();
                t.signature = format!("{}\u{0}{ri}", t.title);
            }
            if let Some(k) = u.get("kind") {
                t.kind = k.clone();
            }
            if let Some(l) = u.get("locations").filter(|v| !v.is_null()) {
                t.locations = l.clone();
            }
            let ready = has_input(&t.raw_input)
                || matches!(
                    u["status"].as_str(),
                    Some("in_progress" | "completed" | "failed")
                );
            if ready && !t.recorded {
                self.record_call(&id);
            }
        }
        if let Some(t) = self.tools.get_mut(&id) {
            for key in ["terminal_output_delta", "terminal_output"] {
                if let Some(d) = u["_meta"][key]["data"].as_str() {
                    if t.streamed.len() < TOOL_OUTPUT_LIMIT * 2 {
                        t.streamed.push_str(d);
                    }
                }
            }
        }
        let status = u["status"].as_str().unwrap_or_default();
        if status != "completed" && status != "failed" {
            return;
        }
        self.record_call(&id);
        let Some(info) = self.tools.get_mut(&id) else {
            return;
        };
        if info.finished {
            return;
        }
        info.finished = true;
        let is_mcp = info.is_mcp;
        let signature = info.signature.clone();
        let title = info.title.clone();
        let streamed = std::mem::take(&mut info.streamed);
        let mut output = acp::content_text(&u["content"]);
        if output.is_empty() {
            output = streamed;
        }
        let exit_code = u["rawOutput"]["exit_code"]
            .as_i64()
            .or(u["rawOutput"]["exitCode"].as_i64());
        if output.is_empty() {
            if let Some(ro) = u.get("rawOutput") {
                output = match ro {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    v => v.to_string(),
                };
            }
        }
        let (output, truncated) = truncate(&output, TOOL_OUTPUT_LIMIT);
        let sandbox_denied = looks_like_sandbox_denial(&output);
        if !is_mcp {
            self.append(
                EventType::ToolResult,
                json!({"tool_call_id": id, "name": title, "status": status, "output": output, "truncated": truncated, "sandbox_denied": sandbox_denied, "exit_code": exit_code}),
            );
        }
        if status == "failed" {
            let n = self.failures.entry(signature).or_insert(0);
            *n += 1;
            if *n >= REPEAT_FAILURE_LIMIT && !self.cancel_requested {
                let reason = format!("the same tool call failed {REPEAT_FAILURE_LIMIT} times in one turn ({title}); the host stopped the turn");
                tracing::warn!(session = %self.session.session_id, "{reason}");
                self.request_cancel(Some(reason)).await;
            }
        }
    }

    /// Stop the agent so the sandbox runtime can clean up after itself:
    /// close stdin, then SIGTERM the runtime alone (it forwards the signal and
    /// removes its mount points), and only then kill the whole group.
    async fn stop_agent(&mut self) {
        let Some(mut a) = self.agent.take() else {
            return;
        };
        drop(a.conn);
        drop(a.incoming);
        let mut exited = matches!(
            tokio::time::timeout(Duration::from_secs(2), a.child.wait()).await,
            Ok(Ok(_))
        );
        if !exited {
            sandbox::terminate(&mut a.child);
            exited = matches!(
                tokio::time::timeout(Duration::from_secs(5), a.child.wait()).await,
                Ok(Ok(_))
            );
        }
        if !exited {
            sandbox::kill_tree(&mut a.child);
            let _ = tokio::time::timeout(Duration::from_secs(2), a.child.wait()).await;
        }
        sandbox::kill_group(&mut a.child);
        if !self.ctx.cfg.fake_agent_unsandboxed || self.spec.kind != AgentKind::Fake {
            let others = self
                .ctx
                .store
                .session_list(Some(&self.workspace.workspace_id))
                .map(|l| {
                    l.iter().any(|s| {
                        s.session_id != self.session.session_id
                            && !s.state.is_terminal()
                            && s.state != SessionState::Created
                    })
                })
                .unwrap_or(true);
            if !others {
                sandbox::sweep_mount_points(
                    std::path::Path::new(&self.workspace.path),
                    self.started_at,
                );
            }
        }
    }
}

/// Whether an ACP tool call is a call to our MCP gateway (named `agenthost`),
/// which records MCP calls itself. Agents name MCP tools differently:
/// `mcp__agenthost__<tool>` (Claude Code), `agenthost.<tool>` or
/// `agenthost/<tool>` (Codex, OpenCode).
pub fn is_gateway_tool(title: &str, update: &Value) -> bool {
    let t = title.trim().to_ascii_lowercase();
    let named = t.starts_with("mcp__agenthost__")
        || t.starts_with("agenthost.")
        || t.starts_with("agenthost/")
        || t.starts_with("agenthost_")
        || t.starts_with("agenthost:")
        || t.starts_with("agenthost-")
        || t.contains("(agenthost");
    let server = update["rawInput"]["server"]
        .as_str()
        .or(update["rawInput"]["providerIdentifier"].as_str())
        .or(update["_meta"]["server"].as_str())
        == Some("agenthost");
    named || server
}

/// Output that suggests the sandbox refused a file operation.
pub fn looks_like_sandbox_denial(output: &str) -> bool {
    let o = output.to_ascii_lowercase();
    o.contains("read-only file system")
        || o.contains("operation not permitted")
        || o.contains("permission denied")
}

fn last_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// A start-up failure with a user-facing reason and structured detail.
#[derive(Debug)]
struct StartFailure {
    reason: String,
    detail: Value,
}

impl StartFailure {
    fn new(reason: impl Into<String>, detail: Value) -> anyhow::Error {
        anyhow::Error::new(StartFailure {
            reason: reason.into(),
            detail,
        })
    }
}

impl std::fmt::Display for StartFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for StartFailure {}

fn prefix_failure(e: anyhow::Error, prefix: &str) -> anyhow::Error {
    match e.downcast::<StartFailure>() {
        Ok(f) => StartFailure::new(format!("{prefix}: {}", f.reason), f.detail),
        Err(e) => e.context(prefix.to_string()),
    }
}

#[allow(dead_code)]
fn _assert_send() {
    fn is_send<T: Send>() {}
    is_send::<Actor>();
}

impl From<StartFailure> for RpcError {
    fn from(f: StartFailure) -> Self {
        RpcError::new(jsonrpc::codes::UNAVAILABLE, f.reason).with_data(f.detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_tools_by_agent() {
        let none = json!({});
        assert!(is_gateway_tool("mcp__agenthost__probe__echo", &none)); // Claude Code
        assert!(is_gateway_tool("agenthost: magic_word", &none)); // Cursor
        assert!(is_gateway_tool("agenthost-magic_word: magic_word", &none)); // Cursor (permission)
        assert!(is_gateway_tool(
            "MCP: tool",
            &json!({"rawInput": {"providerIdentifier": "agenthost"}})
        ));
        assert!(is_gateway_tool("magic_word (agenthost MCP Server)", &none)); // Gemini CLI
        assert!(!is_gateway_tool("Shell: ls", &none));
        assert!(!is_gateway_tool(
            "MCP: tool",
            &json!({"rawInput": {"providerIdentifier": "figma"}})
        ));
    }
}
