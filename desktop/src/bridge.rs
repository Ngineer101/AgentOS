//! The link between the UI thread and the host: a background thread with a
//! tokio runtime owns the protocol client. The UI sends [`ToHost`] commands;
//! replies and notifications come back as [`FromHost`] and wake the window.

use agenthost_cli::Client;
use agenthost_proto::jsonrpc::{Notification, RpcError};
use agenthost_proto::methods as m;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// What a reply is for, so the UI knows where it goes.
#[derive(Debug, Clone, PartialEq)]
pub enum Tag {
    HostInfo,
    Workspaces,
    Sessions,
    Connectors,
    Devices,
    Approvals,
    PairStart,
    SessionCreated,
    /// `connector.add`: opens sign-in straight away if the server needs it.
    ConnectorAdded,
    /// `connector.sign_in` for this connector: open the returned URL.
    SignIn(String),
    /// A user action whose failure should be shown; on success, refresh this list.
    Action { what: String, refresh: Refresh },
    Subscribed(String),
    /// `voice.status` or `voice.download`.
    Voice,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Refresh {
    None,
    Workspaces,
    Connectors,
    Devices,
    Approvals,
    HostInfo,
}

pub enum ToHost {
    Call { tag: Tag, method: &'static str, params: Value },
    /// Send a file from disk; answered by [`FromHost::Uploaded`].
    Upload { local_id: u64, path: PathBuf },
    /// Transcribe a WAV recording; answered by [`FromHost::Transcribed`].
    Transcribe { wav: Vec<u8> },
}

pub enum FromHost {
    Connected { host_id: String },
    Disconnected(String),
    /// The host was not running and the app started it.
    StartedHost,
    Reply { tag: Tag, result: Result<Value, RpcError> },
    Note(Notification),
    Uploaded { local_id: u64, result: Result<agenthost_proto::Attachment, String> },
    Transcribed(Result<String, String>),
}

pub struct Bridge {
    tx: mpsc::UnboundedSender<ToHost>,
    pub rx: std_mpsc::Receiver<FromHost>,
}

impl Bridge {
    pub fn call(&self, tag: Tag, method: &'static str, params: Value) {
        let _ = self.tx.send(ToHost::Call { tag, method, params });
    }
    pub fn send(&self, cmd: ToHost) {
        let _ = self.tx.send(cmd);
    }
}

pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("AGENTHOST_SOCKET") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    if cfg!(target_os = "macos") {
        return PathBuf::from(format!("{home}/Library/Application Support/agenthost/host.sock"));
    }
    if let Ok(x) = std::env::var("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(x).join("agenthost");
        if p.as_os_str().len() < 40 {
            return p.join("host.sock");
        }
    }
    let uid = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("Uid:")).and_then(|l| l.split_whitespace().nth(1).map(str::to_string)))
        .unwrap_or_else(|| "0".into());
    PathBuf::from(format!("/tmp/agenthost-{uid}/host.sock"))
}

/// `agenthostd` next to this executable, else on PATH.
fn daemon_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            let p = d.join("agenthostd");
            if p.is_file() {
                return p;
            }
        }
    }
    PathBuf::from("agenthostd")
}

fn start_daemon() -> bool {
    std::process::Command::new(daemon_path())
        .arg("run")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

pub fn start(waker: rune::Waker) -> Bridge {
    let (tx, mut cmd_rx) = mpsc::unbounded_channel::<ToHost>();
    let (out_tx, rx) = std_mpsc::channel::<FromHost>();
    std::thread::Builder::new()
        .name("agenthost-bridge".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio runtime");
            rt.block_on(async move {
                let send = |m: FromHost| {
                    let _ = out_tx.send(m);
                    waker();
                };
                let socket = socket_path();
                let mut started = false;
                loop {
                    let (client, mut notes) = match Client::connect(&socket).await {
                        Ok(c) => c,
                        Err(e) => {
                            // First launch with no host running: start it.
                            if !started && !socket.exists() && start_daemon() {
                                started = true;
                                send(FromHost::StartedHost);
                            } else {
                                send(FromHost::Disconnected(format!("{e:#}")));
                            }
                            tokio::time::sleep(Duration::from_millis(700)).await;
                            continue;
                        }
                    };
                    let client = Arc::new(client);
                    send(FromHost::Connected { host_id: client.hello.host_id.clone() });
                    // Watch the session list for changes.
                    let c2 = client.clone();
                    let s2 = out_tx.clone();
                    let w2 = waker.clone();
                    tokio::spawn(async move {
                        let r = c2.request(m::SESSION_LIST, json!({"watch": true})).await;
                        let _ = s2.send(FromHost::Reply { tag: Tag::Sessions, result: r });
                        w2();
                    });
                    loop {
                        tokio::select! {
                            cmd = cmd_rx.recv() => {
                                let Some(cmd) = cmd else { return };
                                let c = client.clone();
                                let s = out_tx.clone();
                                let w = waker.clone();
                                tokio::spawn(async move {
                                    let msg = match cmd {
                                        ToHost::Call { tag, method, params } => FromHost::Reply { tag, result: c.request(method, params).await },
                                        ToHost::Upload { local_id, path } => FromHost::Uploaded { local_id, result: c.upload_file(&path).await.map_err(|e| format!("{e:#}")) },
                                        ToHost::Transcribe { wav } => FromHost::Transcribed(c.transcribe_wav(&wav, None).await.map_err(|e| format!("{e:#}"))),
                                    };
                                    let _ = s.send(msg);
                                    w();
                                });
                            }
                            n = notes.recv() => {
                                match n {
                                    Some(n) => send(FromHost::Note(n)),
                                    None => break,
                                }
                            }
                        }
                    }
                    send(FromHost::Disconnected("the host closed the connection".into()));
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            });
        })
        .expect("bridge thread");
    Bridge { tx, rx }
}
