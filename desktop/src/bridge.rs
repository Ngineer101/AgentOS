//! The link between the UI thread and the host: a background thread with a
//! tokio runtime owns the protocol client. The UI sends [`ToHost`] commands;
//! replies and notifications come back as [`FromHost`] and wake the window.

use agenthost_cli::remote::{self, Closed, DeviceState};
use agenthost_cli::Client;
use agenthost_proto::jsonrpc::{Notification, RpcError};
use agenthost_proto::methods::{self as m, Capability};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
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
    /// `builtin.set_policy` for one of `run_command`'s prefixes ("gh pr edit"). `announce`
    /// names the prefix when a toast should say it now runs without asking.
    CommandSet { announce: Option<Vec<String>> },
    /// `session.prompt` of this prompt in this session: the pending bubble goes when the
    /// host records it, or back into the composer if it refuses.
    Prompt { session: String, prompt_id: String },
    /// `automation.list`.
    Automations,
    /// `automation.create` or `automation.update` from the editor: errors show in it.
    AutomationSaved,
    /// A quick `automation.update` (on/off, a new webhook URL): answered with the automation.
    AutomationUpdated { what: String },
    /// `automation.runs` of this automation.
    AutomationRuns(String),
    /// `automation.run_now` of this automation: answered with the run.
    AutomationRan(String),
    /// `automation.check_schedule` of this cron expression.
    ScheduleCheck(String),
    /// `note.list`.
    Notes,
    /// `note.create` (of the new note begun `new_gen`-th) or a text `note.update` from the
    /// Notes editor: answered with the note.
    NoteSaved { new_gen: Option<u64> },
    /// A quick `note.update` (Done, Reopen) or `note.delete`: errors show; the list reloads.
    NoteAction { what: String },
    /// `session.create` from a note: the new session's composer gets the note's text.
    SessionFromNote(String),
    /// `terminal.list`.
    Terminals,
    /// `session.changes` of this session.
    Changes(String),
    /// `session.files` of this session.
    Files(String),
    /// `session.file_read` of this path in this session.
    FileRead { session: String, path: String },
    /// `terminal.open`: answered with the new terminal.
    TerminalOpened,
    /// `terminal.attach` to this terminal: its output follows as notifications.
    TerminalAttached(String),
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
    Automations,
    Notes,
}

pub enum ToHost {
    Call { tag: Tag, method: &'static str, params: Value },
    /// Send a file from disk; answered by [`FromHost::Uploaded`].
    Upload { local_id: u64, path: PathBuf },
    /// Send bytes (a pasted image); answered by [`FromHost::Uploaded`].
    UploadBytes { local_id: u64, name: String, mime: String, data: Vec<u8> },
    /// Fetch an image sent with a prompt; answered by [`FromHost::AttachmentRead`].
    ReadAttachment { session_id: String, attachment_id: String },
    /// Transcribe a WAV recording; answered by [`FromHost::Transcribed`].
    Transcribe { wav: Vec<u8> },
    /// Live dictation, sent in order; answered by [`FromHost::LiveText`].
    Live(Live),
    /// A terminal's keystrokes, size and acknowledgements, sent in order.
    Term(TermCmd),
    /// Pair with a remote host using the QR payload from `agenthost device
    /// pair` on it; answered by [`FromHost::PairCode`] then
    /// [`FromHost::PairResult`], both carrying `attempt` back so answers to
    /// an attempt the form has moved on from are told apart. Ends any
    /// earlier attempt.
    PairRemote { attempt: u64, payload: String, name: String },
    /// Give up the pairing under way (the form was closed).
    CancelPair,
    /// Connect to the paired remote host in this state file (created by
    /// pairing) instead of the local socket.
    UseRemote(PathBuf),
    /// Connect back to the local host's socket.
    UseLocal,
}

/// Traffic for one of the host's terminals. Sent one after another, never side by side:
/// keys must arrive in the order they were typed.
#[derive(Debug, Clone, PartialEq)]
pub enum TermCmd {
    Input { id: String, data: Vec<u8> },
    Resize { id: String, cols: u16, rows: u16 },
    /// Output up to `offset` is on screen: the host may send more.
    Ack { id: String, offset: u64 },
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
    Connected {
        /// Whether this connection may manage the host (the local socket can,
        /// a paired device cannot: it gets sessions and approvals, not policy).
        admin: bool,
        /// The paired host's name when connected through the relay.
        remote: Option<String>,
    },
    Disconnected(String),
    /// The host was not running and the app started it.
    StartedHost,
    /// A paired remote host exists to connect to (its name), or none does.
    /// Sent once when the bridge starts.
    RemotePaired(Option<String>),
    /// Which host the bridge is trying to reach from now on: the paired one
    /// through the relay (`remote`), or this computer's. Sent when the bridge
    /// starts and at every switch (`switched`), before the first connect
    /// there, so the UI knows where it stands even while that host is down.
    Link { remote: bool, switched: bool },
    /// The 6-digit code to compare with the one shown on the host, while
    /// pairing with it.
    PairCode { attempt: u64, code: String },
    /// Pairing finished: the new device's keys (not saved anywhere yet), or
    /// why it failed.
    PairResult { attempt: u64, result: Result<DeviceState, String> },
    Reply { tag: Tag, result: Result<Value, RpcError> },
    Note(Notification),
    Uploaded { local_id: u64, result: Result<agenthost_proto::Attachment, String> },
    AttachmentRead { attachment_id: String, result: Result<Vec<u8>, String> },
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

// ---------------------------------------------------------------------------
// Remote access: the paired-host state and this app's own preferences
// ---------------------------------------------------------------------------

/// How the app reaches the host: this computer's socket, or a paired remote
/// host through the relay.
enum Mode {
    Local,
    Remote(PathBuf),
}

/// The paired remote host's keys: `$AGENTHOST_REMOTE`, or `remote.json` in the
/// app's config folder ([`crate::prefs::config_dir`]). The same file
/// `agenthost remote pair --state` writes, so the CLI and this app can share
/// one paired host.
pub fn remote_state_path() -> PathBuf {
    if let Some(p) = std::env::var_os("AGENTHOST_REMOTE") {
        return PathBuf::from(p);
    }
    crate::prefs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("remote.json")
}

/// The tunnel task reports why it closed on a watch channel; a local
/// connection has none. Waits for the report; `None` when there is no tunnel
/// (local). A tunnel whose sender is gone without a report never answers
/// here: the notifications stream ends too, and that ends the link.
async fn next_closed(closed: Option<&mut tokio::sync::watch::Receiver<Option<Closed>>>) -> Option<Closed> {
    match closed {
        Some(c) => match c.changed().await {
            Ok(()) => c.borrow().clone(),
            Err(_) => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

/// Whether a close is one no retry fixes: the device was removed (4401) or
/// is unknown to the relay (4403).
fn is_terminal(c: &Closed) -> bool {
    c.code == 4401 || c.code == 4403
}

/// Sends one link's replies to the UI. After a switch to another host,
/// replies still in flight from the old one are dropped rather than mistaken
/// for the new host's.
#[derive(Clone)]
struct ReplyTo {
    out: std_mpsc::Sender<FromHost>,
    waker: rune::Waker,
    epoch: Arc<Mutex<u64>>,
    mine: u64,
}

impl ReplyTo {
    fn send(&self, m: FromHost) {
        let now = self.epoch.lock().unwrap_or_else(|e| e.into_inner());
        if *now != self.mine {
            return;
        }
        let _ = self.out.send(m);
        drop(now);
        (self.waker)();
    }
}

/// What a command turned out to be.
enum Sorted {
    /// Connect somewhere else.
    Switch(Mode),
    /// Dealt with here: it needs no host connection.
    Handled,
    /// For the connected host.
    ForHost(ToHost),
}

/// How a wait that kept listening for commands ended.
enum Idle<T> {
    Done(T),
    /// Connect somewhere else instead.
    Switch(Mode),
    /// The app is gone.
    Quit,
}

/// The command queue, read whether or not a host is connected: pairing and
/// switching hosts are what a disconnected app needs most.
struct Cmds {
    rx: mpsc::UnboundedReceiver<ToHost>,
    /// Commands for the host that arrived while none was connected; they run
    /// once one answers.
    backlog: VecDeque<ToHost>,
    /// The pairing under way, so a new attempt or Cancel ends it.
    pairing: Option<tokio::task::AbortHandle>,
    out: std_mpsc::Sender<FromHost>,
    waker: rune::Waker,
    /// Counts host switches; see [`ReplyTo`].
    epoch: Arc<Mutex<u64>>,
}

impl Cmds {
    fn sort(&mut self, cmd: ToHost) -> Sorted {
        match cmd {
            ToHost::UseRemote(path) => Sorted::Switch(Mode::Remote(path)),
            ToHost::UseLocal => Sorted::Switch(Mode::Local),
            ToHost::PairRemote { attempt, payload, name } => {
                self.pair(attempt, payload, name);
                Sorted::Handled
            }
            ToHost::CancelPair => {
                if let Some(p) = self.pairing.take() {
                    p.abort();
                }
                Sorted::Handled
            }
            cmd => Sorted::ForHost(cmd),
        }
    }

    /// Pairing runs beside any connection: the host being paired is another
    /// machine, reached through its relay.
    fn pair(&mut self, attempt: u64, payload: String, name: String) {
        if let Some(p) = self.pairing.take() {
            p.abort();
        }
        let (s2, w2) = (self.out.clone(), self.waker.clone());
        let (s3, w3) = (self.out.clone(), self.waker.clone());
        let task = tokio::spawn(async move {
            let r = remote::pair(&payload, &name, move |code| {
                let _ = s2.send(FromHost::PairCode { attempt, code: code.to_string() });
                w2();
            })
            .await;
            let _ = s3.send(FromHost::PairResult { attempt, result: r.map_err(|e| format!("{e:#}")) });
            w3();
        });
        self.pairing = Some(task.abort_handle());
    }

    /// Wait for `fut` (a connect, or the pause before the next try) while
    /// still pairing and switching hosts when asked.
    async fn during<T>(&mut self, fut: impl std::future::Future<Output = T>) -> Idle<T> {
        tokio::pin!(fut);
        loop {
            tokio::select! {
                r = &mut fut => return Idle::Done(r),
                cmd = self.rx.recv() => {
                    let Some(cmd) = cmd else { return Idle::Quit };
                    match self.sort(cmd) {
                        Sorted::Switch(m) => return Idle::Switch(m),
                        Sorted::Handled => {}
                        Sorted::ForHost(cmd) => self.backlog.push_back(cmd),
                    }
                }
            }
        }
    }

    /// From here on the app talks to another host: what was queued or is
    /// still in flight for the old one is dropped, and the UI is told.
    fn switch(&mut self, to: &Mode) {
        self.backlog.clear();
        let mut epoch = self.epoch.lock().unwrap_or_else(|e| e.into_inner());
        *epoch += 1;
        let _ = self.out.send(FromHost::Link { remote: matches!(to, Mode::Remote(_)), switched: true });
        drop(epoch);
        (self.waker)();
    }

    fn reply_to(&self) -> ReplyTo {
        let mine = *self.epoch.lock().unwrap_or_else(|e| e.into_inner());
        ReplyTo { out: self.out.clone(), waker: self.waker.clone(), epoch: self.epoch.clone(), mine }
    }
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

/// The host runs as a service (scripts/install.sh): the service manager
/// starts and restarts it. The app starting one too races it at login, and
/// the app's copy runs without the service's settings (its relay).
fn host_service_installed() -> bool {
    let Ok(home) = std::env::var("HOME") else { return false };
    let unit = if cfg!(target_os = "macos") {
        PathBuf::from(&home).join("Library/LaunchAgents/dev.agenthost.host.plist")
    } else {
        std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(&home).join(".config"))
            .join("systemd/user/agenthost.service")
    };
    unit.is_file()
}

fn start_daemon() -> bool {
    if host_service_installed() {
        return false;
    }
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

/// Terminal traffic in order: each request waits for the one before. Keys typed while
/// one is on its way go together in the next, and only the newest size is sent.
fn terminal_io(client: Arc<Client>) -> mpsc::UnboundedSender<TermCmd> {
    use agenthost_proto::TERMINAL_INPUT_BYTES;
    use base64::Engine as _;
    let (tx, mut rx) = mpsc::unbounded_channel::<TermCmd>();
    tokio::spawn(async move {
        let mut next: Option<TermCmd> = None;
        loop {
            let cmd = match next.take() {
                Some(c) => c,
                None => match rx.recv().await {
                    Some(c) => c,
                    None => return,
                },
            };
            // Failures are the terminal having gone; its `terminal.changed` says so.
            match cmd {
                TermCmd::Input { id, mut data } => {
                    while data.len() < TERMINAL_INPUT_BYTES {
                        match rx.try_recv() {
                            Ok(TermCmd::Input { id: i, data: more }) if i == id => data.extend_from_slice(&more),
                            Ok(other) => {
                                next = Some(other);
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    for piece in data.chunks(TERMINAL_INPUT_BYTES) {
                        let data = base64::engine::general_purpose::STANDARD.encode(piece);
                        let _ = client.request(m::TERMINAL_INPUT, json!({"terminal_id": id, "data": data})).await;
                    }
                }
                TermCmd::Resize { id, mut cols, mut rows } => {
                    loop {
                        match rx.try_recv() {
                            Ok(TermCmd::Resize { id: i, cols: c, rows: r }) if i == id => (cols, rows) = (c, r),
                            Ok(other) => {
                                next = Some(other);
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    let _ = client.request(m::TERMINAL_RESIZE, json!({"terminal_id": id, "cols": cols, "rows": rows})).await;
                }
                TermCmd::Ack { id, offset } => {
                    let _ = client.request(m::TERMINAL_ACK, json!({"terminal_id": id, "offset": offset})).await;
                }
            }
        }
    });
    tx
}

pub fn start(waker: rune::Waker) -> Bridge {
    let (tx, cmd_rx) = mpsc::unbounded_channel::<ToHost>();
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
                // Where to connect: the state file $AGENTHOST_REMOTE names, the
                // host paired earlier and made the default, or this computer's
                // own host.
                let mut mode = if let Some(p) = std::env::var_os("AGENTHOST_REMOTE") {
                    Mode::Remote(PathBuf::from(p))
                } else if crate::prefs::load().prefer_remote && remote_state_path().is_file() {
                    Mode::Remote(remote_state_path())
                } else {
                    Mode::Local
                };
                send(FromHost::Link { remote: matches!(mode, Mode::Remote(_)), switched: false });
                // Whether a paired host exists at all, whatever we connect to first.
                send(FromHost::RemotePaired(
                    DeviceState::load(&remote_state_path()).ok().map(|s| s.host_name),
                ));
                let mut cmds = Cmds {
                    rx: cmd_rx,
                    backlog: VecDeque::new(),
                    pairing: None,
                    out: out_tx.clone(),
                    waker: waker.clone(),
                    epoch: Arc::new(Mutex::new(0)),
                };
                let socket = socket_path();
                let mut started = false; // started a host, or saw one running
                loop {
                    // One link, local or through the relay, until it ends or a
                    // mode switch arrives. A try that fails says how long to
                    // wait before the next, or where to connect instead.
                    let tried = match &mode {
                        Mode::Local => match cmds.during(Client::connect(&socket)).await {
                            Idle::Done(Ok((c, n))) => {
                                // A host is running (the service, or one we started): after a
                                // disconnect only reconnect, so a restart doesn't race a second host.
                                started = true;
                                Ok((c, n, None, None))
                            }
                            Idle::Done(Err(e)) => {
                                // First launch with no host running: start it.
                                if !started && !socket.exists() && start_daemon() {
                                    started = true;
                                    send(FromHost::StartedHost);
                                } else {
                                    send(FromHost::Disconnected(format!("{e:#}")));
                                }
                                Err(Idle::Done(Duration::from_millis(700)))
                            }
                            Idle::Switch(m) => Err(Idle::Switch(m)),
                            Idle::Quit => return,
                        },
                        Mode::Remote(path) => match DeviceState::load(path) {
                            Err(e) => {
                                send(FromHost::Disconnected(format!("no paired host in {}: {e}", path.display())));
                                Err(Idle::Done(Duration::from_millis(700)))
                            }
                            Ok(st) => match cmds.during(remote::connect(&st)).await {
                                Idle::Done(Ok((c, n, closed))) => Ok((c, n, Some(closed), Some(st.host_name.clone()))),
                                Idle::Done(Err(e)) => {
                                    // A removed or unknown device stays that way; anything
                                    // else (the relay unreachable, the host asleep) may come
                                    // back, so keep trying — slowly for those two.
                                    let terminal = e.downcast_ref::<Closed>().is_some_and(is_terminal);
                                    send(FromHost::Disconnected(format!("{e:#}")));
                                    Err(Idle::Done(Duration::from_secs(if terminal { 30 } else { 2 })))
                                }
                                Idle::Switch(m) => Err(Idle::Switch(m)),
                                Idle::Quit => return,
                            },
                        },
                    };
                    let (client, mut notes, mut closed, remote_name) = match tried {
                        Ok(link) => link,
                        Err(next) => {
                            // Pairing and switching hosts still work through the wait:
                            // a laptop with no host, or a paired host that is asleep
                            // or has removed this device, needs exactly those.
                            let next = match next {
                                Idle::Done(pause) => cmds.during(tokio::time::sleep(pause)).await,
                                Idle::Switch(m) => Idle::Switch(m),
                                Idle::Quit => return,
                            };
                            match next {
                                Idle::Done(()) => {}
                                Idle::Switch(m) => {
                                    cmds.switch(&m);
                                    if matches!(m, Mode::Local) {
                                        started = false; // switching back may start one
                                    }
                                    mode = m;
                                }
                                Idle::Quit => return,
                            }
                            continue;
                        }
                    };
                    let client = Arc::new(client);
                    let admin = client.hello.capabilities.contains(&Capability::Admin);
                    send(FromHost::Connected { admin, remote: remote_name.clone() });
                    let reply = cmds.reply_to();
                    // Watch the session list for changes.
                    let c2 = client.clone();
                    let r2 = reply.clone();
                    tokio::spawn(async move {
                        let r = c2.request(m::SESSION_LIST, json!({"watch": true})).await;
                        r2.send(FromHost::Reply { tag: Tag::Sessions, result: r });
                    });
                    let live_tx = live_dictation(client.clone(), out_tx.clone(), waker.clone());
                    let term_tx = terminal_io(client.clone());
                    // A command for this host: live dictation and terminal traffic
                    // go in order, the rest each run on their own.
                    let run = |cmd: ToHost| {
                        let cmd = match cmd {
                            ToHost::Live(l) => {
                                let _ = live_tx.send(l);
                                return;
                            }
                            ToHost::Term(t) => {
                                let _ = term_tx.send(t);
                                return;
                            }
                            cmd => cmd,
                        };
                        let c = client.clone();
                        let reply = reply.clone();
                        tokio::spawn(async move {
                            let msg = match cmd {
                                ToHost::Call { tag, method, params } => FromHost::Reply { tag, result: c.request(method, params).await },
                                ToHost::Upload { local_id, path } => FromHost::Uploaded { local_id, result: c.upload_file(&path).await.map_err(|e| format!("{e:#}")) },
                                ToHost::UploadBytes { local_id, name, mime, data } => FromHost::Uploaded { local_id, result: c.upload(&name, Some(&mime), &data, |_, _| {}).await.map_err(|e| format!("{e:#}")) },
                                ToHost::ReadAttachment { session_id, attachment_id } => {
                                    let result = c.read_attachment(&session_id, &attachment_id).await.map(|(_, data)| data).map_err(|e| format!("{e:#}"));
                                    FromHost::AttachmentRead { attachment_id, result }
                                }
                                ToHost::Transcribe { wav } => FromHost::Transcribed(c.transcribe_wav(&wav, None).await.map_err(|e| format!("{e:#}"))),
                                ToHost::Live(_) | ToHost::Term(_) | ToHost::UseRemote(_) | ToHost::UseLocal | ToHost::PairRemote { .. } | ToHost::CancelPair => unreachable!(),
                            };
                            reply.send(msg);
                        });
                    };
                    // What was asked while no host answered runs first.
                    while let Some(cmd) = cmds.backlog.pop_front() {
                        run(cmd);
                    }
                    let mut next_mode: Option<Mode> = None;
                    let mut why: Option<Closed> = None;
                    loop {
                        tokio::select! {
                            cmd = cmds.rx.recv() => {
                                let Some(cmd) = cmd else { return };
                                match cmds.sort(cmd) {
                                    Sorted::Switch(m) => {
                                        next_mode = Some(m);
                                        break;
                                    }
                                    Sorted::Handled => {}
                                    Sorted::ForHost(cmd) => run(cmd),
                                }
                            }
                            n = notes.recv() => {
                                match n {
                                    Some(n) => send(FromHost::Note(n)),
                                    // The tunnel records why just before the stream
                                    // ends; the watch arm or the code below has it.
                                    None => break,
                                }
                            }
                            c = next_closed(closed.as_mut()) => {
                                if c.is_some() {
                                    why = c;
                                    break;
                                }
                            }
                        }
                    }
                    let next_mode = match next_mode {
                        // A deliberate switch: reconnect at once, no "disconnected".
                        Some(m) => Some(m),
                        None => {
                            // If the notifications ended first, the tunnel may have
                            // recorded its reason after all.
                            let why = why.or_else(|| closed.as_ref().and_then(|c| c.borrow().clone()));
                            let terminal = why.as_ref().is_some_and(is_terminal);
                            send(FromHost::Disconnected(why.map(|c| c.to_string()).unwrap_or_else(|| "the host closed the connection".into())));
                            let pause = if terminal {
                                Duration::from_secs(30)
                            } else if remote_name.is_some() {
                                Duration::from_secs(1)
                            } else {
                                Duration::from_millis(500)
                            };
                            match cmds.during(tokio::time::sleep(pause)).await {
                                Idle::Done(()) => None,
                                Idle::Switch(m) => Some(m),
                                Idle::Quit => return,
                            }
                        }
                    };
                    if let Some(m) = next_mode {
                        cmds.switch(&m);
                        if matches!(m, Mode::Local) {
                            started = false; // switching back may start one
                        }
                        mode = m;
                    }
                }
            });
        })
        .expect("bridge thread");
    Bridge { tx, rx }
}
