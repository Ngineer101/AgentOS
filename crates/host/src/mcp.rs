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

struct SessionReg {
    session_id: String,
    workspace_id: String,
    calls: VecDeque<Instant>,
}

struct Pending {
    info: PendingApproval,
    tx: oneshot::Sender<(ApprovalDecision, String)>,
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
                "instructions": "Tools from the connectors enabled for this workspace. Tool names are <connector>__<tool>."
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(
                json!({"tools": self.list_tools(workspace_id).into_iter().map(|(_, _, def)| def).collect::<Vec<_>>()}),
            ),
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
                if out.iter().any(|(o, _)| *o == s) {
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
        let call_id = new_id("mcp");
        let found = self
            .list_tools(workspace_id)
            .into_iter()
            .find(|(_, _, d)| d["name"] == name);
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
                "agenthost: too many MCP calls this minute; try again shortly",
                true,
            );
        }
        if policy == ToolPolicy::Ask {
            let decision = self
                .await_approval(session_id, &conn, &tool.name, &args)
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
                        &format!("{t}\n[agenthost: result truncated from {size} bytes]"),
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
                    &format!("agenthost: the connector {} failed: {e}", conn.name),
                    true,
                )
            }
        }
    }

    async fn await_approval(
        self: &Arc<Self>,
        session_id: &str,
        conn: &ConnectorRow,
        tool: &str,
        args: &Value,
    ) -> ApprovalDecision {
        let approval_id = new_id("apr");
        let now = now_ms();
        let info = PendingApproval {
            approval_id: approval_id.clone(),
            session_id: session_id.to_string(),
            connector_id: conn.connector_id.clone(),
            connector_name: conn.name.clone(),
            tool: tool.to_string(),
            arguments: args.clone(),
            requested_at: now,
            expires_at: now + APPROVAL_TIMEOUT.as_millis() as i64,
        };
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
        let (decision, device) = match tokio::time::timeout(APPROVAL_TIMEOUT, rx).await {
            Ok(Ok(d)) => d,
            _ => {
                self.approvals.lock().unwrap().remove(&approval_id);
                (ApprovalDecision::Deny, "timeout".to_string())
            }
        };
        let _ = self.ctx.append(
            session_id,
            EventType::ApprovalResolved,
            json!({"approval_id": approval_id, "decision": decision, "device": device, "timed_out": device == "timeout"}),
        );
        self.set_waiting(session_id, -1);
        decision
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
        let reg: Value = self
            .http
            .post(reg_ep)
            .json(&json!({
                "client_name": "agenthost",
                "redirect_uris": [redirect],
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "none"
            }))
            .send()
            .await?
            .error_for_status()
            .context("client registration")?
            .json()
            .await?;
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
                let tok: Value = gw
                    .http
                    .post(&token_ep)
                    .form(&form)
                    .send()
                    .await?
                    .error_for_status()
                    .context("token exchange")?
                    .json()
                    .await?;
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
                self.refresh_tools(&mut c).await;
                self.view(&c)
            }
            m::CONNECTOR_UPDATE => {
                let u: ConnectorUpdateParams = serde_json::from_value(p).map_err(bad)?;
                let mut c = self.get(&u.connector_id)?;
                if let Some(n) = u.name {
                    let n = n.trim();
                    if n.is_empty() {
                        return Err(RpcError::invalid_params("a connector needs a name"));
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
