//! MCP gateway.
//!
//! Agents get exactly one MCP server: this gateway, at
//! `http://mcp.agenthost.internal/s/<session token>/mcp`, a name only the
//! session's egress proxy routes (to the gateway's Unix socket). The gateway
//! holds every connector credential, lists the tools of the connectors
//! enabled for the session's workspace, applies tool policy (read: auto,
//! write: ask, by default), holds `ask` calls for approval from any paired
//! device, and forwards allowed calls to the remote server with the real
//! credential attached. Agents never see a credential.

use crate::api::Principal;
use crate::bus::BusMsg;
use crate::ctx::{Ctx, GatewayApi};
use crate::host::ConnectorAdmin;
use crate::proxy::GATEWAY_HOST;
use crate::secrets::SecretStore;
use crate::session::SessionManager;
use crate::store::{ConnectorRow, RawTool};
use crate::util::{new_id, now_ms, random_hex, truncate};
use agenthost_proto::jsonrpc::{codes, RpcError};
use agenthost_proto::methods as m;
use agenthost_proto::*;
use anyhow::{anyhow, bail, Context as _, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::net::UnixListener;
use tokio::sync::oneshot;

/// How long an `ask` call waits before it is denied.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);
/// Results larger than this are truncated before they reach the agent.
pub const RESULT_LIMIT: usize = 256 * 1024;
/// Calls per session per minute.
pub const RATE_LIMIT: usize = 120;
const MCP_PROTOCOL: &str = "2025-06-18";

/// The host's own tool: install a command-line tool on the user's computer
/// (outside the sandbox), always after the user approves.
pub const INSTALL_TOOL: &str = "agenthost__install_tool";
/// Push a branch with the user's git credentials, after they approve.
pub const GIT_PUSH_TOOL: &str = "agenthost__git_push";
/// Fetch with the user's git credentials (changes nothing on the remote).
pub const GIT_FETCH_TOOL: &str = "agenthost__git_fetch";
/// Open a pull request with the user's GitHub CLI, after they approve.
pub const PR_TOOL: &str = "agenthost__create_pull_request";
/// Run any command-line program as the user, outside the sandbox, after
/// they approve (or a prefix they let run without asking).
pub const RUN_COMMAND_TOOL: &str = "agenthost__run_command";

struct SessionReg {
    session_id: String,
    workspace_id: String,
    calls: VecDeque<Instant>,
}

struct Pending {
    info: PendingApproval,
    tx: oneshot::Sender<(ApprovalDecision, String)>,
}

/// One wait in `await_approval`. Dropping it ends the wait, whether the user
/// answered or not: the request leaves the pending list, the transcript says
/// how it ended, and the session's count of waits goes down. Its drop is the
/// only way out, so a wait the agent abandons (it gave up on the call, or the
/// turn was cancelled, and the request future was dropped) cannot leave the
/// session showing "needs approval" with nothing left to approve.
struct ApprovalWait<'a> {
    gw: &'a Gateway,
    session_id: &'a str,
    approval_id: String,
    /// The decision and who made it; `None` while waiting.
    outcome: Option<(ApprovalDecision, String)>,
}

impl Drop for ApprovalWait<'_> {
    fn drop(&mut self) {
        self.gw.approvals.lock().unwrap().remove(&self.approval_id);
        let payload = match self.outcome.take() {
            Some((decision, device)) => json!({
                "approval_id": self.approval_id,
                "decision": decision,
                "timed_out": device == "timeout",
                "device": device,
            }),
            // The agent stopped waiting: nothing ran, and nobody denied it.
            None => json!({
                "approval_id": self.approval_id,
                "decision": ApprovalDecision::Deny,
                "device": "",
                "withdrawn": true,
            }),
        };
        let _ = self
            .gw
            .ctx
            .append(self.session_id, EventType::ApprovalResolved, payload);
        self.gw.set_waiting(self.session_id, -1);
    }
}

/// One remote MCP session (the server's `Mcp-Session-Id`).
#[derive(Default, Clone)]
struct RemoteSession {
    session_id: Option<String>,
    initialized: bool,
}

pub struct Gateway {
    ctx: Arc<Ctx>,
    secrets: Arc<dyn SecretStore>,
    sessions: Weak<SessionManager>,
    socket: PathBuf,
    regs: Mutex<HashMap<String, SessionReg>>,
    approvals: Mutex<HashMap<String, Pending>>,
    waiting: Mutex<HashMap<String, usize>>,
    remotes: tokio::sync::Mutex<HashMap<String, RemoteSession>>,
    http: reqwest::Client,
    /// OAuth flows waiting for their browser redirect.
    oauth: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    /// One install at a time: package managers don't like running concurrently.
    install_lock: tokio::sync::Mutex<()>,
    /// Tool prefixes connectors had before a rename (old slug → connector
    /// id): live sessions listed the tools under the old name and keep
    /// calling them so.
    renamed: Mutex<HashMap<String, String>>,
}

fn secret_name(connector_id: &str, kind: &str) -> String {
    format!("mcp-{kind}-{connector_id}")
}

/// Tool name as agents see it: `<connector slug>__<tool>`.
pub fn exposed_name(slug: &str, tool: &str) -> String {
    format!("{slug}__{tool}")
}

pub fn slug(name: &str) -> String {
    agenthost_proto::connector_slug(name)
}

/// The host's own tools a user can let run without asking, with the words
/// Settings shows. Installing always asks: the package's scripts run as the
/// user.
const BUILTINS: &[(&str, &str, &str, bool)] = &[
    ("create_pull_request", "Open pull requests", "Opens a GitHub pull request with your GitHub CLI login.", true),
    ("git_push", "Push branches", "Pushes with your git credentials. A force-push always asks.", true),
    ("install_tool", "Install tools on this computer", "Runs a package manager as you, outside the sandbox. Always asks.", false),
    ("run_command", "Run commands as you", "Runs a program as you, outside the sandbox, with your logins. Asks, except for the commands you let run.", false),
];

fn policy_key(name: &str) -> String {
    format!("builtin_policy.{name}")
}

/// Command prefixes `run_command` runs without asking.
const ALLOWED_COMMANDS_KEY: &str = "builtin_policy.run_command.allowed";

impl Gateway {
    /// Whether one of the host's own tools asks first (the default) or not.
    pub fn builtin_policy(&self, name: &str) -> ToolPolicy {
        let can_auto = BUILTINS.iter().any(|(n, _, _, a)| *n == name && *a);
        match self.ctx.store.setting_get::<ToolPolicy>(&policy_key(name)) {
            Ok(Some(ToolPolicy::Auto)) if can_auto => ToolPolicy::Auto,
            _ => ToolPolicy::Ask,
        }
    }

    pub fn builtin_list(&self) -> Vec<BuiltinTool> {
        BUILTINS
            .iter()
            .map(|(name, title, description, can_auto)| BuiltinTool {
                name: name.to_string(),
                title: title.to_string(),
                description: description.to_string(),
                policy: self.builtin_policy(name),
                can_auto: *can_auto,
                allowed: if *name == "run_command" { self.allowed_commands() } else { vec![] },
            })
            .collect()
    }

    /// The command prefixes the user let run without asking.
    pub fn allowed_commands(&self) -> Vec<Vec<String>> {
        let list: Vec<Vec<String>> = self.ctx.store.setting_get(ALLOWED_COMMANDS_KEY).ok().flatten().unwrap_or_default();
        // A prefix saved before the rules tightened no longer counts.
        list.into_iter().filter(|r| crate::hostcmd::rememberable(r)).collect()
    }

    /// Let a command prefix run without asking (`auto`), or ask again (`ask`).
    fn set_command_policy(&self, rule: &[String], policy: ToolPolicy) -> Result<(), String> {
        let mut list = self.allowed_commands();
        list.retain(|r| r != rule);
        match policy {
            ToolPolicy::Auto => {
                if !crate::hostcmd::rememberable(rule) {
                    return Err(format!(
                        "{} always asks: give a program and its subcommand (gh pr edit); shells, interpreters and build tools always ask",
                        crate::hostcmd::display(rule)
                    ));
                }
                list.push(rule.to_vec());
            }
            ToolPolicy::Ask => {}
            ToolPolicy::Disabled => return Err("commands can ask or run without asking".into()),
        }
        self.ctx.store.setting_set(ALLOWED_COMMANDS_KEY, &list).map_err(|e| e.to_string())?;
        self.ctx.audit.log("builtin.command_policy", json!({"command": rule, "policy": policy}));
        Ok(())
    }

    pub fn set_builtin_policy(&self, p: &BuiltinPolicyParams) -> Result<Vec<BuiltinTool>, String> {
        let Some((_, _, _, can_auto)) = BUILTINS.iter().find(|(n, ..)| *n == p.name) else {
            return Err(format!("no built-in tool {:?}", p.name));
        };
        if p.name == "run_command" {
            let Some(rule) = &p.command else {
                return Err("run_command asks unless you let a command run: name the command".into());
            };
            self.set_command_policy(rule, p.policy)?;
            return Ok(self.builtin_list());
        }
        match p.policy {
            ToolPolicy::Auto if !can_auto => return Err(format!("{} always asks", p.name)),
            ToolPolicy::Disabled => return Err("built-in tools can ask or run without asking".into()),
            _ => {}
        }
        self.ctx.store.setting_set(&policy_key(&p.name), &p.policy).map_err(|e| e.to_string())?;
        self.ctx.audit.log("builtin.policy", json!({"name": p.name, "policy": p.policy}));
        Ok(self.builtin_list())
    }
}

/// The definition of `agenthost__create_pull_request`.
fn pr_tool_def() -> Value {
    json!({
        "name": PR_TOOL,
        "description": "[AgentOS] Open a GitHub pull request with the user's own GitHub CLI login, which your sandbox can't read \
             (so `gh pr create` inside it fails). The user sees the title and description and approves or denies it. \
             Push the branch first (agenthost__git_push).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "title": {"type": "string", "description": "One-line title."},
                "body": {"type": "string", "description": "The description, in Markdown."},
                "base": {"type": "string", "description": "The branch to merge into (optional; the repository's default branch)."},
                "head": {"type": "string", "description": "The branch with the changes (optional; the branch checked out in this session's folder)."},
                "draft": {"type": "boolean", "description": "Open it as a draft."},
                "reason": {"type": "string", "description": "One sentence for the user: why open it now."}
            },
            "required": ["title", "body"]
        },
        "annotations": {"readOnlyHint": false, "destructiveHint": false, "openWorldHint": true, "title": "Open a pull request as the user"}
    })
}

/// The definition of `agenthost__run_command`.
fn run_command_def() -> Value {
    json!({
        "name": RUN_COMMAND_TOOL,
        "description": "[AgentOS] Run a command-line program on the user's computer as the user, outside your sandbox, in this session's folder, \
             with the user's own logins (GitHub CLI, cloud and deploy CLIs, keyring), which your sandbox can't read. Use it when a command \
             fails in the sandbox for want of the user's credentials or network, e.g. `gh pr edit`, `gh pr comment`, `gh issue create`, `gh api`. \
             The user sees the exact command and approves or denies it; nothing runs until they approve (unless they let that command run \
             without asking). No shell: give the program and its arguments as a list; pipes, redirects and `$(…)` aren't read. Pass long text \
             (a description) on stdin, e.g. [\"gh\", \"pr\", \"edit\", \"6\", \"--body-file\", \"-\"]. Nothing prompts; 10-minute limit. \
             For pushing, fetching and opening pull requests prefer agenthost__git_push, agenthost__git_fetch and agenthost__create_pull_request.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "command": {"type": "array", "items": {"type": "string"}, "minItems": 1, "description": "The program (a name on the user's PATH, no path) and its arguments, e.g. [\"gh\", \"pr\", \"edit\", \"6\", \"--title\", \"New title\"]."},
                "stdin": {"type": "string", "description": "Text for the program's standard input (optional)."},
                "reason": {"type": "string", "description": "One sentence for the user: what this does and why."}
            },
            "required": ["command", "reason"]
        },
        "annotations": {"readOnlyHint": false, "destructiveHint": true, "openWorldHint": true, "title": "Run a command as the user"}
    })
}

/// The definitions of `agenthost__git_push` and `agenthost__git_fetch`.
fn git_tools() -> Vec<Value> {
    vec![
        json!({
            "name": GIT_PUSH_TOOL,
            "description": "[AgentOS] Push a branch of this session's repository with the user's own git credentials, which your sandbox can't read \
                 (so `git push` inside it fails). The user sees the exact command and approves or denies it; nothing runs until they approve. \
                 Commit first. Only remotes the repository already has; branch names only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "branch": {"type": "string", "description": "The local branch to push (optional; the branch checked out in this session's folder)."},
                    "remote": {"type": "string", "description": "A remote the repository has (optional; the branch's upstream, else origin)."},
                    "remote_branch": {"type": "string", "description": "The branch name on the remote (optional; the same name)."},
                    "set_upstream": {"type": "boolean", "description": "Track the pushed branch (git push --set-upstream)."},
                    "force_with_lease": {"type": "boolean", "description": "Replace the remote branch if nobody else pushed to it (git push --force-with-lease). Only when the user asked for it."},
                    "reason": {"type": "string", "description": "One sentence for the user: what this push is."}
                },
                "required": ["reason"]
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": true, "openWorldHint": true, "title": "Push with the user's credentials"}
        }),
        json!({
            "name": GIT_FETCH_TOOL,
            "description": "[AgentOS] Fetch from a remote of this session's repository with the user's own git credentials (git fetch <remote>), \
                 which your sandbox can't read. Runs without asking; it changes nothing on the remote. Merge or rebase yourself afterwards.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "remote": {"type": "string", "description": "A remote the repository has (optional; origin)."}
                }
            },
            "annotations": {"readOnlyHint": true, "openWorldHint": true, "title": "Fetch with the user's credentials"}
        }),
    ]
}

fn text_result(text: &str, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

/// Classify a tool: read only when the server marks it read-only.
pub fn classify(tool: &Value) -> ToolKind {
    if tool["annotations"]["readOnlyHint"].as_bool() == Some(true) {
        ToolKind::Read
    } else {
        ToolKind::Write
    }
}

impl Gateway {
    pub async fn start(
        ctx: Arc<Ctx>,
        secrets: Arc<dyn SecretStore>,
        sessions: &Arc<SessionManager>,
    ) -> Result<Arc<Gateway>> {
        let socket = ctx.cfg.runtime_dir.join("gw.sock");
        if socket.exists() {
            std::fs::remove_file(&socket)?;
        }
        let listener = UnixListener::bind(&socket)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        let gw = Arc::new(Gateway {
            ctx,
            secrets,
            sessions: Arc::downgrade(sessions),
            socket,
            regs: Mutex::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
            waiting: Mutex::new(HashMap::new()),
            remotes: tokio::sync::Mutex::new(HashMap::new()),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .user_agent(concat!("agenthost/", env!("CARGO_PKG_VERSION")))
                .build()?,
            oauth: Mutex::new(HashMap::new()),
            install_lock: tokio::sync::Mutex::new(()),
            renamed: Mutex::new(HashMap::new()),
        });
        // Icons for connectors added before the host fetched them.
        let g = gw.clone();
        tokio::spawn(async move {
            for c in g.ctx.store.connector_list().unwrap_or_default() {
                if c.icon.is_none() {
                    g.fetch_connector_icon(&c.connector_id, &c.url).await;
                }
            }
        });
        let g = gw.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let g = g.clone();
                tokio::spawn(async move {
                    let io = hyper_util::rt::TokioIo::new(stream);
                    let svc = hyper::service::service_fn(move |req| {
                        let g = g.clone();
                        async move { Ok::<_, std::convert::Infallible>(g.serve(req).await) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await;
                });
            }
        });
        Ok(gw)
    }

    // -----------------------------------------------------------------------
    // Agent-facing MCP server
    // -----------------------------------------------------------------------

    async fn serve(self: &Arc<Self>, req: Request<hyper::body::Incoming>) -> Response<Full<Bytes>> {
        let resp = |status: StatusCode, body: Value| {
            Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(body.to_string())))
                .unwrap()
        };
        let path = req.uri().path().to_string();
        let token = path
            .strip_prefix("/s/")
            .and_then(|r| r.strip_suffix("/mcp"))
            .unwrap_or("")
            .to_string();
        let reg = self
            .regs
            .lock()
            .unwrap()
            .get(&token)
            .map(|r| (r.session_id.clone(), r.workspace_id.clone()));
        let Some((session_id, workspace_id)) = reg else {
            return resp(StatusCode::NOT_FOUND, json!({"error": "unknown session"}));
        };
        match *req.method() {
            hyper::Method::POST => {}
            hyper::Method::DELETE => return resp(StatusCode::OK, json!({})),
            _ => {
                return Response::builder()
                    .status(StatusCode::METHOD_NOT_ALLOWED)
                    .header("allow", "POST, DELETE")
                    .body(Full::new(Bytes::new()))
                    .unwrap();
            }
        }
        let body = match req.into_body().collect().await {
            Ok(b) => b.to_bytes(),
            Err(_) => return resp(StatusCode::BAD_REQUEST, json!({"error": "bad body"})),
        };
        if body.len() > 4 * 1024 * 1024 {
            return resp(
                StatusCode::PAYLOAD_TOO_LARGE,
                json!({"error": "request too large"}),
            );
        }
        let msg: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                return resp(
                    StatusCode::BAD_REQUEST,
                    json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":e.to_string()}}),
                )
            }
        };
        let batch = msg.is_array();
        let msgs: Vec<Value> = if batch {
            msg.as_array().cloned().unwrap_or_default()
        } else {
            vec![msg]
        };
        let mut out = vec![];
        for m in msgs {
            if let Some(r) = self.handle_mcp(&session_id, &workspace_id, m).await {
                out.push(r);
            }
        }
        if out.is_empty() {
            return Response::builder()
                .status(StatusCode::ACCEPTED)
                .body(Full::new(Bytes::new()))
                .unwrap();
        }
        let body = if batch {
            Value::Array(out)
        } else {
            out.pop().unwrap()
        };
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .header(
                "mcp-session-id",
                format!("ah-{}", &token[..8.min(token.len())]),
            )
            .body(Full::new(Bytes::from(body.to_string())))
            .unwrap()
    }

    async fn handle_mcp(
        self: &Arc<Self>,
        session_id: &str,
        workspace_id: &str,
        msg: Value,
    ) -> Option<Value> {
        let id = msg.get("id").cloned().filter(|v| !v.is_null());
        let method = msg["method"].as_str().unwrap_or_default().to_string();
        let id = id?; // notifications get no response
        let result: Result<Value, (i64, String)> = match method.as_str() {
            "initialize" => Ok(json!({
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or(MCP_PROTOCOL),
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "agenthost", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Tools from the connectors enabled for this workspace (named <connector>__<tool>), plus agenthost__install_tool, which installs a command-line tool on the user's computer after they approve it."
            })),
            "ping" => Ok(json!({})),
            "tools/list" => {
                let mut tools: Vec<Value> = self.builtin_tools();
                tools.extend(self.list_tools(workspace_id).into_iter().map(|(_, _, def)| def));
                Ok(json!({"tools": tools}))
            }
            "tools/call" => {
                let name = msg["params"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let args = msg["params"].get("arguments").cloned().unwrap_or(json!({}));
                Ok(self.call_tool(session_id, workspace_id, &name, args).await)
            }
            "resources/list" => Ok(json!({"resources": []})),
            "prompts/list" => Ok(json!({"prompts": []})),
            other => Err((-32601, format!("method not found: {other}"))),
        };
        Some(match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err((code, message)) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
            }
        })
    }

    /// Enabled connectors for a workspace, with slugs.
    fn enabled_connectors(&self, workspace_id: &str) -> Vec<(String, ConnectorRow)> {
        let Ok(Some(ws)) = self.ctx.store.workspace_get(workspace_id) else {
            return vec![];
        };
        let mut out: Vec<(String, ConnectorRow)> = vec![];
        for cid in ws.connectors {
            if let Ok(Some(c)) = self.ctx.store.connector_get(&cid) {
                if !c.enabled {
                    continue;
                }
                let mut s = slug(&c.name);
                // "agenthost" is the host's own tools.
                if s == BUILTIN_CONNECTOR_ID || out.iter().any(|(o, _)| *o == s) {
                    s = format!(
                        "{s}_{}",
                        &c.connector_id[c.connector_id.len().saturating_sub(4)..]
                    );
                }
                out.push((s, c));
            }
        }
        out
    }

    /// (connector, tool, definition as exposed) for every listed tool.
    fn list_tools(&self, workspace_id: &str) -> Vec<(ConnectorRow, RawTool, Value)> {
        let mut out = vec![];
        for (s, c) in self.enabled_connectors(workspace_id) {
            let overrides = self
                .ctx
                .store
                .tool_policies(&c.connector_id)
                .unwrap_or_default();
            for t in &c.tools {
                let (policy, _) = c.effective_policy(t, &overrides);
                if policy == ToolPolicy::Disabled {
                    continue;
                }
                let mut def = if t.definition.is_object() {
                    t.definition.clone()
                } else {
                    json!({"inputSchema": {"type": "object"}})
                };
                def["name"] = json!(exposed_name(&s, &t.name));
                let desc = t.description.clone().unwrap_or_default();
                def["description"] = json!(format!("[{}] {}", c.name, desc).trim().to_string());
                out.push((c.clone(), t.clone(), def));
            }
        }
        out
    }

    fn rate_limited(&self, session_id: &str) -> bool {
        let mut regs = self.regs.lock().unwrap();
        let Some(r) = regs.values_mut().find(|r| r.session_id == session_id) else {
            return true;
        };
        let now = Instant::now();
        while r
            .calls
            .front()
            .map(|t| now.duration_since(*t) > Duration::from_secs(60))
            .unwrap_or(false)
        {
            r.calls.pop_front();
        }
        if r.calls.len() >= RATE_LIMIT {
            return true;
        }
        r.calls.push_back(now);
        false
    }

    async fn call_tool(
        self: &Arc<Self>,
        session_id: &str,
        workspace_id: &str,
        name: &str,
        args: Value,
    ) -> Value {
        if name == INSTALL_TOOL {
            return self.install_tool(session_id, args).await;
        }
        if name == PR_TOOL {
            return self.pr_tool(session_id, args).await;
        }
        if name == RUN_COMMAND_TOOL {
            return self.run_command(session_id, args).await;
        }
        if name == GIT_PUSH_TOOL || name == GIT_FETCH_TOOL {
            return self.git_tool(session_id, name == GIT_PUSH_TOOL, args).await;
        }
        let call_id = new_id("mcp");
        let tools = self.list_tools(workspace_id);
        let mut found = tools.iter().find(|(_, _, d)| d["name"] == name).cloned();
        if found.is_none() {
            // Listed before its connector was renamed.
            if let Some((prefix, tool)) = name.split_once("__") {
                let id = self.renamed.lock().unwrap().get(prefix).cloned();
                if let Some(id) = id {
                    found = tools
                        .into_iter()
                        .find(|(c, t, _)| c.connector_id == id && t.name == tool);
                }
            }
        }
        let Some((conn, tool, _)) = found else {
            let _ = self.ctx.append(
                session_id,
                EventType::ToolCall,
                json!({"tool_call_id": call_id, "name": name, "arguments": args, "source": "mcp"}),
            );
            let msg = format!("unknown or disabled tool {name}");
            let _ = self.ctx.append(session_id, EventType::ToolResult, json!({"tool_call_id": call_id, "name": name, "status": "failed", "output": msg, "source": "mcp"}));
            return text_result(&msg, true);
        };
        let overrides = self
            .ctx
            .store
            .tool_policies(&conn.connector_id)
            .unwrap_or_default();
        let (policy, _) = conn.effective_policy(&tool, &overrides);
        let _ = self.ctx.append(
            session_id,
            EventType::ToolCall,
            json!({
                "tool_call_id": call_id, "name": tool.name, "exposed_name": name, "arguments": args, "source": "mcp",
                "connector_id": conn.connector_id, "connector_name": conn.name, "tool_kind": tool.kind, "policy": policy
            }),
        );
        let finish = |status: &str, output: &str| {
            let (out, truncated) = truncate(output, crate::session::TOOL_OUTPUT_LIMIT);
            let _ = self.ctx.append(
                session_id,
                EventType::ToolResult,
                json!({"tool_call_id": call_id, "name": tool.name, "status": status, "output": out, "truncated": truncated, "source": "mcp", "connector_id": conn.connector_id}),
            );
        };
        if self.rate_limited(session_id) {
            finish("failed", "rate limit reached");
            return text_result(
                "AgentOS: too many MCP calls this minute; try again shortly",
                true,
            );
        }
        if policy == ToolPolicy::Ask {
            let decision = self
                .await_approval(session_id, &conn.connector_id, &conn.name, &tool.name, &args, None)
                .await;
            if decision != ApprovalDecision::Approve {
                finish("denied", "the user denied this call");
                return text_result("The user denied this tool call.", true);
            }
        }
        match self
            .remote_call(
                &conn,
                "tools/call",
                json!({"name": tool.name, "arguments": args}),
            )
            .await
        {
            Ok(mut result) => {
                let is_error = result["isError"].as_bool().unwrap_or(false);
                let text = crate::acp::content_text(&result["content"]);
                let size = result.to_string().len();
                if size > RESULT_LIMIT {
                    let (t, _) = truncate(&text, RESULT_LIMIT - 1024);
                    result = text_result(
                        &format!("{t}\n[AgentOS: result truncated from {size} bytes]"),
                        is_error,
                    );
                }
                finish(
                    if is_error { "failed" } else { "completed" },
                    if text.is_empty() {
                        "(no text content)"
                    } else {
                        &text
                    },
                );
                result
            }
            Err(e) => {
                finish("failed", &e.to_string());
                text_result(
                    &format!("AgentOS: the connector {} failed: {e}", conn.name),
                    true,
                )
            }
        }
    }

    async fn await_approval(
        self: &Arc<Self>,
        session_id: &str,
        connector_id: &str,
        connector_name: &str,
        tool: &str,
        args: &Value,
        summary: Option<String>,
    ) -> ApprovalDecision {
        let approval_id = new_id("apr");
        let now = now_ms();
        let info = PendingApproval {
            approval_id: approval_id.clone(),
            session_id: session_id.to_string(),
            connector_id: connector_id.to_string(),
            connector_name: connector_name.to_string(),
            tool: tool.to_string(),
            arguments: args.clone(),
            requested_at: now,
            expires_at: now + APPROVAL_TIMEOUT.as_millis() as i64,
            summary,
        };
        // Nobody may be watching an automation's run: give them longer.
        let timeout = if self.run_automation(session_id).is_some() {
            crate::automations::RUN_APPROVAL_TIMEOUT
        } else {
            APPROVAL_TIMEOUT
        };
        let info = PendingApproval { expires_at: now + timeout.as_millis() as i64, ..info };
        let (tx, rx) = oneshot::channel();
        self.approvals.lock().unwrap().insert(
            approval_id.clone(),
            Pending {
                info: info.clone(),
                tx,
            },
        );
        let _ = self.ctx.append(
            session_id,
            EventType::ApprovalRequested,
            serde_json::to_value(&info).unwrap(),
        );
        self.set_waiting(session_id, 1);
        let mut wait = ApprovalWait {
            gw: self,
            session_id,
            approval_id,
            outcome: None,
        };
        let (decision, device) = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(d)) => d,
            _ => (ApprovalDecision::Deny, "timeout".to_string()),
        };
        wait.outcome = Some((decision, device));
        decision
    }

    /// The automation a session is a run of.
    fn run_automation(&self, session_id: &str) -> Option<Automation> {
        let aid = self.ctx.store.session_get(session_id).ok()??.automation_id?;
        Some(self.ctx.store.automation_get(&aid).ok()??.automation)
    }

    /// Whether the automation this session is a run of lets `tool` run
    /// without asking; returns the automation's id.
    fn automation_allows(&self, session_id: &str, tool: &str) -> Option<String> {
        let a = self.run_automation(session_id)?;
        a.auto_approve.iter().any(|t| t == tool).then_some(a.automation_id)
    }

    /// The host's own tools, as listed to agents.
    fn builtin_tools(&self) -> Vec<Value> {
        let mut tools = git_tools();
        tools.push(run_command_def());
        if crate::installer::find("gh", self.ctx.cfg.install_bin_dir.as_deref()).is_some() {
            tools.push(pr_tool_def());
        }
        let found = crate::installer::available(self.ctx.cfg.install_bin_dir.as_deref());
        if found.is_empty() {
            return tools;
        }
        let names: Vec<&str> = found.iter().map(|m| m.name()).collect();
        tools.push(json!({
            "name": INSTALL_TOOL,
            "description": format!(
                "[AgentOS] Install a command-line tool on the user's computer, outside your sandbox, so the user (and later sessions) can run it. \
                 The user sees the exact command and approves or denies it on their desktop or phone; nothing runs until they approve. \
                 Use it when the user asks for a tool to be installed for them. For a helper only you need while working, install it yourself \
                 instead (npm install -g, pipx install, uv tool install, cargo install, go install): inside the sandbox those go to a shared \
                 tools folder that later sessions can use, without asking. Package managers on this computer: {}.",
                names.join(", ")
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "manager": {"type": "string", "enum": names, "description": "The package manager to install with."},
                    "package": {"type": "string", "description": "The package's registry name only, e.g. agent-desktop, @scope/name, ripgrep, github.com/owner/repo/cmd/tool. No URLs, paths or flags."},
                    "version": {"type": "string", "description": "An exact version, e.g. 1.3.1 (optional; latest when left out)."},
                    "reason": {"type": "string", "description": "One sentence for the user: why install it."}
                },
                "required": ["manager", "package", "reason"]
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": true, "openWorldHint": true, "title": "Install a tool on this computer"}
        }));
        tools
    }

    /// The folder a session's git commands run in: its worktree, or its
    /// workspace.
    fn session_git_dir(&self, session_id: &str) -> Option<std::path::PathBuf> {
        let s = self.ctx.store.session_get(session_id).ok()??;
        if let Some(w) = &s.worktree {
            return Some(std::path::PathBuf::from(&w.path));
        }
        let ws = self.ctx.store.workspace_get(&s.workspace_id).ok()??;
        Some(std::path::PathBuf::from(ws.path))
    }

    /// Remember the pull request a command's output links to as the
    /// session's, and tell the clients (the session list shows its number).
    fn record_pull_request(&self, session_id: &str, output: &str) {
        let Some(pr) = agenthost_proto::SessionPullRequest::find_in(output) else { return };
        match self.ctx.store.session_get(session_id) {
            Ok(Some(s)) if s.pull_request.as_ref() != Some(&pr) => {}
            _ => return,
        }
        if let Ok(true) = self.ctx.store.session_set_pull_request(session_id, &pr) {
            if let Ok(Some(s)) = self.ctx.store.session_get(session_id) {
                self.ctx.bus.publish(BusMsg::SessionChanged(s));
            }
        }
    }

    /// `agenthost__create_pull_request`: the title and description shown,
    /// approved, then `gh pr create` as the user.
    async fn pr_tool(self: &Arc<Self>, session_id: &str, args: Value) -> Value {
        let call_id = new_id("mcp");
        let tool = "create_pull_request";
        let s = |k: &str| args[k].as_str().map(str::to_string);
        let base = json!({
            "tool_call_id": call_id, "name": tool, "exposed_name": PR_TOOL, "arguments": args,
            "source": "mcp", "connector_id": BUILTIN_CONNECTOR_ID, "connector_name": "AgentOS",
            "tool_kind": ToolKind::Write, "policy": ToolPolicy::Ask
        });
        let _ = self.ctx.append(session_id, EventType::ToolCall, base);
        let finish = |status: &str, output: &str| {
            let (out, truncated) = truncate(output, crate::session::TOOL_OUTPUT_LIMIT);
            let _ = self.ctx.append(
                session_id,
                EventType::ToolResult,
                json!({"tool_call_id": call_id, "name": tool, "status": status, "output": out, "truncated": truncated, "source": "mcp", "connector_id": BUILTIN_CONNECTOR_ID}),
            );
        };
        if self.rate_limited(session_id) {
            finish("failed", "rate limit reached");
            return text_result("AgentOS: too many calls this minute; try again shortly", true);
        }
        let Some(program) = crate::installer::find("gh", self.ctx.cfg.install_bin_dir.as_deref()) else {
            finish("failed", "the GitHub CLI (gh) is not installed on this computer");
            return text_result("AgentOS: the GitHub CLI (gh) is not installed on the user's computer", true);
        };
        let Some(dir) = self.session_git_dir(session_id) else {
            finish("failed", "no such session");
            return text_result("AgentOS: this session's folder is unknown", true);
        };
        let plan = crate::gitremote::plan_pr(
            &dir,
            &program,
            &s("title").unwrap_or_default(),
            &s("body").unwrap_or_default(),
            s("base").as_deref(),
            s("head").as_deref(),
            args["draft"].as_bool().unwrap_or(false),
        )
        .await;
        let plan = match plan {
            Ok(p) => p,
            Err(e) => {
                finish("failed", &e);
                return text_result(&format!("AgentOS: {e}"), true);
            }
        };
        let (details, _) = truncate(&plan.body, 4000);
        let shown = json!({
            "command": plan.command(),
            "title": plan.title,
            "base": plan.base,
            "head": plan.head,
            "draft": plan.draft,
            "details": details,
            "reason": s("reason").unwrap_or_default(),
            "runs": "on this computer, as you, outside the sandbox",
            "approve_label": "Open Pull Request",
            "ask_title": "Open a Pull Request?",
            "note": "Posts to GitHub as you, with your GitHub CLI login. Push the branch first.",
        });
        // Settings can let pull requests open without asking, and so can the
        // automation this session is a run of.
        let by_automation = self.automation_allows(session_id, tool);
        let auto = self.builtin_policy(tool) == ToolPolicy::Auto || by_automation.is_some();
        let decision = if auto {
            ApprovalDecision::Approve
        } else {
            self.await_approval(session_id, BUILTIN_CONNECTOR_ID, "AgentOS", tool, &shown, Some(plan.summary()))
                .await
        };
        self.ctx.audit.log(
            "pr.decision",
            json!({"session_id": session_id, "command": plan.command(), "dir": plan.dir, "decision": decision, "auto": auto, "automation_id": by_automation}),
        );
        if decision != ApprovalDecision::Approve {
            finish("denied", "the user denied this pull request");
            return text_result("The user denied this pull request.", true);
        }
        let out = crate::gitremote::run_pr(&plan).await;
        self.ctx.audit.log(
            "pr.done",
            json!({"session_id": session_id, "command": plan.command(), "dir": plan.dir, "ok": out.ok}),
        );
        let output = if out.output.is_empty() { "(no output)".to_string() } else { out.output.clone() };
        finish(if out.ok { "completed" } else { "failed" }, &format!("$ {}\n{output}", plan.command()));
        // Also when it failed because the branch already has one: it names it.
        self.record_pull_request(session_id, &out.output);
        if out.ok {
            text_result(&format!("Opened the pull request with the user's GitHub CLI:\n{output}"), false)
        } else {
            text_result(&format!("`gh pr create` failed:\n{output}"), true)
        }
    }

    /// `agenthost__git_push` (asks first) and `agenthost__git_fetch` (doesn't).
    async fn git_tool(self: &Arc<Self>, session_id: &str, push: bool, args: Value) -> Value {
        let call_id = new_id("mcp");
        let tool = if push { "git_push" } else { "git_fetch" };
        let exposed = if push { GIT_PUSH_TOOL } else { GIT_FETCH_TOOL };
        let s = |k: &str| args[k].as_str().map(str::to_string);
        let base = json!({
            "tool_call_id": call_id, "name": tool, "exposed_name": exposed, "arguments": args,
            "source": "mcp", "connector_id": BUILTIN_CONNECTOR_ID, "connector_name": "AgentOS",
            "tool_kind": if push { ToolKind::Write } else { ToolKind::Read },
            "policy": if push { ToolPolicy::Ask } else { ToolPolicy::Auto }
        });
        let _ = self.ctx.append(session_id, EventType::ToolCall, base);
        let finish = |status: &str, output: &str| {
            let (out, truncated) = truncate(output, crate::session::TOOL_OUTPUT_LIMIT);
            let _ = self.ctx.append(
                session_id,
                EventType::ToolResult,
                json!({"tool_call_id": call_id, "name": tool, "status": status, "output": out, "truncated": truncated, "source": "mcp", "connector_id": BUILTIN_CONNECTOR_ID}),
            );
        };
        if self.rate_limited(session_id) {
            finish("failed", "rate limit reached");
            return text_result("AgentOS: too many calls this minute; try again shortly", true);
        }
        let Some(dir) = self.session_git_dir(session_id) else {
            finish("failed", "no such session");
            return text_result("AgentOS: this session's folder is unknown", true);
        };
        let plan = if push {
            crate::gitremote::plan_push(
                &dir,
                s("remote").as_deref(),
                s("branch").as_deref(),
                s("remote_branch").as_deref(),
                args["force_with_lease"].as_bool().unwrap_or(false),
                args["set_upstream"].as_bool().unwrap_or(false),
            )
            .await
        } else {
            crate::gitremote::plan_fetch(&dir, s("remote").as_deref()).await
        };
        let plan = match plan {
            Ok(p) => p,
            Err(e) => {
                finish("failed", &e);
                return text_result(&format!("AgentOS: {e}"), true);
            }
        };
        if push {
            let reason = s("reason").unwrap_or_default();
            let mut note = "Uses your git credentials. Nothing from the repository runs (its hooks are skipped).".to_string();
            if plan.force {
                note = format!("Replaces the remote branch if nobody else pushed to it since your last fetch. {note}");
            }
            let shown = json!({
                "command": plan.command(),
                "remote": plan.remote,
                "branch": plan.branch,
                "force": plan.force,
                "reason": reason,
                "runs": "on this computer, as you, outside the sandbox",
                "approve_label": if plan.force { "Force-Push" } else { "Push" },
                "ask_title": "Push?",
                "note": note,
            });
            // Settings can let pushes run without asking; so can an
            // automation, for its run's own branch. A force-push always asks.
            let own_branch = self
                .ctx
                .store
                .session_get(session_id)
                .ok()
                .flatten()
                .and_then(|s| s.worktree)
                .is_some_and(|w| plan.branch.as_deref() == Some(w.branch.as_str()));
            let by_automation = self.automation_allows(session_id, tool).filter(|_| own_branch);
            let auto = !plan.force && (self.builtin_policy(tool) == ToolPolicy::Auto || by_automation.is_some());
            let decision = if auto {
                ApprovalDecision::Approve
            } else {
                self.await_approval(session_id, BUILTIN_CONNECTOR_ID, "AgentOS", tool, &shown, Some(plan.summary()))
                    .await
            };
            self.ctx.audit.log(
                "git.decision",
                json!({"session_id": session_id, "command": plan.command(), "dir": plan.dir, "decision": decision, "auto": auto, "automation_id": by_automation}),
            );
            if decision != ApprovalDecision::Approve {
                finish("denied", "the user denied this push");
                return text_result("The user denied this push.", true);
            }
        }
        let out = crate::gitremote::run(&plan).await;
        self.ctx.audit.log(
            "git.done",
            json!({"session_id": session_id, "command": plan.command(), "dir": plan.dir, "ok": out.ok}),
        );
        let output = if out.output.is_empty() { "(no output)".to_string() } else { out.output.clone() };
        finish(if out.ok { "completed" } else { "failed" }, &format!("$ {}\n{output}", plan.command()));
        if out.ok {
            text_result(&format!("Ran `{}` with the user's credentials:\n{output}", plan.command()), false)
        } else {
            text_result(&format!("`{}` failed:\n{output}", plan.command()), true)
        }
    }

    /// Folders a session can write: programs there never run outside the sandbox.
    fn session_writable_dirs(&self) -> Vec<std::path::PathBuf> {
        let c = &self.ctx.cfg;
        vec![c.session_tools_dir(), c.worktrees_dir(), c.scratch_dir()]
    }

    /// `agenthost__run_command`: show the exact command, ask (unless the user
    /// let it run), run it as the user in the session's folder.
    async fn run_command(self: &Arc<Self>, session_id: &str, args: Value) -> Value {
        let call_id = new_id("mcp");
        let tool = "run_command";
        let argv: Option<Vec<String>> = match &args["command"] {
            Value::Array(a) => a.iter().map(|x| x.as_str().map(str::to_string)).collect(),
            _ => None,
        };
        let stdin = args["stdin"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
        let reason = args["reason"].as_str().unwrap_or_default().trim().to_string();
        let rule = argv.as_deref().and_then(|a| self.allowed_commands().into_iter().find(|r| crate::hostcmd::matches(r, a)));
        let dir = self.session_git_dir(session_id);
        // An allowed command that names a file outside the session's folder still asks.
        let outside = match (&argv, &dir) {
            (Some(a), Some(d)) => crate::hostcmd::outside_file(d, a),
            _ => None,
        };
        let rule = rule.filter(|_| outside.is_none());
        let base = json!({
            "tool_call_id": call_id, "name": tool, "exposed_name": RUN_COMMAND_TOOL, "arguments": args,
            "source": "mcp", "connector_id": BUILTIN_CONNECTOR_ID, "connector_name": "AgentOS",
            "tool_kind": ToolKind::Write, "policy": if rule.is_some() { ToolPolicy::Auto } else { ToolPolicy::Ask }
        });
        let _ = self.ctx.append(session_id, EventType::ToolCall, base);
        let finish = |status: &str, output: &str| {
            let (out, truncated) = truncate(output, crate::session::TOOL_OUTPUT_LIMIT);
            let _ = self.ctx.append(
                session_id,
                EventType::ToolResult,
                json!({"tool_call_id": call_id, "name": tool, "status": status, "output": out, "truncated": truncated, "source": "mcp", "connector_id": BUILTIN_CONNECTOR_ID}),
            );
        };
        if self.rate_limited(session_id) {
            finish("failed", "rate limit reached");
            return text_result("AgentOS: too many calls this minute; try again shortly", true);
        }
        let Some(argv) = argv else {
            let e = "give `command` as a list of strings: the program, then its arguments";
            finish("failed", e);
            return text_result(&format!("AgentOS: {e}"), true);
        };
        let Some(dir) = dir else {
            finish("failed", "no such session");
            return text_result("AgentOS: this session's folder is unknown", true);
        };
        let plan = crate::hostcmd::plan(
            &dir,
            &argv,
            stdin.as_deref(),
            self.ctx.cfg.install_bin_dir.as_deref(),
            &[self.session_writable_dirs(), vec![dir.clone()]].concat(),
        );
        let plan = match plan {
            Ok(p) => p,
            Err(e) => {
                finish("failed", &e);
                return text_result(&format!("AgentOS: {e}"), true);
            }
        };
        let decision = if rule.is_some() {
            ApprovalDecision::Approve
        } else {
            let input = plan.stdin.as_deref().unwrap_or_default();
            let (mut details, cut) = truncate(input, 4000);
            if cut {
                details.push_str(&format!("\n[… {} more bytes not shown]", input.len() - details.len()));
            }
            let offer = plan.rule();
            let mut note = "Runs with your logins and your access to files, in this session's folder; its output goes back to the agent. Run only what you expected.".to_string();
            if let Some(a) = &outside {
                note = format!("Names a file outside this session's folder ({a}). {note}");
            }
            let shown = json!({
                "command": plan.command(),
                "argv": plan.argv,
                "details": details,
                "details_label": "Input",
                "reason": reason,
                "runs": "on this computer, as you, outside the sandbox",
                "approve_label": "Run",
                "ask_title": "Run a Command?",
                "note": note,
                "outside_file": outside,
                "allow_rule": offer,
                "allow_rule_text": offer.as_deref().map(crate::hostcmd::display),
            });
            self.await_approval(session_id, BUILTIN_CONNECTOR_ID, "AgentOS", tool, &shown, Some(plan.summary()))
                .await
        };
        self.ctx.audit.log(
            "command.decision",
            json!({"session_id": session_id, "argv": plan.argv, "dir": plan.dir, "decision": decision, "auto": rule}),
        );
        if decision != ApprovalDecision::Approve {
            finish("denied", "the user denied this command");
            return text_result("The user denied this command.", true);
        }
        let out = crate::hostcmd::run(&plan).await;
        self.ctx.audit.log(
            "command.done",
            json!({"session_id": session_id, "argv": plan.argv, "dir": plan.dir, "ok": out.ok, "code": out.code}),
        );
        let output = if out.output.is_empty() { "(no output)".to_string() } else { out.output.clone() };
        finish(if out.ok { "completed" } else { "failed" }, &format!("$ {}\n{output}", plan.command()));
        // `gh pr create` run as a plain command opens one too.
        if plan.argv.iter().take(3).map(String::as_str).eq(["gh", "pr", "create"]) {
            self.record_pull_request(session_id, &out.output);
        }
        if out.ok {
            text_result(&format!("Ran `{}` as the user, outside the sandbox:\n{output}", plan.command()), false)
        } else {
            let code = out.code.map(|c| format!(" (exit code {c})")).unwrap_or_default();
            text_result(&format!("`{}` failed{code}:\n{output}", plan.command()), true)
        }
    }

    /// `agenthost__install_tool`: check the request, ask the user, run it.
    async fn install_tool(self: &Arc<Self>, session_id: &str, args: Value) -> Value {
        let call_id = new_id("mcp");
        let s = |k: &str| args[k].as_str().unwrap_or_default().to_string();
        let (manager, package, version, reason) = (s("manager"), s("package"), s("version"), s("reason"));
        let base = json!({
            "tool_call_id": call_id, "name": "install_tool", "exposed_name": INSTALL_TOOL, "arguments": args,
            "source": "mcp", "connector_id": BUILTIN_CONNECTOR_ID, "connector_name": "AgentOS",
            "tool_kind": ToolKind::Write, "policy": ToolPolicy::Ask
        });
        let _ = self.ctx.append(session_id, EventType::ToolCall, base);
        let finish = |status: &str, output: &str| {
            let (out, truncated) = truncate(output, crate::session::TOOL_OUTPUT_LIMIT);
            let _ = self.ctx.append(
                session_id,
                EventType::ToolResult,
                json!({"tool_call_id": call_id, "name": "install_tool", "status": status, "output": out, "truncated": truncated, "source": "mcp", "connector_id": BUILTIN_CONNECTOR_ID}),
            );
        };
        if self.rate_limited(session_id) {
            finish("failed", "rate limit reached");
            return text_result("AgentOS: too many calls this minute; try again shortly", true);
        }
        let version = Some(version.as_str()).filter(|v| !v.trim().is_empty());
        let plan = match crate::installer::plan(&manager, &package, version) {
            Ok(p) => p,
            Err(e) => {
                finish("failed", &e);
                return text_result(&format!("AgentOS: {e}"), true);
            }
        };
        let bin_dir = self.ctx.cfg.install_bin_dir.clone();
        if !crate::installer::available(bin_dir.as_deref()).contains(&plan.manager) {
            let e = format!("{} is not installed on this computer", plan.manager.name());
            finish("failed", &e);
            return text_result(&format!("AgentOS: {e}"), true);
        }
        // What the user sees and approves: the exact command, and why.
        let shown = json!({
            "command": plan.command(),
            "manager": plan.manager.name(),
            "package": plan.package,
            "version": plan.version,
            "reason": reason,
            "runs": "on this computer, as you, outside the sandbox",
            "approve_label": "Install",
            "ask_title": "Install a Tool?",
            "note": "The package manager may run the package's install scripts with your permissions. Install only what you expected.",
        });
        let decision = self
            .await_approval(session_id, BUILTIN_CONNECTOR_ID, "AgentOS", "install_tool", &shown, Some(plan.summary()))
            .await;
        self.ctx.audit.log(
            "install.decision",
            json!({"session_id": session_id, "command": plan.command(), "decision": decision}),
        );
        if decision != ApprovalDecision::Approve {
            finish("denied", "the user denied this install");
            return text_result("The user denied this install.", true);
        }
        let _guard = self.install_lock.lock().await;
        let out = crate::installer::run(&plan, bin_dir.as_deref()).await;
        self.ctx.audit.log(
            "install.done",
            json!({"session_id": session_id, "command": plan.command(), "ok": out.ok}),
        );
        let output = if out.output.is_empty() { "(no output)".to_string() } else { out.output.clone() };
        finish(if out.ok { "completed" } else { "failed" }, &format!("$ {}\n{output}", plan.command()));
        if out.ok {
            text_result(
                &format!(
                    "Installed with `{}`. It is installed for the user, outside your sandbox. Output:\n{output}",
                    plan.command()
                ),
                false,
            )
        } else {
            text_result(&format!("The install failed (`{}`):\n{output}", plan.command()), true)
        }
    }

    fn set_waiting(&self, session_id: &str, delta: i64) {
        let now_waiting = {
            let mut w = self.waiting.lock().unwrap();
            let n = w.entry(session_id.to_string()).or_insert(0);
            *n = (*n as i64 + delta).max(0) as usize;
            *n > 0
        };
        if let Some(s) = self.sessions.upgrade() {
            s.notify_approval(session_id, now_waiting);
        }
    }

    // -----------------------------------------------------------------------
    // Remote MCP client (streamable HTTP)
    // -----------------------------------------------------------------------

    async fn auth_header(&self, c: &ConnectorRow) -> Result<Option<String>> {
        match c.auth {
            ConnectorAuth::None => Ok(None),
            ConnectorAuth::Bearer => {
                let t = self
                    .secrets
                    .get(&secret_name(&c.connector_id, "token"))?
                    .ok_or_else(|| anyhow!("no token stored"))?;
                Ok(Some(format!("Bearer {}", String::from_utf8_lossy(&t))))
            }
            ConnectorAuth::Oauth => {
                let Some(raw) = self.secrets.get(&secret_name(&c.connector_id, "oauth"))? else {
                    bail!(NeedsSignIn)
                };
                let mut tok: Value = serde_json::from_slice(&raw)?;
                let expires = tok["expires_at"].as_i64().unwrap_or(i64::MAX);
                if expires - 60_000 < now_ms() && tok["refresh_token"].is_string() {
                    tok = self.oauth_refresh(c, &tok).await?;
                }
                Ok(Some(format!(
                    "Bearer {}",
                    tok["access_token"].as_str().unwrap_or_default()
                )))
            }
        }
    }

    async fn post(
        &self,
        c: &ConnectorRow,
        body: &Value,
        session: Option<&str>,
        auth: Option<&str>,
    ) -> Result<(
        reqwest::StatusCode,
        Option<String>,
        Option<Value>,
        reqwest::header::HeaderMap,
    )> {
        let mut req = self
            .http
            .post(&c.url)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", MCP_PROTOCOL)
            .json(body);
        if let Some(s) = session {
            req = req.header("mcp-session-id", s);
        }
        if let Some(a) = auth {
            req = req.header("authorization", a);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("connect to {}", c.url))?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let sid = headers
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let is_sse = headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.starts_with("text/event-stream"))
            .unwrap_or(false);
        let text = resp.text().await.unwrap_or_default();
        let want_id = body.get("id").cloned();
        let value = if text.trim().is_empty() {
            None
        } else if is_sse {
            let mut found = None;
            for line in text.lines() {
                if let Some(d) = line.strip_prefix("data:") {
                    if let Ok(v) = serde_json::from_str::<Value>(d.trim()) {
                        if want_id.is_none() || v.get("id") == want_id.as_ref() {
                            found = Some(v);
                        }
                    }
                }
            }
            found
        } else {
            serde_json::from_str(&text).ok()
        };
        Ok((status, sid, value, headers))
    }

    /// One request to a connector, initializing (and re-initializing) its MCP session.
    async fn remote_call(&self, c: &ConnectorRow, method: &str, params: Value) -> Result<Value> {
        for attempt in 0..2 {
            let auth = self.auth_header(c).await?;
            let mut remotes = self.remotes.lock().await;
            let rs = remotes.entry(c.connector_id.clone()).or_default().clone();
            let mut rs = rs;
            if !rs.initialized {
                let init = json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
                    "protocolVersion": MCP_PROTOCOL, "capabilities": {},
                    "clientInfo": {"name": "agenthost", "version": env!("CARGO_PKG_VERSION")}}});
                let (status, sid, v, headers) = self.post(c, &init, None, auth.as_deref()).await?;
                if status == reqwest::StatusCode::UNAUTHORIZED {
                    drop(remotes);
                    if attempt == 0
                        && c.auth == ConnectorAuth::Oauth
                        && self.force_refresh(c).await.is_ok()
                    {
                        continue;
                    }
                    let _ = headers;
                    bail!(NeedsSignIn);
                }
                if !status.is_success() {
                    bail!("initialize failed: HTTP {status}");
                }
                if let Some(e) = v.as_ref().and_then(|v| v.get("error")) {
                    bail!("initialize failed: {e}");
                }
                rs.session_id = sid;
                let _ = self
                    .post(
                        c,
                        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                        rs.session_id.as_deref(),
                        auth.as_deref(),
                    )
                    .await;
                rs.initialized = true;
                remotes.insert(c.connector_id.clone(), rs.clone());
            }
            drop(remotes);
            let id = rand::random::<u32>() as i64;
            let body = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
            let (status, _, v, _) = self
                .post(c, &body, rs.session_id.as_deref(), auth.as_deref())
                .await?;
            if status == reqwest::StatusCode::NOT_FOUND && rs.session_id.is_some() && attempt == 0 {
                // The server forgot our session: start a new one.
                self.remotes.lock().await.remove(&c.connector_id);
                continue;
            }
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.remotes.lock().await.remove(&c.connector_id);
                if attempt == 0
                    && c.auth == ConnectorAuth::Oauth
                    && self.force_refresh(c).await.is_ok()
                {
                    continue;
                }
                bail!(NeedsSignIn);
            }
            if !status.is_success() {
                bail!("HTTP {status}");
            }
            let v = v.ok_or_else(|| anyhow!("empty response"))?;
            if let Some(e) = v.get("error") {
                bail!("{}", e["message"].as_str().unwrap_or(&e.to_string()));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
        bail!("connector request failed")
    }

    /// Fetch and classify a connector's tools, updating its status.
    async fn refresh_tools(&self, c: &mut ConnectorRow) {
        let mut tools = vec![];
        let mut cursor: Option<String> = None;
        let result: Result<()> = async {
            for _ in 0..20 {
                let params = match &cursor {
                    Some(cur) => json!({"cursor": cur}),
                    None => json!({}),
                };
                let r = self.remote_call(c, "tools/list", params).await?;
                for t in r["tools"].as_array().cloned().unwrap_or_default() {
                    let Some(name) = t["name"].as_str() else {
                        continue;
                    };
                    tools.push(RawTool {
                        name: name.to_string(),
                        description: t["description"].as_str().map(str::to_string),
                        kind: classify(&t),
                        definition: t.clone(),
                    });
                }
                cursor = r["nextCursor"].as_str().map(str::to_string);
                if cursor.is_none() {
                    break;
                }
            }
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                c.tools = tools;
                c.status = "ok".into();
                c.status_detail = None;
            }
            Err(e) if e.is::<NeedsSignIn>() => {
                c.status = "needs_sign_in".into();
                c.status_detail = Some("sign in to use this connector".into());
                if c.auth == ConnectorAuth::None {
                    c.auth = ConnectorAuth::Oauth;
                }
            }
            Err(e) => {
                c.status = "error".into();
                c.status_detail = Some(format!("{e:#}"));
            }
        }
        let _ = self.ctx.store.connector_save(c);
        self.ctx
            .bus
            .publish(BusMsg::ConnectorChanged(c.connector_id.clone()));
    }

    // -----------------------------------------------------------------------
    // OAuth (MCP authorization: protected resource metadata, authorization
    // server metadata, dynamic client registration, PKCE)
    // -----------------------------------------------------------------------

    async fn discover(&self, c: &ConnectorRow) -> Result<Value> {
        let server = url::Url::parse(&c.url)?;
        // The resource metadata URL from WWW-Authenticate, else the well-known path.
        let init = json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":MCP_PROTOCOL,"capabilities":{},"clientInfo":{"name":"agenthost","version":"0"}}});
        let (_, _, _, headers) = self.post(c, &init, None, None).await?;
        let mut prm_url = headers
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .and_then(|h| h.split("resource_metadata=").nth(1))
            .map(|r| {
                r.trim_start_matches('"')
                    .split('"')
                    .next()
                    .unwrap_or("")
                    .to_string()
            })
            .filter(|s| !s.is_empty());
        if prm_url.is_none() {
            let mut u = server.clone();
            u.set_path(&format!(
                "/.well-known/oauth-protected-resource{}",
                server.path().trim_end_matches('/')
            ));
            prm_url = Some(u.to_string());
        }
        let mut as_url: Option<String> = None;
        let mut resource = c.url.clone();
        if let Some(p) = prm_url {
            if let Ok(r) = self.http.get(&p).send().await {
                if r.status().is_success() {
                    if let Ok(v) = r.json::<Value>().await {
                        as_url = v["authorization_servers"][0].as_str().map(str::to_string);
                        if let Some(res) = v["resource"].as_str() {
                            resource = res.to_string();
                        }
                    }
                }
            }
        }
        let issuer = url::Url::parse(
            &as_url.unwrap_or_else(|| format!("{}://{}", server.scheme(), server.authority())),
        )?;
        let mut meta = None;
        let path = issuer.path().trim_end_matches('/').to_string();
        for wk in ["oauth-authorization-server", "openid-configuration"] {
            let mut u = issuer.clone();
            u.set_path(&format!("/.well-known/{wk}{path}"));
            if let Ok(r) = self.http.get(u.as_str()).send().await {
                if r.status().is_success() {
                    if let Ok(v) = r.json::<Value>().await {
                        meta = Some(v);
                        break;
                    }
                }
            }
        }
        let mut meta = meta
            .ok_or_else(|| anyhow!("no OAuth authorization server metadata found for {}", c.url))?;
        meta["resource"] = json!(resource);
        Ok(meta)
    }

    /// Start an OAuth sign-in: returns the URL to open in a browser. A
    /// loopback listener on 127.0.0.1 waits up to 5 minutes for the redirect,
    /// then closes.
    async fn oauth_start(self: &Arc<Self>, c: &ConnectorRow) -> Result<String> {
        let meta = self.discover(c).await?;
        let authorize = meta["authorization_endpoint"]
            .as_str()
            .ok_or_else(|| anyhow!("no authorization_endpoint"))?
            .to_string();
        let token_ep = meta["token_endpoint"]
            .as_str()
            .ok_or_else(|| anyhow!("no token_endpoint"))?
            .to_string();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let redirect = format!("http://127.0.0.1:{port}/callback");
        let reg_ep = meta["registration_endpoint"]
            .as_str()
            .ok_or_else(|| anyhow!("the server does not support dynamic client registration"))?;
        let reg = self
            .http
            .post(reg_ep)
            .json(&json!({
                "client_name": "AgentOS",
                // A desktop app with a loopback redirect. Without this, some
                // servers assume a web client and refuse http://127.0.0.1.
                "application_type": "native",
                "redirect_uris": [redirect],
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "none"
            }))
            .send()
            .await?;
        let reg = oauth_json(reg, "client registration").await?;
        let client_id = reg["client_id"]
            .as_str()
            .ok_or_else(|| anyhow!("registration returned no client_id"))?
            .to_string();
        let client_secret = reg["client_secret"].as_str().map(str::to_string);
        let verifier = random_hex(32);
        use base64::Engine;
        use sha2::Digest;
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(verifier.as_bytes()));
        let state = random_hex(16);
        let resource = meta["resource"].as_str().unwrap_or(&c.url).to_string();
        let mut u = url::Url::parse(&authorize)?;
        {
            let mut q = u.query_pairs_mut();
            q.append_pair("response_type", "code")
                .append_pair("client_id", &client_id)
                .append_pair("redirect_uri", &redirect)
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("state", &state)
                .append_pair("resource", &resource);
            if let Some(scopes) = meta["scopes_supported"].as_array() {
                let s: Vec<&str> = scopes.iter().filter_map(Value::as_str).collect();
                if !s.is_empty() {
                    q.append_pair("scope", &s.join(" "));
                }
            }
        }
        let gw = self.clone();
        let cid = c.connector_id.clone();
        let task = tokio::spawn(async move {
            let r = tokio::time::timeout(Duration::from_secs(300), async {
                loop {
                    let (mut s, _) = listener.accept().await?;
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = vec![0u8; 8192];
                    let n = s.read(&mut buf).await?;
                    let head = String::from_utf8_lossy(&buf[..n]).to_string();
                    let target = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let u = url::Url::parse(&format!("http://127.0.0.1{target}"))?;
                    if u.path() != "/callback" {
                        let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                        continue;
                    }
                    let q: HashMap<String, String> = u.query_pairs().into_owned().collect();
                    let ok = q.get("state") == Some(&state) && q.contains_key("code");
                    let page = if ok { "Signed in. You can close this tab." } else { "Sign-in failed. You can close this tab." };
                    let _ = s
                        .write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len()).as_bytes())
                        .await;
                    if !ok {
                        bail!("{}", q.get("error_description").or(q.get("error")).cloned().unwrap_or_else(|| "state mismatch".into()));
                    }
                    return Ok::<String, anyhow::Error>(q["code"].clone());
                }
            })
            .await;
            let outcome: Result<()> = async {
                let code = r.map_err(|_| anyhow!("sign-in timed out"))??;
                let mut form = vec![
                    ("grant_type", "authorization_code".to_string()),
                    ("code", code),
                    ("redirect_uri", redirect.clone()),
                    ("client_id", client_id.clone()),
                    ("code_verifier", verifier.clone()),
                    ("resource", resource.clone()),
                ];
                if let Some(cs) = &client_secret {
                    form.push(("client_secret", cs.clone()));
                }
                let tok = gw.http.post(&token_ep).form(&form).send().await?;
                let tok = oauth_json(tok, "token exchange").await?;
                gw.store_tokens(
                    &cid,
                    &tok,
                    &client_id,
                    client_secret.as_deref(),
                    &token_ep,
                    &resource,
                )?;
                Ok(())
            }
            .await;
            if let Ok(Some(mut c)) = gw.ctx.store.connector_get(&cid) {
                match outcome {
                    Ok(()) => {
                        c.auth = ConnectorAuth::Oauth;
                        gw.remotes.lock().await.remove(&cid);
                        gw.refresh_tools(&mut c).await;
                    }
                    Err(e) => {
                        c.status = "error".into();
                        c.status_detail = Some(format!("sign-in failed: {e:#}"));
                        let _ = gw.ctx.store.connector_save(&c);
                        gw.ctx.bus.publish(BusMsg::ConnectorChanged(cid.clone()));
                    }
                }
            }
            gw.oauth.lock().unwrap().remove(&cid);
        });
        if let Some(old) = self
            .oauth
            .lock()
            .unwrap()
            .insert(c.connector_id.clone(), task)
        {
            old.abort();
        }
        Ok(u.to_string())
    }

    fn store_tokens(
        &self,
        cid: &str,
        tok: &Value,
        client_id: &str,
        client_secret: Option<&str>,
        token_ep: &str,
        resource: &str,
    ) -> Result<()> {
        let access = tok["access_token"]
            .as_str()
            .ok_or_else(|| anyhow!("token response has no access_token"))?;
        let expires_at = tok["expires_in"].as_i64().map(|s| now_ms() + s * 1000);
        let rec = json!({
            "access_token": access,
            "refresh_token": tok["refresh_token"],
            "expires_at": expires_at,
            "client_id": client_id,
            "client_secret": client_secret,
            "token_endpoint": token_ep,
            "resource": resource,
        });
        self.secrets
            .set(&secret_name(cid, "oauth"), rec.to_string().as_bytes())
    }

    async fn oauth_refresh(&self, c: &ConnectorRow, tok: &Value) -> Result<Value> {
        let mut form = vec![
            ("grant_type", "refresh_token".to_string()),
            (
                "refresh_token",
                tok["refresh_token"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            ),
            (
                "client_id",
                tok["client_id"].as_str().unwrap_or_default().to_string(),
            ),
        ];
        if let Some(r) = tok["resource"].as_str() {
            form.push(("resource", r.to_string()));
        }
        if let Some(cs) = tok["client_secret"].as_str() {
            form.push(("client_secret", cs.to_string()));
        }
        let ep = tok["token_endpoint"]
            .as_str()
            .ok_or_else(|| anyhow!("no token endpoint"))?;
        let resp = self.http.post(ep).form(&form).send().await?;
        if !resp.status().is_success() {
            bail!(NeedsSignIn);
        }
        let mut new: Value = resp.json().await?;
        if new["refresh_token"].is_null() {
            new["refresh_token"] = tok["refresh_token"].clone();
        }
        self.store_tokens(
            &c.connector_id,
            &new,
            tok["client_id"].as_str().unwrap_or_default(),
            tok["client_secret"].as_str(),
            ep,
            tok["resource"].as_str().unwrap_or(&c.url),
        )?;
        let raw = self
            .secrets
            .get(&secret_name(&c.connector_id, "oauth"))?
            .unwrap_or_default();
        Ok(serde_json::from_slice(&raw)?)
    }

    async fn force_refresh(&self, c: &ConnectorRow) -> Result<()> {
        let raw = self
            .secrets
            .get(&secret_name(&c.connector_id, "oauth"))?
            .ok_or(NeedsSignIn)?;
        let tok: Value = serde_json::from_slice(&raw)?;
        if !tok["refresh_token"].is_string() {
            bail!(NeedsSignIn);
        }
        self.oauth_refresh(c, &tok).await.map(|_| ())
    }

    // -----------------------------------------------------------------------
    // Admin
    // -----------------------------------------------------------------------

    /// Fetch and store a connector's icon (its site's favicon), then tell
    /// clients. Failures leave it without one.
    async fn fetch_connector_icon(&self, connector_id: &str, url: &str) {
        let Some(base) = self.ctx.cfg.favicon_url.as_deref() else {
            return;
        };
        let Some(address) = crate::integrations::favicon_address(base, url) else {
            return;
        };
        let Some(icon) = crate::integrations::fetch_icon(&self.http, &address).await else {
            tracing::debug!(connector = connector_id, "no icon from {address}");
            return;
        };
        if let Ok(true) = self.ctx.store.connector_set_icon(connector_id, &icon) {
            self.ctx
                .bus
                .publish(BusMsg::ConnectorChanged(connector_id.to_string()));
        }
    }

    fn view(&self, c: &ConnectorRow) -> Result<Value, RpcError> {
        serde_json::to_value(
            self.ctx
                .store
                .connector_view(c)
                .map_err(|e| RpcError::internal(e.to_string()))?,
        )
        .map_err(|e| RpcError::internal(e.to_string()))
    }

    fn get(&self, id: &str) -> Result<ConnectorRow, RpcError> {
        self.ctx
            .store
            .connector_get(id)
            .map_err(|e| RpcError::internal(e.to_string()))?
            .ok_or_else(|| RpcError::not_found(format!("no connector {id}")))
    }
}

/// The remote server needs (new) authorization.
#[derive(Debug)]
pub struct NeedsSignIn;
impl std::fmt::Display for NeedsSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("sign-in required")
    }
}
impl std::error::Error for NeedsSignIn {}

impl GatewayApi for Gateway {
    fn register_session(&self, session_id: &str, workspace_id: &str) -> Option<String> {
        let token = random_hex(24);
        let mut regs = self.regs.lock().unwrap();
        regs.retain(|_, r| r.session_id != session_id);
        regs.insert(
            token.clone(),
            SessionReg {
                session_id: session_id.into(),
                workspace_id: workspace_id.into(),
                calls: VecDeque::new(),
            },
        );
        Some(format!("http://{GATEWAY_HOST}/s/{token}/mcp"))
    }

    fn unregister_session(&self, session_id: &str) {
        self.regs
            .lock()
            .unwrap()
            .retain(|_, r| r.session_id != session_id);
        // Deny anything still waiting for this session.
        let ids: Vec<String> = self
            .approvals
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, p)| p.info.session_id == session_id)
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            if let Some(p) = self.approvals.lock().unwrap().remove(&id) {
                let _ = p.tx.send((ApprovalDecision::Deny, "session ended".into()));
            }
        }
    }

    fn socket_path(&self) -> PathBuf {
        self.socket.clone()
    }
}

#[async_trait::async_trait]
impl ConnectorAdmin for Arc<Gateway> {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let p = if params.is_null() { json!({}) } else { params };
        let bad = |e: serde_json::Error| RpcError::invalid_params(e.to_string());
        match method {
            m::BUILTIN_LIST => Ok(serde_json::to_value(self.builtin_list()).unwrap_or_default()),
            m::BUILTIN_SET_POLICY => {
                let bp: BuiltinPolicyParams = serde_json::from_value(p).map_err(bad)?;
                let list = self.set_builtin_policy(&bp).map_err(RpcError::invalid_params)?;
                Ok(serde_json::to_value(list).unwrap_or_default())
            }
            m::CONNECTOR_LIST => {
                let list = self
                    .ctx
                    .store
                    .connector_list()
                    .map_err(|e| RpcError::internal(e.to_string()))?;
                let views: Result<Vec<Value>, _> = list.iter().map(|c| self.view(c)).collect();
                Ok(Value::Array(views?))
            }
            m::CONNECTOR_ADD => {
                let a: ConnectorAddParams = serde_json::from_value(p).map_err(bad)?;
                let u = url::Url::parse(&a.url)
                    .map_err(|e| RpcError::invalid_params(format!("bad url: {e}")))?;
                if u.scheme() != "https"
                    && !(u.scheme() == "http"
                        && matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
                {
                    return Err(RpcError::invalid_params(
                        "connectors must use https (http only for localhost)",
                    ));
                }
                let auth = a.auth.unwrap_or(if a.token.is_some() {
                    ConnectorAuth::Bearer
                } else {
                    ConnectorAuth::None
                });
                let name = match a.name.as_deref().map(str::trim) {
                    Some(n) if !n.is_empty() => n.to_string(),
                    _ => {
                        let existing = self
                            .ctx
                            .store
                            .connector_list()
                            .map_err(|e| RpcError::internal(e.to_string()))?;
                        let names: Vec<&str> = existing.iter().map(|c| c.name.as_str()).collect();
                        agenthost_proto::default_connector_name(
                            u.host_str().unwrap_or("mcp"),
                            &names,
                        )
                    }
                };
                let mut c = ConnectorRow {
                    connector_id: new_id("con"),
                    name,
                    url: a.url,
                    auth,
                    enabled: true,
                    status: "unknown".into(),
                    status_detail: None,
                    read_policy: ToolPolicy::Auto,
                    write_policy: ToolPolicy::Ask,
                    tools: vec![],
                    oauth_meta: None,
                    created_at: now_ms(),
                    icon: None,
                };
                if let Some(t) = &a.token {
                    self.secrets
                        .set(&secret_name(&c.connector_id, "token"), t.as_bytes())
                        .map_err(|e| RpcError::internal(e.to_string()))?;
                }
                self.ctx
                    .store
                    .connector_insert(&c)
                    .map_err(|e| RpcError::internal(e.to_string()))?;
                // On in every workspace; each can switch it off.
                for mut w in self
                    .ctx
                    .store
                    .workspace_list()
                    .map_err(|e| RpcError::internal(e.to_string()))?
                {
                    w.connectors.push(c.connector_id.clone());
                    self.ctx
                        .store
                        .workspace_update(&w)
                        .map_err(|e| RpcError::internal(e.to_string()))?;
                }
                let g = self.clone();
                let (id, url) = (c.connector_id.clone(), c.url.clone());
                tokio::spawn(async move { g.fetch_connector_icon(&id, &url).await });
                self.refresh_tools(&mut c).await;
                self.view(&c)
            }
            m::CONNECTOR_SEARCH => {
                let sp: ConnectorSearchParams = serde_json::from_value(p).map_err(bad)?;
                if sp.query.trim().is_empty() {
                    return Ok(json!([]));
                }
                let Some(base) = self.ctx.cfg.integrations_url.clone() else {
                    return Err(RpcError::new(
                        codes::UNAVAILABLE,
                        "connector search is turned off on this host; add the connector by its address",
                    ));
                };
                let found = crate::integrations::search(&self.http, &base, &sp.query)
                    .await
                    .map_err(|e| RpcError::new(codes::UNAVAILABLE, format!("{e:#}")))?;
                serde_json::to_value(found).map_err(|e| RpcError::internal(e.to_string()))
            }
            m::CONNECTOR_UPDATE => {
                let u: ConnectorUpdateParams = serde_json::from_value(p).map_err(bad)?;
                let mut c = self.get(&u.connector_id)?;
                if let Some(n) = u.name {
                    let n = n.trim();
                    if n.is_empty() {
                        return Err(RpcError::invalid_params("a connector needs a name"));
                    }
                    // Agents see tools as `<slug>__<tool>`: two connectors
                    // with one slug would rename the other's tools.
                    let new_slug = slug(n);
                    if new_slug == BUILTIN_CONNECTOR_ID {
                        return Err(RpcError::invalid_params(format!(
                            "{n} is taken by the host's own tools; pick another name"
                        )));
                    }
                    let others = self
                        .ctx
                        .store
                        .connector_list()
                        .map_err(|e| RpcError::internal(e.to_string()))?;
                    if let Some(o) = others
                        .iter()
                        .find(|o| o.connector_id != c.connector_id && slug(&o.name) == new_slug)
                    {
                        return Err(RpcError::invalid_params(format!(
                            "{} already gives its tools the prefix {new_slug}; pick a name that differs in more than punctuation or case",
                            o.name
                        )));
                    }
                    let old_slug = slug(&c.name);
                    if old_slug != new_slug {
                        let mut r = self.renamed.lock().unwrap();
                        r.insert(old_slug, c.connector_id.clone());
                        r.remove(&new_slug);
                    }
                    c.name = n.to_string();
                }
                if let Some(e) = u.enabled {
                    c.enabled = e;
                }
                if let Some(wp) = u.write_policy {
                    c.write_policy = wp;
                }
                if let Some(rp) = u.read_policy {
                    c.read_policy = rp;
                }
                if let Some(t) = u.token {
                    self.secrets
                        .set(&secret_name(&c.connector_id, "token"), t.as_bytes())
                        .map_err(|e| RpcError::internal(e.to_string()))?;
                    c.auth = ConnectorAuth::Bearer;
                }
                if let Some(tp) = u.tool_policies {
                    for (tool, pol) in tp {
                        if !c.tools.iter().any(|t| t.name == tool) {
                            return Err(RpcError::invalid_params(format!(
                                "{} has no tool {tool}",
                                c.name
                            )));
                        }
                        self.ctx
                            .store
                            .set_tool_policy(&c.connector_id, &tool, pol)
                            .map_err(|e| RpcError::internal(e.to_string()))?;
                    }
                }
                self.ctx
                    .store
                    .connector_save(&c)
                    .map_err(|e| RpcError::internal(e.to_string()))?;
                self.view(&c)
            }
            m::CONNECTOR_REMOVE => {
                let id: IdParams = serde_json::from_value(p).map_err(bad)?;
                let c = self.get(&id.id)?;
                for w in self
                    .ctx
                    .store
                    .workspace_list()
                    .map_err(|e| RpcError::internal(e.to_string()))?
                {
                    if w.connectors.contains(&c.connector_id) {
                        let mut w = w;
                        w.connectors.retain(|x| *x != c.connector_id);
                        let _ = self.ctx.store.workspace_update(&w);
                    }
                }
                let _ = self.secrets.delete(&secret_name(&c.connector_id, "token"));
                let _ = self.secrets.delete(&secret_name(&c.connector_id, "oauth"));
                self.remotes.lock().await.remove(&c.connector_id);
                self.ctx
                    .store
                    .connector_remove(&c.connector_id)
                    .map_err(|e| RpcError::internal(e.to_string()))?;
                Ok(json!({"removed": true}))
            }
            m::CONNECTOR_REFRESH => {
                let id: IdParams = serde_json::from_value(p).map_err(bad)?;
                let mut c = self.get(&id.id)?;
                self.remotes.lock().await.remove(&c.connector_id);
                self.refresh_tools(&mut c).await;
                self.view(&c)
            }
            m::CONNECTOR_SIGN_IN => {
                let id: IdParams = serde_json::from_value(p).map_err(bad)?;
                let mut c = self.get(&id.id)?;
                let url = self.oauth_start(&c).await.map_err(|e| {
                    RpcError::new(
                        codes::UNAVAILABLE,
                        format!("sign-in could not start: {e:#}"),
                    )
                })?;
                // Until the browser comes back: success sets `ok`, failure `error`.
                c.status = "needs_sign_in".into();
                c.status_detail = Some("waiting for sign-in in the browser".into());
                let _ = self.ctx.store.connector_save(&c);
                serde_json::to_value(ConnectorSignInResult {
                    authorize_url: Some(url),
                    status: "waiting_for_browser".into(),
                })
                .map_err(|e| RpcError::internal(e.to_string()))
            }
            _ => Err(RpcError::new(codes::METHOD_NOT_FOUND, method.to_string())),
        }
    }

    fn pending(&self) -> Vec<PendingApproval> {
        let mut v: Vec<PendingApproval> = self
            .approvals
            .lock()
            .unwrap()
            .values()
            .map(|p| p.info.clone())
            .collect();
        v.sort_by_key(|a| a.requested_at);
        v
    }

    fn respond(&self, p: &ApprovalRespondParams, who: &Principal) -> Result<(), RpcError> {
        let pending = self.approvals.lock().unwrap().remove(&p.approval_id);
        match pending {
            Some(x) => {
                let _ = x.tx.send((p.decision, who.label.clone()));
                Ok(())
            }
            None => Err(RpcError::not_found(
                "no such pending approval (already answered or expired)",
            )),
        }
    }
}

/// The JSON body of an OAuth response, or an error carrying the server's own
/// explanation (`error_description` / `error`) rather than a bare status.
async fn oauth_json(r: reqwest::Response, what: &str) -> Result<Value> {
    let status = r.status();
    let body = r.text().await.unwrap_or_default();
    if status.is_success() {
        return serde_json::from_str(&body).with_context(|| format!("{what}: response is not JSON"));
    }
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let why = v["error_description"]
        .as_str()
        .or(v["error"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| body.chars().take(200).collect());
    if why.trim().is_empty() {
        bail!("{what} failed (HTTP {status})");
    }
    bail!("{what} failed: {} (HTTP {status})", why.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_classification() {
        assert_eq!(slug("GitHub Issues!"), "github_issues");
        assert_eq!(slug("--"), "mcp");
        assert_eq!(exposed_name("gh", "create_issue"), "gh__create_issue");
        assert_eq!(
            classify(&json!({"annotations": {"readOnlyHint": true}})),
            ToolKind::Read
        );
        assert_eq!(
            classify(&json!({"annotations": {"readOnlyHint": false}})),
            ToolKind::Write
        );
        assert_eq!(classify(&json!({})), ToolKind::Write);
    }
}
