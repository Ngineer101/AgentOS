//! Async client for the AgentOS protocol over the host's Unix socket or,
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

    /// Start a session as one login of the agent: `profile_id` from
    /// `host.info`'s agent entries (`None` or the agent's name: the default login).
    pub async fn create_session_as(
        &self,
        workspace_id: &str,
        agent: AgentKind,
        profile_id: Option<&str>,
    ) -> Result<Session> {
        self.call(
            methods::SESSION_CREATE,
            json!({"workspace_id": workspace_id, "agent": agent, "profile_id": profile_id}),
        )
        .await
    }

    pub async fn add_profile(
        &self,
        agent: AgentKind,
        name: &str,
        config_dir: &str,
    ) -> Result<AgentProfile> {
        self.call(
            methods::AGENT_PROFILE_ADD,
            json!({"agent": agent, "name": name, "config_dir": config_dir}),
        )
        .await
    }

    pub async fn remove_profile(&self, profile_id: &str) -> Result<()> {
        self.request(methods::AGENT_PROFILE_REMOVE, json!({"profile_id": profile_id}))
            .await
            .map_err(|e| anyhow!(e))?;
        Ok(())
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

    /// Send a prompt with attachments (ids from [`Client::upload`]).
    pub async fn prompt_with_attachments(
        &self,
        session_id: &str,
        prompt_id: &str,
        text: &str,
        attachments: &[String],
    ) -> Result<SessionPromptResult> {
        self.call(
            methods::SESSION_PROMPT,
            json!({"session_id": session_id, "prompt_id": prompt_id, "text": text, "attachments": attachments}),
        )
        .await
    }

    /// Send bytes to the host in pieces; `progress(sent, total)` after each.
    pub async fn upload(
        &self,
        name: &str,
        mime: Option<&str>,
        data: &[u8],
        mut progress: impl FnMut(u64, u64),
    ) -> Result<Attachment> {
        use base64::Engine;
        let total = data.len() as u64;
        let b: UploadBeginResult = self
            .call(
                methods::UPLOAD_BEGIN,
                json!({"name": name, "size": total, "mime": mime}),
            )
            .await?;
        let chunk = b.chunk_bytes.clamp(1, UPLOAD_CHUNK_BYTES);
        let mut offset = 0usize;
        progress(0, total);
        for piece in data.chunks(chunk) {
            let _: UploadAppendResult = self
                .call(
                    methods::UPLOAD_APPEND,
                    json!({
                        "upload_id": b.upload_id,
                        "offset": offset,
                        "data": base64::engine::general_purpose::STANDARD.encode(piece),
                    }),
                )
                .await?;
            offset += piece.len();
            progress(offset as u64, total);
        }
        self.call(methods::UPLOAD_FINISH, json!({"upload_id": b.upload_id}))
            .await
    }

    /// Upload a file from disk under its own name.
    pub async fn upload_file(&self, path: &Path) -> Result<Attachment> {
        let meta = tokio::fs::metadata(path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        if !meta.is_file() {
            return Err(anyhow!("{} is not a file", path.display()));
        }
        if meta.len() > MAX_ATTACHMENT_BYTES {
            return Err(anyhow!(
                "{} is too large: files can be up to {} MB",
                path.display(),
                MAX_ATTACHMENT_BYTES / (1024 * 1024)
            ));
        }
        let data = tokio::fs::read(path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".into());
        self.upload(&name, None, &data, |_, _| {}).await
    }

    /// Upload a WAV recording and turn it into text.
    pub async fn transcribe_wav(&self, wav: &[u8], language: Option<&str>) -> Result<String> {
        let a = self.upload("recording.wav", Some("audio/wav"), wav, |_, _| {}).await?;
        let r: VoiceTranscribeResult = self
            .call(
                methods::VOICE_TRANSCRIBE,
                json!({"attachment_id": a.attachment_id, "language": language}),
            )
            .await?;
        Ok(r.text)
    }

    /// An image sent with one of a session's prompts, read back whole (protocol 7).
    pub async fn read_attachment(&self, session_id: &str, attachment_id: &str) -> Result<(Attachment, Vec<u8>)> {
        use base64::Engine;
        let mut data = vec![];
        loop {
            let r: AttachmentReadResult = self
                .call(
                    methods::ATTACHMENT_READ,
                    json!({"session_id": session_id, "attachment_id": attachment_id, "offset": data.len()}),
                )
                .await?;
            let piece = base64::engine::general_purpose::STANDARD
                .decode(r.data.as_bytes())
                .map_err(|_| anyhow!("the host sent bad base64"))?;
            if piece.is_empty() || data.len() + piece.len() > r.size as usize {
                return Err(anyhow!("the host stopped after {} of {} bytes", data.len(), r.size));
            }
            data.extend(piece);
            if data.len() as u64 >= r.size {
                let a = Attachment { attachment_id: attachment_id.into(), name: r.name, mime: r.mime, size: r.size };
                return Ok((a, data));
            }
        }
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
