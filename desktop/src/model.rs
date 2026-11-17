//! App state and how host messages change it.

use crate::bridge::{Bridge, FromHost, Refresh, Tag};
use agenthost_proto::methods as m;
use agenthost_proto::*;
use rune::TextEdit;
use rune_kit::shell::PaneState;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[derive(Clone, PartialEq, Debug)]
pub enum Screen {
    Session(String),
    NewSession,
    Workspaces,
    Connectors,
    Devices,
    Host,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Conn {
    Connecting,
    StartingHost,
    Connected,
    Down(String),
}

#[derive(Default)]
pub struct Transcript {
    pub events: Vec<Event>,
    pub last_seq: i64,
    /// Text streaming in (`session.delta`) that is not yet an event.
    pub live: String,
}

#[derive(Default, Clone)]
pub struct Pairing {
    pub pairing_id: String,
    pub qr: String,
    pub expires_at: i64,
    pub pending: Option<PairingPending>,
    pub done: Option<String>,
}

pub struct App {
    pub bridge: Option<Bridge>,
    pub conn: Conn,
    pub screen: Screen,
    pub sidebar: PaneState,
    pub info: Option<HostInfo>,
    pub workspaces: Vec<Workspace>,
    pub sessions: Vec<Session>,
    pub connectors: Vec<Connector>,
    pub devices: Vec<Device>,
    pub approvals: Vec<PendingApproval>,
    pub transcripts: HashMap<String, Transcript>,
    /// Sessions subscribed on the current connection.
    pub subscribed: HashSet<String>,
    pub expanded: HashSet<String>,
    pub prompt: TextEdit,
    pub ws_path: TextEdit,
    pub custom_host: TextEdit,
    pub host_for: Option<String>,
    pub conn_url: TextEdit,
    /// Connectors whose browser sign-in is under way, with the sign-in link
    /// once the host has sent it.
    pub signing_in: HashMap<String, String>,
    /// Links to open in the browser on the next poll (handlers and replies
    /// have no `Tx`; `poll` does).
    pub open_queue: Vec<String>,
    pub rename: TextEdit,
    pub renaming: Option<String>,
    pub new_ws: Option<String>,
    pub new_agent: Option<AgentKind>,
    pub pairing: Option<Pairing>,
    pub error: Option<String>,
    pub notice: Option<String>,
}

pub fn init() -> App {
    App {
        bridge: None,
        conn: Conn::Connecting,
        screen: Screen::NewSession,
        sidebar: PaneState::new(276.0).range(180.0, 520.0),
        info: None,
        workspaces: vec![],
        sessions: vec![],
        connectors: vec![],
        devices: vec![],
        approvals: vec![],
        transcripts: HashMap::new(),
        subscribed: HashSet::new(),
        expanded: HashSet::new(),
        prompt: TextEdit::new(""),
        ws_path: TextEdit::new(""),
        custom_host: TextEdit::new(""),
        host_for: None,
        conn_url: TextEdit::new(""),
        signing_in: HashMap::new(),
        open_queue: vec![],
        rename: TextEdit::new(""),
        renaming: None,
        new_ws: None,
        new_agent: None,
        pairing: None,
        error: None,
        notice: None,
    }
}

fn parse<T: serde::de::DeserializeOwned>(v: Value) -> Option<T> {
    serde_json::from_value(v).ok()
}

impl App {
    pub fn call(&self, tag: Tag, method: &'static str, params: Value) {
        if let Some(b) = &self.bridge {
            b.call(tag, method, params);
        }
    }

    /// A user action: errors are shown; success refreshes a list.
    pub fn act(&self, what: &str, method: &'static str, params: Value, refresh: Refresh) {
        self.call(Tag::Action { what: what.into(), refresh }, method, params);
    }

    pub fn refresh(&self, r: Refresh) {
        match r {
            Refresh::None => {}
            Refresh::Workspaces => self.call(Tag::Workspaces, m::WORKSPACE_LIST, json!({})),
            Refresh::Connectors => self.call(Tag::Connectors, m::CONNECTOR_LIST, json!({})),
            Refresh::Devices => self.call(Tag::Devices, m::DEVICE_LIST, json!({})),
            Refresh::Approvals => self.call(Tag::Approvals, m::APPROVAL_LIST, json!({})),
            Refresh::HostInfo => self.call(Tag::HostInfo, m::HOST_INFO, json!({})),
        }
    }

    pub fn refresh_all(&self) {
        for r in [Refresh::HostInfo, Refresh::Workspaces, Refresh::Connectors, Refresh::Devices, Refresh::Approvals] {
            self.refresh(r);
        }
    }

    /// Open a session: subscribe from what we already have, so nothing is lost
    /// or repeated when switching or reconnecting.
    pub fn open_session(&mut self, id: &str) {
        self.screen = Screen::Session(id.to_string());
        self.prompt.focused = true;
        self.ensure_subscribed(id);
    }

    pub fn ensure_subscribed(&mut self, id: &str) {
        if self.conn != Conn::Connected || self.subscribed.contains(id) {
            return;
        }
        let after = self.transcripts.get(id).map(|t| t.last_seq).unwrap_or(0);
        self.subscribed.insert(id.to_string());
        self.call(Tag::Subscribed(id.into()), m::EVENTS_SUBSCRIBE, json!({"session_id": id, "after_seq": after}));
    }

    pub fn session(&self, id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.session_id == id)
    }

    pub fn workspace(&self, id: &str) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.workspace_id == id)
    }

    pub fn send_prompt(&mut self) {
        let Screen::Session(id) = &self.screen else { return };
        let text = self.prompt.text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let id = id.clone();
        self.act(
            "Send prompt",
            m::SESSION_PROMPT,
            json!({"session_id": id, "prompt_id": uuid::Uuid::new_v4().to_string(), "text": text}),
            Refresh::None,
        );
        self.prompt.clear();
    }

    /// Apply everything the bridge has delivered. True if anything changed.
    pub fn drain(&mut self) -> bool {
        let mut msgs = vec![];
        if let Some(b) = &self.bridge {
            while let Ok(m) = b.rx.try_recv() {
                msgs.push(m);
            }
        }
        let changed = !msgs.is_empty();
        for msg in msgs {
            self.apply(msg);
        }
        changed
    }

    pub fn apply(&mut self, msg: FromHost) {
        match msg {
            FromHost::StartedHost => self.conn = Conn::StartingHost,
            FromHost::Connected { .. } => {
                self.conn = Conn::Connected;
                self.subscribed.clear();
                self.error = None;
                self.refresh_all();
                if let Screen::Session(id) = self.screen.clone() {
                    self.ensure_subscribed(&id);
                }
            }
            FromHost::Disconnected(e) => {
                if self.conn != Conn::StartingHost || self.info.is_some() {
                    self.conn = Conn::Down(e);
                }
                self.subscribed.clear();
            }
            FromHost::Reply { tag, result } => match (tag, result) {
                (Tag::HostInfo, Ok(v)) => self.info = parse(v),
                (Tag::Workspaces, Ok(v)) => self.workspaces = parse(v).unwrap_or_default(),
                (Tag::Sessions, Ok(v)) => {
                    self.sessions = parse(v).unwrap_or_default();
                    if self.screen == Screen::NewSession && self.sessions.is_empty() && self.workspaces.is_empty() {
                        self.screen = Screen::Workspaces;
                    }
                }
                (Tag::Connectors, Ok(v)) => {
                    self.connectors = parse(v).unwrap_or_default();
                    self.settle_sign_ins();
                }
                (Tag::ConnectorAdded, Ok(v)) => {
                    if let Some(c) = parse::<Connector>(v) {
                        let (id, name, url) = (c.connector_id.clone(), c.name.clone(), c.url.clone());
                        let twin = self.connectors.iter().any(|x| x.url == url && x.connector_id != id);
                        let needs = c.status == "needs_sign_in";
                        self.upsert_connector(c);
                        if needs {
                            self.sign_in(&id);
                            self.notice = Some(if twin {
                                format!("Sign in to {name} in your browser. If the browser is still signed in to your other account, switch accounts there first.")
                            } else {
                                format!("Sign in to {name} in your browser to finish adding it.")
                            });
                        } else {
                            self.notice = Some(format!("Added {name}."));
                        }
                    }
                }
                (Tag::SignIn(id), Ok(v)) => match parse::<ConnectorSignInResult>(v).and_then(|r| r.authorize_url) {
                    Some(u) => {
                        self.signing_in.insert(id, u.clone());
                        self.open_queue.push(u);
                    }
                    None => self.refresh(Refresh::Connectors),
                },
                (Tag::SignIn(id), Err(e)) => {
                    self.signing_in.remove(&id);
                    let why = e.message.strip_prefix("sign-in could not start: ").unwrap_or(&e.message);
                    self.error = Some(format!("Sign-in could not start: {why}"));
                }
                (Tag::ConnectorAdded, Err(e)) => self.error = Some(format!("Could not add the server: {}", e.message)),
                (Tag::Devices, Ok(v)) => self.devices = parse(v).unwrap_or_default(),
                (Tag::Approvals, Ok(v)) => self.approvals = parse(v).unwrap_or_default(),
                (Tag::PairStart, Ok(v)) => {
                    if let Some(p) = parse::<PairStartResult>(v) {
                        self.pairing = Some(Pairing { pairing_id: p.pairing_id, qr: p.qr_payload, expires_at: p.expires_at, ..Default::default() });
                    }
                }
                (Tag::SessionCreated, Ok(v)) => {
                    if let Some(s) = parse::<Session>(v) {
                        let id = s.session_id.clone();
                        self.upsert_session(s);
                        self.open_session(&id);
                    }
                }
                (Tag::Subscribed(_), Ok(_)) => {}
                (Tag::Action { refresh, .. }, Ok(_)) => self.refresh(refresh),
                (Tag::Subscribed(id), Err(e)) => {
                    self.subscribed.remove(&id);
                    self.error = Some(format!("Could not open the session: {}", e.message));
                }
                (Tag::Action { what, .. }, Err(e)) => self.error = Some(format!("{what} failed: {}", e.message)),
                (Tag::PairStart, Err(e)) => self.error = Some(format!("Pairing could not start: {}", e.message)),
                (Tag::SessionCreated, Err(e)) => self.error = Some(format!("Could not start the session: {}", e.message)),
                (_, Err(e)) => self.error = Some(e.message),
            },
            FromHost::Note(n) => self.on_note(n),
        }
    }

    /// Start a connector's OAuth sign-in; the browser opens when it replies.
    pub fn sign_in(&mut self, id: &str) {
        self.signing_in.insert(id.to_string(), String::new());
        self.call(Tag::SignIn(id.into()), m::CONNECTOR_SIGN_IN, json!({"connector_id": id}));
    }

    /// Add the server in the URL field; names come from the host.
    pub fn add_connector(&mut self) {
        let mut url = self.conn_url.text.trim().to_string();
        if url.is_empty() {
            return;
        }
        if !url.contains("://") {
            url = format!("https://{url}");
        }
        self.error = None;
        self.call(Tag::ConnectorAdded, m::CONNECTOR_ADD, json!({"url": url}));
        self.conn_url.clear();
    }

    /// Sign-ins that finished, one way or the other, since the last list.
    fn settle_sign_ins(&mut self) {
        let mut done = vec![];
        for c in &self.connectors {
            if self.signing_in.contains_key(&c.connector_id) && c.status != "needs_sign_in" {
                done.push((c.connector_id.clone(), c.name.clone(), c.status == "ok", c.tools.len()));
            }
        }
        let known: HashSet<&str> = self.connectors.iter().map(|c| c.connector_id.as_str()).collect();
        self.signing_in.retain(|id, _| known.contains(id.as_str()));
        for (id, name, ok, n) in done {
            self.signing_in.remove(&id);
            self.notice = Some(if ok { format!("Signed in to {name}. {n} tools available.") } else { format!("Sign-in to {name} didn't finish.") });
        }
    }

    fn upsert_connector(&mut self, c: Connector) {
        match self.connectors.iter_mut().find(|x| x.connector_id == c.connector_id) {
            Some(x) => *x = c,
            None => self.connectors.push(c),
        }
    }

    fn upsert_session(&mut self, s: Session) {
        match self.sessions.iter_mut().find(|x| x.session_id == s.session_id) {
            Some(x) => *x = s,
            None => self.sessions.push(s),
        }
    }

    fn on_note(&mut self, n: agenthost_proto::jsonrpc::Notification) {
        match n.method.as_str() {
            m::N_EVENT => {
                let Some(e) = parse::<Event>(n.params) else { return };
                let kind = e.kind;
                let t = self.transcripts.entry(e.session_id.clone()).or_default();
                if e.seq <= t.last_seq {
                    return; // already have it
                }
                t.last_seq = e.seq;
                if matches!(kind, EventType::AgentMessage | EventType::TurnEnd) {
                    t.live.clear();
                }
                t.events.push(e);
                if matches!(kind, EventType::ApprovalRequested | EventType::ApprovalResolved | EventType::NetDenied) {
                    self.refresh(Refresh::Approvals);
                }
            }
            m::N_SESSION_DELTA => {
                if let Some(d) = parse::<SessionDelta>(n.params) {
                    if d.kind == EventType::AgentMessage {
                        self.transcripts.entry(d.session_id).or_default().live.push_str(&d.text);
                    }
                }
            }
            m::N_SESSION_CHANGED => {
                if let Some(s) = parse::<Session>(n.params) {
                    let waiting = s.state == SessionState::WaitingApproval;
                    let was = self.session(&s.session_id).map(|x| x.state == SessionState::WaitingApproval).unwrap_or(false);
                    self.upsert_session(s);
                    if waiting != was {
                        self.refresh(Refresh::Approvals);
                    }
                }
            }
            m::N_PAIRING_PENDING => {
                if let Some(p) = parse::<PairingPending>(n.params) {
                    if let Some(pr) = self.pairing.as_mut().filter(|pr| pr.pairing_id == p.pairing_id) {
                        pr.pending = Some(p);
                    }
                }
            }
            m::N_CONNECTOR_CHANGED => self.refresh(Refresh::Connectors),
            m::N_PAIRING_DONE => {
                let ok = n.params["ok"] == json!(true);
                let name = n.params["device_name"].as_str().unwrap_or("device").to_string();
                let reason = n.params["reason"].as_str().unwrap_or("refused").to_string();
                if let Some(pr) = self.pairing.as_mut() {
                    pr.pending = None;
                    pr.done = Some(if ok { format!("Paired {name}.") } else { format!("Pairing ended: {reason}") });
                }
                self.refresh(Refresh::Devices);
            }
            _ => {}
        }
    }
}

/// "3m ago" style ages.
pub fn ago(ts: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let s = ((now - ts) / 1000).max(0);
    match s {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}
