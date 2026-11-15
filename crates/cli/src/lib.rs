//! Async client for the agenthost protocol over the host's Unix socket or,
//! for a paired device, through the relay (see [`remote`]).

pub mod remote;

use agenthost_proto::jsonrpc::{Message, Notification, Request, RpcError};
use agenthost_proto::*;
use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>>;

/// A connected, greeted client.
pub struct Client {
    out: mpsc::Sender<String>,
    next_id: AtomicI64,
    pending: Pending,
    pub hello: HelloResult,
}

impl Client {
    /// Connect to the host's Unix socket and send `hello`.
    pub async fn connect(socket: &Path) -> Result<(Client, mpsc::Receiver<Notification>)> {
        let stream = UnixStream::connect(socket).await.with_context(|| {
            format!(
                "connect to host at {} (is agenthostd running?)",
                socket.display()
            )
        })?;
        let (r, mut w) = stream.into_split();
        let (out_tx, mut out_rx) = mpsc::channel::<String>(256);
        tokio::spawn(async move {
            while let Some(line) = out_rx.recv().await {
                if w.write_all(line.as_bytes()).await.is_err() || w.write_all(b"\n").await.is_err()
                {
                    break;
                }
            }
        });
        let (in_tx, in_rx) = mpsc::channel::<String>(4096);
        tokio::spawn(async move {
            let mut lines = BufReader::new(r).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if in_tx.send(line).await.is_err() {
                    break;
                }
            }
        });
        Self::over(out_tx, in_rx, "agenthost-cli").await
    }

    /// Run the protocol over any line transport (the relay tunnel uses this).
    pub async fn over(
        out: mpsc::Sender<String>,
        mut inbound: mpsc::Receiver<String>,
        client_name: &str,
    ) -> Result<(Client, mpsc::Receiver<Notification>)> {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (ntx, nrx) = mpsc::channel::<Notification>(4096);
        let pend = pending.clone();
        tokio::spawn(async move {
            while let Some(line) = inbound.recv().await {
                match Message::parse(&line) {
                    Ok(Message::Response(r)) => {
                        if let Some(id) = r.id.as_i64() {
                            if let Some(tx) = pend.lock().unwrap().remove(&id) {
                                let _ = tx.send(r.into_result());
                            }
                        }
                    }
                    Ok(Message::Notification(n)) => {
                        if ntx.send(n).await.is_err() {
                            // Nobody listens for notifications; keep serving responses.
                        }
                    }
                    _ => {}
                }
            }
            for (_, tx) in pend.lock().unwrap().drain() {
                let _ = tx.send(Err(RpcError::internal("connection closed")));
            }
        });
        let mut c = Client {
            out,
            next_id: AtomicI64::new(1),
            pending,
            hello: HelloResult {
                host_id: String::new(),
                host_version: String::new(),
                protocol_version: 0,
                capabilities: vec![],
                device_id: None,
            },
        };
        let hello: HelloResult = c
            .call(
                methods::HELLO,
                json!({"protocol_version": PROTOCOL_VERSION, "client_name": client_name, "client_version": env!("CARGO_PKG_VERSION")}),
            )
            .await?;
        c.hello = hello;
        Ok((c, nrx))
    }

    /// Send a raw request.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let msg = Message::Request(Request::new(id, method, params)).to_json();
        if self.out.send(msg).await.is_err() {
            self.pending.lock().unwrap().remove(&id);
            return Err(RpcError::internal("connection closed"));
        }
        rx.await
            .unwrap_or_else(|_| Err(RpcError::internal("connection closed")))
    }

    /// Send a request and decode the result.
    pub async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        let v = self.request(method, params).await.map_err(|e| anyhow!(e))?;
        serde_json::from_value(v).map_err(|e| anyhow!("bad response to {method}: {e}"))
    }

    pub async fn add_workspace(
        &self,
        path: &str,
        network: Option<NetworkProfile>,
    ) -> Result<Workspace> {
        self.call(
            methods::WORKSPACE_ADD,
            json!({"path": path, "network": network}),
        )
        .await
    }

    pub async fn create_session(&self, workspace_id: &str, agent: AgentKind) -> Result<Session> {
        self.call(
            methods::SESSION_CREATE,
            json!({"workspace_id": workspace_id, "agent": agent}),
        )
        .await
    }

    pub async fn get_session(&self, session_id: &str) -> Result<Session> {
        self.call(methods::SESSION_GET, json!({"session_id": session_id}))
            .await
    }

    pub async fn prompt(&self, session_id: &str, text: &str) -> Result<SessionPromptResult> {
        let prompt_id = uuid::Uuid::new_v4().to_string();
        self.prompt_with_id(session_id, &prompt_id, text).await
    }

    pub async fn prompt_with_id(
        &self,
        session_id: &str,
        prompt_id: &str,
        text: &str,
    ) -> Result<SessionPromptResult> {
        self.call(
            methods::SESSION_PROMPT,
            json!({"session_id": session_id, "prompt_id": prompt_id, "text": text}),
        )
        .await
    }

    pub async fn subscribe(
        &self,
        session_id: &str,
        after_seq: i64,
    ) -> Result<EventsSubscribeResult> {
        self.call(
            methods::EVENTS_SUBSCRIBE,
            json!({"session_id": session_id, "after_seq": after_seq}),
        )
        .await
    }
}

/// Parse an `event` notification.
pub fn as_event(n: &Notification) -> Option<Event> {
    (n.method == methods::N_EVENT)
        .then(|| serde_json::from_value(n.params.clone()).ok())
        .flatten()
}
