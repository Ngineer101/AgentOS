//! ACP client: JSON-RPC 2.0 over an agent's stdio.
//!
//! The host is the ACP client; each agent (or its adapter) is an ACP server
//! started as a child process. This module only moves messages; the session
//! actor decides what they mean.

use agenthost_proto::jsonrpc::{Message, Notification, Request, Response, RpcError};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{mpsc, oneshot};

pub const ACP_PROTOCOL_VERSION: i64 = 1;

/// Something the agent sent that is not a response to us.
#[derive(Debug)]
pub enum Incoming {
    Notification(Notification),
    Request(Request),
    /// A response, delivered in order with notifications. The consumer must
    /// pass it to [`AcpConn::resolve`], so a result never overtakes the
    /// updates the agent sent before it.
    Response(Response),
    /// The agent's stdout closed; carries the exit description.
    Closed(String),
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>>;

/// The last lines the agent wrote to stderr, for failure reasons.
#[derive(Clone, Default)]
pub struct StderrTail(Arc<Mutex<VecDeque<String>>>);

impl StderrTail {
    pub fn push(&self, line: String) {
        let mut q = self.0.lock().unwrap();
        if q.len() >= 40 {
            q.pop_front();
        }
        q.push_back(line);
    }
    pub fn text(&self) -> String {
        self.0
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub struct AcpConn {
    stdin: tokio::sync::Mutex<ChildStdin>,
    next_id: AtomicI64,
    pending: Pending,
    pub stderr: StderrTail,
}

impl AcpConn {
    /// Take over a spawned child's stdio. Returns the connection and the
    /// stream of agent-initiated messages.
    pub fn attach(
        child: &mut Child,
        log_path: Option<std::path::PathBuf>,
    ) -> Result<(Arc<AcpConn>, mpsc::Receiver<Incoming>)> {
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("agent has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("agent has no stdout"))?;
        let stderr = child.stderr.take();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let tail = StderrTail::default();
        let (tx, rx) = mpsc::channel(1024);
        let trace_path = log_path.as_ref().map(|p| p.with_extension("acp.log"));

        if let Some(stderr) = stderr {
            let tail = tail.clone();
            tokio::spawn(async move {
                let mut log = match log_path {
                    Some(p) => tokio::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(p)
                        .await
                        .ok(),
                    None => None,
                };
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(f) = log.as_mut() {
                        let _ = f.write_all(format!("{line}\n").as_bytes()).await;
                    }
                    tail.push(line);
                }
            });
        }

        let pend = pending.clone();
        let trace = if std::env::var("AGENTHOST_ACP_TRACE").as_deref() == Ok("1") {
            trace_path.clone()
        } else {
            None
        };
        tokio::spawn(async move {
            let mut trace_file = match trace {
                Some(p) => tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                    .await
                    .ok(),
                None => None,
            };
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let text = line.trim();
                if text.is_empty() {
                    continue;
                }
                if let Some(f) = trace_file.as_mut() {
                    let _ = f.write_all(format!("<< {text}\n").as_bytes()).await;
                }
                match Message::parse(text) {
                    Ok(Message::Response(r)) => {
                        if tx.send(Incoming::Response(r)).await.is_err() {
                            break;
                        }
                    }
                    Ok(Message::Notification(n)) => {
                        if tx.send(Incoming::Notification(n)).await.is_err() {
                            break;
                        }
                    }
                    Ok(Message::Request(r)) => {
                        if tx.send(Incoming::Request(r)).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        // Agents sometimes print non-protocol lines; ignore them.
                        tracing::debug!(line = text, "non-JSON-RPC line from agent");
                    }
                }
            }
            // Outstanding requests fail when the connection is dropped; queued
            // responses ahead of this point are still delivered in order.
            drop(pend);
            let _ = tx
                .send(Incoming::Closed("agent closed its output".into()))
                .await;
        });

        Ok((
            Arc::new(AcpConn {
                stdin: tokio::sync::Mutex::new(stdin),
                next_id: AtomicI64::new(1),
                pending,
                stderr: tail,
            }),
            rx,
        ))
    }

    /// Complete the request a response belongs to.
    pub fn resolve(&self, r: Response) {
        if let Some(id) = r.id.as_i64() {
            if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
                let _ = tx.send(r.into_result());
            }
        }
    }

    async fn write(&self, msg: &Message) -> Result<()> {
        let mut line = msg.to_json();
        line.push('\n');
        let mut w = self.stdin.lock().await;
        w.write_all(line.as_bytes()).await?;
        w.flush().await?;
        Ok(())
    }

    /// Send a request; the returned receiver yields the result.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<oneshot::Receiver<Result<Value, RpcError>>> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if let Err(e) = self
            .write(&Message::Request(Request::new(id, method, params)))
            .await
        {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        Ok(rx)
    }

    /// Send a request and wait for its result.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let rx = self
            .request(method, params)
            .await
            .map_err(|e| RpcError::internal(e.to_string()))?;
        rx.await
            .unwrap_or_else(|_| Err(RpcError::internal("agent exited")))
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.write(&Message::Notification(Notification::new(method, params)))
            .await
    }

    pub async fn respond(&self, id: Value, result: Result<Value, RpcError>) -> Result<()> {
        let r = match result {
            Ok(v) => Response::ok(id, v),
            Err(e) => Response::err(id, e),
        };
        self.write(&Message::Response(r)).await
    }
}

/// Parameters for `initialize`. The host advertises no client-side file
/// system or terminal: agents use their own tools, inside the sandbox.
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": ACP_PROTOCOL_VERSION,
        "clientCapabilities": {
            "fs": {"readTextFile": false, "writeTextFile": false},
            "terminal": false
        },
        "clientInfo": {"name": "agenthost", "version": env!("CARGO_PKG_VERSION")}
    })
}

/// Pick the option that allows a tool call once (or always, if that is all there is).
pub fn auto_allow_outcome(params: &Value) -> Value {
    let options = params["options"].as_array().cloned().unwrap_or_default();
    let pick = |kind: &str| {
        options
            .iter()
            .find(|o| o["kind"] == kind)
            .and_then(|o| o["optionId"].as_str())
            .map(str::to_string)
    };
    match pick("allow_once").or_else(|| pick("allow_always")) {
        Some(id) => json!({"outcome": {"outcome": "selected", "optionId": id}}),
        None => json!({"outcome": {"outcome": "cancelled"}}),
    }
}

/// Text from an ACP content block or a list of tool-call content items.
pub fn content_text(v: &Value) -> String {
    match v {
        Value::Array(items) => items
            .iter()
            .map(content_text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(o) => {
            if let Some(t) = o.get("text").and_then(Value::as_str) {
                return t.to_string();
            }
            if let Some(c) = o.get("content") {
                return content_text(c);
            }
            if o.get("type").and_then(Value::as_str) == Some("diff") {
                let path = o.get("path").and_then(Value::as_str).unwrap_or("");
                return format!("diff {path}");
            }
            String::new()
        }
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

/// Whether an ACP error means the agent is not logged in.
pub fn is_auth_error(e: &RpcError) -> bool {
    let m = e.message.to_lowercase();
    e.code == -32000
        || m.contains("auth")
        || m.contains("not logged in")
        || m.contains("login")
        || m.contains("unauthorized")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_allow_once() {
        let p = json!({"options":[
            {"optionId":"a","kind":"allow_always"},
            {"optionId":"b","kind":"allow_once"},
            {"optionId":"c","kind":"reject_once"}]});
        assert_eq!(auto_allow_outcome(&p)["outcome"]["optionId"], "b");
        let p = json!({"options":[{"optionId":"c","kind":"reject_once"}]});
        assert_eq!(auto_allow_outcome(&p)["outcome"]["outcome"], "cancelled");
    }

    #[test]
    fn extracts_text() {
        let v = json!([{"type":"content","content":{"type":"text","text":"hi"}},{"type":"diff","path":"a.rs"}]);
        assert_eq!(content_text(&v), "hi\ndiff a.rs");
    }
}
