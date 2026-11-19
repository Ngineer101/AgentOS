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
    /// `project.browse` with no path: the project folders.
    ProjectRoots,
    /// `screen.status`, `screen.set_policy` or `screen.stop`: answered with the status.
    Screen,
    /// `session.list` with `archived: true`.
    Archived,
    /// `session.side_ask` in this session: the reply names the side conversation.
    SideAsked(String),
    /// A call answered with the updated session (`session.set_config`).
    SessionUpdated,
    /// `connector.search` for this query.
    ConnectorSearch(String),
    /// `connector.update` renaming this connector: errors show by the name field.
    Rename(String),
    /// `session.rename` of this session: the reply is the session; errors are toasts and
    /// the header's field stays open.
    SessionRenamed(String),
    /// `builtin.list`: the host's own tools and whether each asks first.
    Builtins,
    /// `builtin.set_policy`, answered with the updated list. `announce` names the tool
    /// "Approve and Don't Ask Again" just set, for its toast.
    BuiltinSet { announce: Option<String> },
    /// Housekeeping whose answer doesn't matter.
    Quiet,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Refresh {
    None,
    Workspaces,
    Connectors,
    Devices,
    Approvals,
    HostInfo,
    ProjectRoots,
    Archived,
}

pub enum ToHost {
    Call { tag: Tag, method: &'static str, params: Value },
    /// Send a file from disk; answered by [`FromHost::Uploaded`].
    Upload { local_id: u64, path: PathBuf },
    /// Send bytes (a pasted image); answered by [`FromHost::Uploaded`].
    UploadBytes { local_id: u64, name: String, mime: String, data: Vec<u8> },
    /// Transcribe a WAV recording; answered by [`FromHost::Transcribed`].
    Transcribe { wav: Vec<u8> },
    /// Live dictation, sent in order; answered by [`FromHost::LiveText`].
    Live(Live),
}

/// One live dictation (`gen`) as it happens: audio is mono 16-bit PCM bytes.
pub enum Live {
    Begin { gen: u64, sample_rate: u32 },
    Audio { gen: u64, pcm: Vec<u8> },
    Finish { gen: u64, pcm: Vec<u8>, cancel: bool },
}

impl Live {
    fn gen(&self) -> u64 {
        match self {
            Live::Begin { gen, .. } | Live::Audio { gen, .. } | Live::Finish { gen, .. } => *gen,
        }
    }
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
    /// Everything heard so far in dictation `gen`; `done` with the final text.
    LiveText { gen: u64, done: bool, result: Result<String, String> },
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

/// Most audio merged into one `voice.stream_append` when pieces pile up.
const LIVE_MERGE_BYTES: usize = 2 * 1024 * 1024;

/// Live dictation for one connection: requests go one at a time, in order, so audio
/// can't overtake itself. Pieces that queue up while the host decodes are merged,
/// so a busy host gets fewer, larger pieces instead of falling behind.
fn live_dictation(client: Arc<Client>, out: std_mpsc::Sender<FromHost>, waker: rune::Waker) -> mpsc::UnboundedSender<Live> {
    use base64::Engine as _;
    let b64 = |pcm: &[u8]| base64::engine::general_purpose::STANDARD.encode(pcm);
    let (tx, mut rx) = mpsc::unbounded_channel::<Live>();
    tokio::spawn(async move {
        let send = |gen: u64, done: bool, result: Result<Value, RpcError>| {
            let result = result.map(|v| v["text"].as_str().unwrap_or_default().to_string()).map_err(|e| e.message);
            let _ = out.send(FromHost::LiveText { gen, done, result });
            waker();
        };
        // The open stream: (dictation, stream id). None after a failure, so the rest of
        // that dictation is dropped.
        let mut open: Option<(u64, String)> = None;
        let mut next: Option<Live> = None;
        loop {
            let cmd = match next.take() {
                Some(c) => c,
                None => match rx.recv().await {
                    Some(c) => c,
                    None => return,
                },
            };
            let gen = cmd.gen();
            let id = open.as_ref().filter(|(g, _)| *g == gen).map(|(_, id)| id.clone());
            match cmd {
                Live::Begin { gen, sample_rate } => match client.request(m::VOICE_STREAM_BEGIN, json!({"sample_rate": sample_rate})).await {
                    Ok(v) => open = v["stream_id"].as_str().map(|id| (gen, id.to_string())),
                    Err(e) => {
                        open = None;
                        send(gen, true, Err(e));
                    }
                },
                Live::Audio { mut pcm, .. } => {
                    while pcm.len() < LIVE_MERGE_BYTES {
                        match rx.try_recv() {
                            Ok(Live::Audio { gen: g, pcm: more }) if g == gen => pcm.extend_from_slice(&more),
                            Ok(other) => {
                                next = Some(other);
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    let Some(id) = id else { continue };
                    let r = client.request(m::VOICE_STREAM_APPEND, json!({"stream_id": id, "data": b64(&pcm)})).await;
                    if r.is_err() {
                        open = None;
                        send(gen, true, r);
                    } else {
                        send(gen, false, r);
                    }
                }
                Live::Finish { pcm, cancel, .. } => {
                    let Some(id) = id else { continue };
                    open = None;
                    let mut p = json!({"stream_id": id, "cancel": cancel});
                    if !pcm.is_empty() && !cancel {
                        p["data"] = json!(b64(&pcm));
                    }
                    let r = client.request(m::VOICE_STREAM_FINISH, p).await;
                    if !cancel {
                        send(gen, true, r);
                    }
                }
            }
        }
    });
    tx
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
                let mut started = false; // started a host, or saw one running
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
                    // A host is running (the service, or one we started): after a
                    // disconnect only reconnect, so a restart doesn't race a second host.
                    started = true;
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
                    let live_tx = live_dictation(client.clone(), out_tx.clone(), waker.clone());
                    loop {
                        tokio::select! {
                            cmd = cmd_rx.recv() => {
                                let Some(cmd) = cmd else { return };
                                let cmd = match cmd {
                                    ToHost::Live(l) => {
                                        let _ = live_tx.send(l);
                                        continue;
                                    }
                                    cmd => cmd,
                                };
                                let c = client.clone();
                                let s = out_tx.clone();
                                let w = waker.clone();
                                tokio::spawn(async move {
                                    let msg = match cmd {
                                        ToHost::Call { tag, method, params } => FromHost::Reply { tag, result: c.request(method, params).await },
                                        ToHost::Upload { local_id, path } => FromHost::Uploaded { local_id, result: c.upload_file(&path).await.map_err(|e| format!("{e:#}")) },
                                        ToHost::UploadBytes { local_id, name, mime, data } => FromHost::Uploaded { local_id, result: c.upload(&name, Some(&mime), &data, |_, _| {}).await.map_err(|e| format!("{e:#}")) },
                                        ToHost::Transcribe { wav } => FromHost::Transcribed(c.transcribe_wav(&wav, None).await.map_err(|e| format!("{e:#}"))),
                                        ToHost::Live(_) => unreachable!(),
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
