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
use crate::title::{self, TitleSource};
use crate::util::{now_ms, truncate};
use agenthost_proto::jsonrpc::RpcError;
use agenthost_proto::*;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

/// Tool output above this is truncated in the event log.
pub const TOOL_OUTPUT_LIMIT: usize = 64 * 1024;
/// The same failing tool call this many times in one turn cancels the turn.
pub const REPEAT_FAILURE_LIMIT: usize = 3;
/// How long an agent gets to honour `session/cancel` before it is restarted.
pub const CANCEL_GRACE: Duration = Duration::from_secs(3);
const INIT_TIMEOUT: Duration = Duration::from_secs(60);
/// How long text the agent streams outside a prompt's turn (a reply to a
/// background task finishing, say) waits for more before it is written out.
/// No turn end comes to flush it, so without this it sat unwritten until the
/// next prompt, and showed up below that prompt.
const OUT_OF_TURN_SETTLE: Duration = Duration::from_millis(800);

#[derive(Debug)]
pub enum Cmd {
    Prompt {
        seq: i64,
        text: String,
        attachments: Vec<Attachment>,
    },
    Cancel,
    End,
    /// The MCP gateway is holding (true) or released (false) a call for approval.
    Approval {
        waiting: bool,
    },
    /// Stop the actor without changing the session's state (host shutdown).
    Shutdown,
    /// Change one of the agent's session settings (`session/set_config_option`).
    SetConfig {
        config_id: String,
        value: String,
        reply: oneshot::Sender<Result<Session, RpcError>>,
    },
    /// Ask a side question; replies with its side id once it is recorded.
    SideAsk {
        side_id: Option<String>,
        text: String,
        reply: oneshot::Sender<Result<String, RpcError>>,
    },
    // The actor's own: answers to its requests, sent back from the tasks
    // that wait for them so the actor keeps serving the agent meanwhile.
    ConfigDone {
        config_id: String,
        value: String,
        result: Result<Value, RpcError>,
        reply: oneshot::Sender<Result<Session, RpcError>>,
    },
    SideForked {
        side_id: String,
        result: Result<Value, RpcError>,
    },
    SideDone {
        side_id: String,
        result: Result<Value, RpcError>,
    },
    /// A step of asking the agent for a title (see [`Actor::ask_title`]).
    Title {
        ask: u64,
        step: TitleStep,
        result: Result<Value, RpcError>,
    },
}

/// How the host introduces the files attached to a message, in the text it
/// sends the agent.
pub const ATTACHED_FILES: &str = "Attached files (read them from these paths):";

/// Where asking the agent for a session's title has got to.
#[derive(Debug, Clone, Copy)]
pub enum TitleStep {
    /// The agent's list of its sessions has come (`session/list`): it may
    /// have titled this one already.
    Listed,
    /// Its conversation exists (`session/new`).
    Created,
    /// It is on its quickest model (`session/set_config_option`).
    ModelSet,
    /// It has answered (`session/prompt`).
    Answered,
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
    /// When this host run started (placeholders older than this aren't ours).
    started_at: std::time::SystemTime,
    /// Folders several sessions' sandboxes share.
    folders: Arc<sandbox::SharedFolders>,
}

/// How long `session.set_config` and `session.side_ask` wait for the actor.
const REPLY_TIMEOUT: Duration = Duration::from_secs(60);

impl SessionManager {
    pub fn new(ctx: Arc<Ctx>) -> Arc<Self> {
        Arc::new(Self {
            ctx,
            live: Mutex::new(HashMap::new()),
            started_at: std::time::SystemTime::now(),
            folders: Arc::new(sandbox::SharedFolders::default()),
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
            .insert(session.session_id.clone(), SessionHandle { tx: tx.clone() });
        let mgr = self.clone();
        let sid = session.session_id.clone();
        tokio::spawn(async move {
            let actor = Actor::new(mgr.ctx.clone(), session, workspace, rx, tx, mgr.folders.clone());
            actor.run(resume).await;
            mgr.live.lock().unwrap().remove(&sid);
        });
    }

    /// Create and start a session. In a git workspace it gets a worktree and
    /// a branch of its own; elsewhere sessions share the folder.
    pub async fn create(
        self: &Arc<Self>,
        p: SessionCreateParams,
        origin: Option<&str>,
    ) -> Result<Session, RpcError> {
        self.create_inner(p, origin, None).await
    }

    /// Create a session as a run of an automation. Only the host does this
    /// (never a client), because an automation's runs may use the tools it
    /// pre-approves.
    pub async fn create_for_automation(
        self: &Arc<Self>,
        p: SessionCreateParams,
        automation_id: &str,
    ) -> Result<Session, RpcError> {
        self.create_inner(p, Some("automation"), Some(automation_id)).await
    }

    async fn create_inner(
        self: &Arc<Self>,
        p: SessionCreateParams,
        origin: Option<&str>,
        automation_id: Option<&str>,
    ) -> Result<Session, RpcError> {
        let ws = self
            .ctx
            .store
            .workspace_get(&p.workspace_id)
            .map_err(|e| RpcError::internal(e.to_string()))?
            .ok_or_else(|| RpcError::not_found(format!("no workspace {}", p.workspace_id)))?;
        if p.agent == AgentKind::Fake && !self.ctx.cfg.enable_fake_agent {
            return Err(RpcError::new(
                jsonrpc::codes::UNAVAILABLE,
                "the fake agent is not enabled on this host",
            ));
        }
        // The agent's own name stands for its default login.
        let profile_id = match p.profile_id.as_deref() {
            None => None,
            Some(id) if id == p.agent.as_str() => None,
            Some(id) => {
                let prof = self
                    .ctx
                    .store
                    .profile_get(id)
                    .map_err(|e| RpcError::internal(e.to_string()))?
                    .ok_or_else(|| RpcError::not_found(format!("no agent profile {id}")))?;
                if prof.agent != p.agent {
                    return Err(RpcError::invalid_params(format!(
                        "profile {} ({}) is not a {} profile",
                        prof.name,
                        prof.agent.display_name(),
                        p.agent.display_name()
                    )));
                }
                Some(prof.profile_id)
            }
        };
        let session_id = crate::util::new_id("ses");
        // Quick tasks each have a folder of their own already.
        let worktree = if ws.kind == WorkspaceKind::Scratch {
            None
        } else {
            crate::worktree::create(&self.ctx.cfg, Path::new(&ws.path), &session_id)
                .await
                .map_err(|e| {
                    RpcError::new(
                        jsonrpc::codes::UNAVAILABLE,
                        format!("{e:#}; check the repository with `git worktree list`"),
                    )
                })?
        };
        let now = now_ms();
        let s = Session {
            session_id,
            workspace_id: ws.workspace_id.clone(),
            agent: p.agent,
            profile_id: profile_id.clone(),
            archived_at: None,
            worktree: worktree.as_ref().map(|(w, _)| w.clone()),
            config_options: vec![],
            side_questions: false,
            pull_request: None,
            automation_id: None,
            state: SessionState::Created,
            state_reason: None,
            acp_session_id: None,
            title: p.title,
            created_at: now,
            updated_at: now,
            last_seq: 0,
        };
        let s = Session { automation_id: automation_id.map(str::to_string), ..s };
        self.ctx
            .store
            .session_insert(&s)
            .map_err(|e| RpcError::internal(e.to_string()))?;
        if let Some((wt, subdir)) = &worktree {
            self.ctx
                .store
                .session_set_worktree(&s.session_id, wt, subdir)
                .map_err(|e| RpcError::internal(e.to_string()))?;
        }
        // Creation is the first `session.state` event.
        self.ctx
            .append(&s.session_id, EventType::SessionState, json!({"state": "created", "agent": p.agent, "profile_id": profile_id, "workspace_id": ws.workspace_id, "by": origin, "worktree": s.worktree}))
            .map_err(|e| RpcError::internal(e.to_string()))?;
        let s = self
            .ctx
            .store
            .session_get(&s.session_id)
            .ok()
            .flatten()
            .unwrap_or(s);
        // The first login started in a workspace becomes its default.
        if ws.default_agent.is_none() {
            let _ = self.ctx.store.workspace_set_default_agent(
                &ws.workspace_id,
                Some(p.agent),
                profile_id.as_deref(),
            );
        }
        self.ctx.bus.publish(BusMsg::SessionChanged(s.clone()));
        self.spawn_actor(s.clone(), ws, false);
        Ok(s)
    }

    /// Archive (ending it first if it is live) or unarchive a session. An
    /// archived session's worktree is removed when it has no uncommitted
    /// changes (its branch stays); unarchiving brings it back from the branch.
    pub async fn set_archived(&self, session_id: &str, archived: bool) -> Result<Session, RpcError> {
        let s = self.get(session_id)?;
        if archived {
            self.end(session_id).await?;
            if let Some(wt) = &s.worktree {
                match crate::worktree::remove_if_clean(wt).await {
                    Ok(removed) => tracing::info!(session = session_id, removed, "archived session's worktree"),
                    Err(e) => tracing::warn!(session = session_id, "could not remove the worktree {}: {e:#}", wt.path),
                }
            }
        } else if let Some(wt) = &s.worktree {
            let ws = self
                .ctx
                .store
                .workspace_get(&s.workspace_id)
                .map_err(|e| RpcError::internal(e.to_string()))?;
            if let Some(ws) = ws {
                if let Err(e) = crate::worktree::ensure(Path::new(&ws.path), wt).await {
                    tracing::warn!(session = session_id, "{e:#}");
                }
            }
        }
        self.ctx
            .store
            .session_set_archived(session_id, archived.then(now_ms))
            .map_err(|e| RpcError::internal(e.to_string()))?;
        let s = self.get(session_id)?;
        self.ctx.bus.publish(BusMsg::SessionChanged(s.clone()));
        Ok(s)
    }

    /// Give a session a title of its own, or clear it (`None` or blank).
    pub fn rename(&self, session_id: &str, title: Option<&str>) -> Result<Session, RpcError> {
        self.get(session_id)?;
        let title = title.map(str::trim).filter(|t| !t.is_empty());
        if title.is_some_and(|t| t.chars().count() > agenthost_proto::MAX_SESSION_TITLE) {
            return Err(RpcError::invalid_params(format!(
                "a session title has at most {} characters",
                agenthost_proto::MAX_SESSION_TITLE
            )));
        }
        self.ctx
            .store
            .session_set_title(session_id, title)
            .map_err(|e| RpcError::internal(e.to_string()))?;
        let s = self.get(session_id)?;
        self.ctx.bus.publish(BusMsg::SessionChanged(s.clone()));
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
        if p.text.trim().is_empty() && p.attachments.is_empty() {
            return Err(RpcError::invalid_params("empty prompt"));
        }
        if p.attachments.len() > MAX_ATTACHMENTS_PER_PROMPT {
            return Err(RpcError::invalid_params(format!(
                "at most {MAX_ATTACHMENTS_PER_PROMPT} attachments per message"
            )));
        }
        let mut attachments = vec![];
        for id in &p.attachments {
            match self.ctx.uploads.get(id) {
                Ok((a, _)) => attachments.push(a),
                // A retried prompt whose files were already handed over.
                Err(e) => {
                    if let Ok(Some(seq)) = self.ctx.store.prompt_seq(&p.session_id, &p.prompt_id) {
                        return Ok(SessionPromptResult {
                            seq,
                            accepted: false,
                        });
                    }
                    return Err(e);
                }
            }
        }
        let mut payload = json!({"text": p.text, "prompt_id": p.prompt_id, "device": origin});
        if !attachments.is_empty() {
            payload["attachments"] = serde_json::to_value(&attachments).unwrap();
        }
        let (ev, seq) = self
            .ctx
            .store
            .append_prompt(&p.session_id, &p.prompt_id, payload)
            .map_err(|e| RpcError::internal(e.to_string()))?;
        let Some(ev) = ev else {
            return Ok(SessionPromptResult {
                seq,
                accepted: false,
            });
        };
        self.ctx.bus.publish(BusMsg::Event(ev));
        // Until the agent names it, a session is named by its first message
        // (kept under a title the user gives it, for if they clear theirs).
        let t = title::from_prompt(&p.text).or_else(|| attachments.first().and_then(|a| title::from_prompt(&a.name)));
        if let Some(t) = t {
            match self.ctx.store.session_set_auto_title(&p.session_id, &t, TitleSource::Prompt) {
                Ok(true) => self.ctx.publish_session(&p.session_id),
                Ok(false) => {}
                Err(e) => tracing::warn!(session = %p.session_id, "title from the first message: {e:#}"),
            }
        }
        let h = self
            .handle(&p.session_id)
            .ok_or_else(|| RpcError::invalid_state("session is not running"))?;
        h.send(Cmd::Prompt {
            seq,
            text: p.text,
            attachments,
        });
        Ok(SessionPromptResult {
            seq,
            accepted: true,
        })
    }

    /// Change one of the agent's settings for a live session, and remember
    /// the choice for the next session with the same login.
    pub async fn set_config(&self, p: SessionSetConfigParams) -> Result<Session, RpcError> {
        let s = self.get(&p.session_id)?;
        if s.state.is_terminal() {
            return Err(RpcError::invalid_state(format!(
                "session is {}; start a new session to change its settings",
                s.state
            )));
        }
        let Some(opt) = s.config_options.iter().find(|o| o.id == p.config_id) else {
            return Err(RpcError::invalid_params(format!(
                "{} has no setting {} in this session",
                s.agent.display_name(),
                p.config_id
            )));
        };
        if !opt.options.iter().any(|v| v.value == p.value) {
            let choices: Vec<&str> = opt.options.iter().map(|v| v.value.as_str()).collect();
            return Err(RpcError::invalid_params(format!(
                "{} is not a choice for {}; pick one of: {}",
                p.value,
                opt.name,
                choices.join(", ")
            )));
        }
        let h = self
            .handle(&p.session_id)
            .ok_or_else(|| RpcError::invalid_state("session is not running"))?;
        let (tx, rx) = oneshot::channel();
        h.send(Cmd::SetConfig {
            config_id: p.config_id,
            value: p.value,
            reply: tx,
        });
        await_reply(rx).await
    }

    /// Ask a side question in a fork of the conversation; returns its side id
    /// at once, the answer follows as `side.*` events.
    pub async fn side_ask(&self, p: SessionSideAskParams) -> Result<SessionSideAskResult, RpcError> {
        let s = self.get(&p.session_id)?;
        if s.state.is_terminal() {
            return Err(RpcError::invalid_state(format!(
                "session is {}; side questions need a live session",
                s.state
            )));
        }
        if !s.side_questions {
            return Err(RpcError::invalid_state(format!(
                "{} can't answer side questions (it can't fork a conversation)",
                s.agent.display_name()
            )));
        }
        if p.text.trim().is_empty() {
            return Err(RpcError::invalid_params("empty question"));
        }
        let h = self
            .handle(&p.session_id)
            .ok_or_else(|| RpcError::invalid_state("session is not running"))?;
        let (tx, rx) = oneshot::channel();
        h.send(Cmd::SideAsk {
            side_id: p.side_id,
            text: p.text,
            reply: tx,
        });
        Ok(SessionSideAskResult {
            side_id: await_reply(rx).await?,
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

    /// Stop every agent, wait (up to `timeout`) for them to exit, then remove
    /// sandbox placeholders this host run left in workspaces. Each session's
    /// own sweep skips a workspace while other sessions there are live, which
    /// at shutdown is all of them.
    pub async fn shutdown_and_wait(&self, timeout: Duration) {
        self.shutdown();
        let deadline = Instant::now() + timeout;
        while self.live_count() > 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if self.live_count() > 0 {
            return; // still running: their placeholders are still mounted
        }
        if let Ok(list) = self.ctx.store.workspace_list() {
            // Quick tasks' folders are swept by their own sessions.
            for w in list.into_iter().filter(|w| w.kind != WorkspaceKind::Scratch) {
                sandbox::sweep_mount_points(Path::new(&w.path), self.started_at);
            }
        }
    }

    pub fn live_count(&self) -> usize {
        self.live.lock().unwrap().len()
    }

    /// How many live sandboxes share `folder` (a workspace outside git).
    pub fn folder_users(&self, folder: &Path) -> usize {
        self.folders.users(folder)
    }
}

async fn await_reply<T>(rx: oneshot::Receiver<Result<T, RpcError>>) -> Result<T, RpcError> {
    match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => Err(RpcError::invalid_state("the session ended")),
        Err(_) => Err(RpcError::new(
            jsonrpc::codes::UNAVAILABLE,
            "the agent did not answer in time; try again",
        )),
    }
}

/// The settings key for a login's last choice of a session setting.
fn config_choice_key(agent: AgentKind, profile_id: Option<&str>, config_id: &str) -> String {
    format!(
        "config_choice:{}:{}:{config_id}",
        agent.as_str(),
        profile_id.unwrap_or("")
    )
}

/// ACP `configOptions`: the `select` ones (grouped choices flattened), less
/// the permission mode, which the host sets. `None` when there are none.
pub fn parse_config_options(v: &Value) -> Option<Vec<SessionConfigOption>> {
    let list = v.as_array()?;
    let mut out = vec![];
    for o in list {
        if o["type"].as_str().unwrap_or("select") != "select" {
            continue;
        }
        let (Some(id), Some(current)) = (o["id"].as_str(), o["currentValue"].as_str()) else {
            continue;
        };
        let category = o["category"].as_str().map(str::to_string);
        if category.as_deref() == Some("mode") || id == "mode" {
            continue;
        }
        let mut options = vec![];
        fn flatten(items: &Value, out: &mut Vec<SessionConfigValue>) {
            for it in items.as_array().into_iter().flatten() {
                if it.get("options").is_some() {
                    flatten(&it["options"], out);
                } else if let Some(value) = it["value"].as_str() {
                    out.push(SessionConfigValue {
                        value: value.to_string(),
                        name: it["name"].as_str().unwrap_or(value).to_string(),
                        description: it["description"].as_str().map(str::to_string),
                    });
                }
            }
        }
        flatten(&o["options"], &mut options);
        out.push(SessionConfigOption {
            id: id.to_string(),
            name: o["name"].as_str().unwrap_or(id).to_string(),
            description: o["description"].as_str().map(str::to_string),
            category,
            current_value: current.to_string(),
            options,
        });
    }
    Some(out)
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
    /// The agent's own name for the tool (Claude: `_meta.claudeCode.toolName`).
    tool_name: Option<String>,
    /// The subagent call this call was made inside
    /// (Claude: `_meta.claudeCode.parentToolUseId`); later updates of a call
    /// can lose it, so the first one seen is kept.
    parent: Option<String>,
}

/// `_meta.claudeCode.parentToolUseId` of an update: the subagent's
/// `Agent`/`Task` call that produced it, if a subagent did.
fn parent_tool_use_id(u: &Value) -> Option<String> {
    u["_meta"]["claudeCode"]["parentToolUseId"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Whether a tool starts a subagent (Claude Code's `Agent`, formerly `Task`).
fn is_subagent_tool(name: Option<&str>) -> bool {
    matches!(name, Some("Agent" | "Task"))
}

/// `{description, agent_type, prompt}` of a subagent call, from its input.
fn subagent_info(raw_input: &Value) -> Value {
    let mut o = serde_json::Map::new();
    for (from, to) in [
        ("description", "description"),
        ("subagent_type", "agent_type"),
        ("prompt", "prompt"),
    ] {
        if let Some(v) = raw_input[from].as_str() {
            o.insert(to.into(), json!(v));
        }
    }
    Value::Object(o)
}

struct Running {
    child: Child,
    conn: Arc<AcpConn>,
    incoming: mpsc::Receiver<Incoming>,
    _proxy: EgressProxy,
}

struct Queued {
    seq: i64,
    text: String,
    attachments: Vec<Attachment>,
}

fn resource_link(uri: &str, a: &Attachment) -> Value {
    json!({"type": "resource_link", "uri": uri, "name": a.name, "mimeType": a.mime, "size": a.size})
}

fn is_text_mime(m: &str) -> bool {
    m.starts_with("text/") || m == "application/json"
}

fn human_size(b: u64) -> String {
    if b >= 1024 * 1024 {
        format!("{:.1} MB", b as f64 / (1024.0 * 1024.0))
    } else if b >= 1024 {
        format!("{} KB", b / 1024)
    } else {
        format!("{b} bytes")
    }
}

/// `dir/name`, or `dir/name (2).ext` and so on if taken.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (2..)
        .map(|i| dir.join(format!("{stem} ({i}){ext}")))
        .find(|p| !p.exists())
        .unwrap()
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
    queue: VecDeque<Queued>,
    /// The agent's `promptCapabilities` from `initialize`.
    prompt_caps: Value,
    turn: Option<TurnFuture>,
    turn_prompt_seq: i64,
    cancel_deadline: Option<Instant>,
    cancel_requested: bool,
    host_cancel_reason: Option<String>,
    /// Text being streamed: (event kind, accumulated text).
    buf: Option<(EventType, String)>,
    /// When text last streamed in; out of a turn, it is written out once it settles.
    streamed_at: Option<Instant>,
    /// What each running subagent is saying, by its parent tool call id, in
    /// the order the subagents first spoke; kept apart from `buf`, since
    /// parallel subagents interleave with each other and the main agent.
    sub_bufs: Vec<(String, String)>,
    tools: HashMap<String, ToolInfo>,
    failures: HashMap<String, usize>,
    /// Ignore `session/update` while the agent replays history on load.
    loading: bool,
    /// When this actor started the agent (for the mount-point sweep).
    started_at: std::time::SystemTime,
    /// For tasks that wait on the agent's answers and report back.
    self_tx: mpsc::UnboundedSender<Cmd>,
    folders: Arc<sandbox::SharedFolders>,
    /// The shared folder this actor's sandbox counts as a user of.
    shared_folder: Option<PathBuf>,
    /// The workspace's own folder (the session works in its worktree when
    /// it has one, or its quick-task folder).
    repo_path: PathBuf,
    /// Side conversations (`/btw`), by side id.
    sides: HashMap<String, Side>,
    /// The fork's ACP session id → side id.
    side_routes: HashMap<String, String>,
    /// The agent can close a conversation (`session/close`).
    can_close: bool,
    /// The agent can list its sessions, with their titles (`session/list`).
    can_list: bool,
    /// The agent has been asked for a title since this actor started (once is enough).
    title_asked: bool,
    /// The title being asked for.
    title_ask: Option<TitleAsk>,
}

/// Asking the agent for a title, in a conversation of its own that no client
/// sees: no tools, no connectors, just the session's first message.
struct TitleAsk {
    /// Tells this ask's answers from an earlier one's (before a restart).
    id: u64,
    /// The conversation, once it exists.
    acp_id: Option<String>,
    /// The message asking for the title.
    prompt: String,
    /// The answer so far.
    text: String,
}

/// A side conversation: a fork of the session the agent answers in.
struct Side {
    acp_id: Option<String>,
    /// A question is being answered.
    busy: bool,
    /// The question waiting for the fork to exist.
    pending: Option<String>,
    /// The answer streamed so far.
    text: String,
}

enum Exit {
    Ended,
    Failed(String, Value),
    Shutdown,
}

/// A quick task's own folder. Kept after the session ends, with whatever the
/// agent left there.
pub fn scratch_folder(ctx: &Ctx, session_id: &str) -> PathBuf {
    ctx.cfg.scratch_dir().join(session_id)
}

/// The session's agent spec: its profile's login (if it has one and the
/// profile still exists), with every other profile's folder to hide.
fn session_spec(ctx: &Ctx, session: &Session) -> AgentSpec {
    let profiles = ctx.store.profile_list().unwrap_or_default();
    let own = session
        .profile_id
        .as_deref()
        .and_then(|id| profiles.iter().find(|p| p.profile_id == id));
    let mut spec = agents::spec_for(session.agent, own, &ctx.cfg);
    spec.other_logins = profiles
        .iter()
        .filter(|p| Some(p.profile_id.as_str()) != session.profile_id.as_deref())
        .map(|p| PathBuf::from(&p.config_dir))
        .collect();
    spec
}

impl Actor {
    fn new(
        ctx: Arc<Ctx>,
        session: Session,
        workspace: Workspace,
        rx: mpsc::UnboundedReceiver<Cmd>,
        self_tx: mpsc::UnboundedSender<Cmd>,
        folders: Arc<sandbox::SharedFolders>,
    ) -> Self {
        let spec = session_spec(&ctx, &session);
        let mut workspace = workspace;
        let repo_path = PathBuf::from(&workspace.path);
        if workspace.kind == WorkspaceKind::Scratch {
            workspace.path = scratch_folder(&ctx, &session.session_id)
                .display()
                .to_string();
        } else if let Some(wt) = &session.worktree {
            // The same folder of the repository, in the session's worktree.
            let sub = ctx
                .store
                .session_worktree_subdir(&session.session_id)
                .unwrap_or_default();
            let mut p = PathBuf::from(&wt.path);
            if !sub.is_empty() {
                p.push(sub);
            }
            workspace.path = p.display().to_string();
        }
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
            prompt_caps: Value::Null,
            turn: None,
            turn_prompt_seq: 0,
            cancel_deadline: None,
            cancel_requested: false,
            host_cancel_reason: None,
            buf: None,
            streamed_at: None,
            sub_bufs: Vec::new(),
            tools: HashMap::new(),
            failures: HashMap::new(),
            loading: false,
            started_at: std::time::SystemTime::now(),
            self_tx,
            folders,
            shared_folder: None,
            repo_path,
            sides: HashMap::new(),
            side_routes: HashMap::new(),
            can_close: false,
            can_list: false,
            title_asked: false,
            title_ask: None,
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
            Ok(()) => {
                // A session still named by its first message (one from before
                // the host titled sessions, or whose asking failed) is titled
                // now, not at its next message, which may never come.
                if resume {
                    self.ask_title(None).await;
                }
                self.main_loop().await
            }
            Err(e) => match e.downcast::<StartFailure>() {
                Ok(f) => Exit::Failed(f.reason, f.detail),
                Err(e) => Exit::Failed(format!("{e:#}"), Value::Null),
            },
        };
        self.end_sides(match exit {
            Exit::Shutdown => "the host restarted; ask again",
            _ => "the session ended",
        });
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

        if let Some(id) = &self.session.profile_id {
            if self.spec.profile.is_none() {
                return Err(StartFailure::new(
                    format!("the agent profile {id} was removed"),
                    Value::Null,
                ));
            }
        }
        if let Some(problem) = &self.spec.profile_problem {
            return Err(StartFailure::new(problem.clone(), Value::Null));
        }
        let status = agents::status_for(
            self.session.agent,
            self.spec.profile.as_ref(),
            &self.ctx.cfg,
        );
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
                &self.spec.codex_home(&self.ctx.cfg),
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
                &self.spec.codex_home(&self.ctx.cfg),
                std::path::Path::new(&self.workspace.path),
            );
            self.spec
                .env
                .push(("CODEX_CONFIG".into(), thread_cfg.to_string()));
        }
        let workspace_path = PathBuf::from(&self.workspace.path);
        if self.workspace.kind == WorkspaceKind::Scratch {
            crate::config::create_private_dir(&workspace_path)?;
        }
        let mut git_access = None;
        if let Some(wt) = self.session.worktree.clone() {
            // Removed when the session was archived: back from its branch.
            crate::worktree::ensure(&self.repo_path, &wt)
                .await
                .map_err(|e| StartFailure::new(format!("{e:#}"), Value::Null))?;
            let access = crate::worktree::access(Path::new(&wt.path))
                .await
                .map_err(|e| StartFailure::new(format!("the session's worktree {} is not usable: {e:#}", wt.path), Value::Null))?;
            crate::worktree::protect_common_dir(&access.common_dir)
                .map_err(|e| StartFailure::new(format!("could not protect the repository's .git: {e:#}"), Value::Null))?;
            git_access = Some(access);
        }
        if !workspace_path.is_dir() {
            return Err(StartFailure::new(
                format!("workspace folder {} does not exist", self.workspace.path),
                Value::Null,
            ));
        }
        let policy: SandboxPolicy = match &git_access {
            Some(git) => sandbox::policy_for_worktree(
                &self.ctx.cfg,
                &self.spec,
                &workspace_path,
                &self.temp_dir,
                &self.workspace.network,
                git,
            ),
            None => sandbox::policy_for(
                &self.ctx.cfg,
                &self.spec,
                &workspace_path,
                &self.temp_dir,
                &self.workspace.network,
            ),
        };
        let sandboxed = !(self.spec.kind == AgentKind::Fake && self.ctx.cfg.fake_agent_unsandboxed);
        // A folder other sessions may be using at the same time.
        if sandboxed
            && git_access.is_none()
            && self.workspace.kind != WorkspaceKind::Scratch
            && self.shared_folder.is_none()
        {
            self.folders.acquire(&workspace_path);
            self.shared_folder = Some(workspace_path.clone());
        }

        let proxy = EgressProxy::start(Arc::new(SessionNet {
            ctx: self.ctx.clone(),
            session_id: self.sid(),
            workspace_id: self.workspace.workspace_id.clone(),
            agent_hosts: self.spec.hosts.clone(),
        }))
        .await?;

        let launch = if !sandboxed {
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
        self.prompt_caps = init["agentCapabilities"]["promptCapabilities"].clone();
        let can_fork = init["agentCapabilities"]["sessionCapabilities"]
            .get("fork")
            .map(|f| !f.is_null())
            .unwrap_or(false);
        self.can_close = init["agentCapabilities"]["sessionCapabilities"]
            .get("close")
            .is_some_and(|c| !c.is_null());
        self.can_list = init["agentCapabilities"]["sessionCapabilities"]
            .get("list")
            .is_some_and(|c| !c.is_null());
        if can_fork != self.session.side_questions {
            self.ctx
                .store
                .session_set_side_questions(&self.session.session_id, can_fork)?;
            self.session.side_questions = can_fork;
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
            let r = r.map_err(|e| prefix_failure(e, "resume failed"))?;
            if let Some(opts) = parse_config_options(&r["configOptions"]) {
                self.set_config_options(opts);
            }
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
            if let Some(opts) = parse_config_options(&r["configOptions"]) {
                self.set_config_options(opts);
            }
        }
        self.apply_session_mode().await?;
        if !resume {
            self.apply_remembered_config().await;
        }
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

    /// Store the agent's settings as it reports them, and tell clients.
    fn set_config_options(&mut self, opts: Vec<SessionConfigOption>) {
        if opts == self.session.config_options {
            return;
        }
        if let Err(e) = self
            .ctx
            .store
            .session_set_config_options(&self.session.session_id, &opts)
        {
            tracing::error!(session = %self.session.session_id, "store config options: {e}");
        }
        self.session.config_options = opts;
        // While starting, the change goes out with the session becoming idle.
        if self.state != SessionState::Starting {
            self.ctx.publish_session(&self.session.session_id);
        }
    }

    /// A new session starts with the choices last made for this login
    /// (model, effort, …), where the agent still offers them.
    async fn apply_remembered_config(&mut self) {
        let ids: Vec<String> = self.session.config_options.iter().map(|o| o.id.clone()).collect();
        for id in ids {
            let key = config_choice_key(self.session.agent, self.session.profile_id.as_deref(), &id);
            let Ok(Some(value)) = self.ctx.store.setting_get::<String>(&key) else {
                continue;
            };
            // Re-read: changing one setting (the model) can change another's choices.
            let Some(opt) = self.session.config_options.iter().find(|o| o.id == id) else {
                continue;
            };
            if opt.current_value == value || !opt.options.iter().any(|v| v.value == value) {
                continue;
            }
            let acp_id = self.session.acp_session_id.clone().unwrap_or_default();
            match self
                .call_during_start(
                    "session/set_config_option",
                    json!({"sessionId": acp_id, "configId": id, "value": value}),
                )
                .await
            {
                Ok(r) => self.config_changed(&id, &value, &r),
                Err(e) => tracing::warn!(session = %self.session.session_id, "could not apply the remembered {id}: {e:#}"),
            }
        }
    }

    /// After `session/set_config_option`: the agent's settings as it now reports them.
    fn config_changed(&mut self, config_id: &str, value: &str, result: &Value) {
        let opts = parse_config_options(&result["configOptions"]).unwrap_or_else(|| {
            let mut o = self.session.config_options.clone();
            if let Some(x) = o.iter_mut().find(|x| x.id == config_id) {
                x.current_value = value.to_string();
            }
            o
        });
        self.set_config_options(opts);
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
                if let Some(q) = self.queue.pop_front() {
                    if let Err(e) = self.start_turn(q).await {
                        return Exit::Failed(format!("could not send prompt: {e}"), Value::Null);
                    }
                }
            }
            enum Step {
                Cmd(Option<Cmd>),
                Msg(Option<Incoming>),
                Turn(Result<Value, RpcError>),
                CancelTimeout,
                Settled,
            }
            let cancel_deadline = self.cancel_deadline;
            let settle_at = self
                .streamed_at
                .filter(|_| self.turn.is_none() && (self.buf.is_some() || !self.sub_bufs.is_empty()))
                .map(|t| t + OUT_OF_TURN_SETTLE);
            let step = {
                let agent = self.agent.as_mut().expect("agent running in main loop");
                let turn = &mut self.turn;
                let has_turn = turn.is_some();
                tokio::select! {
                    c = self.rx.recv() => Step::Cmd(c),
                    m = agent.incoming.recv() => Step::Msg(m),
                    r = async { turn.as_mut().unwrap().await }, if has_turn => Step::Turn(r),
                    _ = async { tokio::time::sleep_until(cancel_deadline.unwrap()).await }, if cancel_deadline.is_some() && has_turn => Step::CancelTimeout,
                    _ = async { tokio::time::sleep_until(settle_at.unwrap()).await }, if settle_at.is_some() => Step::Settled,
                }
            };
            match step {
                Step::Cmd(None) | Step::Cmd(Some(Cmd::Shutdown)) => return Exit::Shutdown,
                Step::Cmd(Some(Cmd::End)) => return Exit::Ended,
                Step::Cmd(Some(Cmd::Prompt {
                    seq,
                    text,
                    attachments,
                })) => {
                    // Anything the agent said on its own goes before this prompt's reply.
                    if self.turn.is_none() {
                        self.flush_all();
                    }
                    self.queue.push_back(Queued {
                        seq,
                        text,
                        attachments,
                    })
                }
                Step::Settled => {
                    self.record_pending_calls();
                    self.flush_all();
                    self.streamed_at = None;
                }
                Step::Cmd(Some(Cmd::Cancel)) => self.request_cancel(None).await,
                Step::Cmd(Some(Cmd::SetConfig {
                    config_id,
                    value,
                    reply,
                })) => self.send_config(config_id, value, reply).await,
                Step::Cmd(Some(Cmd::ConfigDone {
                    config_id,
                    value,
                    result,
                    reply,
                })) => {
                    let r = match result {
                        Ok(v) => {
                            self.config_changed(&config_id, &value, &v);
                            let key = config_choice_key(
                                self.session.agent,
                                self.session.profile_id.as_deref(),
                                &config_id,
                            );
                            if let Err(e) = self.ctx.store.setting_set(&key, &value) {
                                tracing::warn!("remember {config_id}: {e}");
                            }
                            Ok(self.ctx.store.session_get(&self.session.session_id).ok().flatten().unwrap_or_else(|| self.session.clone()))
                        }
                        Err(e) => Err(RpcError::new(
                            jsonrpc::codes::UNAVAILABLE,
                            format!("{} refused the change: {}", self.spec.kind.display_name(), e.message),
                        )),
                    };
                    let _ = reply.send(r);
                }
                Step::Cmd(Some(Cmd::SideAsk {
                    side_id,
                    text,
                    reply,
                })) => {
                    let r = self.side_ask(side_id, text).await;
                    let _ = reply.send(r);
                }
                Step::Cmd(Some(Cmd::SideForked { side_id, result })) => {
                    self.side_forked(side_id, result).await
                }
                Step::Cmd(Some(Cmd::SideDone { side_id, result })) => {
                    self.side_done(&side_id, result)
                }
                Step::Cmd(Some(Cmd::Title { ask, step, result })) => {
                    self.title_step(ask, step, result).await
                }
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
                        self.flush_all();
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
                    self.flush_all();
                    let reason = self.host_cancel_reason.take();
                    self.append(EventType::TurnEnd, json!({
                        "stop_reason": "cancelled",
                        "host_reason": reason.unwrap_or_else(|| "the agent did not stop in time; it was restarted".into()),
                        "prompt_seq": self.turn_prompt_seq,
                        "forced": true
                    }));
                    self.cancel_requested = false;
                    self.end_sides("the agent was restarted; ask again");
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

    /// Send a request to the agent and have its answer come back as a
    /// command, so the actor keeps serving the agent while it waits.
    async fn request_then(
        &mut self,
        method: &str,
        params: Value,
        then: impl FnOnce(Result<Value, RpcError>) -> Cmd + Send + 'static,
    ) {
        let tx = self.self_tx.clone();
        let rx = match &self.agent {
            Some(a) => a.conn.request(method, params).await,
            None => Err(anyhow!("the agent is not running")),
        };
        match rx {
            Ok(rx) => {
                tokio::spawn(async move {
                    let r = rx
                        .await
                        .unwrap_or_else(|_| Err(RpcError::internal("the agent exited")));
                    let _ = tx.send(then(r));
                });
            }
            Err(e) => {
                let _ = tx.send(then(Err(RpcError::internal(e.to_string()))));
            }
        }
    }

    async fn send_config(
        &mut self,
        config_id: String,
        value: String,
        reply: oneshot::Sender<Result<Session, RpcError>>,
    ) {
        let acp_id = self.session.acp_session_id.clone().unwrap_or_default();
        let params = json!({"sessionId": acp_id, "configId": config_id, "value": value});
        self.request_then("session/set_config_option", params, move |result| Cmd::ConfigDone {
            config_id,
            value,
            result,
            reply,
        })
        .await;
    }

    /// Record a side question and put it to its fork (forking first for a
    /// new side conversation). The main turn, if any, carries on meanwhile.
    async fn side_ask(&mut self, side_id: Option<String>, text: String) -> Result<String, RpcError> {
        if self.agent.is_none() {
            return Err(RpcError::invalid_state("the agent is not running"));
        }
        let side_id = match side_id {
            Some(id) => {
                let Some(side) = self.sides.get(&id) else {
                    return Err(RpcError::invalid_state(
                        "this side conversation ended when the host restarted; ask again",
                    ));
                };
                if side.busy {
                    return Err(RpcError::invalid_state(
                        "this side conversation is still answering; wait for it to finish",
                    ));
                }
                id
            }
            None => crate::util::new_id("side"),
        };
        self.append(
            EventType::SideQuestion,
            json!({"side_id": side_id, "text": text}),
        );
        match self.sides.get_mut(&side_id) {
            Some(side) => {
                side.busy = true;
                side.text.clear();
                let acp = side.acp_id.clone().unwrap_or_default();
                self.side_prompt(&side_id, &acp, &text).await;
            }
            None => {
                self.sides.insert(
                    side_id.clone(),
                    Side {
                        acp_id: None,
                        busy: true,
                        pending: Some(text),
                        text: String::new(),
                    },
                );
                let mut params = json!({
                    "sessionId": self.session.acp_session_id.clone().unwrap_or_default(),
                    "cwd": self.workspace.path,
                    // No connectors in a side conversation: their calls would
                    // land in the session's own transcript.
                    "mcpServers": [],
                });
                if let Some(m) = &self.spec.session_meta {
                    params["_meta"] = m.clone();
                }
                let id = side_id.clone();
                self.request_then("session/fork", params, move |result| Cmd::SideForked {
                    side_id: id,
                    result,
                })
                .await;
            }
        }
        Ok(side_id)
    }

    async fn side_forked(&mut self, side_id: String, result: Result<Value, RpcError>) {
        let acp = match result {
            Ok(v) => v["sessionId"].as_str().map(str::to_string),
            Err(e) => {
                self.side_done(&side_id, Err(e));
                return;
            }
        };
        let Some(acp) = acp else {
            self.side_done(&side_id, Err(RpcError::internal("session/fork returned no sessionId")));
            return;
        };
        let Some(side) = self.sides.get_mut(&side_id) else {
            return; // ended meanwhile
        };
        side.acp_id = Some(acp.clone());
        let text = side.pending.take().unwrap_or_default();
        self.side_routes.insert(acp.clone(), side_id.clone());
        self.side_prompt(&side_id, &acp, &text).await;
    }

    async fn side_prompt(&mut self, side_id: &str, acp_id: &str, text: &str) {
        let id = side_id.to_string();
        self.request_then(
            "session/prompt",
            json!({"sessionId": acp_id, "prompt": [{"type": "text", "text": text}]}),
            move |result| Cmd::SideDone { side_id: id, result },
        )
        .await;
    }

    /// Record a side answer (or its failure) once the fork's turn is over.
    fn side_done(&mut self, side_id: &str, result: Result<Value, RpcError>) {
        let Some(side) = self.sides.get_mut(side_id) else {
            return; // from before a restart
        };
        if !side.busy {
            return;
        }
        side.busy = false;
        let text = std::mem::take(&mut side.text);
        let forked = side.acp_id.is_some();
        if !forked {
            self.sides.remove(side_id);
        }
        if !text.is_empty() {
            self.append(
                EventType::SideMessage,
                json!({"side_id": side_id, "text": text}),
            );
        }
        let payload = match result {
            Ok(v) => json!({"side_id": side_id, "stop_reason": v["stopReason"].as_str().unwrap_or("end_turn")}),
            Err(e) => json!({"side_id": side_id, "stop_reason": "error", "error": e.message}),
        };
        self.append(EventType::SideEnd, payload);
    }

    /// End every side conversation (the agent is stopping), recording the
    /// end of any answer still running.
    fn end_sides(&mut self, why: &str) {
        let busy: Vec<String> = self
            .sides
            .iter()
            .filter(|(_, s)| s.busy)
            .map(|(k, _)| k.clone())
            .collect();
        for id in busy {
            self.side_done(&id, Err(RpcError::internal(why)));
        }
        self.sides.clear();
        self.side_routes.clear();
    }

    /// An update from a side conversation's fork: only its answer counts.
    fn on_side_update(&mut self, side_id: &str, u: &Value) {
        if u["sessionUpdate"].as_str() != Some("agent_message_chunk")
            || parent_tool_use_id(u).is_some()
        {
            return; // tool calls, thoughts, plans, subagents: not the answer
        }
        let text = acp::content_text(&u["content"]);
        let Some(side) = self.sides.get_mut(side_id) else {
            return;
        };
        if text.is_empty() || !side.busy {
            return;
        }
        side.text.push_str(&text);
        self.ctx.bus.publish(BusMsg::Delta(SessionDelta {
            session_id: self.session.session_id.clone(),
            kind: EventType::SideMessage,
            text,
            side_id: Some(side_id.to_string()),
        }));
    }

    /// Ask the agent for a title for the session, once per run of the agent,
    /// unless it has a good one already (the user's, or the agent's own). The
    /// session's first message is what's titled; `fallback` (the message
    /// being sent) if none is recorded.
    ///
    /// A resumed session (no `fallback`) may have been titled by the agent
    /// already, out of the host's sight, so its list of sessions is read first.
    async fn ask_title(&mut self, fallback: Option<&str>) {
        if self.title_asked {
            return;
        }
        let sid = self.session.session_id.clone();
        match self.ctx.store.session_wants_title(&sid) {
            Ok(true) => {}
            Ok(false) => {
                self.title_asked = true;
                return;
            }
            Err(e) => {
                tracing::warn!(session = %sid, "title: {e:#}");
                return;
            }
        }
        let first = self.ctx.store.first_prompt_text(&sid).ok().flatten();
        let Some(request) = first.or(fallback.map(str::to_string)) else {
            return; // nothing said yet: asked at the first message
        };
        self.title_asked = true;
        if request.trim().is_empty() {
            return; // only files: the name from the first message stands
        }
        let id = self.title_ask.as_ref().map_or(1, |a| a.id + 1);
        self.title_ask = Some(TitleAsk {
            id,
            acp_id: None,
            prompt: title::ask(&request),
            text: String::new(),
        });
        if fallback.is_none() && self.can_list {
            let params = json!({"cwd": self.workspace.path});
            self.request_then("session/list", params, move |result| Cmd::Title {
                ask: id,
                step: TitleStep::Listed,
                result,
            })
            .await;
        } else {
            self.title_conversation(id).await;
        }
    }

    /// Start the hidden conversation the title is asked for in.
    async fn title_conversation(&mut self, ask: u64) {
        let mut params = json!({"cwd": self.workspace.path, "mcpServers": []});
        if let Some(m) = self.spec.title_session_meta() {
            params["_meta"] = m;
        }
        self.request_then("session/new", params, move |result| Cmd::Title {
            ask,
            step: TitleStep::Created,
            result,
        })
        .await;
    }

    /// Whether an agent's title for the session only repeats one of its
    /// messages (see [`title::echoes`]).
    fn echoes_a_message(&self, t: &str) -> bool {
        match self.ctx.store.prompt_texts(&self.session.session_id) {
            Ok(prompts) => title::echoes(t, prompts.iter().map(String::as_str)),
            Err(e) => {
                tracing::warn!(session = %self.session.session_id, "title: {e:#}");
                false
            }
        }
    }

    /// A title the agent gave the session itself, unless it is only a message.
    fn agent_title(&self, raw: &str) -> Option<String> {
        if self.echoes_a_message(raw) {
            tracing::debug!(session = %self.session.session_id, "the agent's title only repeats a message");
            return None;
        }
        title::from_agent(raw)
    }

    async fn title_step(&mut self, ask: u64, step: TitleStep, result: Result<Value, RpcError>) {
        let Some(a) = self.title_ask.as_mut().filter(|a| a.id == ask) else {
            return; // from before a restart
        };
        let sid = self.session.session_id.clone();
        let then = move |step| move |result| Cmd::Title { ask, step, result };
        match step {
            TitleStep::Listed => {
                let main = self.session.acp_session_id.as_deref().unwrap_or_default();
                let listed = result.as_ref().ok().and_then(|v| {
                    v["sessions"].as_array()?.iter().find(|s| !main.is_empty() && s["sessionId"] == main)?["title"]
                        .as_str()
                        .map(str::to_string)
                });
                match listed.and_then(|t| self.agent_title(&t)) {
                    Some(t) => {
                        self.title_ask = None;
                        self.set_auto_title(&t, TitleSource::Agent);
                    }
                    None => self.title_conversation(ask).await,
                }
            }
            TitleStep::Created => {
                let Some(acp_id) = result.as_ref().ok().and_then(|v| v["sessionId"].as_str()).map(str::to_string) else {
                    tracing::info!(session = %sid, "could not ask the agent for a title: {:?}", result.err().map(|e| e.message));
                    self.title_ask = None;
                    return;
                };
                a.acp_id = Some(acp_id.clone());
                let options = result.ok().and_then(|v| parse_config_options(&v["configOptions"])).unwrap_or_default();
                match title::quick_model(&options) {
                    Some((config_id, value)) => {
                        let params = json!({"sessionId": acp_id, "configId": config_id, "value": value});
                        self.request_then("session/set_config_option", params, then(TitleStep::ModelSet)).await;
                    }
                    None => self.title_prompt(ask).await,
                }
            }
            // On its own model if it couldn't switch: a title is still wanted.
            TitleStep::ModelSet => self.title_prompt(ask).await,
            TitleStep::Answered => {
                let a = self.title_ask.take().expect("checked above");
                if let Some(acp_id) = a.acp_id.filter(|_| self.can_close) {
                    // Done with: closed, its answer ignored (asks are numbered from 1).
                    self.request_then("session/close", json!({"sessionId": acp_id}), |_| Cmd::Title {
                        ask: 0,
                        step: TitleStep::Answered,
                        result: Ok(Value::Null),
                    })
                    .await;
                }
                if let Err(e) = &result {
                    tracing::info!(session = %sid, "the agent did not give a title: {}", e.message);
                    return;
                }
                let Some(t) = title::from_answer(&a.text) else {
                    tracing::info!(session = %sid, "the agent's title answer had no title in it");
                    return;
                };
                self.set_auto_title(&t, TitleSource::Asked);
            }
        }
    }

    async fn title_prompt(&mut self, ask: u64) {
        let Some((acp_id, prompt)) = self
            .title_ask
            .as_ref()
            .and_then(|a| Some((a.acp_id.clone()?, a.prompt.clone())))
        else {
            return;
        };
        self.request_then(
            "session/prompt",
            json!({"sessionId": acp_id, "prompt": [{"type": "text", "text": prompt}]}),
            move |result| Cmd::Title { ask, step: TitleStep::Answered, result },
        )
        .await;
    }

    /// Record a title the host or agent came up with, and tell clients if it shows.
    fn set_auto_title(&mut self, t: &str, source: TitleSource) {
        let sid = &self.session.session_id;
        match self.ctx.store.session_set_auto_title(sid, t, source) {
            Ok(true) => self.ctx.publish_session(sid),
            Ok(false) => {}
            Err(e) => tracing::warn!(session = %sid, "store the title: {e:#}"),
        }
    }

    async fn start_turn(&mut self, q: Queued) -> Result<()> {
        self.ask_title(Some(&q.text)).await;
        let seq = q.seq;
        let acp_id = self.session.acp_session_id.clone().unwrap_or_default();
        let conn = self.agent.as_ref().unwrap().conn.clone();
        let prompt = self.prompt_blocks(q)?;
        let rx = conn
            .request(
                "session/prompt",
                json!({"sessionId": acp_id, "prompt": prompt}),
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

    /// ACP content for a prompt. Attached files are copied into the session's
    /// temp folder, which the agent can read, and named in the text so every
    /// agent sees them; images also go inline and small text files embedded
    /// when the agent says it accepts those.
    fn prompt_blocks(&self, q: Queued) -> Result<Vec<Value>> {
        if q.attachments.is_empty() {
            return Ok(vec![json!({"type": "text", "text": q.text})]);
        }
        let dir = self.temp_dir.join("attachments");
        std::fs::create_dir_all(&dir)?;
        let image_ok = self.prompt_caps["image"].as_bool().unwrap_or(false);
        let embed_ok = self.prompt_caps["embeddedContext"].as_bool().unwrap_or(false);
        let mut listing = String::from(ATTACHED_FILES);
        let mut extra = vec![];
        for a in &q.attachments {
            let src = match self.ctx.uploads.get(&a.attachment_id) {
                Ok((_, p)) => p,
                Err(_) => {
                    listing.push_str(&format!("\n- {} (no longer available)", a.name));
                    continue;
                }
            };
            let dest = unique_path(&dir, &a.name);
            std::fs::copy(&src, &dest)?;
            self.ctx.uploads.remove(&a.attachment_id);
            let path = dest.to_string_lossy().to_string();
            listing.push_str(&format!("\n- {path} ({}, {})", a.mime, human_size(a.size)));
            let uri = format!("file://{path}");
            if a.is_image() && image_ok && a.size <= 5 * 1024 * 1024 && a.mime != "image/heic" {
                use base64::Engine;
                let data = base64::engine::general_purpose::STANDARD.encode(std::fs::read(&dest)?);
                extra.push(json!({"type": "image", "mimeType": a.mime, "data": data, "uri": uri}));
            } else if embed_ok && is_text_mime(&a.mime) && a.size <= 256 * 1024 {
                match std::fs::read_to_string(&dest) {
                    Ok(text) => extra.push(json!({
                        "type": "resource",
                        "resource": {"uri": uri, "mimeType": a.mime, "text": text}
                    })),
                    Err(_) => extra.push(resource_link(&uri, a)),
                }
            } else {
                extra.push(resource_link(&uri, a));
            }
        }
        let text = if q.text.trim().is_empty() {
            listing
        } else {
            format!("{}\n\n{listing}", q.text)
        };
        let mut blocks = vec![json!({"type": "text", "text": text})];
        blocks.extend(extra);
        Ok(blocks)
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
        self.flush_all();
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

    /// Write out the main agent's streamed text and every subagent's.
    fn flush_all(&mut self) {
        self.flush();
        for (parent, text) in std::mem::take(&mut self.sub_bufs) {
            self.append_subagent_text(&parent, text);
        }
    }

    /// Write out what one subagent has said since its last step.
    fn flush_subagent(&mut self, parent: &str) {
        if let Some(i) = self.sub_bufs.iter().position(|(p, _)| p == parent) {
            let (parent, text) = self.sub_bufs.remove(i);
            self.append_subagent_text(&parent, text);
        }
    }

    fn append_subagent_text(&self, parent: &str, text: String) {
        if !text.trim().is_empty() {
            self.append(
                EventType::SubagentMessage,
                json!({"parent_tool_call_id": parent, "text": text}),
            );
        }
    }

    /// A subagent's message chunk: coalesced per subagent, never part of the
    /// main reply (and not streamed as deltas).
    fn stream_subagent(&mut self, parent: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        self.record_pending_calls_of(Some(parent));
        self.streamed_at = Some(Instant::now());
        match self.sub_bufs.iter_mut().find(|(p, _)| p == parent) {
            Some((_, buf)) => buf.push_str(text),
            None => self.sub_bufs.push((parent.to_string(), text.to_string())),
        }
    }

    fn stream(&mut self, kind: EventType, text: &str) {
        if text.is_empty() {
            return;
        }
        self.record_pending_calls_of(None);
        self.streamed_at = Some(Instant::now());
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
            side_id: None,
        }));
    }

    async fn on_incoming(&mut self, msg: Incoming) {
        match msg {
            Incoming::Notification(n) if n.method == "session/update" => {
                // Updates for another ACP session are a side conversation's
                // (or one the host doesn't know): never the transcript's.
                if let (Some(from), Some(main)) =
                    (n.params["sessionId"].as_str(), self.session.acp_session_id.as_deref())
                {
                    if from != main {
                        if let Some(side_id) = self.side_routes.get(from).cloned() {
                            self.on_side_update(&side_id, &n.params["update"]);
                        } else if let Some(a) = self.title_ask.as_mut().filter(|a| a.acp_id.as_deref() == Some(from)) {
                            let u = &n.params["update"];
                            if u["sessionUpdate"] == "agent_message_chunk" && parent_tool_use_id(u).is_none() {
                                a.text.push_str(&acp::content_text(&u["content"]));
                            }
                        }
                        return;
                    }
                }
                // A replayed history isn't news, but the session's title is.
                let u = &n.params["update"];
                if !self.loading || u["sessionUpdate"] == "session_info_update" {
                    self.on_update(u).await;
                }
            }
            Incoming::Notification(_) => {}
            Incoming::Request(r) => {
                let conn = self.agent.as_ref().unwrap().conn.clone();
                let result = match r.method.as_str() {
                    // Sandbox-local tools are approved automatically: the
                    // sandbox is the boundary.
                    // Writing a title needs no tools.
                    "session/request_permission" if self.is_title_conversation(&r.params) => {
                        Ok(json!({"outcome": {"outcome": "cancelled"}}))
                    }
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

    fn is_title_conversation(&self, params: &Value) -> bool {
        let from = params["sessionId"].as_str();
        from.is_some() && self.title_ask.as_ref().is_some_and(|a| a.acp_id.as_deref() == from)
    }

    async fn on_update(&mut self, u: &Value) {
        let kind = u["sessionUpdate"].as_str().unwrap_or_default();
        let parent = parent_tool_use_id(u);
        match kind {
            "agent_message_chunk" => match parent {
                Some(p) => self.stream_subagent(&p, &acp::content_text(&u["content"])),
                None => self.stream(EventType::AgentMessage, &acp::content_text(&u["content"])),
            },
            // A subagent's thinking is dropped: its steps and words are shown.
            "agent_thought_chunk" if parent.is_some() => {}
            "agent_thought_chunk" => {
                self.stream(EventType::AgentThought, &acp::content_text(&u["content"]))
            }
            "tool_call" | "tool_call_update" => self.on_tool(kind == "tool_call", u).await,
            "config_option_update" => {
                if let Some(opts) = parse_config_options(&u["configOptions"]) {
                    self.set_config_options(opts);
                }
            }
            // The agent names the session itself: its name wins over the host's.
            // Unless it only repeats a message, as an agent with no title of
            // its own may: then the title it writes when asked is better.
            "session_info_update" => {
                if let Some(t) = u["title"].as_str().and_then(|t| self.agent_title(t)) {
                    self.set_auto_title(&t, TitleSource::Agent);
                }
            }
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
        let mut payload = json!({
            "tool_call_id": id,
            "name": t.title,
            "kind": t.kind,
            "arguments": t.raw_input,
            "source": "sandbox",
            "locations": t.locations,
        });
        if let Some(n) = &t.tool_name {
            payload["tool_name"] = json!(n);
        }
        if is_subagent_tool(t.tool_name.as_deref()) {
            payload["subagent"] = subagent_info(&t.raw_input);
        }
        let parent = t.parent.clone().filter(|p| p != id);
        if let Some(p) = &parent {
            payload["parent_tool_call_id"] = json!(p);
            // The subagent's call comes first, then what it said before
            // this step.
            self.record_call(p);
            self.flush_subagent(p);
        }
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

    /// Record the pending calls of one agent (`None`: the main agent's)
    /// before its text, leaving parallel subagents' calls to their input.
    fn record_pending_calls_of(&mut self, parent: Option<&str>) {
        let ids: Vec<String> = self
            .tools
            .iter()
            .filter(|(_, t)| !t.recorded && t.parent.as_deref() == parent)
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
        let parent = parent_tool_use_id(u);
        let tool_name = u["_meta"]["claudeCode"]["toolName"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if is_new || !self.tools.contains_key(&id) {
            // A main-agent call ends the main agent's text; a subagent's
            // call ends only that subagent's (in `record_call`).
            if parent.is_none() {
                self.flush();
            }
            let title = u["title"].as_str().unwrap_or("tool").to_string();
            let raw_input = u.get("rawInput").cloned().unwrap_or(Value::Null);
            // Calls to our own MCP gateway are recorded by the gateway itself.
            let is_mcp = is_gateway_tool(&title, u);
            let signature = call_signature(parent.as_deref(), &title, &raw_input);
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
                    tool_name,
                    parent,
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
            if t.parent.is_none() {
                t.parent = parent;
            }
            if tool_name.is_some() {
                t.tool_name = tool_name;
            }
            if let Some(ri) = u.get("rawInput").filter(|v| has_input(v)) {
                t.raw_input = ri.clone();
                t.signature = call_signature(t.parent.as_deref(), &t.title, ri);
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
        let parent = info.parent.clone();
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
        // A finished subagent's last words come before its report.
        self.flush_subagent(&id);
        if let Some(p) = &parent {
            self.flush_subagent(p);
        }
        if !is_mcp {
            let mut payload = json!({"tool_call_id": id, "name": title, "status": status, "output": output, "truncated": truncated, "sandbox_denied": sandbox_denied, "exit_code": exit_code});
            if let Some(p) = &parent {
                payload["parent_tool_call_id"] = json!(p);
            }
            self.append(EventType::ToolResult, payload);
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
        self.title_ask = None;
        let Some(mut a) = self.agent.take() else {
            if let Some(dir) = self.shared_folder.take() {
                self.folders.release(&dir, self.started_at);
            }
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
        if let Some(dir) = self.shared_folder.take() {
            // The last session out removes the placeholders.
            self.folders.release(&dir, self.started_at);
        } else if !self.ctx.cfg.fake_agent_unsandboxed || self.spec.kind != AgentKind::Fake {
            // A folder of its own: a quick task's, or its worktree.
            sandbox::sweep_mount_points(
                std::path::Path::new(&self.workspace.path),
                self.started_at,
            );
        }
    }
}

/// What the repeat-failure guard counts: the same call by the same agent
/// (parallel subagents doing the same thing are counted apart).
fn call_signature(parent: Option<&str>, title: &str, raw_input: &Value) -> String {
    format!("{}\u{0}{title}\u{0}{raw_input}", parent.unwrap_or(""))
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
