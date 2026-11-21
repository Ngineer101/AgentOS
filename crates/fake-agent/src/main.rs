//! A scripted ACP agent (protocol version 1) over stdio.
//!
//! It behaves like a real agent from the host's point of view: it answers
//! `initialize`, `session/new`, `session/load`, `session/prompt` and
//! `session/cancel`, streams `session/update` notifications, and asks for
//! permission before running tools. What it does is decided by the prompt:
//!
//! | Prompt                 | Behaviour                                              |
//! |------------------------|--------------------------------------------------------|
//! | `sh <command>`         | Runs the command with `sh -c` as an `execute` tool     |
//! | `write <path> <text>`  | Writes a file as an `edit` tool                        |
//! | `sleep <secs>`         | Waits, honouring `session/cancel`                      |
//! | `fail-loop`            | Repeats the same failing tool call until cancelled     |
//! | `mcp <tool> [json]`    | Calls a tool on the first MCP server it was given      |
//! | `mcp-list`             | Lists the tools of the first MCP server                |
//! | `hang <secs>`          | Waits, ignoring `session/cancel`                       |
//! | `crash`                | Exits the process mid-turn                             |
//! | `retitle <title>`      | Names the session (`session_info_update`)              |
//! | `Name this conversation.…` | The host asking for a title: replies `Title: About <the request's first words>` |
//! | anything else          | Thinks, then replies `echo: <prompt>` in chunks        |
//!
//! Sessions have two settings (`configOptions`): `model` (`fake-fast`,
//! `fake-smart`, in a group) and `thought_level` (`low`, `medium`, `high`),
//! changed with `session/set_config_option`, plus a `mode` select. A
//! `session/fork` copies a session; a fork answers every prompt with a
//! `peek` tool call and then `side answer: <prompt>`.
//!
//! Environment: `FAKE_AGENT_HOME` (state, default `$HOME/.fake-agent`),
//! `FAKE_AGENT_LOGGED_OUT=1` (fail `session/new` with auth required),
//! `FAKE_AGENT_PROTOCOL_VERSION` (override the version it reports),
//! `FAKE_AGENT_CHUNK_MS` (pause between reply chunks), `FAKE_AGENT_NO_FORK=1`
//! (no `session/fork`).

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{oneshot, Mutex};

struct SessionData {
    cwd: String,
    mcp_servers: Vec<Value>,
    cancel: Arc<AtomicBool>,
    model: String,
    effort: String,
    /// A fork made for a side question.
    fork: bool,
}

impl SessionData {
    fn new(cwd: String, mcp_servers: Vec<Value>) -> Self {
        SessionData {
            cwd,
            mcp_servers,
            cancel: Arc::new(AtomicBool::new(false)),
            model: "fake-fast".into(),
            effort: "medium".into(),
            fork: false,
        }
    }

    fn config_options(&self) -> Value {
        json!([
            {"id": "mode", "name": "Mode", "category": "mode", "type": "select", "currentValue": "default",
             "options": [{"value": "default", "name": "Default"}]},
            {"id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": self.model,
             "options": [{"group": "fake", "name": "Fake models", "options": [
                {"value": "fake-fast", "name": "Fake Fast", "description": "Quick answers"},
                {"value": "fake-smart", "name": "Fake Smart"}
             ]}]},
            {"id": "thought_level", "name": "Effort", "category": "thought_level", "type": "select", "currentValue": self.effort,
             "options": [{"value": "low", "name": "Low"}, {"value": "medium", "name": "Medium"}, {"value": "high", "name": "High"}]},
            {"id": "fast", "name": "Fast", "type": "boolean", "currentValue": false}
        ])
    }
}

struct Agent {
    out: Mutex<tokio::io::Stdout>,
    next_id: AtomicI64,
    pending: StdMutex<HashMap<i64, oneshot::Sender<Value>>>,
    sessions: StdMutex<HashMap<String, SessionData>>,
    home: PathBuf,
}

impl Agent {
    async fn send(&self, v: Value) {
        let mut line = v.to_string();
        line.push('\n');
        let mut out = self.out.lock().await;
        let _ = out.write_all(line.as_bytes()).await;
        let _ = out.flush().await;
    }

    async fn respond(&self, id: Value, result: Result<Value, (i64, String)>) {
        let msg = match result {
            Ok(r) => json!({"jsonrpc":"2.0","id":id,"result":r}),
            Err((code, message)) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
            }
        };
        self.send(msg).await;
    }

    async fn update(&self, session_id: &str, update: Value) {
        self.send(json!({
            "jsonrpc":"2.0","method":"session/update",
            "params":{"sessionId":session_id,"update":update}
        }))
        .await;
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await;
        let resp = rx.await.map_err(|_| anyhow!("client went away"))?;
        if let Some(err) = resp.get("error") {
            return Err(anyhow!("client error: {err}"));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    fn history_path(&self, session_id: &str) -> PathBuf {
        self.home
            .join("sessions")
            .join(format!("{session_id}.json"))
    }

    fn load_history(&self, session_id: &str) -> Vec<Value> {
        std::fs::read_to_string(self.history_path(session_id))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn append_history(&self, session_id: &str, role: &str, text: &str) {
        let mut h = self.load_history(session_id);
        h.push(json!({"role":role,"text":text}));
        let p = self.history_path(session_id);
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(p, serde_json::to_string(&h).unwrap());
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let home = std::env::var_os("FAKE_AGENT_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into()))
                .join(".fake-agent")
        });
    let agent = Arc::new(Agent {
        out: Mutex::new(tokio::io::stdout()),
        next_id: AtomicI64::new(1),
        pending: StdMutex::new(HashMap::new()),
        sessions: StdMutex::new(HashMap::new()),
        home,
    });

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        let id = msg.get("id").cloned().filter(|v| !v.is_null());
        match (method, id) {
            (Some(method), Some(id)) => {
                let agent = agent.clone();
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                tokio::spawn(async move {
                    let r = handle_request(&agent, &method, params).await;
                    agent.respond(id, r).await;
                });
            }
            (Some(method), None) => {
                if method == "session/cancel" {
                    let sid = msg["params"]["sessionId"].as_str().unwrap_or_default();
                    if let Some(s) = agent.sessions.lock().unwrap().get(sid) {
                        s.cancel.store(true, Ordering::SeqCst);
                    }
                }
            }
            (None, Some(id)) => {
                if let Some(id) = id.as_i64() {
                    if let Some(tx) = agent.pending.lock().unwrap().remove(&id) {
                        let _ = tx.send(msg);
                    }
                }
            }
            (None, None) => {}
        }
    }
    // stdin closed: give in-flight responses a moment to flush, then exit.
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok(())
}

async fn handle_request(
    agent: &Arc<Agent>,
    method: &str,
    params: Value,
) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => {
            let version: i64 = std::env::var("FAKE_AGENT_PROTOCOL_VERSION")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            let session_caps = if std::env::var("FAKE_AGENT_NO_FORK").as_deref() == Ok("1") {
                json!({})
            } else {
                json!({"fork": {}})
            };
            Ok(json!({
                "protocolVersion": version,
                "agentCapabilities": {
                    "loadSession": true,
                    "sessionCapabilities": session_caps,
                    "promptCapabilities": {"image": false, "audio": false, "embeddedContext": false},
                    "mcpCapabilities": {"http": true, "sse": false}
                },
                "authMethods": [{"id":"fake-login","name":"Fake login","description":"Run `fake-agent login`"}],
                "agentInfo": {"name":"agenthost-fake-agent","version":env!("CARGO_PKG_VERSION")}
            }))
        }
        "authenticate" => Err((-32000, "Authentication required".into())),
        "session/new" | "session/load" => {
            if std::env::var("FAKE_AGENT_LOGGED_OUT").as_deref() == Ok("1") {
                return Err((-32000, "Authentication required".into()));
            }
            let cwd = params["cwd"].as_str().unwrap_or(".").to_string();
            let mcp_servers = params["mcpServers"].as_array().cloned().unwrap_or_default();
            let session_id = if method == "session/new" {
                format!("fake-{}", uuid::Uuid::new_v4())
            } else {
                let sid = params["sessionId"].as_str().unwrap_or_default().to_string();
                if !agent.history_path(&sid).exists() {
                    return Err((-32002, format!("Resource not found: session {sid}")));
                }
                sid
            };
            let options = {
                let mut sessions = agent.sessions.lock().unwrap();
                let data = SessionData::new(cwd, mcp_servers);
                let options = data.config_options();
                sessions.insert(session_id.clone(), data);
                options
            };
            if method == "session/load" {
                for item in agent.load_history(&session_id) {
                    let kind = if item["role"] == "user" {
                        "user_message_chunk"
                    } else {
                        "agent_message_chunk"
                    };
                    agent
                        .update(&session_id, json!({"sessionUpdate":kind,"content":{"type":"text","text":item["text"]}}))
                        .await;
                }
                Ok(json!({"configOptions": options}))
            } else {
                agent.append_history(&session_id, "system", "created");
                Ok(json!({"sessionId": session_id, "configOptions": options}))
            }
        }
        "session/fork" => {
            if std::env::var("FAKE_AGENT_NO_FORK").as_deref() == Ok("1") {
                return Err((-32601, "method not found: session/fork".into()));
            }
            let source = params["sessionId"].as_str().unwrap_or_default().to_string();
            let fork_id = format!("fake-fork-{}", uuid::Uuid::new_v4());
            let options = {
                let mut sessions = agent.sessions.lock().unwrap();
                let s = sessions
                    .get(&source)
                    .ok_or((-32002, format!("unknown session {source}")))?;
                let mut data = SessionData::new(
                    params["cwd"].as_str().unwrap_or(&s.cwd).to_string(),
                    params["mcpServers"].as_array().cloned().unwrap_or_default(),
                );
                data.model = s.model.clone();
                data.effort = s.effort.clone();
                data.fork = true;
                let options = data.config_options();
                sessions.insert(fork_id.clone(), data);
                options
            };
            let history = agent.load_history(&source);
            for item in history {
                agent.append_history(
                    &fork_id,
                    item["role"].as_str().unwrap_or("system"),
                    item["text"].as_str().unwrap_or(""),
                );
            }
            Ok(json!({"sessionId": fork_id, "configOptions": options}))
        }
        "session/set_config_option" => {
            let sid = params["sessionId"].as_str().unwrap_or_default().to_string();
            let id = params["configId"].as_str().unwrap_or_default();
            let value = params["value"].as_str().unwrap_or_default().to_string();
            let options = {
                let mut sessions = agent.sessions.lock().unwrap();
                let s = sessions
                    .get_mut(&sid)
                    .ok_or((-32002, format!("unknown session {sid}")))?;
                match (id, value.as_str()) {
                    ("model", "fake-fast" | "fake-smart") => s.model = value,
                    ("thought_level", "low" | "medium" | "high") => s.effort = value,
                    _ => return Err((-32602, format!("Invalid value for config option {id}: {value}"))),
                }
                s.config_options()
            };
            agent
                .update(&sid, json!({"sessionUpdate": "config_option_update", "configOptions": options}))
                .await;
            Ok(json!({"configOptions": options}))
        }
        "session/prompt" => {
            let sid = params["sessionId"].as_str().unwrap_or_default().to_string();
            let text: String = params["prompt"]
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            let (cancel, cwd, mcp, fork) = {
                let sessions = agent.sessions.lock().unwrap();
                let s = sessions
                    .get(&sid)
                    .ok_or((-32002, format!("unknown session {sid}")))?;
                s.cancel.store(false, Ordering::SeqCst);
                (s.cancel.clone(), s.cwd.clone(), s.mcp_servers.clone(), s.fork)
            };
            agent.append_history(&sid, "user", &text);
            if text.starts_with("Name this conversation.") {
                let request = text
                    .split_once("<request>\n")
                    .and_then(|(_, r)| r.split_once("\n</request>"))
                    .map(|(r, _)| r)
                    .unwrap_or_default();
                let words = request.split_whitespace().take(4).collect::<Vec<_>>().join(" ");
                say(agent, &sid, &format!("Title: About {words}")).await;
                return Ok(json!({"stopReason": "end_turn"}));
            }
            if fork {
                // A side conversation: a look around (which the host must not
                // put in the transcript), then the answer.
                agent
                    .update(&sid, json!({"sessionUpdate":"tool_call","toolCallId":"peek-1","title":"peek","kind":"read","status":"completed","rawInput":{"path":"."}}))
                    .await;
                say(agent, &sid, &format!("side answer: {text}")).await;
                return Ok(json!({"stopReason": "end_turn"}));
            }
            let stop = run_prompt(agent, &sid, &text, &cwd, &mcp, &cancel).await;
            Ok(
                json!({"stopReason": stop, "usage": {"inputTokens": text.len(), "outputTokens": 10}}),
            )
        }
        _ => Err((-32601, format!("method not found: {method}"))),
    }
}

async fn say(agent: &Agent, sid: &str, text: &str) {
    agent.append_history(sid, "agent", text);
    // Stream in a few chunks so the host sees deltas.
    let chars: Vec<char> = text.chars().collect();
    // FAKE_AGENT_CHUNK_MS: pause between chunks, to stream like a real model.
    let pause = std::env::var("FAKE_AGENT_CHUNK_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    for chunk in chars.chunks(8) {
        if pause > 0 {
            tokio::time::sleep(Duration::from_millis(pause)).await;
        }
        let s: String = chunk.iter().collect();
        agent
            .update(
                sid,
                json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":s}}),
            )
            .await;
    }
}

async fn ask_permission(agent: &Agent, sid: &str, tool_call_id: &str, title: &str) -> bool {
    let r = agent
        .request(
            "session/request_permission",
            json!({
                "sessionId": sid,
                "toolCall": {"toolCallId": tool_call_id, "title": title},
                "options": [
                    {"optionId":"allow","name":"Allow","kind":"allow_once"},
                    {"optionId":"reject","name":"Reject","kind":"reject_once"}
                ]
            }),
        )
        .await;
    match r {
        Ok(v) => v["outcome"]["outcome"] == "selected" && v["outcome"]["optionId"] == "allow",
        Err(_) => false,
    }
}

async fn tool(
    agent: &Agent,
    sid: &str,
    kind: &str,
    title: &str,
    raw_input: Value,
    run: impl std::future::Future<Output = (bool, String)>,
) -> Option<(bool, String)> {
    let id = format!("call-{}", uuid::Uuid::new_v4());
    agent
        .update(
            sid,
            json!({"sessionUpdate":"tool_call","toolCallId":id,"title":title,"kind":kind,"status":"pending","rawInput":raw_input}),
        )
        .await;
    if !ask_permission(agent, sid, &id, title).await {
        agent
            .update(sid, json!({"sessionUpdate":"tool_call_update","toolCallId":id,"status":"failed","content":[{"type":"content","content":{"type":"text","text":"permission denied"}}]}))
            .await;
        return None;
    }
    agent
        .update(
            sid,
            json!({"sessionUpdate":"tool_call_update","toolCallId":id,"status":"in_progress"}),
        )
        .await;
    let (ok, output) = run.await;
    agent
        .update(
            sid,
            json!({
                "sessionUpdate":"tool_call_update","toolCallId":id,
                "status": if ok {"completed"} else {"failed"},
                "content":[{"type":"content","content":{"type":"text","text":output}}],
                "rawOutput":{"output":output}
            }),
        )
        .await;
    Some((ok, output))
}

/// `_meta` the Claude ACP adapter puts on a tool report: the tool's name
/// and, inside a subagent, the subagent's `Agent` call.
fn claude_meta(tool_name: Option<&str>, parent: Option<&str>) -> Value {
    let mut cc = serde_json::Map::new();
    if let Some(n) = tool_name {
        cc.insert("toolName".into(), json!(n));
    }
    if let Some(p) = parent {
        cc.insert("parentToolUseId".into(), json!(p));
    }
    json!({"claudeCode": cc})
}

/// Two parallel subagents, the way the Claude ACP adapter reports them to a
/// client without the `subagents` capability: each starts as an `Agent`
/// tool call, and everything it does is stamped with that call's id.
async fn subagents(agent: &Agent, sid: &str) {
    let (a, b) = ("toolu_sub_a", "toolu_sub_b");
    let up = |v: Value| agent.update(sid, v);
    let chunk = |parent: &str, kind: &str, text: &str| {
        json!({"sessionUpdate": kind, "content": {"type":"text","text": text},
               "_meta": claude_meta(None, Some(parent))})
    };
    let call = |id: &str, parent: &str, cmd: &str| {
        json!({"sessionUpdate":"tool_call","toolCallId": id, "title": format!("`{cmd}`"),
               "kind":"execute","status":"pending","rawInput":{"command": cmd},
               "_meta": claude_meta(Some("Bash"), Some(parent))})
    };
    let done = |id: &str, parent: Option<&str>, ok: bool, out: &str| {
        let mut v = json!({"sessionUpdate":"tool_call_update","toolCallId": id,
               "status": if ok {"completed"} else {"failed"},
               "content":[{"type":"content","content":{"type":"text","text": out}}]});
        if parent.is_some() {
            v["_meta"] = claude_meta(Some("Bash"), parent);
        }
        v
    };
    let input_a = json!({"description":"Review the parser","subagent_type":"general-purpose","prompt":"Read src/parser.rs and report problems."});
    let input_b = json!({"description":"Review the lexer","subagent_type":"Explore","prompt":"Read src/lexer.rs and report problems."});

    say(agent, sid, "I'll split this between two subagents.").await;
    // A streams in: its input arrives in a later update, as with a streamed
    // tool_use. B arrives whole.
    up(json!({"sessionUpdate":"tool_call","toolCallId": a,"title":"Agent","kind":"think","status":"pending","rawInput":{},"_meta": claude_meta(Some("Agent"), None)})).await;
    up(json!({"sessionUpdate":"tool_call","toolCallId": b,"title":"Review the lexer","kind":"think","status":"pending","rawInput": input_b,"_meta": claude_meta(Some("Agent"), None)})).await;
    up(json!({"sessionUpdate":"tool_call_update","toolCallId": a,"title":"Review the parser","rawInput": input_a,"status":"in_progress","_meta": claude_meta(Some("Agent"), None)})).await;
    up(json!({"sessionUpdate":"tool_call_update","toolCallId": b,"status":"in_progress","_meta": claude_meta(Some("Agent"), None)})).await;

    up(chunk(a, "agent_message_chunk", "Looking at the ")).await;
    up(chunk(b, "agent_message_chunk", "Checking ")).await;
    up(chunk(a, "agent_message_chunk", "parser.")).await;
    up(chunk(b, "agent_thought_chunk", "the lexer is probably fine")).await;
    up(chunk(b, "agent_message_chunk", "the lexer.")).await;

    up(call("toolu_a1", a, "ls src")).await;
    up(call("toolu_b1", b, "false")).await;
    up(done("toolu_b1", Some(b), false, "exit 1")).await;
    // A later update that lost the parent stamp.
    up(done("toolu_a1", None, true, "lexer.rs\nparser.rs")).await;
    // The same failing call twice in each subagent: four in all, two each.
    up(call("toolu_b2", b, "false")).await;
    up(call("toolu_a2", a, "false")).await;
    up(done("toolu_a2", Some(a), false, "exit 1")).await;
    up(done("toolu_b2", Some(b), false, "exit 1")).await;
    up(call("toolu_a3", a, "false")).await;
    up(done("toolu_a3", Some(a), false, "exit 1")).await;

    up(chunk(b, "agent_message_chunk", "Lexer is fine.")).await;
    up(chunk(a, "agent_message_chunk", "Parser is fine.")).await;

    for (id, report) in [(b, "Report B: the lexer has no problems."), (a, "Report A: the parser has no problems.")] {
        up(json!({"sessionUpdate":"tool_call_update","toolCallId": id,"status":"completed",
                  "content":[{"type":"content","content":{"type":"text","text": report}}],
                  "_meta": claude_meta(Some("Agent"), None)})).await;
    }
    say(agent, sid, "Both subagents are done.").await;
}

async fn run_sh(cmd: &str, cwd: &str) -> (bool, String) {
    match tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .output()
        .await
    {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).to_string();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            (
                o.status.success(),
                format!("{}exit {}", s, o.status.code().unwrap_or(-1)),
            )
        }
        Err(e) => (false, format!("spawn failed: {e}")),
    }
}

async fn run_prompt(
    agent: &Arc<Agent>,
    sid: &str,
    text: &str,
    cwd: &str,
    mcp: &[Value],
    cancel: &AtomicBool,
) -> &'static str {
    let (cmd, rest) = text.split_once(' ').unwrap_or((text, ""));
    match cmd {
        "sh" => {
            let r = tool(
                agent,
                sid,
                "execute",
                &format!("sh: {rest}"),
                json!({"command": rest}),
                run_sh(rest, cwd),
            )
            .await;
            match r {
                Some((_, out)) => say(agent, sid, &format!("ran it:\n{out}")).await,
                None => say(agent, sid, "not allowed").await,
            }
        }
        "write" => {
            let (path, content) = rest.split_once(' ').unwrap_or((rest, ""));
            let full = std::path::Path::new(cwd).join(path);
            let content = content.to_string();
            let r = tool(
                agent,
                sid,
                "edit",
                &format!("write {path}"),
                json!({"path": path}),
                async move {
                    match std::fs::write(&full, content) {
                        Ok(()) => (true, format!("wrote {}", full.display())),
                        Err(e) => (false, format!("write {} failed: {e}", full.display())),
                    }
                },
            )
            .await;
            let msg = r.map(|(_, o)| o).unwrap_or_else(|| "not allowed".into());
            say(agent, sid, &msg).await;
        }
        "sleep" => {
            let secs: f64 = rest.trim().parse().unwrap_or(5.0);
            let steps = (secs * 10.0) as u64;
            for _ in 0..steps {
                if cancel.load(Ordering::SeqCst) {
                    return "cancelled";
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            say(agent, sid, &format!("slept {secs}s")).await;
        }
        "hang" => {
            // Ignores session/cancel, like a stuck agent.
            let secs: u64 = rest.trim().parse().unwrap_or(60);
            tokio::time::sleep(Duration::from_secs(secs)).await;
            say(agent, sid, "done hanging").await;
        }
        "fail-loop" => loop {
            if cancel.load(Ordering::SeqCst) {
                return "cancelled";
            }
            tool(
                agent,
                sid,
                "execute",
                "sh: mkdir /nonexistent/dir",
                json!({"command":"mkdir /nonexistent/dir"}),
                run_sh("mkdir /nonexistent/dir", cwd),
            )
            .await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        },
        "subagents" => subagents(agent, sid).await,
        "later" => {
            // Ends the turn now, then speaks again on its own, as an agent
            // does when a background task it started finishes.
            let (secs, msg) = rest.split_once(' ').unwrap_or((rest, "done later"));
            let ms = (secs.trim().parse::<f64>().unwrap_or(0.5) * 1000.0) as u64;
            let (a, s, msg) = (agent.clone(), sid.to_string(), msg.to_string());
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                say(&a, &s, &msg).await;
            });
            say(agent, sid, "started it").await;
        }
        "retitle" => {
            agent
                .update(sid, json!({"sessionUpdate": "session_info_update", "title": rest}))
                .await;
            say(agent, sid, "retitled").await;
        }
        "crash" => {
            say(agent, sid, "about to crash").await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            std::process::exit(3);
        }
        "mcp-list" | "mcp" => {
            let Some(server) = mcp.first() else {
                say(agent, sid, "no MCP server configured").await;
                return "end_turn";
            };
            let result = if cmd == "mcp-list" {
                mcp_call(server, "tools/list", json!({})).await
            } else {
                let (tool_name, args) = rest.split_once(' ').unwrap_or((rest, "{}"));
                let args: Value = serde_json::from_str(args).unwrap_or(json!({}));
                let title = format!(
                    "mcp__{}__{}",
                    server["name"].as_str().unwrap_or("mcp"),
                    tool_name
                );
                let server = server.clone();
                let tool_name = tool_name.to_string();
                let r = tool(
                    agent,
                    sid,
                    "fetch",
                    &title,
                    json!({"arguments": args.clone()}),
                    async move {
                        match mcp_call(
                            &server,
                            "tools/call",
                            json!({"name": tool_name, "arguments": args}),
                        )
                        .await
                        {
                            Ok(v) => (v.get("isError") != Some(&Value::Bool(true)), v.to_string()),
                            Err(e) => (false, e.to_string()),
                        }
                    },
                )
                .await;
                r.map(|(_, o)| Ok(Value::String(o)))
                    .unwrap_or_else(|| Err(anyhow!("not allowed")))
            };
            match result {
                Ok(v) => say(agent, sid, &format!("mcp result: {v}")).await,
                Err(e) => say(agent, sid, &format!("mcp error: {e}")).await,
            }
        }
        _ => {
            agent
                .update(sid, json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"thinking about it"}}))
                .await;
            say(agent, sid, &format!("echo: {text}")).await;
        }
    }
    if cancel.load(Ordering::SeqCst) {
        "cancelled"
    } else {
        "end_turn"
    }
}

/// Minimal MCP streamable-HTTP client: initialize, then one request.
async fn mcp_call(server: &Value, method: &str, params: Value) -> Result<Value> {
    let url = server["url"]
        .as_str()
        .ok_or_else(|| anyhow!("MCP server has no url"))?;
    let mut headers = reqwest::header::HeaderMap::new();
    for h in server["headers"].as_array().cloned().unwrap_or_default() {
        if let (Some(n), Some(v)) = (h["name"].as_str(), h["value"].as_str()) {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(n.as_bytes())?,
                reqwest::header::HeaderValue::from_str(v)?,
            );
        }
    }
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(700))
        .build()?;
    let post = |body: Value, session: Option<String>| {
        let mut req = client
            .post(url)
            .header("accept", "application/json, text/event-stream")
            .json(&body);
        if let Some(s) = session {
            req = req.header("mcp-session-id", s);
        }
        req.send()
    };
    let init = post(
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fake-agent","version":"0"}}}),
        None,
    )
    .await?;
    let session = init
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let _ = parse_mcp_body(init).await?;
    let _ = post(
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        session.clone(),
    )
    .await?;
    let resp = post(
        json!({"jsonrpc":"2.0","id":2,"method":method,"params":params}),
        session,
    )
    .await?;
    let v = parse_mcp_body(resp).await?;
    if let Some(e) = v.get("error") {
        return Err(anyhow!("{e}"));
    }
    Ok(v.get("result").cloned().unwrap_or(Value::Null))
}

async fn parse_mcp_body(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    let is_sse = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.starts_with("text/event-stream"))
        .unwrap_or(false);
    let body = resp.text().await?;
    if !status.is_success() {
        return Err(anyhow!("HTTP {status}: {body}"));
    }
    if is_sse {
        for line in body.lines() {
            if let Some(data) = line.strip_prefix("data:") {
                if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                    return Ok(v);
                }
            }
        }
        return Err(anyhow!("no data in event stream"));
    }
    if body.trim().is_empty() {
        return Ok(Value::Null);
    }
    Ok(serde_json::from_str(&body)?)
}
