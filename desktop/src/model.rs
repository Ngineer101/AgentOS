//! App state and how host messages change it.

use crate::bridge::{remote_state_path, Bridge, FromHost, Refresh, Tag, ToHost};
use agenthost_proto::methods as m;
use agenthost_proto::*;
use rune::TextEdit;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[derive(Clone, PartialEq, Debug)]
pub enum Screen {
    /// No session open: the empty state.
    Home,
    Session(String),
    /// The automations: the list, the selected one and its runs.
    Automations,
    /// The notes: every workspace's, beside the chosen one's editor.
    Notes,
}

/// A Settings tab (macOS) or Preferences page (GNOME).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    General,
    Agents,
    Workspaces,
    Connectors,
    Devices,
    Archived,
}

impl Tab {
    pub const ALL: [Tab; 6] = [Tab::General, Tab::Agents, Tab::Workspaces, Tab::Connectors, Tab::Devices, Tab::Archived];
    pub fn title(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Agents => "Agents",
            Tab::Workspaces => "Workspaces",
            Tab::Connectors => "Connectors",
            Tab::Devices => "Devices",
            Tab::Archived => "Archived",
        }
    }
}

/// An in-app pop-up menu (where the platform has no native one, and on GNOME).
#[derive(Clone, PartialEq, Debug)]
pub enum Menu {
    /// The toolbar's session menu.
    Session,
    /// GNOME's main menu (F10).
    Main,
    /// The New session dialog's login drop-down (GNOME).
    Login,
    /// A sidebar row's context menu, at a window point.
    Row(String, f32, f32),
    /// A sidebar section's context menu (its workspace), at a window point.
    Section(String, f32, f32),
    /// The composer's model popover: model, effort and fast mode.
    Model,
    /// A pop-up picker in the automation editor (where there's no native menu), by its key.
    Pick(&'static str),
}

/// One status vocabulary everywhere: the sidebar, the toolbar capsule, the header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// Created, starting or running: a spinner.
    Working,
    /// Waiting for an approval: orange.
    NeedsYou,
    /// Red, with the reason in the session.
    Failed,
    /// Idle: the agent finished its turn and waits for you. The accent colour.
    YourTurn,
    /// Grey.
    Ended,
}

impl Status {
    pub fn of(st: SessionState) -> Status {
        match st {
            SessionState::Created | SessionState::Starting | SessionState::Running => Status::Working,
            SessionState::WaitingApproval => Status::NeedsYou,
            SessionState::Failed => Status::Failed,
            SessionState::Idle => Status::YourTurn,
            SessionState::Ended => Status::Ended,
        }
    }
    pub fn word(self) -> &'static str {
        match self {
            Status::Working => "Working",
            Status::NeedsYou => "Needs you",
            Status::Failed => "Failed",
            Status::YourTurn => "Your turn",
            Status::Ended => "Ended",
        }
    }
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
    /// Side answers streaming in, by side conversation.
    pub side_live: HashMap<String, String>,
    /// Prompts sent from here that the host hasn't recorded yet: shown at once, so a
    /// message never seems to vanish between Send and the host's copy of it.
    pub pending: Vec<PendingPrompt>,
}

/// A prompt on its way to the host: its id, and a `prompt` event payload to show.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingPrompt {
    pub prompt_id: String,
    pub payload: Value,
}

/// One side conversation (`/btw`), as the session's events tell it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SideThread {
    pub side_id: String,
    pub turns: Vec<SideTurn>,
    /// The answer still arriving for the last turn.
    pub live: String,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct SideTurn {
    pub question: String,
    pub answer: String,
    pub ended: bool,
    pub error: Option<String>,
}

/// A subagent: an `Agent`/`Task` tool call, the calls made inside it, what it said, and
/// (once its call has a result) its report.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Subagent {
    pub call_id: String,
    pub description: String,
    pub agent_type: String,
    pub prompt: String,
    pub started: i64,
    /// When its result arrived.
    pub ended: Option<i64>,
    /// "running", "stopped" (the turn ended without a result), "completed", "failed"…
    pub status: String,
    pub report: String,
    /// Its tool calls and messages, in order.
    pub items: Vec<SubItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SubItem {
    /// A tool call made inside the subagent: its call event's seq and id.
    Step { seq: i64, call_id: String },
    Message(String),
}

impl Subagent {
    pub fn running(&self) -> bool {
        self.status == "running"
    }
    pub fn steps(&self) -> usize {
        self.items.iter().filter(|i| matches!(i, SubItem::Step { .. })).count()
    }
    /// The title: its description, else "Subagent".
    pub fn title(&self) -> &str {
        if self.description.trim().is_empty() { "Subagent" } else { self.description.trim() }
    }
}

/// A `tool.call` payload that starts a subagent.
pub fn is_subagent_call(p: &Value) -> bool {
    p["subagent"].is_object() || matches!(p["tool_name"].as_str(), Some("Agent" | "Task"))
}

/// An event that belongs to a subagent, not the main transcript.
/// What to say under the transcript while the agent is quiet: from Send until its first
/// word or tool call, and between steps of a turn. `None` while something else shows the
/// work (a running tool call, a subagent's card, an approval) or there's no work.
pub fn quiet_work(state: SessionState, events: &[Event], pending: bool) -> Option<&'static str> {
    let label = match state {
        SessionState::Created | SessionState::Starting => "Starting…",
        _ => "Working…",
    };
    let last = events.iter().rev().find(|e| !matches!(e.kind, EventType::SideQuestion | EventType::SideMessage | EventType::SideEnd));
    // Sent between turns: the host starts one as soon as it has the prompt (recorded
    // just before the session says it's running).
    if state == SessionState::Idle && (pending || last.is_some_and(|e| e.kind == EventType::Prompt)) {
        return Some(label);
    }
    if !matches!(state, SessionState::Created | SessionState::Starting | SessionState::Running) {
        return None;
    }
    match last {
        Some(e) if is_child_event(e) => None,
        Some(e) if matches!(e.kind, EventType::ToolCall | EventType::ApprovalRequested) => None,
        Some(e) if e.kind == EventType::ToolResult && matches!(e.payload["status"].as_str(), Some("running" | "pending" | "in_progress")) => None,
        _ => Some(label),
    }
}

pub fn is_child_event(e: &Event) -> bool {
    e.kind == EventType::SubagentMessage || (matches!(e.kind, EventType::ToolCall | EventType::ToolResult) && e.payload["parent_tool_call_id"].as_str().is_some_and(|p| !p.is_empty()))
}

/// The subagents in `events`. `active`: the session is still working (else a subagent without
/// a result has stopped).
pub fn subagents_of(events: &[Event], active: bool) -> Vec<Subagent> {
    let mut out: Vec<Subagent> = vec![];
    for e in events {
        let p = &e.payload;
        match e.kind {
            EventType::ToolCall if is_subagent_call(p) => {
                let sa = &p["subagent"];
                let desc = sa["description"].as_str().or(p["arguments"]["description"].as_str()).unwrap_or("");
                let kind = sa["agent_type"].as_str().or(p["arguments"]["subagent_type"].as_str()).unwrap_or("");
                let prompt = sa["prompt"].as_str().or(p["arguments"]["prompt"].as_str()).unwrap_or("");
                out.push(Subagent {
                    call_id: p["tool_call_id"].as_str().unwrap_or("").to_string(),
                    description: desc.to_string(),
                    agent_type: kind.to_string(),
                    prompt: prompt.to_string(),
                    started: e.ts,
                    status: "running".into(),
                    ..Default::default()
                });
            }
            EventType::ToolResult => {
                let cid = p["tool_call_id"].as_str().unwrap_or("");
                if let Some(a) = out.iter_mut().find(|a| a.call_id == cid) {
                    a.ended = Some(e.ts);
                    a.status = p["status"].as_str().unwrap_or("completed").to_string();
                    a.report = p["output"].as_str().unwrap_or("").to_string();
                    continue;
                }
            }
            _ => {}
        }
        let parent = p["parent_tool_call_id"].as_str().unwrap_or("");
        if parent.is_empty() {
            continue;
        }
        let Some(a) = out.iter_mut().find(|a| a.call_id == parent) else { continue };
        match e.kind {
            EventType::ToolCall => a.items.push(SubItem::Step { seq: e.seq, call_id: p["tool_call_id"].as_str().unwrap_or("").to_string() }),
            EventType::SubagentMessage => {
                let t = p["text"].as_str().unwrap_or("").trim();
                if !t.is_empty() {
                    a.items.push(SubItem::Message(t.to_string()));
                }
            }
            _ => {}
        }
    }
    if !active {
        for a in &mut out {
            if a.running() {
                a.status = "stopped".into();
            }
        }
    }
    out
}

/// Markdown as plain lines: no headings' hashes, list marks, emphasis, code fences or link
/// targets. Blank lines go.
pub fn plain_lines(md: &str) -> Vec<String> {
    let mut out = vec![];
    for line in md.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with("```") || l.chars().all(|c| matches!(c, '-' | '*' | '_' | '=' | '|' | ' ' | ':')) {
            continue;
        }
        let l = l.trim_start_matches('#').trim_start_matches('>').trim_start();
        let l = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")).or_else(|| l.strip_prefix("+ ")).unwrap_or(l);
        // [text](url) → text
        let mut s = String::new();
        let mut rest = l;
        while let Some(i) = rest.find('[') {
            match (rest[i..].find("]("), rest[i..].find(')')) {
                (Some(j), Some(k)) if j < k => {
                    s.push_str(&rest[..i]);
                    s.push_str(&rest[i + 1..i + j]);
                    rest = &rest[i + k + 1..];
                }
                _ => {
                    s.push_str(&rest[..=i]);
                    rest = &rest[i + 1..];
                }
            }
        }
        s.push_str(rest);
        let s: String = s.replace("**", "").replace("__", "").replace('`', "");
        if !s.trim().is_empty() {
            out.push(s.trim().to_string());
        }
    }
    out
}

/// "38s", "1m 12s".
pub fn span_label(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    if s < 60 { format!("{s}s") } else if s < 3600 { format!("{}m {}s", s / 60, s % 60) } else { format!("{}h {}m", s / 3600, (s % 3600) / 60) }
}

/// "1 agent running", "2 agents and 1 command running".
pub fn running_label(agents: usize, commands: usize) -> Option<String> {
    let n = |k: usize, one: &str, many: &str| format!("{k} {}", if k == 1 { one } else { many });
    match (agents, commands) {
        (0, 0) => None,
        (a, 0) => Some(format!("{} running", n(a, "agent", "agents"))),
        (0, c) => Some(format!("{} running", n(c, "command", "commands"))),
        (a, c) => Some(format!("{} and {} running", n(a, "agent", "agents"), n(c, "command", "commands"))),
    }
}

impl SideThread {
    /// The tab's title: the first question.
    pub fn title(&self) -> &str {
        self.turns.first().map(|t| t.question.as_str()).unwrap_or("Side question")
    }
    /// The last question is still being answered.
    pub fn streaming(&self) -> bool {
        self.turns.last().is_some_and(|t| !t.ended)
    }
}

/// `/btw <question>`: the question, if the text is a side question. "/btw" alone is an empty one.
pub fn parse_btw(text: &str) -> Option<&str> {
    let t = text.trim_start();
    let rest = t.strip_prefix("/btw")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None; // "/btwx"
    }
    Some(rest.trim())
}

/// Slash commands the composer offers as you type "/": the command and what it does.
pub fn slash_suggestions(text: &str, side_questions: bool) -> Vec<(&'static str, &'static str)> {
    let mut out = vec![];
    if side_questions && text.starts_with('/') && !text.contains(char::is_whitespace) && "/btw".starts_with(text) {
        out.push(("/btw", "Ask a side question without interrupting"));
    }
    out
}

/// How long a toast stays, not counting time under the pointer.
pub const TOAST_LIFETIME: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToastKind {
    Info,
    Error,
    /// An agent waits for an approval (in another session).
    Approval,
}

#[derive(Clone, PartialEq, Debug)]
pub enum ToastAction {
    /// "Show": open the session and scroll to the approval card.
    ShowApproval { session_id: String, approval_id: String },
    /// "Settings" (GNOME: "Preferences"): open Settings on this tab.
    OpenSettings(Tab),
}

/// A short message over the window that goes by itself (AdwToast, or a floating panel on macOS).
#[derive(Clone, Debug)]
pub struct Toast {
    pub id: u64,
    pub kind: ToastKind,
    pub text: String,
    pub action: Option<ToastAction>,
    /// Time shown before the current stretch.
    shown: std::time::Duration,
    /// When the current stretch started; None while paused (under the pointer).
    since: Option<std::time::Instant>,
}

impl Toast {
    fn elapsed(&self, now: std::time::Instant) -> std::time::Duration {
        self.shown + self.since.map(|s| now.saturating_duration_since(s)).unwrap_or_default()
    }
}

/// What a paste into the composer becomes.
#[derive(Clone, Debug, PartialEq)]
pub enum PasteAs {
    /// Copied files (Finder, Files): attachments.
    Files(Vec<std::path::PathBuf>),
    /// An image (a screenshot, "Copy Image"): a PNG attachment.
    Image(Vec<u8>),
    /// Anything else: the editor pastes the text.
    Text,
}

/// Decide what a paste is. Files win (Finder also puts the names and an icon on the
/// clipboard); an image wins over text only when the text is empty or just describes the image
/// (a browser's `<img>`), so a spreadsheet's cells still paste as text.
pub fn paste_as(content: &rune::ClipboardContent) -> PasteAs {
    if !content.files().is_empty() {
        return PasteAs::Files(content.files().to_vec());
    }
    if let Some(img) = content.image() {
        let text_is_image = content.text().trim().is_empty() || content.html().is_some_and(|h| h.contains("<img"));
        if text_is_image {
            return PasteAs::Image(img.png().to_vec());
        }
    }
    PasteAs::Text
}

/// "Pasted image.png", then "Pasted image 2.png", … so chips stay apart.
pub fn pasted_image_name(taken: &[&str]) -> String {
    let mut n = 1;
    loop {
        let name = if n == 1 { "Pasted image.png".to_string() } else { format!("Pasted image {n}.png") };
        if !taken.contains(&name.as_str()) {
            return name;
        }
        n += 1;
    }
}

/// The add-connector search waits this long after the last keystroke.
pub const SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(300);

/// Whether this app can add a catalog server: OAuth and open ones, not API keys yet.
pub fn suggestion_supported(s: &ConnectorSuggestion) -> bool {
    matches!(s.auth.as_str(), "oauth" | "none")
}

/// An image sent with a message, fetched from the host to show in the transcript.
pub enum SentImage {
    Loading,
    Ready(rune::Image),
    Failed(String),
}

/// A message's attachments that are images, as `(attachment_id, name)`.
pub fn sent_images(payload: &Value) -> Vec<(String, String)> {
    payload["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["mime"].as_str().is_some_and(|m| m.starts_with("image/")))
        .filter_map(|a| Some((a["attachment_id"].as_str()?.to_string(), a["name"].as_str().unwrap_or("Image").to_string())))
        .collect()
}

/// A file picked for the next message.
pub struct PendingFile {
    pub local_id: u64,
    pub name: String,
    pub state: FileState,
}

pub enum FileState {
    Uploading,
    Ready(Attachment),
    Failed(String),
}

/// The composer's microphone.
pub enum Mic {
    Idle,
    Recording(crate::audio::Recorder),
    /// Stopped: waiting for the final text (live) or the transcription (older hosts).
    Transcribing,
}

/// A live dictation: the words heard so far sit in the composer from `start`, as `shown`
/// (with the spaces that separate them from the text around).
pub struct LiveDictation {
    pub gen: u64,
    pub(crate) start: usize,
    pub(crate) shown: String,
}

#[derive(Default, Clone)]
pub struct Pairing {
    pub pairing_id: String,
    pub qr: String,
    pub expires_at: i64,
    pub pending: Option<PairingPending>,
    pub done: Option<String>,
}

/// Pairing this app with a remote host — the other direction of [`Pairing`]:
/// paste what `agenthost device pair` shows on the host, compare the 6-digit
/// code with the one there, and it accepts or refuses.
#[derive(Clone)]
pub struct PairRemote {
    /// The `agenthost://pair?…` payload from the host's screen.
    pub payload: TextEdit,
    /// What the host's Devices list will show for this app.
    pub name: TextEdit,
    /// The code to compare with the host's, once the host side has it.
    pub code: Option<String>,
    pub error: Option<String>,
    /// A pair request is under way; the fields stand still until it ends.
    pub busy: bool,
    /// Which request that is: answers to any other (one cancelled, or from a
    /// form since closed) are dropped.
    pub attempt: u64,
}

pub struct App {
    pub bridge: Option<Bridge>,
    pub conn: Conn,
    pub screen: Screen,
    pub look: crate::look::Look,
    pub sidebar_open: bool,
    /// The New session sheet (macOS) or dialog (GNOME) is open.
    pub new_open: bool,
    /// Settings (macOS) or Preferences (GNOME) is open, on this tab.
    pub settings: Option<Tab>,
    /// GNOME's keyboard shortcuts window.
    pub shortcuts_open: bool,
    pub menu: Option<Menu>,
    /// The effort slider's stop while it's dragged; the setting changes on release.
    pub effort_drag: Option<usize>,
    /// Filters the sidebar.
    pub search: TextEdit,
    /// Archived sessions (Settings › Archived), once asked for.
    pub archived: Vec<Session>,
    pub archive_search: TextEdit,
    /// A live session waiting for "Archive ends the session" to be confirmed.
    pub confirm_archive: Option<String>,
    /// The New session sheet's "Make … the default for …" box.
    pub make_default: bool,
    /// This computer's name, for the sidebar's host line.
    pub hostname: String,
    /// Settings selections.
    pub sel_agent: Option<String>,
    pub sel_ws: Option<String>,
    pub sel_conn: Option<String>,
    pub sel_root: Option<String>,
    pub adding_profile: bool,
    pub adding_connector: bool,
    pub adding_workspace: bool,
    /// A folder just added from the New session sheet: select it once it's listed.
    pub pick_added: Option<String>,
    pub info: Option<HostInfo>,
    pub workspaces: Vec<Workspace>,
    pub sessions: Vec<Session>,
    pub connectors: Vec<Connector>,
    /// The host's own tools (opening pull requests, pushing, installing) and their policies;
    /// empty for a host without `builtin.list`.
    pub builtins: Vec<BuiltinTool>,
    pub devices: Vec<Device>,
    pub approvals: Vec<PendingApproval>,
    pub transcripts: HashMap<String, Transcript>,
    /// Sessions subscribed on the current connection.
    pub subscribed: HashSet<String>,
    pub expanded: HashSet<String>,
    pub prompt: TextEdit,
    /// The session whose message `prompt` holds, and the unsent messages of the others:
    /// each session keeps what you typed in it while you look at another.
    pub prompt_for: Option<String>,
    pub drafts: HashMap<String, TextEdit>,
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
    /// The chosen login for a new session ([`login_key`]).
    pub new_agent: Option<String>,
    /// The "Add an agent profile" form on the host screen.
    pub profile_agent: Option<AgentKind>,
    pub profile_name: TextEdit,
    pub profile_dir: TextEdit,
    /// Folders the phone may create projects in (`project.browse`).
    pub project_roots: Vec<String>,
    pub pairing: Option<Pairing>,
    /// Pairing this app with a remote host.
    pub pair_remote: Option<PairRemote>,
    /// Pair requests sent so far; the next one's [`PairRemote::attempt`].
    pub pair_attempts: u64,
    /// A paired remote host exists to connect to (its name).
    pub remote_host: Option<String>,
    /// The current connection goes through the relay, as a paired device
    /// rather than over the local socket.
    pub link_remote: bool,
    /// The current connection may manage the host (the local socket always
    /// can; a paired device never gets policy, by design).
    pub admin: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
    /// Files waiting to go with the next message, by session.
    pub files: HashMap<String, Vec<PendingFile>>,
    /// Images sent with the open session's messages, by attachment id (protocol 7).
    pub images: HashMap<String, SentImage>,
    /// Decodes [`App::images`]; set up with the window's waker.
    pub image_cache: Option<rune::ImageCache>,
    /// The image open in the viewer: its attachment id.
    pub viewing: Option<String>,
    pub next_local_id: u64,
    pub voice: Option<VoiceStatus>,
    /// Phone screen sharing (protocol 3); `None` on older hosts.
    pub screen_share: Option<ScreenStatus>,
    pub mic: Mic,
    /// Recent microphone level, 0..1, for the meter.
    pub mic_level: f32,
    /// Asking whether to download the speech model.
    pub ask_voice_download: bool,
    /// The dictation in progress, when the host takes live dictation.
    pub live: Option<LiveDictation>,
    pub next_dictation: u64,
    /// Return was pressed while dictating: send once the last words are in.
    pub send_after_dictation: bool,
    /// Subscription id per subscribed session (to replace it on a resync).
    pub sub_ids: HashMap<String, String>,
    /// Sessions the host says are ahead of us: (its last seq, when we heard).
    pub behind: HashMap<String, (i64, std::time::Instant)>,
    /// Toasts on screen, oldest first.
    pub toasts: Vec<Toast>,
    pub next_toast: u64,
    /// The pointer is over the toasts: their clocks stand still.
    pub toasts_hovered: bool,
    /// Approvals a toast already announced.
    pub toasted_approvals: HashSet<String>,
    /// The right panel is showing, and which of its tabs: see [`App::panel_shown`].
    pub panel_open: bool,
    pub panel_tab: PanelTab,
    /// The panel's width with the terminal in it, once its edge has been dragged.
    pub panel_w: Option<f32>,
    /// The host's terminals (the panel's Terminal tab).
    pub terms: crate::terminal::Terms,
    /// The open session's changes and files (the panel's Changes and Files tabs).
    pub browse: crate::files::FilesState,
    /// Side conversations whose tab was closed here.
    pub side_closed: HashSet<String>,
    /// The selected side conversation, by session.
    pub side_tab: HashMap<String, String>,
    /// A new side question waiting for its id, by session.
    pub side_pending: HashMap<String, String>,
    /// The side pane's follow-up field.
    pub side_prompt: TextEdit,
    /// Add connector: the catalog search field, and when its search is due.
    pub conn_search: TextEdit,
    pub search_due: Option<std::time::Instant>,
    /// The query the results below are for (or are being fetched for).
    pub search_query: String,
    pub searching: bool,
    pub suggestions: Vec<ConnectorSuggestion>,
    pub search_error: Option<String>,
    /// Add connector: typing a URL instead of searching.
    pub add_manual: bool,
    pub conn_name: TextEdit,
    /// A rename the host refused: (connector, why).
    pub rename_error: Option<(String, String)>,
    /// The session whose title the header is renaming in place, and the field.
    pub session_rename: Option<String>,
    pub title_edit: TextEdit,
    /// The subagent shown in the agent panel: (session, its Agent/Task call id).
    pub agent_open: Option<(String, String)>,
    /// The agent panel shows the task's whole prompt.
    pub agent_task_open: bool,
    /// Automations (protocol 5), as the host lists them.
    pub automations: Vec<Automation>,
    /// The automation shown on the Automations screen.
    pub sel_auto: Option<String>,
    /// Runs of automations, newest first, by automation (once asked for).
    pub runs: HashMap<String, Vec<AutomationRun>>,
    /// The automation editor is open, on `draft`.
    pub auto_open: bool,
    pub draft: AutoDraft,
    /// An automation waiting for Delete to be confirmed.
    pub confirm_delete_auto: Option<String>,
    /// An automation waiting for "New URL" to be confirmed (the old one stops working).
    pub confirm_rotate: Option<String>,
    /// Notes (protocol 9), as the host lists them: open ones first, newest first.
    pub notes: Vec<Note>,
    /// The note the Notes screen shows (when not writing a new one).
    pub sel_note: Option<String>,
    /// Writing a new note in this workspace: the editor isn't a saved note yet.
    pub new_note_ws: Option<String>,
    /// The Notes editor's text: the shown note's, or the new one's.
    pub note_edit: TextEdit,
    /// The text the editor started from (the note as last loaded or saved): edits are
    /// what differs from it.
    pub note_base: String,
    /// A save of the editor's text is on its way.
    pub note_saving: bool,
    /// Counts new notes begun, so a late `note.create` answer only adopts the one it saved.
    pub note_gen: u64,
    /// The Notes list shows done notes too.
    pub notes_show_done: bool,
    /// A note waiting for Delete to be confirmed.
    pub confirm_delete_note: Option<String>,
    /// The New Session sheet starts a session from this note: (note id, workspace, text).
    pub start_note: Option<(String, String, String)>,
    /// The sidebar's search field is open (⌘F / Ctrl+F or the search button).
    pub search_open: bool,
    /// Workspaces whose sidebar group is collapsed; saved between launches.
    pub collapsed: HashSet<String>,
    /// Workspaces whose group shows every session, not just the newest few, until quit.
    pub show_all: HashSet<String>,
    /// The sidebar row under the pointer: it shows Pin and Archive.
    pub hover_row: Option<String>,
    /// The jump keys' modifier is held (⌘ on macOS, Alt on GNOME): the sidebar shows
    /// ⌘1…9 / Alt+1…9 in place of the ages.
    pub jump_held: bool,
    /// Save `collapsed` and `sidebar_w` to the prefs file (off in tests and offline snapshots).
    pub save_prefs: bool,
    /// The sidebar's width as dragged by its edge; `None` is the look's own.
    pub sidebar_w: Option<f32>,
    /// Per session, the newest event seen while it was open (see [`App::unread`]). `None`
    /// until the first session list, which marks every session seen so nothing starts unread.
    pub seen: Option<HashMap<String, i64>>,
}

/// A sidebar group shows this many sessions until "Show N more".
pub const GROUP_SHOWN: usize = 8;
/// How narrow and wide the sidebar's edge drags it.
/// A tab of the right panel. A new feature for the panel starts as a variant here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelTab {
    /// Shells the host runs for you.
    Terminal,
    /// What the open session changed, as a diff.
    Changes,
    /// The open session's folder, to read.
    Files,
    /// The open session's side questions (`/btw`).
    Side,
}

pub const SIDEBAR_MIN_W: f32 = 220.0;
pub const SIDEBAR_MAX_W: f32 = 480.0;

/// One login of an agent: its profile id, or (hosts before profiles) its name.
pub fn login_key(a: &AgentStatus) -> String {
    a.profile_id.clone().unwrap_or_else(|| a.agent.as_str().to_string())
}

pub fn init() -> App {
    App {
        bridge: None,
        conn: Conn::Connecting,
        screen: Screen::Home,
        look: crate::look::Look::current(),
        sidebar_open: true,
        new_open: false,
        settings: None,
        shortcuts_open: false,
        menu: None,
        search: TextEdit::new(""),
        archived: vec![],
        archive_search: TextEdit::new(""),
        confirm_archive: None,
        make_default: false,
        hostname: String::new(),
        sel_agent: None,
        sel_ws: None,
        sel_conn: None,
        sel_root: None,
        adding_profile: false,
        adding_connector: false,
        adding_workspace: false,
        pick_added: None,
        info: None,
        workspaces: vec![],
        sessions: vec![],
        connectors: vec![],
        builtins: vec![],
        devices: vec![],
        approvals: vec![],
        transcripts: HashMap::new(),
        subscribed: HashSet::new(),
        expanded: HashSet::new(),
        prompt: TextEdit::new(""),
        prompt_for: None,
        drafts: HashMap::new(),
        images: HashMap::new(),
        image_cache: None,
        viewing: None,
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
        profile_agent: None,
        profile_name: TextEdit::new(""),
        profile_dir: TextEdit::new(""),
        project_roots: vec![],
        pairing: None,
        pair_remote: None,
        pair_attempts: 0,
        remote_host: None,
        link_remote: false,
        admin: true,
        error: None,
        notice: None,
        files: HashMap::new(),
        next_local_id: 1,
        voice: None,
        screen_share: None,
        mic: Mic::Idle,
        mic_level: 0.0,
        ask_voice_download: false,
        live: None,
        next_dictation: 0,
        send_after_dictation: false,
        sub_ids: HashMap::new(),
        behind: HashMap::new(),
        toasts: vec![],
        next_toast: 1,
        toasts_hovered: false,
        toasted_approvals: HashSet::new(),
        panel_open: false,
        panel_tab: PanelTab::Terminal,
        panel_w: None,
        terms: Default::default(),
        browse: Default::default(),
        side_closed: HashSet::new(),
        side_tab: HashMap::new(),
        side_pending: HashMap::new(),
        side_prompt: TextEdit::new(""),
        conn_search: TextEdit::new(""),
        search_due: None,
        search_query: String::new(),
        searching: false,
        suggestions: vec![],
        search_error: None,
        add_manual: false,
        conn_name: TextEdit::new(""),
        rename_error: None,
        session_rename: None,
        title_edit: TextEdit::new(""),
        agent_open: None,
        agent_task_open: false,
        effort_drag: None,
        automations: vec![],
        sel_auto: None,
        runs: HashMap::new(),
        auto_open: false,
        draft: AutoDraft::blank(),
        confirm_delete_auto: None,
        confirm_rotate: None,
        notes: vec![],
        sel_note: None,
        new_note_ws: None,
        note_edit: TextEdit::new(""),
        note_base: String::new(),
        note_saving: false,
        note_gen: 0,
        notes_show_done: false,
        confirm_delete_note: None,
        start_note: None,
        search_open: false,
        collapsed: HashSet::new(),
        show_all: HashSet::new(),
        hover_row: None,
        jump_held: false,
        save_prefs: false,
        sidebar_w: None,
        seen: None,
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
    /// Start a session in the chosen workspace as the chosen login.
    pub fn start_session(&self) {
        let (Some(ws), Some(key)) = (&self.new_ws, &self.new_agent) else { return };
        let agents = self.info.as_ref().map(|i| i.agents.as_slice()).unwrap_or_default();
        let Some(a) = agents.iter().find(|a| &login_key(a) == key) else { return };
        let mut params = json!({"workspace_id": ws, "agent": a.agent});
        if let Some(id) = &a.profile_id {
            params["profile_id"] = json!(id);
        }
        // From a note, unless another workspace was picked: it opens with the note's text.
        if let Some((note, _, text)) = self.start_note.as_ref().filter(|(_, w, _)| w == ws) {
            params["note_id"] = json!(note);
            self.call(Tag::SessionFromNote(text.clone()), m::SESSION_CREATE, params);
            return;
        }
        self.call(Tag::SessionCreated, m::SESSION_CREATE, params);
    }

    /// The sheet's Start button: start the session; with the box checked, the login becomes
    /// the workspace's default.
    pub fn start_from_sheet(&mut self) {
        let (Some(ws), Some(key)) = (self.new_ws.clone(), self.new_agent.clone()) else {
            self.notice = Some("Pick a workspace and a login first.".into());
            return;
        };
        self.start_session();
        self.start_note = None;
        if self.make_default && self.default_login(&ws).as_deref() != Some(key.as_str()) {
            self.set_default_login(&ws, Some(&key));
        }
        self.make_default = false;
        self.new_open = false;
        self.menu = None;
    }

    /// The workspace's default login ([`login_key`]), if it's set and still usable.
    pub fn default_login(&self, ws: &str) -> Option<String> {
        let w = self.workspace(ws)?;
        let agent = w.default_agent?;
        let key = w.default_profile_id.clone().unwrap_or_else(|| agent.as_str().to_string());
        let agents = self.info.as_ref().map(|i| i.agents.as_slice()).unwrap_or_default();
        agents.iter().find(|a| login_key(a) == key && a.agent == agent && a.available).map(login_key)
    }

    /// Pick a workspace in the sheet: its default login comes with it.
    pub fn pick_workspace(&mut self, ws: &str) {
        self.new_ws = Some(ws.to_string());
        self.make_default = false;
        if let Some(key) = self.default_login(ws) {
            self.new_agent = Some(key);
        }
    }

    /// Set (or with `None` clear) the login new sessions in a workspace start with.
    pub fn set_default_login(&self, ws: &str, key: Option<&str>) {
        let agents = self.info.as_ref().map(|i| i.agents.as_slice()).unwrap_or_default();
        let params = match key.and_then(|k| agents.iter().find(|a| login_key(a) == k)) {
            Some(a) => json!({"workspace_id": ws, "agent": a.agent, "profile_id": login_key(a)}),
            None => json!({"workspace_id": ws, "agent": null}),
        };
        self.act("Change the default login", m::WORKSPACE_SET_DEFAULT_AGENT, params, Refresh::Workspaces);
    }

    /// Archive a session: a live one asks first (the host ends it), an ended one goes at once.
    pub fn archive(&mut self, id: &str) {
        let Some(sess) = self.sessions.iter().find(|x| x.session_id == id) else { return };
        if !sess.state.is_terminal() && self.confirm_archive.as_deref() != Some(id) {
            self.confirm_archive = Some(id.to_string());
            return;
        }
        self.confirm_archive = None;
        // Select the next session (or the empty state) before it leaves the list.
        if self.screen == Screen::Session(id.to_string()) {
            let order = self.sidebar_order();
            let i = order.iter().position(|x| x == id);
            let next = i.and_then(|i| order.get(i + 1).or_else(|| i.checked_sub(1).and_then(|j| order.get(j)))).cloned();
            match next {
                Some(n) => self.open_session(&n),
                None => self.screen = Screen::Home,
            }
        }
        self.act("Archive session", m::SESSION_ARCHIVE, json!({"session_id": id}), Refresh::Archived);
    }

    /// Archive every ended session in a workspace.
    pub fn archive_ended(&mut self, ws: &str) {
        let ids: Vec<String> = self.sessions.iter().filter(|x| x.workspace_id == ws && x.state.is_terminal()).map(|x| x.session_id.clone()).collect();
        for id in ids {
            self.archive(&id);
        }
    }

    pub fn unarchive(&mut self, id: &str) {
        self.act("Unarchive session", m::SESSION_UNARCHIVE, json!({"session_id": id}), Refresh::Archived);
    }

    /// Open the New session sheet, preselecting Quick tasks for a quick task, else the last
    /// choice (or the first folder); and the workspace's default login, else the last login
    /// (or the first one that's ready).
    pub fn open_new_session(&mut self, quick: bool) {
        self.close_overlays();
        self.start_note = None;
        if quick || self.new_ws.as_ref().is_none_or(|w| self.workspace(w).is_none()) {
            let first_folder = self.workspaces.iter().find(|w| w.kind != WorkspaceKind::Scratch).map(|w| w.workspace_id.clone());
            let quick_ws = self.workspaces.iter().find(|w| w.kind == WorkspaceKind::Scratch).map(|w| w.workspace_id.clone());
            self.new_ws = if quick { quick_ws.or(first_folder) } else { first_folder.or(quick_ws) };
        }
        let agents = self.info.as_ref().map(|i| i.agents.clone()).unwrap_or_default();
        if let Some(key) = self.new_ws.clone().and_then(|w| self.default_login(&w)) {
            self.new_agent = Some(key);
        } else if !agents.iter().any(|a| a.available && Some(login_key(a)) == self.new_agent) {
            self.new_agent = agents.iter().find(|a| a.available).map(login_key);
        }
        self.make_default = false;
        self.new_open = true;
    }

    /// A workspace's "+" in the sidebar (and "New Session Here"): start a session there at
    /// once, as the login the sheet would preselect (the workspace's default, else the last
    /// one picked, else the first available). With no login to use, the sheet opens instead.
    pub fn open_new_session_in(&mut self, ws: &str) {
        self.new_ws = Some(ws.to_string());
        let Some(key) = self.quick_login(ws) else {
            self.open_new_session(false);
            return;
        };
        self.close_overlays();
        self.start_note = None;
        self.new_agent = Some(key);
        self.start_session();
    }

    /// The login a session started without the sheet uses in `ws`.
    fn quick_login(&self, ws: &str) -> Option<String> {
        let agents = self.info.as_ref().map(|i| i.agents.as_slice()).unwrap_or_default();
        let usable = |k: &String| agents.iter().any(|a| a.available && &login_key(a) == k);
        self.default_login(ws).or_else(|| self.new_agent.clone().filter(usable)).or_else(|| agents.iter().find(|a| a.available).map(login_key))
    }

    pub fn open_settings(&mut self, tab: Tab) {
        self.close_overlays();
        self.settings = Some(tab);
        match tab {
            Tab::General | Tab::Agents => self.refresh(Refresh::HostInfo),
            Tab::Workspaces => self.refresh(Refresh::Workspaces),
            Tab::Connectors => self.refresh(Refresh::Connectors),
            Tab::Devices => self.refresh(Refresh::Devices),
            Tab::Archived => self.refresh(Refresh::Archived),
        }
    }

    /// Close the sheet, Settings, the shortcuts window and any menu.
    pub fn close_overlays(&mut self) {
        self.new_open = false;
        self.settings = None;
        self.shortcuts_open = false;
        self.menu = None;
        self.confirm_archive = None;
        self.auto_open = false;
        self.confirm_delete_auto = None;
        self.confirm_rotate = None;
        self.confirm_delete_note = None;
    }

    /// The open session.
    pub fn current(&self) -> Option<&Session> {
        match &self.screen {
            Screen::Session(id) => self.session(id),
            Screen::Home | Screen::Automations | Screen::Notes => None,
        }
    }

    /// The login a session runs as, as everywhere shows it: "Claude Code · Work".
    pub fn login_label(&self, s: &Session) -> String {
        let agents = self.info.as_ref().map(|i| i.agents.as_slice()).unwrap_or_default();
        let key = s.profile_id.clone().unwrap_or_else(|| s.agent.as_str().to_string());
        agents.iter().find(|a| login_key(a) == key).map(|a| a.label()).unwrap_or_else(|| s.agent.display_name().to_string())
    }

    /// Whether a session matches the sidebar's search (its title, login or workspace).
    fn matches_search(&self, x: &Session, q: &str) -> bool {
        q.is_empty()
            || x.title.as_deref().unwrap_or("").to_lowercase().contains(q)
            || self.login_label(x).to_lowercase().contains(q)
            || self.workspace(&x.workspace_id).is_some_and(|w| w.name.to_lowercase().contains(q))
    }

    fn search_query(&self) -> String {
        self.search.text.trim().to_lowercase()
    }

    /// The sidebar's Pinned section, in the order they were pinned, filtered by the search.
    pub fn pinned_sessions(&self) -> Vec<&Session> {
        let q = self.search_query();
        let mut l: Vec<&Session> = self.sessions.iter().filter(|x| x.pinned_at.is_some() && self.matches_search(x, &q)).collect();
        l.sort_by_key(|x| (x.pinned_at, x.created_at));
        l
    }

    /// Sessions as the sidebar lists them under Workspaces: Quick tasks first, then the
    /// rest by name so groups stay put while agents work, newest first inside each,
    /// filtered by the search field. Pinned sessions are in the Pinned section instead.
    pub fn sidebar_groups(&self) -> Vec<(&Workspace, Vec<&Session>)> {
        let q = self.search_query();
        let mut ws: Vec<&Workspace> = self.workspaces.iter().collect();
        ws.sort_by_cached_key(|w| (w.kind != WorkspaceKind::Scratch, w.name.to_lowercase(), w.workspace_id.clone()));
        let mut out = vec![];
        for w in ws {
            let mut list: Vec<&Session> = self
                .sessions
                .iter()
                .filter(|x| x.workspace_id == w.workspace_id && x.pinned_at.is_none() && self.matches_search(x, &q))
                .collect();
            if list.is_empty() {
                continue;
            }
            list.sort_by_key(|x| std::cmp::Reverse(x.updated_at));
            out.push((w, list));
        }
        out
    }

    /// Whether a workspace's group is collapsed; a search shows every match regardless.
    pub fn group_collapsed(&self, ws: &str) -> bool {
        self.search_query().is_empty() && self.collapsed.contains(ws)
    }

    /// The rows a group shows, and how many "Show N more" hides: the newest
    /// [`GROUP_SHOWN`], and past them any session that needs you or is working.
    pub fn group_rows<'a>(&self, ws: &str, list: &[&'a Session]) -> (Vec<&'a Session>, usize) {
        if self.group_collapsed(ws) {
            return (vec![], 0);
        }
        if !self.search_query().is_empty() || self.show_all.contains(ws) || list.len() <= GROUP_SHOWN {
            return (list.to_vec(), 0);
        }
        let shown: Vec<&Session> = list
            .iter()
            .enumerate()
            .filter(|(i, x)| *i < GROUP_SHOWN || matches!(Status::of(x.state), Status::NeedsYou | Status::Working))
            .map(|(_, x)| *x)
            .collect();
        let hidden = list.len() - shown.len();
        (shown, hidden)
    }

    /// Collapse a workspace's group, or expand it again; remembered between launches.
    pub fn toggle_group(&mut self, ws: &str) {
        if !self.collapsed.remove(ws) {
            self.collapsed.insert(ws.to_string());
        }
        if self.save_prefs {
            // Load-modify-save, so the other remembered things (which host to
            // connect to) survive this one.
            let mut p = crate::prefs::load();
            p.collapsed = self.collapsed.clone();
            crate::prefs::save(&p);
        }
    }

    /// Drag the sidebar's edge to `w` (clamped); `save` on release keeps it between launches.
    pub fn resize_sidebar(&mut self, w: f32, save: bool) {
        self.sidebar_w = Some(w.clamp(SIDEBAR_MIN_W, SIDEBAR_MAX_W));
        if save && self.save_prefs {
            let mut p = crate::prefs::load();
            p.sidebar_w = self.sidebar_w;
            crate::prefs::save(&p);
        }
    }

    /// Session ids in sidebar order, as the rows show: Pinned, then each group's visible
    /// rows. ⌥⌘↑/↓ walk it and ⌘1…⌘9 number the live ones in this order.
    pub fn sidebar_order(&self) -> Vec<String> {
        let mut out: Vec<String> = self.pinned_sessions().into_iter().map(|x| x.session_id.clone()).collect();
        for (w, l) in self.sidebar_groups() {
            out.extend(self.group_rows(&w.workspace_id, &l).0.into_iter().map(|x| x.session_id.clone()));
        }
        out
    }
    pub fn numbered(&self) -> Vec<String> {
        self.sidebar_order().into_iter().filter(|id| self.session(id).is_some_and(|x| !x.state.is_terminal())).take(9).collect()
    }

    /// The host can pin sessions (protocol 8).
    pub fn has_pins(&self) -> bool {
        self.info.as_ref().is_some_and(|i| i.protocol_version >= 8)
    }

    /// Pin a session to the top of the sidebar, or unpin it. The row moves at once; the
    /// host's reply (and its `session.changed`) confirm it.
    pub fn set_pinned(&mut self, id: &str, pinned: bool) {
        let Some(x) = self.sessions.iter_mut().find(|x| x.session_id == id) else { return };
        if pinned == x.pinned_at.is_some() {
            return;
        }
        x.pinned_at = pinned.then(crate::view::now_ms);
        self.hover_row = None;
        self.act(if pinned { "Pin session" } else { "Unpin session" }, m::SESSION_PIN, json!({"session_id": id, "pinned": pinned}), Refresh::None);
    }

    /// Open the sidebar's search field (⌘F / Ctrl+F, the search button).
    pub fn open_search(&mut self) {
        self.sidebar_open = true;
        self.search_open = true;
        self.search.focused = true;
    }

    /// Escape in the search field: clear it, then close it.
    pub fn close_search(&mut self) {
        if !self.search.text.is_empty() {
            self.search.replace_text("");
        } else {
            self.search_open = false;
            self.search.focused = false;
        }
    }

    /// One of the host's own tools, by name ("create_pull_request").
    pub fn builtin(&self, name: &str) -> Option<&BuiltinTool> {
        self.builtins.iter().find(|b| b.name == name)
    }

    /// Let one of the host's own tools run without asking, or ask first again.
    pub fn set_builtin_policy(&self, name: &str, policy: ToolPolicy) {
        self.call(Tag::BuiltinSet { announce: None }, m::BUILTIN_SET_POLICY, json!({"name": name, "policy": policy}));
    }

    /// Whether an approval's card offers "Approve and Don't Ask Again": one of the host's
    /// own tools that Settings can let run without asking, but never a force-push; for a
    /// command, the prefix the host offers ("gh pr edit"), never for a shell or a build tool.
    pub fn can_stop_asking(&self, a: &PendingApproval) -> bool {
        if a.connector_id != BUILTIN_CONNECTOR_ID {
            return false;
        }
        if a.tool == "run_command" {
            return match (allow_rule(a), self.builtin("run_command")) {
                (Some(rule), Some(b)) => !b.allowed.contains(&rule),
                _ => false,
            };
        }
        a.arguments["force"] != json!(true) && self.builtin(&a.tool).is_some_and(|b| b.can_auto && b.policy == ToolPolicy::Ask)
    }

    /// "Approve and Don't Ask Again": stop asking for this tool (or this command), and
    /// approve this request.
    pub fn approve_and_stop_asking(&self, approval_id: &str) {
        let Some(a) = self.approvals.iter().find(|a| a.approval_id == approval_id) else { return };
        if !self.can_stop_asking(a) {
            return self.respond(approval_id, true);
        }
        if let Some(rule) = allow_rule(a).filter(|_| a.tool == "run_command") {
            self.call(Tag::CommandSet { announce: Some(rule.clone()) }, m::BUILTIN_SET_POLICY, json!({"name": "run_command", "policy": ToolPolicy::Auto, "command": rule}));
        } else {
            self.call(Tag::BuiltinSet { announce: Some(a.tool.clone()) }, m::BUILTIN_SET_POLICY, json!({"name": a.tool, "policy": ToolPolicy::Auto}));
        }
        self.respond(approval_id, true);
    }

    /// Ask again before a command prefix runs ("gh pr edit"), from Settings.
    pub fn forget_command(&self, rule: &[String]) {
        self.call(Tag::CommandSet { announce: None }, m::BUILTIN_SET_POLICY, json!({"name": "run_command", "policy": ToolPolicy::Ask, "command": rule}));
    }

    /// The toast after "Always Allow" on a command: which commands and where to undo it.
    pub fn command_no_ask_line(&self, rule: &[String]) -> String {
        let place = if self.look.mac() { "Settings → Connectors" } else { "Preferences → Connectors" };
        format!("\u{201c}{}\u{201d} commands now run without asking — change this in {place}", rule.join(" "))
    }

    /// The toast after "Approve and Don't Ask Again": what changed and where to undo it.
    pub fn no_ask_line(&self, name: &str) -> String {
        let place = if self.look.mac() { "Settings → Connectors" } else { "Preferences → Connectors" };
        match name {
            "create_pull_request" => format!("Pull requests now open without asking — change this in {place}"),
            "git_push" => format!("Pushes now run without asking (a force-push still asks) — change this in {place}"),
            other => {
                let title = self.builtin(other).map(|b| b.title.clone()).unwrap_or_else(|| crate::session::humanize(other));
                format!("{title} no longer asks — change this in {place}")
            }
        }
    }

    /// Answer an approval.
    pub fn respond(&self, approval_id: &str, approve: bool) {
        let (what, decision) = if approve { ("Approve", "approve") } else { ("Deny", "deny") };
        self.act(what, m::APPROVAL_RESPOND, json!({"approval_id": approval_id, "decision": decision}), Refresh::Approvals);
    }

    /// The first approval waiting in the open session.
    pub fn current_approval(&self) -> Option<&PendingApproval> {
        let Screen::Session(id) = &self.screen else { return None };
        self.approvals.iter().find(|a| &a.session_id == id)
    }

    /// Show a folder in the file manager.
    pub fn reveal(&mut self, path: &str) {
        let path = match path.strip_prefix("~/") {
            Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
            None => path.to_string(),
        };
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        if let Err(e) = std::process::Command::new(opener).arg(&path).spawn() {
            self.error = Some(format!("Could not open {path}: {e}"));
        }
    }

    /// Answer the pairing on screen: the codes match (pair) or not (refuse).
    pub fn confirm_pairing(&self, accept: bool) {
        let Some(p) = &self.pairing else { return };
        let what = if accept { "Confirm pairing" } else { "Refuse pairing" };
        self.act(what, m::DEVICE_PAIR_CONFIRM, json!({"pairing_id": p.pairing_id, "accept": accept}), Refresh::Devices);
    }

    /// Open the form that pairs this app with a remote host: paste the pairing
    /// code that host's `agenthost device pair` shows.
    pub fn start_remote_pair(&mut self) {
        self.cancel_remote_pair();
        let name = if self.hostname.is_empty() {
            "Desktop".to_string()
        } else {
            format!("Desktop on {}", self.hostname)
        };
        self.pair_remote = Some(PairRemote {
            payload: TextEdit::new(""),
            name: TextEdit::new(name),
            code: None,
            error: None,
            busy: false,
            attempt: 0,
        });
        if let Some(p) = self.pair_remote.as_mut() {
            p.payload.focused = true;
        }
    }

    /// Close the pairing form, giving up a request under way: its answer, if
    /// one still comes, changes nothing.
    pub fn cancel_remote_pair(&mut self) {
        let Some(p) = self.pair_remote.take() else { return };
        if p.busy {
            if let Some(b) = &self.bridge {
                b.send(ToHost::CancelPair);
            }
        }
    }

    /// Send the pairing request now in the form.
    pub fn send_remote_pair(&mut self) {
        let Some(p) = &self.pair_remote else { return };
        if p.busy {
            return;
        }
        let payload = p.payload.text.trim().to_string();
        if payload.is_empty() {
            return;
        }
        let name = {
            let n = p.name.text.trim().to_string();
            if n.is_empty() { "AgentOS Desktop".to_string() } else { n }
        };
        self.pair_attempts += 1;
        let attempt = self.pair_attempts;
        if let Some(p) = self.pair_remote.as_mut() {
            p.busy = true;
            p.error = None;
            p.code = None;
            p.attempt = attempt;
        }
        if let Some(b) = &self.bridge {
            b.send(ToHost::PairRemote { attempt, payload, name });
        }
    }

    /// Pairing finished: keep the keys, and go to the host unless this app is
    /// happily using a local one.
    fn paired_remotely(&mut self, r: Result<agenthost_cli::remote::DeviceState, String>) {
        match r {
            Ok(st) => {
                let name = st.host_name.clone();
                // A laptop that has remembered nothing yet has no config
                // folder; the keys are the first thing kept there.
                let path = remote_state_path();
                if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(e) = st.save(&path) {
                    if let Some(p) = self.pair_remote.as_mut() {
                        p.error = Some(format!("Paired, but the keys could not be kept: {e:#}"));
                        p.busy = false;
                    }
                    return;
                }
                self.remote_host = Some(name.clone());
                self.pair_remote = None;
                let was_local = self.conn == Conn::Connected && !self.link_remote;
                if was_local {
                    // This computer has its own host in use; switching is a choice.
                    self.notice = Some(format!("Paired with {name}. Switch to it in Settings \u{2192} General."));
                } else {
                    // Nothing local to keep working with: go there now, and next
                    // launch too.
                    let mut prefs = crate::prefs::load();
                    prefs.prefer_remote = true;
                    crate::prefs::save(&prefs);
                    self.conn = Conn::Connecting;
                    if let Some(b) = &self.bridge {
                        b.send(ToHost::UseRemote(remote_state_path()));
                    }
                    self.notice = Some(format!("Paired with {name}."));
                }
            }
            Err(e) => {
                if let Some(p) = self.pair_remote.as_mut() {
                    p.error = Some(e);
                    p.busy = false;
                    p.code = None;
                }
            }
        }
    }

    /// Connect to the paired remote host: now, and at every launch after.
    pub fn connect_remote(&mut self) {
        let Some(b) = &self.bridge else { return };
        self.conn = Conn::Connecting;
        b.send(ToHost::UseRemote(remote_state_path()));
        let mut prefs = crate::prefs::load();
        prefs.prefer_remote = true;
        crate::prefs::save(&prefs);
    }

    /// Back to this computer's own host, by choice (not a fallback).
    pub fn use_local_host(&mut self) {
        let Some(b) = &self.bridge else { return };
        self.conn = Conn::Connecting;
        b.send(ToHost::UseLocal);
        let mut prefs = crate::prefs::load();
        prefs.prefer_remote = false;
        crate::prefs::save(&prefs);
    }

    /// The app talks to another host from here on: everything the last one
    /// said, and everything on screen that names its sessions, workspaces or
    /// connectors, goes. Settings, the pairing form and the window stay.
    fn forget_host(&mut self) {
        if self.live.take().is_some() {
            self.mic = Mic::Idle;
            self.send_after_dictation = false;
        }
        if matches!(self.screen, Screen::Session(_)) {
            self.screen = Screen::Home;
        }
        self.menu = None;
        self.new_open = false;
        self.auto_open = false;
        self.viewing = None;
        self.info = None;
        self.voice = None;
        self.screen_share = None;
        self.workspaces.clear();
        self.sessions.clear();
        self.archived.clear();
        self.connectors.clear();
        self.builtins.clear();
        self.devices.clear();
        self.approvals.clear();
        self.project_roots.clear();
        self.automations.clear();
        self.runs.clear();
        self.notes.clear();
        self.terminals_reset();
        self.suggestions.clear();
        self.transcripts.clear();
        self.subscribed.clear();
        self.sub_ids.clear();
        self.behind.clear();
        self.expanded.clear();
        self.show_all.clear();
        self.prompt = TextEdit::new("");
        self.prompt_for = None;
        self.drafts.clear();
        self.files.clear();
        self.images.clear();
        self.signing_in.clear();
        self.toasts.clear();
        self.toasted_approvals.clear();
        self.side_closed.clear();
        self.side_tab.clear();
        self.side_pending.clear();
        self.pairing = None;
        self.sel_agent = None;
        self.sel_ws = None;
        self.sel_conn = None;
        self.sel_root = None;
        self.sel_auto = None;
        self.sel_note = None;
        self.new_ws = None;
        self.new_agent = None;
        self.new_note_ws = None;
        self.start_note = None;
        self.pick_added = None;
        self.host_for = None;
        self.renaming = None;
        self.session_rename = None;
        self.agent_open = None;
        self.hover_row = None;
        self.confirm_archive = None;
        self.confirm_delete_auto = None;
        self.confirm_rotate = None;
        self.confirm_delete_note = None;
        self.adding_profile = false;
        self.adding_connector = false;
        self.adding_workspace = false;
    }

    /// Add a folder as a workspace; from the New session sheet it's then selected.
    pub fn add_workspace_path(&mut self, path: &str, select: bool) {
        self.act("Add workspace", m::WORKSPACE_ADD, json!({"path": path}), Refresh::Workspaces);
        if select {
            self.pick_added = Some(path.to_string());
        }
    }

    /// Add the profile from the host screen's form.
    pub fn add_profile(&mut self) {
        let name = self.profile_name.text.trim().to_string();
        let dir = self.profile_dir.text.trim().to_string();
        let Some(agent) = self.profile_agent else {
            self.error = Some("Pick the agent the profile is for.".into());
            return;
        };
        if name.is_empty() || dir.is_empty() {
            self.error = Some("A profile needs a name and its config folder.".into());
            return;
        }
        self.act("Add profile", m::AGENT_PROFILE_ADD, json!({"agent": agent, "name": name, "config_dir": dir}), Refresh::HostInfo);
        self.adding_profile = false;
        self.profile_name.clear();
        self.profile_dir.clear();
    }

    /// Replace the project folders (an empty list keeps phones from creating projects).
    pub fn set_project_roots(&self, roots: Vec<String>) {
        self.act("Change project folders", m::PROJECT_SET_ROOTS, json!({"roots": roots}), Refresh::ProjectRoots);
    }

    pub fn act(&self, what: &str, method: &'static str, params: Value, refresh: Refresh) {
        self.call(Tag::Action { what: what.into(), refresh }, method, params);
    }

    pub fn refresh(&self, r: Refresh) {
        match r {
            Refresh::None => {}
            Refresh::Workspaces => self.call(Tag::Workspaces, m::WORKSPACE_LIST, json!({})),
            Refresh::Connectors => {
                self.call(Tag::Connectors, m::CONNECTOR_LIST, json!({}));
                self.call(Tag::Builtins, m::BUILTIN_LIST, json!({}));
            }
            Refresh::Devices => self.call(Tag::Devices, m::DEVICE_LIST, json!({})),
            Refresh::Approvals => self.call(Tag::Approvals, m::APPROVAL_LIST, json!({})),
            Refresh::HostInfo => self.call(Tag::HostInfo, m::HOST_INFO, json!({})),
            Refresh::ProjectRoots => self.call(Tag::ProjectRoots, m::PROJECT_BROWSE, json!({})),
            Refresh::Archived => self.call(Tag::Archived, m::SESSION_LIST, json!({"archived": true})),
            Refresh::Automations => self.call(Tag::Automations, m::AUTOMATION_LIST, json!({})),
            Refresh::Notes => self.call(Tag::Notes, m::NOTE_LIST, json!({})),
        }
    }

    pub fn refresh_all(&self) {
        for r in [Refresh::HostInfo, Refresh::Workspaces, Refresh::Connectors, Refresh::Devices, Refresh::Approvals, Refresh::ProjectRoots, Refresh::Automations, Refresh::Notes] {
            self.refresh(r);
        }
    }

    /// Open a session: subscribe from what we already have, so nothing is lost
    /// or repeated when switching or reconnecting.
    pub fn open_session(&mut self, id: &str) {
        // What arrived in the session being left was on screen.
        self.mark_seen(false);
        if self.screen == Screen::Notes {
            self.save_note();
        }
        self.menu = None;
        self.viewing = None;
        if self.session_rename.as_deref() != Some(id) {
            self.session_rename = None;
        }
        self.swap_draft(id);
        self.screen = Screen::Session(id.to_string());
        self.prompt.focused = true;
        if self.agent_open.as_ref().is_some_and(|(s, _)| s != id) {
            self.agent_open = None;
        }
        self.ensure_subscribed(id);
        self.files_sync();
        self.mark_seen(false);
    }

    /// The agent finished a turn since you last had the session open: its dot says so until
    /// you open it.
    pub fn unread(&self, x: &Session) -> bool {
        if Status::of(x.state) != Status::YourTurn || matches!(&self.screen, Screen::Session(id) if *id == x.session_id) {
            return false;
        }
        let Some(seen) = &self.seen else { return false };
        x.last_seq > seen.get(&x.session_id).copied().unwrap_or(0)
    }

    /// Mark the open session seen up to its newest event, remembered between launches. The
    /// first full session list (`listed`) ever marks every session seen.
    pub(crate) fn mark_seen(&mut self, listed: bool) {
        if self.seen.is_none() && !listed {
            return;
        }
        let mut changed = false;
        let seen = self.seen.get_or_insert_with(|| {
            changed = true;
            self.sessions.iter().map(|x| (x.session_id.clone(), x.last_seq)).collect()
        });
        if let Screen::Session(id) = &self.screen {
            // The list's number, or the transcript's when events have come in since.
            let newest = self.sessions.iter().find(|x| &x.session_id == id).map(|x| x.last_seq).max(self.transcripts.get(id).map(|t| t.last_seq));
            if let Some(newest) = newest {
                let at = seen.entry(id.clone()).or_insert(0);
                if *at < newest {
                    *at = newest;
                    changed = true;
                }
            }
        }
        if changed && self.save_prefs {
            // Kept across hosts (a paired one and this computer's); only a long history is
            // cut back to the sessions listed now.
            if seen.len() > 2000 {
                seen.retain(|id, _| self.sessions.iter().any(|x| &x.session_id == id));
            }
            let mut p = crate::prefs::load();
            p.seen = Some(seen.clone());
            crate::prefs::save(&p);
        }
    }

    /// Put the composer's text away with its session and bring back `id`'s.
    fn swap_draft(&mut self, id: &str) {
        if self.prompt_for.as_deref() == Some(id) {
            return;
        }
        // A dictation writes into the text it started in.
        self.mic_cancel();
        if let Some(old) = self.prompt_for.take() {
            let mut text = std::mem::replace(&mut self.prompt, TextEdit::new(""));
            text.focused = false;
            if !text.text.is_empty() {
                self.drafts.insert(old, text);
            }
            if let Some(d) = self.drafts.remove(id) {
                self.prompt = d;
            }
        }
        self.prompt_for = Some(id.to_string());
    }

    pub fn ensure_subscribed(&mut self, id: &str) {
        if self.conn != Conn::Connected || self.subscribed.contains(id) {
            return;
        }
        let after = self.transcripts.get(id).map(|t| t.last_seq).unwrap_or(0);
        self.subscribed.insert(id.to_string());
        self.call(Tag::Subscribed(id.into()), m::EVENTS_SUBSCRIBE, json!({"session_id": id, "after_seq": after}));
    }

    /// A session by id: a listed one, else an archived one.
    pub fn session(&self, id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.session_id == id).or_else(|| self.archived.iter().find(|s| s.session_id == id))
    }

    pub fn workspace(&self, id: &str) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.workspace_id == id)
    }

    pub fn send_prompt(&mut self) {
        let Screen::Session(id) = &self.screen else { return };
        // While dictating, Return stops the microphone and sends once the last words are in.
        match self.mic {
            Mic::Recording(_) => {
                self.send_after_dictation = true;
                self.mic_press();
                return;
            }
            Mic::Transcribing => {
                self.send_after_dictation = true;
                return;
            }
            Mic::Idle => {}
        }
        let id = id.clone();
        let text = self.prompt.text.trim().to_string();
        let files = self.files.get(&id).map(|f| f.as_slice()).unwrap_or(&[]);
        if files.iter().any(|f| matches!(f.state, FileState::Uploading)) {
            self.notice = Some("Wait a moment: files are still uploading.".into());
            return;
        }
        let ready: Vec<&Attachment> = files
            .iter()
            .filter_map(|f| match &f.state {
                FileState::Ready(a) => Some(a),
                _ => None,
            })
            .collect();
        let ids: Vec<String> = ready.iter().map(|a| a.attachment_id.clone()).collect();
        let shown = json!(ready);
        if text.is_empty() && ids.is_empty() {
            return;
        }
        // `/btw <question>` asks on the side, when the agent can.
        let side = self.session(&id).is_some_and(|x| x.side_questions);
        if let Some(q) = parse_btw(&text).filter(|_| side) {
            if q.is_empty() {
                self.notice = Some("Type your question after /btw.".into());
                return;
            }
            let q = q.to_string();
            self.side_ask(&id, None, &q);
            self.prompt.clear();
            return;
        }
        let prompt_id = uuid::Uuid::new_v4().to_string();
        let mut params = json!({"session_id": id, "prompt_id": prompt_id, "text": text});
        let mut payload = json!({"text": text, "prompt_id": prompt_id});
        if !ids.is_empty() {
            params["attachments"] = json!(ids);
            payload["attachments"] = shown;
        }
        // Shown now, not after the round trip: the bubble, and "Working…" under it.
        self.transcripts.entry(id.clone()).or_default().pending.push(PendingPrompt { prompt_id: prompt_id.clone(), payload });
        self.call(Tag::Prompt { session: id.clone(), prompt_id }, m::SESSION_PROMPT, params);
        self.prompt.clear();
        self.files.remove(&id);
    }

    /// Put the open session's last prompt back in the empty composer. True if there was one.
    pub fn recall_last_prompt(&mut self) -> bool {
        let Screen::Session(id) = &self.screen else { return false };
        let last = self.transcripts.get(id).and_then(|t| t.events.iter().rev().find(|e| e.kind == EventType::Prompt)).and_then(|e| e.payload["text"].as_str()).map(String::from);
        match last.filter(|t| !t.is_empty()) {
            Some(t) => {
                self.prompt.replace_text(&t);
                true
            }
            None => false,
        }
    }

    /// Whether the host gives back images sent with messages (protocol 7).
    pub fn host_keeps_images(&self) -> bool {
        self.info.as_ref().map(|i| i.protocol_version >= 7).unwrap_or(false)
    }

    /// Ask the host for the open session's images not loaded yet; let go of other
    /// sessions' images. Cheap when there's nothing to do: `poll` calls it each time.
    pub fn fetch_images(&mut self) {
        let Screen::Session(id) = &self.screen else { return };
        if self.conn != Conn::Connected || !self.host_keeps_images() {
            return;
        }
        let events = self.transcripts.get(id).map(|t| t.events.as_slice()).unwrap_or_default();
        let wanted: Vec<String> = events.iter().filter(|e| e.kind == EventType::Prompt).flat_map(|e| sent_images(&e.payload)).map(|(a, _)| a).collect();
        let keep: HashSet<&String> = wanted.iter().collect();
        self.images.retain(|k, _| keep.contains(k));
        for a in wanted {
            if self.images.contains_key(&a) {
                continue;
            }
            self.images.insert(a.clone(), SentImage::Loading);
            if let Some(b) = &self.bridge {
                b.send(crate::bridge::ToHost::ReadAttachment { session_id: id.clone(), attachment_id: a });
            }
        }
    }

    /// Open a sent image in the viewer, or close it with `None`.
    pub fn view_image(&mut self, attachment_id: Option<String>) {
        self.viewing = attachment_id;
        self.menu = None;
    }

    /// Whether the host takes attachments and voice (protocol 2).
    pub fn host_has_media(&self) -> bool {
        self.info.as_ref().map(|i| i.protocol_version >= 2).unwrap_or(false)
    }

    /// Attach files to the current session's next message.
    pub fn attach(&mut self, paths: &[std::path::PathBuf]) {
        let Screen::Session(id) = self.screen.clone() else { return };
        if !self.host_has_media() {
            self.notice = Some("Update agenthostd to attach files.".into());
            return;
        }
        for p in paths {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string());
            if p.is_dir() {
                self.notice = Some(format!("{name} is a folder. Attach files, or tell the agent where the folder is."));
                continue;
            }
            let list = self.files.entry(id.clone()).or_default();
            if list.len() >= MAX_ATTACHMENTS_PER_PROMPT {
                self.notice = Some(format!("A message can carry up to {MAX_ATTACHMENTS_PER_PROMPT} files."));
                break;
            }
            let local_id = self.next_local_id;
            self.next_local_id += 1;
            list.push(PendingFile { local_id, name, state: FileState::Uploading });
            if let Some(b) = &self.bridge {
                b.send(crate::bridge::ToHost::Upload { local_id, path: p.clone() });
            }
        }
        self.prompt.focused = true;
    }

    /// A paste into the composer: files and images become attachments. False lets the editor
    /// paste the text.
    pub fn paste_into_composer(&mut self, content: &rune::ClipboardContent) -> bool {
        match paste_as(content) {
            PasteAs::Text => false,
            PasteAs::Files(paths) => {
                self.attach(&paths);
                true
            }
            PasteAs::Image(png) => {
                self.attach_bytes(png, "image/png");
                true
            }
        }
    }

    /// Attach a pasted image to the current session's next message.
    pub fn attach_bytes(&mut self, data: Vec<u8>, mime: &str) {
        let Screen::Session(id) = self.screen.clone() else { return };
        if !self.host_has_media() {
            self.notice = Some("Update agenthostd to attach files.".into());
            return;
        }
        if data.len() as u64 > MAX_ATTACHMENT_BYTES {
            self.notice = Some(format!("That image is too large: files can be up to {} MB.", MAX_ATTACHMENT_BYTES / (1024 * 1024)));
            return;
        }
        let list = self.files.entry(id).or_default();
        if list.len() >= MAX_ATTACHMENTS_PER_PROMPT {
            self.notice = Some(format!("A message can carry up to {MAX_ATTACHMENTS_PER_PROMPT} files."));
            return;
        }
        let taken: Vec<&str> = list.iter().map(|f| f.name.as_str()).collect();
        let name = pasted_image_name(&taken);
        let local_id = self.next_local_id;
        self.next_local_id += 1;
        list.push(PendingFile { local_id, name: name.clone(), state: FileState::Uploading });
        if let Some(b) = &self.bridge {
            b.send(crate::bridge::ToHost::UploadBytes { local_id, name, mime: mime.to_string(), data });
        }
        self.prompt.focused = true;
    }

    pub fn remove_file(&mut self, local_id: u64) {
        for list in self.files.values_mut() {
            list.retain(|f| f.local_id != local_id);
        }
    }

    /// The microphone button: start, or stop and transcribe; offer the
    /// model download the first time.
    pub fn mic_press(&mut self) {
        match std::mem::replace(&mut self.mic, Mic::Idle) {
            Mic::Idle => {
                let Some(v) = self.voice.clone() else {
                    self.notice = Some("Update agenthostd to use voice input.".into());
                    return;
                };
                if !v.ready {
                    if let Some(p) = &v.progress {
                        self.notice = Some(format!("The speech model is still downloading ({}%).", p.received * 100 / p.total.max(1)));
                    } else {
                        self.ask_voice_download = true;
                    }
                    return;
                }
                match crate::audio::Recorder::start() {
                    Ok(r) => {
                        self.send_after_dictation = false;
                        if v.streaming {
                            self.begin_live(r.rate());
                        }
                        self.mic = Mic::Recording(r);
                        self.prompt.focused = true;
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Mic::Recording(r) => {
                if let Some(gen) = self.live.as_ref().map(|l| l.gen) {
                    let pcm = r.take_new();
                    drop(r);
                    self.mic = Mic::Transcribing;
                    self.live_send(crate::bridge::Live::Finish { gen, pcm, cancel: false });
                    return;
                }
                match r.finish() {
                    Some(wav) => {
                        self.mic = Mic::Transcribing;
                        if let Some(b) = &self.bridge {
                            b.send(crate::bridge::ToHost::Transcribe { wav });
                        }
                    }
                    None if std::mem::take(&mut self.send_after_dictation) => self.send_prompt(),
                    None => self.notice = Some("That was too short to hear anything. Click the microphone, talk, then click it again.".into()),
                }
            }
            Mic::Transcribing => self.mic = Mic::Transcribing,
        }
    }

    fn live_send(&self, l: crate::bridge::Live) {
        if let Some(b) = &self.bridge {
            b.send(crate::bridge::ToHost::Live(l));
        }
    }

    /// Open a live dictation at the caret (replacing a selection).
    fn begin_live(&mut self, sample_rate: u32) {
        self.next_dictation += 1;
        let gen = self.next_dictation;
        let (a, b) = self.prompt.range();
        if a != b {
            self.prompt.insert("");
        }
        self.live = Some(LiveDictation { gen, start: self.prompt.range().0, shown: String::new() });
        self.live_send(crate::bridge::Live::Begin { gen, sample_rate });
    }

    /// While dictating live: send what the microphone heard since the last call.
    pub fn pump_dictation(&mut self) {
        let (Mic::Recording(r), Some(l)) = (&self.mic, &self.live) else { return };
        let pcm = r.take_new();
        if !pcm.is_empty() {
            self.live_send(crate::bridge::Live::Audio { gen: l.gen, pcm });
        }
    }

    /// Put the words heard so far in the composer, in place of the last ones shown.
    pub fn show_live(&mut self, text: &str) {
        let Some(l) = self.live.as_mut() else { return };
        let t = &self.prompt.text;
        // Where the words are: where they were put, unless an edit moved them.
        let start = if t.get(l.start..l.start + l.shown.len()) == Some(l.shown.as_str()) {
            l.start
        } else if let Some(i) = (!l.shown.is_empty()).then(|| t.rfind(&l.shown)).flatten() {
            i
        } else if l.shown.is_empty() {
            let mut i = l.start.min(t.len());
            while !t.is_char_boundary(i) {
                i -= 1;
            }
            i
        } else {
            // The words were edited away: start again after the text.
            l.shown.clear();
            t.len()
        };
        let end = start + l.shown.len();
        let text = text.trim();
        let mut s = String::new();
        if !text.is_empty() {
            if t[..start].chars().last().is_some_and(|c| !c.is_whitespace()) {
                s.push(' ');
            }
            s.push_str(text);
            if t[end..].chars().next().is_some_and(|c| !c.is_whitespace()) {
                s.push(' ');
            }
        }
        if s == l.shown && start == l.start {
            return;
        }
        let (ca, cb) = self.prompt.range();
        self.prompt.set_selection(start, end);
        self.prompt.insert(&s);
        let new_end = start + s.len();
        // The caret follows the words unless it was put somewhere else.
        if cb < start {
            self.prompt.set_selection(ca, cb);
        } else if ca > end {
            self.prompt.set_selection(ca + new_end - end, cb + new_end - end);
        } else {
            self.prompt.set_caret(start + s.trim_end().len());
        }
        l.start = start;
        l.shown = s;
    }

    /// A live dictation ended (final text, or a failure): maybe send, as Return asked.
    fn end_live(&mut self, ok: bool) {
        let heard = self.live.take().is_some_and(|l| !l.shown.is_empty());
        if !matches!(self.mic, Mic::Idle) {
            self.mic = Mic::Idle;
        }
        let send = std::mem::take(&mut self.send_after_dictation);
        if !ok {
            return;
        }
        if send {
            self.send_prompt();
        } else if !heard {
            self.notice = Some("No speech was recognised. Try again a little closer to the microphone.".into());
        }
        self.prompt.focused = true;
    }

    /// Stop recording without using it: a live dictation's words come back out.
    pub fn mic_cancel(&mut self) {
        if !matches!(self.mic, Mic::Recording(_)) {
            return;
        }
        self.mic = Mic::Idle;
        self.send_after_dictation = false;
        if let Some(l) = self.live.take() {
            self.live_send(crate::bridge::Live::Finish { gen: l.gen, pcm: vec![], cancel: true });
            let t = &self.prompt.text;
            if !l.shown.is_empty() && t.get(l.start..l.start + l.shown.len()) == Some(l.shown.as_str()) {
                self.prompt.set_selection(l.start, l.start + l.shown.len());
                self.prompt.insert("");
            }
        }
    }

    pub fn download_voice(&mut self) {
        self.ask_voice_download = false;
        self.call(Tag::Voice, m::VOICE_DOWNLOAD, json!({}));
    }

    /// Put dictated text at the caret, spaced from what is there.
    fn insert_dictation(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            self.notice = Some("No speech was recognised. Try again a little closer to the microphone.".into());
            return;
        }
        let (start, end) = self.prompt.range();
        let before = self.prompt.text[..start].chars().last();
        let after = self.prompt.text[end..].chars().next();
        let mut s = String::new();
        if before.is_some_and(|c| !c.is_whitespace()) {
            s.push(' ');
        }
        s.push_str(text);
        if after.is_some_and(|c| !c.is_whitespace()) {
            s.push(' ');
        }
        self.prompt.insert(&s);
        self.prompt.focused = true;
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
            FromHost::RemotePaired(name) => {
                // Only ever narrows to Some: a missing file is not news.
                if name.is_some() {
                    self.remote_host = name;
                }
            }
            FromHost::Link { remote, switched } => {
                self.link_remote = remote;
                // The local socket manages its host and a paired device never
                // does; `Connected` says what this link really holds.
                self.admin = !remote;
                if switched {
                    self.forget_host();
                }
            }
            // Answers to the request the form is waiting on; any other is from
            // one cancelled or replaced.
            FromHost::PairCode { attempt, code } => {
                if let Some(p) = self.pair_remote.as_mut().filter(|p| p.busy && p.attempt == attempt) {
                    p.code = Some(code);
                }
            }
            FromHost::PairResult { attempt, result } => {
                if self.pair_remote.as_ref().is_some_and(|p| p.busy && p.attempt == attempt) {
                    self.paired_remotely(result);
                }
            }
            FromHost::Connected { admin, remote } => {
                self.conn = Conn::Connected;
                self.admin = admin;
                self.link_remote = remote.is_some();
                if let Some(name) = remote {
                    self.remote_host = Some(name);
                }
                self.subscribed.clear();
                self.sub_ids.clear();
                self.behind.clear();
                // Images that failed to load (the host was away) try again.
                self.images.retain(|_, i| !matches!(i, SentImage::Failed(_)));
                self.error = None;
                // Runs may have started or finished while away.
                self.runs.clear();
                self.refresh_all();
                self.call(Tag::Voice, m::VOICE_STATUS, json!({}));
                self.call(Tag::Screen, m::SCREEN_STATUS, json!({}));
                // A new connection: the terminals' screens are drawn again from the host.
                self.terminals_reset();
                if admin {
                    self.call(Tag::Terminals, m::TERMINAL_LIST, json!({}));
                }
                if let Screen::Session(id) = self.screen.clone() {
                    self.ensure_subscribed(&id);
                }
            }
            FromHost::Disconnected(e) => {
                // A live dictation can't finish without the host; keep the words so far.
                if self.live.take().is_some() {
                    self.mic = Mic::Idle;
                    self.send_after_dictation = false;
                }
                if self.conn != Conn::StartingHost || self.info.is_some() {
                    self.conn = Conn::Down(e);
                }
                self.subscribed.clear();
            }
            FromHost::Reply { tag, result } => match (tag, result) {
                (Tag::HostInfo, Ok(v)) => self.info = parse(v),
                (Tag::Voice, Ok(v)) => self.voice = parse(v),
                // An older host without voice: no microphone button.
                (Tag::Voice, Err(_)) => self.voice = None,
                (Tag::Workspaces, Ok(v)) => {
                    self.workspaces = parse(v).unwrap_or_default();
                    if let Some(p) = self.pick_added.clone() {
                        if let Some(w) = self.workspaces.iter().find(|w| w.path == p || p.ends_with(&w.path) || w.path.ends_with(p.trim_start_matches('~'))) {
                            self.new_ws = Some(w.workspace_id.clone());
                            self.pick_added = None;
                        }
                    }
                }
                (Tag::ProjectRoots, Ok(v)) => {
                    self.project_roots = parse::<ProjectBrowse>(v).map(|b| b.roots).unwrap_or_default()
                }
                // An older host without projects.
                (Tag::ProjectRoots, Err(_)) => self.project_roots = vec![],
                (Tag::Screen, Ok(v)) => self.screen_share = parse(v),
                (Tag::Screen, Err(e)) => {
                    // An older host has no screen sharing; anything else is worth saying.
                    if e.code != agenthost_proto::jsonrpc::codes::METHOD_NOT_FOUND {
                        self.error = Some(format!("Screen sharing: {}", e.message));
                    }
                }
                (Tag::Sessions, Ok(v)) => {
                    let all: Vec<Session> = parse(v).unwrap_or_default();
                    self.sessions = all.into_iter().filter(|s| s.archived_at.is_none()).collect();
                    self.mark_seen(true);
                }
                (Tag::Archived, Ok(v)) => {
                    let mut all: Vec<Session> = parse(v).unwrap_or_default();
                    all.retain(|s| s.archived_at.is_some());
                    all.sort_by_key(|s| std::cmp::Reverse(s.archived_at));
                    self.archived = all;
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
                        self.sel_conn = Some(id.clone());
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
                (Tag::Approvals, Ok(v)) => {
                    self.approvals = parse(v).unwrap_or_default();
                    self.toast_approvals();
                }
                (Tag::SideAsked(sess), Ok(v)) => {
                    self.side_pending.remove(&sess);
                    if let Some(r) = parse::<SessionSideAskResult>(v) {
                        self.side_closed.remove(&r.side_id);
                        self.side_tab.insert(sess, r.side_id);
                        self.show_panel(PanelTab::Side);
                    }
                }
                (Tag::SideAsked(sess), Err(e)) => {
                    self.side_pending.remove(&sess);
                    self.error = Some(format!("The side question failed: {}", e.message));
                }
                (Tag::SessionUpdated, Ok(v)) => {
                    if let Some(x) = parse::<Session>(v) {
                        self.upsert_session(x);
                    }
                }
                (Tag::SessionUpdated, Err(e)) => self.error = Some(format!("Could not change the setting: {}", e.message)),
                (Tag::ConnectorSearch(q), Ok(v)) => {
                    if q == self.search_query {
                        self.searching = false;
                        self.suggestions = parse(v).unwrap_or_default();
                    }
                }
                (Tag::ConnectorSearch(q), Err(e)) => {
                    if q == self.search_query {
                        self.searching = false;
                        self.suggestions.clear();
                        self.search_error = Some(e.message);
                    }
                }
                (Tag::Rename(id), Ok(_)) => {
                    if self.renaming.as_deref() == Some(id.as_str()) {
                        self.renaming = None;
                    }
                    self.rename_error = None;
                    self.refresh(Refresh::Connectors);
                }
                (Tag::Rename(id), Err(e)) => self.rename_error = Some((id, e.message)),
                (Tag::SessionRenamed(id), Ok(v)) => {
                    if self.session_rename.as_deref() == Some(id.as_str()) {
                        self.session_rename = None;
                    }
                    if let Some(x) = parse::<Session>(v) {
                        self.upsert_session(x);
                    }
                }
                // The field stays open with what was typed, to fix it.
                (Tag::SessionRenamed(_), Err(e)) => self.error = Some(format!("Could not rename the session: {}", e.message)),
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
                        // The first session in a workspace sets its default login.
                        self.refresh(Refresh::Workspaces);
                    }
                }
                // Started from a note: its text waits in the composer, to edit and send.
                (Tag::SessionFromNote(text), Ok(v)) => {
                    if let Some(s) = parse::<Session>(v) {
                        let id = s.session_id.clone();
                        self.upsert_session(s);
                        self.open_session(&id);
                        if self.prompt.text.is_empty() {
                            self.prompt.replace_text(&text);
                        }
                        self.refresh(Refresh::Workspaces);
                    }
                }
                (Tag::SessionFromNote(_), Err(e)) => self.error = Some(format!("Could not start the session: {}", e.message)),
                (Tag::Notes, Ok(v)) => {
                    self.notes = parse(v).unwrap_or_default();
                    sort_notes(&mut self.notes);
                    self.sync_note_editor();
                }
                // An older host without notes: the screen says to update it.
                (Tag::Notes, Err(_)) => self.notes.clear(),
                (Tag::Changes(id), r) => {
                    let r = r.map_err(|e| e.message).and_then(|v| parse(v).ok_or_else(|| "the host's answer made no sense".to_string()));
                    self.changes_loaded(&id, r);
                }
                (Tag::Files(id), r) => {
                    let r = r.map_err(|e| e.message).and_then(|v| parse(v).ok_or_else(|| "the host's answer made no sense".to_string()));
                    self.files_listed(&id, r);
                }
                (Tag::FileRead { session, path }, r) => {
                    let r = r.map_err(|e| e.message).and_then(|v| parse(v).ok_or_else(|| "the host's answer made no sense".to_string()));
                    self.file_read(&session, &path, r);
                }
                (Tag::Terminals, Ok(v)) => self.terminals_listed(parse::<TerminalList>(v).map(|l| l.terminals).unwrap_or_default()),
                // An older host without terminals: the tab says so when it is opened.
                (Tag::Terminals, Err(_)) => {}
                (Tag::TerminalOpened, Ok(v)) => match parse::<Terminal>(v) {
                    Some(t) => self.terminal_opened(Ok(t)),
                    None => self.terminal_opened(Err("the host's answer made no sense".into())),
                },
                (Tag::TerminalOpened, Err(e)) => self.terminal_opened(Err(e.message)),
                (Tag::TerminalAttached(_), Ok(_)) => {}
                (Tag::TerminalAttached(id), Err(_)) => self.terminal_attach_failed(&id),
                (Tag::NoteSaved { new_gen }, Ok(v)) => {
                    self.note_saving = false;
                    if let Some(n) = parse::<Note>(v) {
                        let id = n.note_id.clone();
                        self.upsert_note(n);
                        // A new note becomes the one shown, unless another was picked meanwhile.
                        if new_gen.is_some_and(|g| g == self.note_gen) && self.new_note_ws.is_some() {
                            self.new_note_ws = None;
                            self.sel_note = Some(id.clone());
                            self.note_base = self.note(&id).map(|n| n.text.clone()).unwrap_or_default();
                        }
                    }
                }
                (Tag::NoteSaved { .. }, Err(e)) => {
                    self.note_saving = false;
                    self.error = Some(format!("Could not save the note: {}", e.message));
                }
                (Tag::NoteAction { .. }, Ok(v)) => {
                    if let Some(n) = parse::<Note>(v) {
                        self.upsert_note(n);
                    }
                }
                (Tag::NoteAction { what }, Err(e)) => {
                    self.error = Some(format!("{what} failed: {}", e.message));
                    self.refresh(Refresh::Notes);
                }
                (Tag::Subscribed(id), Ok(v)) => {
                    if let Some(sub) = v["subscription_id"].as_str() {
                        self.sub_ids.insert(id, sub.to_string());
                    }
                }
                (Tag::Builtins, Ok(v)) => self.builtins = parse(v).unwrap_or_default(),
                // An older host without built-in tool settings.
                (Tag::Builtins, Err(_)) => self.builtins.clear(),
                (Tag::BuiltinSet { announce }, Ok(v)) => {
                    if let Some(list) = parse::<Vec<BuiltinTool>>(v) {
                        self.builtins = list;
                    }
                    if let Some(name) = announce.filter(|n| self.builtin(n).is_some_and(|b| b.policy == ToolPolicy::Auto)) {
                        let text = self.no_ask_line(&name);
                        self.push_toast(ToastKind::Info, text, Some(ToastAction::OpenSettings(Tab::Connectors)), std::time::Instant::now());
                    }
                }
                (Tag::CommandSet { announce }, Ok(v)) => {
                    if let Some(list) = parse::<Vec<BuiltinTool>>(v) {
                        self.builtins = list;
                    }
                    if let Some(rule) = announce.filter(|r| self.builtin("run_command").is_some_and(|b| b.allowed.contains(r))) {
                        let text = self.command_no_ask_line(&rule);
                        self.push_toast(ToastKind::Info, text, Some(ToastAction::OpenSettings(Tab::Connectors)), std::time::Instant::now());
                    }
                }
                (Tag::BuiltinSet { .. } | Tag::CommandSet { .. }, Err(e)) => {
                    self.error = Some(format!("Could not change the setting: {}", e.message));
                    self.call(Tag::Builtins, m::BUILTIN_LIST, json!({}));
                }
                // Accepted: the `prompt` event replaces the bubble. Already recorded
                // (a resend): that event is in the transcript, so drop the copy.
                (Tag::Prompt { session, prompt_id }, Ok(v)) => {
                    if v["accepted"] == false {
                        self.drop_pending(&session, &prompt_id);
                    }
                }
                // Not sent: the words go back in an empty composer, to fix or resend.
                (Tag::Prompt { session, prompt_id }, Err(e)) => {
                    if let Some(p) = self.drop_pending(&session, &prompt_id) {
                        let back = p.payload["text"].as_str().unwrap_or("");
                        if self.prompt.text.is_empty() && self.screen == Screen::Session(session.clone()) && !back.is_empty() {
                            self.prompt.replace_text(back);
                        }
                    }
                    self.error = Some(format!("Send prompt failed: {}", e.message));
                }
                (Tag::Automations, Ok(v)) => {
                    self.automations = parse(v).unwrap_or_default();
                    // On screen without its runs (the first list, a reconnect): fetch them.
                    if self.screen == Screen::Automations {
                        if let Some(id) = self.selected_automation().map(|a| a.automation_id.clone()).filter(|id| !self.runs.contains_key(id)) {
                            self.select_automation(&id);
                        }
                    }
                }
                // An older host without automations: the screen says to update it.
                (Tag::Automations, Err(_)) => self.automations.clear(),
                (Tag::AutomationSaved, Ok(v)) => {
                    self.draft.saving = false;
                    self.draft.secret = TextEdit::new("").secure();
                    if let Some(a) = parse::<Automation>(v) {
                        let id = a.automation_id.clone();
                        self.upsert_automation(a);
                        self.auto_open = false;
                        self.menu = None;
                        self.screen = Screen::Automations;
                        self.select_automation(&id);
                    }
                }
                // The editor stays open with what was typed, and says what the host refused.
                (Tag::AutomationSaved, Err(e)) => {
                    self.draft.saving = false;
                    self.draft.error = Some(e.message);
                }
                (Tag::AutomationUpdated { .. }, Ok(v)) => {
                    if let Some(a) = parse::<Automation>(v) {
                        self.upsert_automation(a);
                    }
                }
                (Tag::AutomationUpdated { what }, Err(e)) => {
                    self.error = Some(format!("{what} failed: {}", e.message));
                    self.refresh(Refresh::Automations);
                }
                (Tag::AutomationRuns(id), Ok(v)) => {
                    self.runs.insert(id, parse(v).unwrap_or_default());
                }
                (Tag::AutomationRuns(_), Err(e)) => self.error = Some(format!("Could not load the runs: {}", e.message)),
                (Tag::AutomationRan(id), Ok(v)) => {
                    if let Some(r) = parse::<AutomationRun>(v) {
                        if let Some(a) = self.automations.iter_mut().find(|a| a.automation_id == id) {
                            a.last_run = Some(r.clone());
                        }
                        upsert_run(self.runs.entry(id).or_default(), r);
                    }
                }
                (Tag::AutomationRan(_), Err(e)) => self.error = Some(format!("Could not start the run: {}", e.message)),
                (Tag::ScheduleCheck(cron), Ok(v)) => {
                    if self.draft.checked == cron {
                        self.draft.check = parse(v);
                    }
                }
                (Tag::ScheduleCheck(cron), Err(e)) => {
                    if self.draft.checked == cron {
                        self.draft.check = Some(ScheduleCheck { error: Some(e.message), text: None, next: vec![] });
                    }
                }
                (Tag::Quiet, _) => {}
                (Tag::Action { refresh, .. }, Ok(_)) => self.refresh(refresh),
                (Tag::Subscribed(id), Err(e)) => {
                    self.subscribed.remove(&id);
                    self.error = Some(format!("Could not open the session: {}", e.message));
                }
                (Tag::Action { what, refresh }, Err(e)) => {
                    self.error = Some(format!("{what} failed: {}", e.message));
                    // A delete already took it off the list: put it back.
                    if refresh == Refresh::Automations {
                        self.refresh(refresh);
                    }
                }
                (Tag::PairStart, Err(e)) => self.error = Some(format!("Pairing could not start: {}", e.message)),
                (Tag::SessionCreated, Err(e)) => self.error = Some(format!("Could not start the session: {}", e.message)),
                (_, Err(e)) => self.error = Some(e.message),
            },
            FromHost::Note(n) => self.on_note(n),
            FromHost::Uploaded { local_id, result } => {
                for list in self.files.values_mut() {
                    if let Some(f) = list.iter_mut().find(|f| f.local_id == local_id) {
                        f.state = match result {
                            Ok(a) => FileState::Ready(a),
                            Err(e) => FileState::Failed(e),
                        };
                        break;
                    }
                }
            }
            FromHost::AttachmentRead { attachment_id, result } => {
                // Dropped while it loaded (another session opened): don't keep it.
                if let Some(slot) = self.images.get_mut(&attachment_id) {
                    *slot = match (result, &self.image_cache) {
                        (Ok(bytes), Some(cache)) => SentImage::Ready(cache.load(bytes)),
                        (Ok(_), None) => SentImage::Failed("Images can't be shown here.".into()),
                        (Err(e), _) => SentImage::Failed(e),
                    };
                }
            }
            FromHost::Transcribed(r) => {
                self.mic = Mic::Idle;
                let send = std::mem::take(&mut self.send_after_dictation);
                match r {
                    Ok(text) => {
                        self.insert_dictation(&text);
                        if send {
                            self.send_prompt();
                        }
                    }
                    Err(e) => self.error = Some(format!("Voice input failed: {e}")),
                }
            }
            // Replies for a dictation already discarded or ended are dropped.
            FromHost::LiveText { gen, done, result } if self.live.as_ref().map(|l| l.gen) == Some(gen) => {
                match result {
                    Ok(text) => {
                        self.show_live(&text);
                        if done {
                            self.end_live(true);
                        }
                    }
                    Err(e) => {
                        self.error = Some(format!("Voice input failed: {e}"));
                        self.end_live(false);
                    }
                }
            }
            FromHost::LiveText { .. } => {}
        }
    }

    /// Safety net for live updates: a session the host says has newer events
    /// than we got within a moment is subscribed again from where we are, so
    /// a lost stream heals without reopening or restarting. True while any
    /// check is pending (poll wakes again).
    pub fn catch_up(&mut self) -> bool {
        let now = std::time::Instant::now();
        let mut redo = vec![];
        self.behind.retain(|id, (want, since)| {
            let have = self.transcripts.get(id).map(|t| t.last_seq).unwrap_or(0);
            if have >= *want {
                return false;
            }
            if now.duration_since(*since) < std::time::Duration::from_millis(1500) {
                return true;
            }
            redo.push(id.clone());
            false
        });
        for id in redo {
            if let Some(old) = self.sub_ids.remove(&id) {
                self.call(Tag::Quiet, m::EVENTS_UNSUBSCRIBE, json!({"subscription_id": old}));
            }
            self.subscribed.remove(&id);
            self.ensure_subscribed(&id);
        }
        !self.behind.is_empty()
    }

    /// Start a connector's OAuth sign-in; the browser opens when it replies.
    pub fn sign_in(&mut self, id: &str) {
        self.signing_in.insert(id.to_string(), String::new());
        self.call(Tag::SignIn(id.into()), m::CONNECTOR_SIGN_IN, json!({"connector_id": id}));
    }

    /// Add the server in the URL field (and the name, if one was typed). The host works out
    /// how it signs in; OAuth then opens the browser.
    pub fn add_connector(&mut self) {
        let mut url = self.conn_url.text.trim().to_string();
        if url.is_empty() {
            return;
        }
        if !url.contains("://") {
            url = format!("https://{url}");
        }
        self.error = None;
        let name = self.conn_name.text.trim().to_string();
        let mut params = json!({"url": url});
        if !name.is_empty() {
            params["name"] = json!(name);
        }
        self.call(Tag::ConnectorAdded, m::CONNECTOR_ADD, params);
        self.conn_url.clear();
        self.conn_name.clear();
        self.close_add_connector();
    }

    /// Add a server picked from the catalog search.
    pub fn add_suggestion(&mut self, sug: &ConnectorSuggestion) {
        if !suggestion_supported(sug) {
            return;
        }
        self.error = None;
        self.call(Tag::ConnectorAdded, m::CONNECTOR_ADD, json!({"url": sug.url, "name": sug.name}));
        self.close_add_connector();
    }

    /// Open the add-connector form on its search.
    pub fn open_add_connector(&mut self) {
        self.adding_connector = true;
        self.add_manual = false;
        self.conn_search.focused = true;
    }

    pub fn close_add_connector(&mut self) {
        self.adding_connector = false;
        self.add_manual = false;
        self.conn_search.clear();
        self.suggestions.clear();
        self.search_query.clear();
        self.search_due = None;
        self.searching = false;
        self.search_error = None;
    }

    /// The search field changed: search once typing pauses.
    pub fn search_edited(&mut self, now: std::time::Instant) {
        let q = self.conn_search.text.trim().to_string();
        self.search_error = None;
        if q.is_empty() {
            self.search_due = None;
            self.searching = false;
            self.suggestions.clear();
            self.search_query.clear();
            return;
        }
        self.search_due = Some(now + SEARCH_DEBOUNCE);
    }

    /// Run the search if it's due. The time left otherwise, for the next wake.
    pub fn search_if_due(&mut self, now: std::time::Instant) -> Option<std::time::Duration> {
        let due = self.search_due?;
        if now < due {
            return Some(due - now);
        }
        self.search_due = None;
        let q = self.conn_search.text.trim().to_string();
        if q.is_empty() || (q == self.search_query && !self.searching && self.search_error.is_none() && !self.suggestions.is_empty()) {
            return None;
        }
        self.search_query = q.clone();
        self.searching = true;
        self.call(Tag::ConnectorSearch(q.clone()), m::CONNECTOR_SEARCH, json!({"query": q}));
        None
    }

    /// Rename a connector from its detail; errors show by the field.
    pub fn rename_connector(&mut self, id: &str) {
        let n = self.rename.text.trim().to_string();
        let current = self.connectors.iter().find(|c| c.connector_id == id).map(|c| c.name.clone());
        if n.is_empty() {
            self.rename_error = Some((id.to_string(), "A connector needs a name.".into()));
            return;
        }
        self.rename_error = None;
        if current.as_deref() == Some(n.as_str()) {
            self.renaming = None;
            return;
        }
        self.call(Tag::Rename(id.to_string()), m::CONNECTOR_UPDATE, json!({"connector_id": id, "name": n}));
    }

    // ---- session titles -------------------------------------------------------------------

    /// What a session is called before its first message: the host names it from then on
    /// (the same name on every device), so this shows only until then.
    pub fn untitled_name(&self, _x: &Session) -> String {
        "New session".into()
    }

    /// The session's name: its title, else [`App::untitled_name`].
    pub fn session_title(&self, x: &Session) -> String {
        match x.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => t.to_string(),
            None => self.untitled_name(x),
        }
    }

    /// The header's subtitle before the branch: "Claude Code · Work · relay".
    pub fn session_context(&self, x: &Session) -> String {
        let mut parts = vec![self.login_label(x)];
        if let Some(w) = self.workspace(&x.workspace_id) {
            parts.push(w.name.clone());
        }
        parts.join(" · ")
    }

    /// Rename Session…: the header's title becomes a field holding the current title.
    pub fn start_session_rename(&mut self) {
        let Some(x) = self.current().filter(|x| x.archived_at.is_none()) else { return };
        let (id, title) = (x.session_id.clone(), x.title.clone().unwrap_or_default());
        self.close_overlays();
        let len = title.len();
        self.session_rename = Some(id);
        self.title_edit = TextEdit::new(title);
        self.title_edit.set_selection(0, len);
        self.title_edit.focused = true;
    }

    pub fn cancel_session_rename(&mut self) {
        self.session_rename = None;
    }

    /// Return in the header's field: `session.rename`. A blank title clears it (the header then
    /// shows the workspace's name); the same title just closes the field.
    pub fn save_session_rename(&mut self) {
        let Some(id) = self.session_rename.clone() else { return };
        let title = self.title_edit.text.trim().to_string();
        if title.chars().count() > MAX_SESSION_TITLE {
            self.error = Some(format!("A session title can be at most {MAX_SESSION_TITLE} characters."));
            return;
        }
        let current = self.session(&id).and_then(|x| x.title.clone()).unwrap_or_default();
        if current.trim() == title {
            self.session_rename = None;
            return;
        }
        let title = if title.is_empty() { Value::Null } else { json!(title) };
        self.call(Tag::SessionRenamed(id.clone()), m::SESSION_RENAME, json!({"session_id": id, "title": title}));
    }

    pub fn start_rename(&mut self, id: &str) {
        let name = self.connectors.iter().find(|c| c.connector_id == id).map(|c| c.name.clone()).unwrap_or_default();
        self.renaming = Some(id.to_string());
        self.rename_error = None;
        let len = name.len();
        self.rename = TextEdit::new(name);
        self.rename.set_selection(0, len);
        self.rename.focused = true;
    }

    // ---- side questions -------------------------------------------------------------------

    /// Ask a side question: a new side conversation, or a follow-up in `side_id`.
    pub fn side_ask(&mut self, session: &str, side_id: Option<&str>, text: &str) {
        let mut params = json!({"session_id": session, "text": text});
        if let Some(sid) = side_id {
            params["side_id"] = json!(sid);
        } else {
            self.side_pending.insert(session.to_string(), text.to_string());
        }
        self.show_panel(PanelTab::Side);
        self.call(Tag::SideAsked(session.to_string()), m::SESSION_SIDE_ASK, params);
    }

    /// The side pane's follow-up field: ask in the selected side conversation.
    pub fn side_follow_up(&mut self) {
        let Screen::Session(id) = self.screen.clone() else { return };
        let text = self.side_prompt.text.trim().to_string();
        let Some(tab) = self.side_selected(&id) else { return };
        if text.is_empty() || self.side_threads(&id).iter().any(|t| t.side_id == tab && t.streaming()) {
            return;
        }
        self.side_ask(&id, Some(&tab), &text);
        self.side_prompt.clear();
    }

    /// A session's side conversations, from its events, in the order they started.
    pub fn side_threads(&self, session: &str) -> Vec<SideThread> {
        let Some(tr) = self.transcripts.get(session) else { return vec![] };
        let mut out: Vec<SideThread> = vec![];
        for e in &tr.events {
            if !matches!(e.kind, EventType::SideQuestion | EventType::SideMessage | EventType::SideEnd) {
                continue;
            }
            let Some(sid) = e.payload["side_id"].as_str() else { continue };
            let i = match out.iter().position(|t| t.side_id == sid) {
                Some(i) => i,
                None => {
                    out.push(SideThread { side_id: sid.to_string(), ..Default::default() });
                    out.len() - 1
                }
            };
            let th = &mut out[i];
            let text = e.payload["text"].as_str().unwrap_or("");
            match e.kind {
                EventType::SideQuestion => th.turns.push(SideTurn { question: text.to_string(), ..Default::default() }),
                EventType::SideMessage => {
                    if th.turns.last().is_none_or(|t| t.ended) {
                        th.turns.push(SideTurn::default());
                    }
                    let a = &mut th.turns.last_mut().unwrap().answer;
                    if !a.is_empty() {
                        a.push_str("\n\n");
                    }
                    a.push_str(text);
                }
                _ => {
                    if let Some(t) = th.turns.last_mut() {
                        t.ended = true;
                        t.error = e.payload["error"].as_str().map(String::from);
                    }
                }
            }
        }
        for th in &mut out {
            if th.streaming() {
                th.live = tr.side_live.get(&th.side_id).cloned().unwrap_or_default();
            }
        }
        out
    }

    /// The side conversations whose tabs show here.
    pub fn side_tabs(&self, session: &str) -> Vec<SideThread> {
        self.side_threads(session).into_iter().filter(|t| !self.side_closed.contains(&t.side_id)).collect()
    }

    /// The selected tab: the chosen one if it's still open, else the newest.
    pub fn side_selected(&self, session: &str) -> Option<String> {
        let tabs = self.side_tabs(session);
        let chosen = self.side_tab.get(session).filter(|id| tabs.iter().any(|t| &t.side_id == *id)).cloned();
        chosen.or_else(|| tabs.last().map(|t| t.side_id.clone()))
    }

    /// The session has side conversations to show (or one being asked).
    pub fn has_side(&self, session: &str) -> bool {
        self.side_pending.contains_key(session) || !self.side_tabs(session).is_empty()
    }

    /// The side pane is on screen for the open session.
    pub fn side_visible(&self) -> bool {
        self.panel_shown() == Some(PanelTab::Side)
    }

    /// A trailing pane (the right panel or a subagent) is on screen.
    pub fn trailing_visible(&self) -> bool {
        self.panel_shown().is_some() || self.agent_visible()
    }

    /// How wide the trailing pane on screen is; 0 with none.
    pub fn trailing_width(&self, t: &crate::look::Theme) -> f32 {
        if self.agent_visible() {
            crate::side::width(t)
        } else if self.panel_shown().is_some() {
            self.panel_width(t)
        } else {
            0.0
        }
    }

    // ---- subagents -------------------------------------------------------------------------

    /// A session's subagents, from its events, in the order they started.
    pub fn subagents(&self, session: &str) -> Vec<Subagent> {
        let Some(tr) = self.transcripts.get(session) else { return vec![] };
        let active = self.session(session).is_some_and(|x| matches!(x.state, SessionState::Created | SessionState::Starting | SessionState::Running | SessionState::WaitingApproval));
        subagents_of(&tr.events, active)
    }

    /// One subagent of a session by its call id.
    pub fn subagent(&self, session: &str, call_id: &str) -> Option<Subagent> {
        self.subagents(session).into_iter().find(|a| a.call_id == call_id)
    }

    /// Subagents still running, and background commands still running, in a session.
    pub fn running_work(&self, session: &str) -> (usize, usize) {
        let agents = self.subagents(session).iter().filter(|a| a.running()).count();
        let Some(tr) = self.transcripts.get(session) else { return (agents, 0) };
        let active = self.session(session).is_some_and(|x| matches!(x.state, SessionState::Created | SessionState::Starting | SessionState::Running | SessionState::WaitingApproval));
        if !active {
            return (agents, 0);
        }
        let done: HashSet<&str> = tr.events.iter().filter(|e| e.kind == EventType::ToolResult).filter_map(|e| e.payload["tool_call_id"].as_str()).collect();
        let commands = tr
            .events
            .iter()
            .filter(|e| e.kind == EventType::ToolCall && e.payload["background"] == true && !is_subagent_call(&e.payload))
            .filter(|e| e.payload["tool_call_id"].as_str().is_some_and(|c| !done.contains(c)))
            .count();
        (agents, commands)
    }

    /// The agent panel is on screen for the open session.
    pub fn agent_visible(&self) -> bool {
        match (&self.agent_open, &self.screen) {
            (Some((sid, _)), Screen::Session(cur)) => sid == cur,
            _ => false,
        }
    }

    pub fn open_agent(&mut self, session: &str, call_id: &str) {
        if self.agent_open.as_ref().is_none_or(|(s, c)| s != session || c != call_id) {
            self.agent_task_open = false;
        }
        self.agent_open = Some((session.to_string(), call_id.to_string()));
    }

    pub fn close_agent(&mut self) {
        self.agent_open = None;
    }

    /// Close a side tab here (the conversation stays in the session).
    pub fn close_side_tab(&mut self, session: &str, side_id: &str) {
        self.side_closed.insert(side_id.to_string());
        if self.side_tab.get(session).map(String::as_str) == Some(side_id) {
            self.side_tab.remove(session);
        }
        if !self.has_side(session) && self.panel_tab == PanelTab::Side {
            self.panel_open = false;
        }
    }

    // ---- the agent's session settings ------------------------------------------------------

    /// The session's setting in this ACP category (`model`, `thought_level`).
    pub fn config_option<'a>(sess: &'a Session, category: &str) -> Option<&'a SessionConfigOption> {
        sess.config_options.iter().find(|o| o.category.as_deref() == Some(category) && !o.options.is_empty())
    }

    /// The fast-mode setting, as the Claude adapter reports it: id `fast` (or category
    /// `model_config`) with exactly the values `on` and `off`.
    pub fn fast_option(sess: &Session) -> Option<&SessionConfigOption> {
        sess.config_options.iter().find(|o| {
            let mut v: Vec<&str> = o.options.iter().map(|x| x.value.as_str()).collect();
            v.sort_unstable();
            (o.id == "fast" || o.category.as_deref() == Some("model_config")) && v == ["off", "on"]
        })
    }

    /// Whether the composer's model chip has anything to show (a model, effort or fast mode).
    pub fn has_model_settings(sess: &Session) -> bool {
        Self::config_option(sess, "model").is_some() || Self::config_option(sess, "thought_level").is_some() || Self::fast_option(sess).is_some()
    }

    pub fn set_config(&self, session: &str, config_id: &str, value: &str) {
        self.call(Tag::SessionUpdated, m::SESSION_SET_CONFIG, json!({"session_id": session, "config_id": config_id, "value": value}));
    }

    // ---- toasts ---------------------------------------------------------------------------

    pub fn push_toast(&mut self, kind: ToastKind, text: String, action: Option<ToastAction>, now: std::time::Instant) {
        // The same message again restarts its toast instead of stacking a twin.
        self.toasts.retain(|t| !(t.text == text && t.kind == kind));
        let id = self.next_toast;
        self.next_toast += 1;
        let since = (!self.toasts_hovered).then_some(now);
        self.toasts.push(Toast { id, kind, text, action, shown: std::time::Duration::ZERO, since });
        // A stack stays short.
        while self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    /// Messages set since the last frame (`error`, `notice`) become toasts.
    pub fn flush_messages(&mut self, now: std::time::Instant) {
        if let Some(e) = self.error.take() {
            self.push_toast(ToastKind::Error, e, None, now);
        }
        if let Some(n) = self.notice.take() {
            self.push_toast(ToastKind::Info, n, None, now);
        }
    }

    /// Drop toasts whose time is up. The time until the next one goes, if any.
    pub fn tick_toasts(&mut self, now: std::time::Instant) -> Option<std::time::Duration> {
        self.toasts.retain(|t| t.elapsed(now) < TOAST_LIFETIME);
        if self.toasts_hovered {
            return None;
        }
        self.toasts.iter().map(|t| TOAST_LIFETIME - t.elapsed(now)).min()
    }

    /// The pointer entered or left the toasts: pause or resume their clocks.
    pub fn hover_toasts(&mut self, inside: bool, now: std::time::Instant) {
        if inside == self.toasts_hovered {
            return;
        }
        self.toasts_hovered = inside;
        for t in &mut self.toasts {
            if inside {
                t.shown = t.elapsed(now);
                t.since = None;
            } else {
                t.since = Some(now);
            }
        }
    }

    pub fn dismiss_toast(&mut self, id: u64) {
        self.toasts.retain(|t| t.id != id);
        if self.toasts.is_empty() {
            self.toasts_hovered = false;
        }
    }

    /// A toast's button.
    pub fn toast_action(&mut self, id: u64, tx: &mut rune::Tx) {
        let Some(t) = self.toasts.iter().find(|t| t.id == id).cloned() else { return };
        self.dismiss_toast(id);
        match t.action {
            Some(ToastAction::ShowApproval { session_id, .. }) => {
                self.close_overlays();
                self.open_session(&session_id);
                tx.scroll_into_view("approval-card");
            }
            Some(ToastAction::OpenSettings(tab)) => self.open_settings(tab),
            None => {}
        }
    }

    /// "Claude Code wants to create issue on GitHub": an approval in a sentence.
    pub fn approval_line(&self, a: &PendingApproval) -> String {
        let agent = self.session(&a.session_id).map(|x| x.agent.display_name().to_string()).unwrap_or_else(|| "An agent".into());
        match &a.summary {
            Some(sum) => {
                let mut cs = sum.chars();
                let low = cs.next().map(|c| c.to_lowercase().collect::<String>() + cs.as_str()).unwrap_or_default();
                format!("{agent} wants to {low}")
            }
            None => format!("{agent} wants to {} on {}", crate::session::humanize(&a.tool), a.connector_name),
        }
    }

    /// New approvals in other sessions get a toast with Show; answered ones lose theirs.
    fn toast_approvals(&mut self) {
        let now = std::time::Instant::now();
        let here = match &self.screen {
            Screen::Session(id) => Some(id.clone()),
            Screen::Home | Screen::Automations | Screen::Notes => None,
        };
        let fresh: Vec<PendingApproval> = self.approvals.iter().filter(|a| !self.toasted_approvals.contains(&a.approval_id)).cloned().collect();
        for a in fresh {
            self.toasted_approvals.insert(a.approval_id.clone());
            if here.as_deref() == Some(a.session_id.as_str()) {
                continue; // its card is right there
            }
            let line = self.approval_line(&a);
            self.push_toast(ToastKind::Approval, line, Some(ToastAction::ShowApproval { session_id: a.session_id.clone(), approval_id: a.approval_id.clone() }), now);
        }
        let live: HashSet<&str> = self.approvals.iter().map(|a| a.approval_id.as_str()).collect();
        self.toasts.retain(|t| match &t.action {
            Some(ToastAction::ShowApproval { approval_id, .. }) => live.contains(approval_id.as_str()),
            Some(ToastAction::OpenSettings(_)) | None => true,
        });
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

    /// Take a sent prompt off the transcript (the host refused it, or already has it).
    fn drop_pending(&mut self, session: &str, prompt_id: &str) -> Option<PendingPrompt> {
        let t = self.transcripts.get_mut(session)?;
        let i = t.pending.iter().position(|p| p.prompt_id == prompt_id)?;
        Some(t.pending.remove(i))
    }

    fn upsert_session(&mut self, s: Session) {
        match self.sessions.iter_mut().find(|x| x.session_id == s.session_id) {
            Some(x) => *x = s,
            None => self.sessions.push(s),
        }
        self.mark_seen(false);
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

                if kind == EventType::Prompt {
                    let pid = e.payload["prompt_id"].as_str().unwrap_or("");
                    t.pending.retain(|p| p.prompt_id != pid);
                }
                if matches!(kind, EventType::SideMessage | EventType::SideEnd) {
                    if let Some(sid) = e.payload["side_id"].as_str() {
                        t.side_live.remove(sid);
                    }
                }
                let session = (kind == EventType::TurnEnd).then(|| e.session_id.clone());
                t.events.push(e);
                if let Some(id) = session {
                    self.files_turn_ended(&id);
                }
                if matches!(kind, EventType::ApprovalRequested | EventType::ApprovalResolved | EventType::NetDenied) {
                    self.refresh(Refresh::Approvals);
                }
            }
            m::N_SESSION_DELTA => {
                if let Some(d) = parse::<SessionDelta>(n.params) {
                    match (d.kind, d.side_id) {
                        (EventType::AgentMessage, _) => self.transcripts.entry(d.session_id).or_default().live.push_str(&d.text),
                        (EventType::SideMessage, Some(sid)) => self.transcripts.entry(d.session_id).or_default().side_live.entry(sid).or_default().push_str(&d.text),
                        _ => {}
                    }
                }
            }
            m::N_SESSION_CHANGED => {
                if let Some(s) = parse::<Session>(n.params) {
                    let have = self.transcripts.get(&s.session_id).map(|t| t.last_seq).unwrap_or(0);
                    if self.subscribed.contains(&s.session_id) && s.last_seq > have {
                        // Usually the events are just behind this message; if
                        // they don't come, `catch_up` asks for them again.
                        let e = self.behind.entry(s.session_id.clone()).or_insert((s.last_seq, std::time::Instant::now()));
                        e.0 = e.0.max(s.last_seq);
                    }
                    let waiting = s.state == SessionState::WaitingApproval;
                    let was = self.session(&s.session_id).map(|x| x.state == SessionState::WaitingApproval).unwrap_or(false);
                    // Archived sessions leave the list (and come back when unarchived).
                    if s.archived_at.is_some() {
                        self.sessions.retain(|x| x.session_id != s.session_id);
                        match self.archived.iter_mut().find(|x| x.session_id == s.session_id) {
                            Some(x) => *x = s,
                            None => self.archived.insert(0, s),
                        }
                        return;
                    }
                    self.archived.retain(|x| x.session_id != s.session_id);
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
            m::N_NOTE_CHANGED => {
                if let Some(ch) = parse::<NoteChanged>(n.params) {
                    self.on_note_changed(ch);
                }
            }
            m::N_TERMINAL_OUTPUT => {
                if let Some(o) = parse::<TerminalOutput>(n.params) {
                    self.terminal_output(o);
                }
            }
            m::N_TERMINAL_CHANGED => {
                if let Some(ch) = parse::<TerminalChanged>(n.params) {
                    self.terminal_changed(ch);
                }
            }
            m::N_AUTOMATION_CHANGED => {
                if let Some(ch) = parse::<AutomationChanged>(n.params) {
                    self.on_automation_changed(ch);
                }
            }
            m::N_SCREEN_CHANGED => {
                if let Some(st) = parse::<ScreenStatus>(n.params) {
                    self.screen_share = Some(st);
                }
            }
            m::N_VOICE_CHANGED => {
                if let Some(v) = parse::<VoiceStatus>(n.params) {
                    let was_ready = self.voice.as_ref().map(|v| v.ready).unwrap_or(false);
                    if v.ready && !was_ready {
                        self.notice = Some(if v.streaming {
                            "Voice input is ready. Click the microphone and talk; press Return to send.".into()
                        } else {
                            "Voice input is ready. Click the microphone, talk, then click it again.".into()
                        });
                    } else if let (Some(e), None) = (&v.error, &v.progress) {
                        if self.voice.as_ref().is_some_and(|o| o.progress.is_some()) {
                            self.error = Some(format!("Speech model: {e}"));
                        }
                    }
                    self.voice = Some(v);
                }
            }
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

/// Where a value of a stepped slider of `n` stops is: `current`'s index, else the first.
pub fn stop_of(o: &SessionConfigOption) -> usize {
    o.options.iter().position(|v| v.value == o.current_value).unwrap_or(0)
}

/// An arrow key's (or Page Up/Down's) step on a slider of `n` stops, clamped to its ends.
pub fn step_stop(n: usize, from: usize, delta: i32) -> usize {
    if n == 0 {
        return 0;
    }
    (from as i64 + delta as i64).clamp(0, n as i64 - 1) as usize
}

/// The stop nearest `x` on a slider of `n` stops spread evenly over `span` points (x and span
/// measured from the first stop).
pub fn stop_at(n: usize, x: f32, span: f32) -> usize {
    if n < 2 || span <= 0.0 {
        return 0;
    }
    ((x / span).clamp(0.0, 1.0) * (n - 1) as f32).round() as usize
}

/// The command prefix a `run_command` approval offers to stop asking for ("gh pr edit").
pub fn allow_rule(a: &PendingApproval) -> Option<Vec<String>> {
    let rule: Vec<String> = a.arguments["allow_rule"].as_array()?.iter().map(|x| x.as_str().map(str::to_string)).collect::<Option<_>>()?;
    (!rule.is_empty()).then_some(rule)
}

// ---------------------------------------------------------------------------
// Automations
// ---------------------------------------------------------------------------

/// What starts an automation, as the editor offers it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TriggerKind {
    Schedule,
    Webhook,
    Manual,
}

impl TriggerKind {
    pub const ALL: [TriggerKind; 3] = [TriggerKind::Schedule, TriggerKind::Webhook, TriggerKind::Manual];
    pub fn title(self) -> &'static str {
        match self {
            TriggerKind::Schedule => "Schedule",
            TriggerKind::Webhook => "Webhook",
            TriggerKind::Manual => "By hand",
        }
    }
    pub fn of(t: &AutomationTrigger) -> TriggerKind {
        match t {
            AutomationTrigger::Schedule { .. } => TriggerKind::Schedule,
            AutomationTrigger::Webhook { .. } => TriggerKind::Webhook,
            AutomationTrigger::Manual => TriggerKind::Manual,
        }
    }
}

/// The schedule editor's simple choices; each builds a cron expression.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Preset {
    Hourly,
    Daily,
    Weekdays,
    Weekly,
    Monthly,
    Custom,
}

impl Preset {
    pub const ALL: [Preset; 6] = [Preset::Hourly, Preset::Daily, Preset::Weekdays, Preset::Weekly, Preset::Monthly, Preset::Custom];
    pub fn title(self) -> &'static str {
        match self {
            Preset::Hourly => "Every hour",
            Preset::Daily => "Every day",
            Preset::Weekdays => "Weekdays",
            Preset::Weekly => "Every week",
            Preset::Monthly => "Every month",
            Preset::Custom => "Custom (cron)",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Preset::Hourly => "hourly",
            Preset::Daily => "daily",
            Preset::Weekdays => "weekdays",
            Preset::Weekly => "weekly",
            Preset::Monthly => "monthly",
            Preset::Custom => "custom",
        }
    }
    pub fn of_id(id: &str) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| p.id() == id)
    }
}

/// Days of the week as cron numbers them (0 is Sunday).
pub const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// A time of day as typed: "09:00", "9:30", "9" or "21.15".
pub fn parse_time(s: &str) -> Option<(u32, u32)> {
    let s = s.trim();
    let (h, m) = match s.split_once([':', '.']) {
        Some((h, m)) => (h.trim(), m.trim()),
        None => (s, "0"),
    };
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some((h, m))
}

/// The cron expression a preset makes ("30 9 * * 1-5"), or what to fix in its fields.
pub fn preset_cron(p: Preset, time: &str, weekday: u8, day: &str, custom: &str) -> Result<String, String> {
    if p == Preset::Hourly {
        return Ok("0 * * * *".into());
    }
    if p == Preset::Custom {
        let c = custom.split_whitespace().collect::<Vec<_>>().join(" ");
        return if c.is_empty() { Err("Type a cron expression: minute, hour, day of month, month, day of week.".into()) } else { Ok(c) };
    }
    let (h, m) = parse_time(time).ok_or("Type the time as HH:MM, like 09:00.")?;
    Ok(match p {
        Preset::Daily => format!("{m} {h} * * *"),
        Preset::Weekdays => format!("{m} {h} * * 1-5"),
        Preset::Weekly => format!("{m} {h} * * {}", weekday.min(6)),
        _ => {
            let d: u32 = day.trim().parse().ok().filter(|d| (1..=31).contains(d)).ok_or("Type the day of the month, 1 to 31.")?;
            format!("{m} {h} {d} * *")
        }
    })
}

/// How the editor shows an existing cron: the preset that makes it, with its time ("09:00"),
/// weekday and day of the month; anything else is Custom.
pub fn preset_of(cron: &str) -> (Preset, String, u8, String) {
    let f: Vec<&str> = cron.split_whitespace().collect();
    let num = |s: &str, max: u32| s.parse::<u32>().ok().filter(|n| *n <= max);
    let custom = (Preset::Custom, "09:00".to_string(), 1, "1".to_string());
    if f == ["0", "*", "*", "*", "*"] {
        return (Preset::Hourly, "09:00".into(), 1, "1".into());
    }
    let [m, h, dom, mon, dow] = f.as_slice() else { return custom };
    let (Some(m), Some(h)) = (num(m, 59), num(h, 23)) else { return custom };
    let time = format!("{h:02}:{m:02}");
    match (*dom, *mon, *dow) {
        ("*", "*", "*") => (Preset::Daily, time, 1, "1".into()),
        ("*", "*", "1-5") => (Preset::Weekdays, time, 1, "1".into()),
        ("*", "*", d) if num(d, 7).is_some() => (Preset::Weekly, time, (num(d, 7).unwrap_or(0) % 7) as u8, "1".into()),
        (d, "*", "*") if num(d, 31).is_some_and(|d| d >= 1) => (Preset::Monthly, time, 1, d.to_string()),
        _ => custom,
    }
}

/// The prompt a new automation starts with for a webhook source.
pub fn source_prompt(src: WebhookSource) -> &'static str {
    match src {
        WebhookSource::Sentry => SENTRY_PROMPT,
        WebhookSource::Github => GITHUB_PROMPT,
        WebhookSource::Generic => GENERIC_PROMPT,
    }
}

/// The prompt is empty or one the app filled in (a webhook preset or a schedule example), so
/// changing the trigger may replace it.
pub fn prompt_is_preset(text: &str) -> bool {
    let t = text.trim();
    t.is_empty() || [SENTRY_PROMPT, GITHUB_PROMPT, GENERIC_PROMPT].iter().chain(SCHEDULE_EXAMPLES.iter().map(|(_, p)| p)).any(|p| p.trim() == t)
}

/// What starts an automation, in a few words: its schedule, "Sentry webhook", or "By hand".
pub fn trigger_line(a: &Automation) -> String {
    match &a.trigger {
        AutomationTrigger::Schedule { cron } => a.schedule_text.clone().unwrap_or_else(|| cron.clone()),
        AutomationTrigger::Webhook { source: WebhookSource::Generic, .. } => "Webhook".into(),
        AutomationTrigger::Webhook { source, .. } => format!("{} webhook", source.display_name()),
        AutomationTrigger::Manual => "By hand".into(),
    }
}

pub fn outcome_word(o: RunOutcome) -> &'static str {
    match o {
        RunOutcome::Running => "Running",
        RunOutcome::Succeeded => "Succeeded",
        RunOutcome::Failed => "Failed",
        RunOutcome::Cancelled => "Cancelled",
        RunOutcome::TimedOut => "Timed out",
        RunOutcome::Skipped => "Skipped",
    }
}

pub fn run_trigger_word(t: RunTrigger) -> &'static str {
    match t {
        RunTrigger::Schedule => "Scheduled",
        RunTrigger::CatchUp => "Catch-up",
        RunTrigger::Webhook => "Webhook",
        RunTrigger::Manual => "By hand",
    }
}

/// The two tools an automation may run without asking, as the editor names them.
pub fn auto_approve_title(tool: &str) -> &'static str {
    match tool {
        "create_pull_request" => "Open pull requests",
        "git_push" => "Push its own branch",
        _ => "Use a tool",
    }
}

/// The automation editor's fields.
pub struct AutoDraft {
    /// The automation being edited; `None` for a new one.
    pub id: Option<String>,
    pub name: TextEdit,
    pub workspace: Option<String>,
    /// The login ([`login_key`]).
    pub login: Option<String>,
    pub prompt: TextEdit,
    pub trigger: TriggerKind,
    pub preset: Preset,
    pub time: TextEdit,
    pub weekday: u8,
    pub day: TextEdit,
    pub cron: TextEdit,
    /// The host's word on the schedule, and the cron it is (being) checked for.
    pub check: Option<ScheduleCheck>,
    pub checked: String,
    pub source: WebhookSource,
    /// A new signing secret; blank keeps the stored one.
    pub secret: TextEdit,
    pub remove_secret: bool,
    /// Dedupe, cooldown and filters are showing.
    pub advanced: bool,
    pub dedupe: TextEdit,
    pub cooldown: TextEdit,
    /// Filter rules, one `field = value` per line.
    pub filters: TextEdit,
    pub auto_approve: Vec<String>,
    pub max_minutes: TextEdit,
    pub enabled: bool,
    /// Why the last Save didn't go through.
    pub error: Option<String>,
    pub saving: bool,
}

impl AutoDraft {
    pub fn blank() -> AutoDraft {
        AutoDraft {
            id: None,
            name: TextEdit::new(""),
            workspace: None,
            login: None,
            prompt: TextEdit::new(""),
            trigger: TriggerKind::Schedule,
            preset: Preset::Daily,
            time: TextEdit::new("09:00"),
            weekday: 1,
            day: TextEdit::new("1"),
            cron: TextEdit::new(""),
            check: None,
            checked: String::new(),
            source: WebhookSource::Sentry,
            secret: TextEdit::new("").secure(),
            remove_secret: false,
            advanced: false,
            dedupe: TextEdit::new(""),
            cooldown: TextEdit::new("1440"),
            filters: TextEdit::new(""),
            auto_approve: vec![],
            max_minutes: TextEdit::new("60"),
            enabled: true,
            error: None,
            saving: false,
        }
    }

    /// The editor on an existing automation.
    pub fn of(a: &Automation) -> AutoDraft {
        let mut d = AutoDraft::blank();
        d.id = Some(a.automation_id.clone());
        d.name = TextEdit::new(a.name.clone());
        d.workspace = Some(a.workspace_id.clone());
        d.login = Some(a.profile_id.clone().unwrap_or_else(|| a.agent.as_str().to_string()));
        d.prompt = TextEdit::new(a.prompt.clone());
        d.trigger = TriggerKind::of(&a.trigger);
        match &a.trigger {
            AutomationTrigger::Schedule { cron } => {
                let (p, time, wd, day) = preset_of(cron);
                d.preset = p;
                d.time = TextEdit::new(time);
                d.weekday = wd;
                d.day = TextEdit::new(day);
                d.cron = TextEdit::new(cron.clone());
            }
            AutomationTrigger::Webhook { source, filter, dedupe_key, cooldown_minutes } => {
                d.source = *source;
                d.dedupe = TextEdit::new(dedupe_key.clone().unwrap_or_default());
                d.cooldown = TextEdit::new(cooldown_minutes.to_string());
                d.filters = TextEdit::new(filter.iter().map(|f| format!("{} = {}", f.path, f.equals)).collect::<Vec<_>>().join("\n"));
                d.advanced = dedupe_key.is_some() || !filter.is_empty() || *cooldown_minutes != 24 * 60;
            }
            AutomationTrigger::Manual => {}
        }
        d.auto_approve = a.auto_approve.clone();
        d.max_minutes = TextEdit::new(a.max_minutes.to_string());
        d.enabled = a.enabled;
        d
    }

    /// The schedule's cron expression, for a schedule.
    pub fn cron_expr(&self) -> Option<Result<String, String>> {
        (self.trigger == TriggerKind::Schedule).then(|| preset_cron(self.preset, &self.time.text, self.weekday, &self.day.text, &self.cron.text))
    }
}

/// The editor's fields as `automation.create` params, or what to fix first.
pub fn draft_params(d: &AutoDraft, agents: &[AgentStatus]) -> Result<AutomationCreateParams, String> {
    let name = d.name.text.trim().to_string();
    if name.is_empty() {
        return Err("Give the automation a name.".into());
    }
    let workspace_id = d.workspace.clone().ok_or("Pick the workspace it runs in.")?;
    let login = d.login.as_deref().and_then(|k| agents.iter().find(|a| login_key(a) == k)).ok_or("Pick the agent login it runs as.")?;
    let prompt = d.prompt.text.trim().to_string();
    if prompt.is_empty() {
        return Err("Say what each run should do.".into());
    }
    if prompt.chars().count() > MAX_AUTOMATION_PROMPT {
        return Err(format!("The prompt is too long: at most {MAX_AUTOMATION_PROMPT} characters."));
    }
    let trigger = match d.trigger {
        TriggerKind::Schedule => AutomationTrigger::Schedule { cron: d.cron_expr().unwrap_or_else(|| Err(String::new()))? },
        TriggerKind::Manual => AutomationTrigger::Manual,
        TriggerKind::Webhook => {
            let mut filter = vec![];
            for line in d.filters.text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                match line.split_once('=') {
                    Some((p, v)) if !p.trim().is_empty() => filter.push(WebhookMatch { path: p.trim().into(), equals: v.trim().into() }),
                    _ => return Err(format!("A filter is field = value, one per line, not \u{201c}{line}\u{201d}.")),
                }
            }
            let dedupe = d.dedupe.text.trim();
            let cooldown = d.cooldown.text.trim();
            let cooldown_minutes = if cooldown.is_empty() { 24 * 60 } else { cooldown.parse().map_err(|_| "The cooldown is a number of minutes.")? };
            AutomationTrigger::Webhook { source: d.source, filter, dedupe_key: (!dedupe.is_empty()).then(|| dedupe.to_string()), cooldown_minutes }
        }
    };
    let max: u32 = d.max_minutes.text.trim().parse().ok().filter(|m| (1..=720).contains(m)).ok_or("The longest a run may take is 1 to 720 minutes.")?;
    let secret = d.secret.text.trim();
    Ok(AutomationCreateParams {
        name,
        workspace_id,
        agent: login.agent,
        profile_id: login.profile_id.clone(),
        prompt,
        trigger: trigger.clone(),
        auto_approve: AUTO_APPROVABLE.iter().filter(|t| d.auto_approve.iter().any(|x| x == *t)).map(|t| t.to_string()).collect(),
        max_minutes: Some(max),
        enabled: d.enabled,
        webhook_secret: (matches!(trigger, AutomationTrigger::Webhook { .. }) && !secret.is_empty()).then(|| secret.to_string()),
    })
}

/// `automation.update` params with only what changed from `a`. `remove_secret` clears the
/// stored secret (a new one in `p` wins).
pub fn update_diff(a: &Automation, p: &AutomationCreateParams, remove_secret: bool) -> AutomationUpdateParams {
    let mut u = AutomationUpdateParams { automation_id: a.automation_id.clone(), ..Default::default() };
    if a.name != p.name {
        u.name = Some(p.name.clone());
    }
    if a.enabled != p.enabled {
        u.enabled = Some(p.enabled);
    }
    if a.workspace_id != p.workspace_id {
        u.workspace_id = Some(p.workspace_id.clone());
    }
    // The login: an agent always goes with its profile (the old profile is another agent's).
    let key = |agent: AgentKind, pid: &Option<String>| pid.clone().filter(|id| !id.is_empty()).unwrap_or_else(|| agent.as_str().to_string());
    if a.agent != p.agent {
        u.agent = Some(p.agent);
    }
    if a.agent != p.agent || key(a.agent, &a.profile_id) != key(p.agent, &p.profile_id) {
        u.profile_id = Some(p.profile_id.clone().unwrap_or_default());
    }
    if a.prompt != p.prompt {
        u.prompt = Some(p.prompt.clone());
    }
    if a.trigger != p.trigger {
        u.trigger = Some(p.trigger.clone());
    }
    if a.auto_approve != p.auto_approve {
        u.auto_approve = Some(p.auto_approve.clone());
    }
    if p.max_minutes.is_some_and(|m| m != a.max_minutes) {
        u.max_minutes = p.max_minutes;
    }
    u.webhook_secret = match (&p.webhook_secret, remove_secret) {
        (Some(s), _) => Some(s.clone()),
        (None, true) => Some(String::new()),
        (None, false) => None,
    };
    u
}

/// Put a run in a newest-first list (replacing its older copy).
pub fn upsert_run(list: &mut Vec<AutomationRun>, r: AutomationRun) {
    match list.iter_mut().find(|x| x.run_id == r.run_id) {
        Some(x) => *x = r,
        None => list.push(r),
    }
    list.sort_by_key(|x| std::cmp::Reverse(x.started_at));
}

/// Apply `automation.changed`: the automation is replaced, added or gone; a loaded run list
/// takes its latest run.
pub fn apply_automation_changed(list: &mut Vec<Automation>, runs: &mut HashMap<String, Vec<AutomationRun>>, ch: AutomationChanged) {
    if ch.removed {
        list.retain(|a| a.automation_id != ch.automation_id);
        runs.remove(&ch.automation_id);
        return;
    }
    let Some(a) = ch.automation else { return };
    if let (Some(r), Some(rl)) = (&a.last_run, runs.get_mut(&a.automation_id)) {
        upsert_run(rl, r.clone());
    }
    match list.iter_mut().find(|x| x.automation_id == a.automation_id) {
        Some(x) => *x = a,
        None => list.push(a),
    }
}

impl App {
    /// The host has automations (protocol 5).
    pub fn host_has_automations(&self) -> bool {
        self.info.as_ref().is_some_and(|i| i.protocol_version >= 5)
    }

    pub fn automation(&self, id: &str) -> Option<&Automation> {
        self.automations.iter().find(|a| a.automation_id == id)
    }

    /// The automation the screen shows: the chosen one, else the first.
    pub fn selected_automation(&self) -> Option<&Automation> {
        self.sel_auto.as_deref().and_then(|id| self.automation(id)).or(self.automations.first())
    }

    /// Open the Automations screen (on the automation chosen last, else the first).
    pub fn open_automations(&mut self) {
        if self.screen == Screen::Notes {
            self.save_note();
        }
        self.close_overlays();
        self.session_rename = None;
        self.screen = Screen::Automations;
        self.refresh(Refresh::Automations);
        if let Some(id) = self.selected_automation().map(|a| a.automation_id.clone()) {
            self.select_automation(&id);
        }
    }

    /// Show an automation and fetch its runs.
    pub fn select_automation(&mut self, id: &str) {
        self.sel_auto = Some(id.to_string());
        self.call(Tag::AutomationRuns(id.to_string()), m::AUTOMATION_RUNS, json!({"automation_id": id, "limit": 50}));
    }

    /// The New Automation editor: the last workspace picked for a session (else the first
    /// folder), its default login (else the first that's ready), every day at 09:00.
    pub fn new_automation(&mut self) {
        self.close_overlays();
        let mut d = AutoDraft::blank();
        d.workspace = self
            .new_ws
            .clone()
            .filter(|w| self.workspace(w).is_some_and(|w| w.kind != WorkspaceKind::Scratch))
            .or_else(|| self.workspaces.iter().find(|w| w.kind != WorkspaceKind::Scratch).or(self.workspaces.first()).map(|w| w.workspace_id.clone()));
        let agents = self.info.as_ref().map(|i| i.agents.clone()).unwrap_or_default();
        d.login = d.workspace.as_deref().and_then(|w| self.default_login(w)).or_else(|| agents.iter().find(|a| a.available).map(login_key));
        self.draft = d;
        self.draft.name.focused = true;
        self.auto_open = true;
        self.recheck_schedule();
    }

    pub fn edit_automation(&mut self, id: &str) {
        let Some(a) = self.automation(id) else { return };
        let d = AutoDraft::of(a);
        self.close_overlays();
        self.draft = d;
        self.auto_open = true;
        self.recheck_schedule();
    }

    pub fn close_automation_editor(&mut self) {
        self.auto_open = false;
        self.menu = None;
    }

    /// The editor's workspace: its default login comes with it.
    pub fn draft_pick_workspace(&mut self, ws: &str) {
        self.draft.workspace = Some(ws.to_string());
        if let Some(key) = self.default_login(ws) {
            self.draft.login = Some(key);
        }
    }

    /// Change what starts it. A prompt the app filled in follows: a new webhook gets its
    /// source's prompt, and a webhook prompt goes when the trigger stops being a webhook.
    pub fn draft_set_trigger(&mut self, kind: TriggerKind) {
        let d = &mut self.draft;
        if d.trigger == kind {
            return;
        }
        d.trigger = kind;
        if d.id.is_none() && prompt_is_preset(&d.prompt.text) {
            let webhook_preset = [SENTRY_PROMPT, GITHUB_PROMPT, GENERIC_PROMPT].iter().any(|p| p.trim() == d.prompt.text.trim());
            if kind == TriggerKind::Webhook {
                d.prompt.replace_text(source_prompt(d.source));
            } else if webhook_preset {
                d.prompt.replace_text("");
            }
        }
        self.recheck_schedule();
    }

    /// Change the webhook's sender; on a new automation an untouched prompt becomes its preset.
    pub fn draft_set_source(&mut self, src: WebhookSource) {
        let d = &mut self.draft;
        if d.source == src {
            return;
        }
        d.source = src;
        if d.id.is_none() && prompt_is_preset(&d.prompt.text) {
            d.prompt.replace_text(source_prompt(src));
        }
    }

    /// Start from one of [`SCHEDULE_EXAMPLES`]: its prompt, and its name if none is typed.
    pub fn draft_use_example(&mut self, i: usize) {
        let Some((name, prompt)) = SCHEDULE_EXAMPLES.get(i) else { return };
        if self.draft.name.text.trim().is_empty() || SCHEDULE_EXAMPLES.iter().any(|(n, _)| *n == self.draft.name.text.trim()) {
            self.draft.name.replace_text(*name);
        }
        self.draft.prompt.replace_text(*prompt);
    }

    pub fn draft_set_preset(&mut self, p: Preset) {
        // Custom starts from what the preset made, to edit.
        if p == Preset::Custom && self.draft.cron.text.trim().is_empty() {
            if let Some(Ok(c)) = self.draft.cron_expr() {
                self.draft.cron.replace_text(&c);
            }
        }
        self.draft.preset = p;
        self.recheck_schedule();
    }

    pub fn draft_set_weekday(&mut self, d: u8) {
        self.draft.weekday = d.min(6);
        self.recheck_schedule();
    }

    pub fn draft_toggle_approve(&mut self, tool: &str) {
        let list = &mut self.draft.auto_approve;
        if list.iter().any(|t| t == tool) {
            list.retain(|t| t != tool);
        } else {
            list.push(tool.to_string());
        }
    }

    /// Ask the host about the schedule as it now stands (each change; a stale answer is
    /// dropped). A schedule that can't be built says why at once.
    pub fn recheck_schedule(&mut self) {
        match self.draft.cron_expr() {
            None => {
                self.draft.check = None;
                self.draft.checked.clear();
            }
            Some(Err(e)) => {
                self.draft.checked.clear();
                self.draft.check = Some(ScheduleCheck { error: Some(e), text: None, next: vec![] });
            }
            Some(Ok(cron)) => {
                if cron == self.draft.checked && self.draft.check.is_some() {
                    return;
                }
                self.draft.checked = cron.clone();
                self.draft.check = None;
                if self.host_has_automations() {
                    self.call(Tag::ScheduleCheck(cron.clone()), m::AUTOMATION_CHECK_SCHEDULE, json!({"cron": cron}));
                }
            }
        }
    }

    /// Save: create it, or send only what changed. Problems show in the editor.
    pub fn save_automation(&mut self) {
        if self.draft.saving {
            return;
        }
        let agents = self.info.as_ref().map(|i| i.agents.clone()).unwrap_or_default();
        let p = match draft_params(&self.draft, &agents) {
            Ok(p) => p,
            Err(e) => {
                self.draft.error = Some(e);
                return;
            }
        };
        if let Some(e) = self.draft.check.as_ref().and_then(|c| c.error.clone()).filter(|_| self.draft.trigger == TriggerKind::Schedule) {
            self.draft.error = Some(e);
            return;
        }
        self.draft.error = None;
        let Some(id) = self.draft.id.clone() else {
            self.draft.saving = true;
            self.call(Tag::AutomationSaved, m::AUTOMATION_CREATE, json!(p));
            return;
        };
        let Some(a) = self.automation(&id) else {
            self.draft.error = Some("This automation was deleted.".into());
            return;
        };
        let u = update_diff(a, &p, self.draft.remove_secret);
        if u == (AutomationUpdateParams { automation_id: id, ..Default::default() }) {
            self.close_automation_editor();
            return;
        }
        self.draft.saving = true;
        self.call(Tag::AutomationSaved, m::AUTOMATION_UPDATE, json!(u));
    }

    /// The on/off switch.
    pub fn set_automation_enabled(&self, id: &str, on: bool) {
        let what = if on { "Turn on the automation" } else { "Turn off the automation" };
        self.call(Tag::AutomationUpdated { what: what.into() }, m::AUTOMATION_UPDATE, json!({"automation_id": id, "enabled": on}));
    }

    /// Run Now is offered unless a run is still going.
    pub fn can_run_now(&self, a: &Automation) -> bool {
        self.conn == Conn::Connected && a.last_run.as_ref().is_none_or(|r| r.outcome.is_done())
    }

    pub fn run_automation_now(&self, id: &str) {
        if self.automation(id).is_some_and(|a| self.can_run_now(a)) {
            self.call(Tag::AutomationRan(id.to_string()), m::AUTOMATION_RUN_NOW, json!({"automation_id": id}));
        }
    }

    /// Delete…: asks first, then deletes.
    pub fn delete_automation(&mut self, id: &str) {
        if self.confirm_delete_auto.as_deref() != Some(id) {
            self.menu = None;
            self.confirm_delete_auto = Some(id.to_string());
            return;
        }
        self.confirm_delete_auto = None;
        if self.draft.id.as_deref() == Some(id) {
            self.close_automation_editor();
        }
        self.automations.retain(|a| a.automation_id != id);
        self.runs.remove(id);
        if self.sel_auto.as_deref() == Some(id) {
            self.sel_auto = None;
        }
        self.act("Delete automation", m::AUTOMATION_DELETE, json!({"automation_id": id}), Refresh::Automations);
    }

    /// New URL…: asks first (the old URL stops working), then rotates.
    pub fn rotate_webhook(&mut self, id: &str) {
        if self.confirm_rotate.as_deref() != Some(id) {
            self.confirm_rotate = Some(id.to_string());
            return;
        }
        self.confirm_rotate = None;
        self.call(Tag::AutomationUpdated { what: "Make a new webhook URL".into() }, m::AUTOMATION_UPDATE, json!({"automation_id": id, "rotate_webhook": true}));
    }

    /// Open a run's session in the session view (an archived one once it's listed).
    pub fn open_run(&mut self, session_id: &str) {
        self.close_overlays();
        if self.session(session_id).is_none() {
            self.refresh(Refresh::Archived);
        }
        self.open_session(session_id);
    }

    /// Automations running now.
    pub fn automations_running(&self) -> usize {
        self.automations.iter().filter(|a| a.last_run.as_ref().is_some_and(|r| r.outcome == RunOutcome::Running)).count()
    }

    fn upsert_automation(&mut self, a: Automation) {
        match self.automations.iter_mut().find(|x| x.automation_id == a.automation_id) {
            Some(x) => *x = a,
            None => self.automations.push(a),
        }
    }

    fn on_automation_changed(&mut self, ch: AutomationChanged) {
        let id = ch.automation_id.clone();
        let removed = ch.removed;
        let had_runs = self.runs.contains_key(&id);
        apply_automation_changed(&mut self.automations, &mut self.runs, ch);
        if removed {
            if self.sel_auto.as_deref() == Some(id.as_str()) {
                self.sel_auto = None;
            }
            if self.auto_open && self.draft.id.as_deref() == Some(id.as_str()) {
                self.close_automation_editor();
                self.notice = Some("The automation you were editing was deleted.".into());
            }
            return;
        }
        // A run started or finished: its list on screen catches up (details, older runs).
        if had_runs && self.screen == Screen::Automations && self.sel_auto.as_deref() == Some(id.as_str()) {
            self.select_automation(&id);
        }
    }
}

// ---------------------------------------------------------------------------
// Notes
// ---------------------------------------------------------------------------

/// A note's text as the host stores it (trailing space and leading blank lines dropped).
pub fn clean_note(text: &str) -> &str {
    text.trim_end().trim_start_matches(['\n', '\r'])
}

/// The host's order: open notes newest first, then done ones, most recently done first.
pub fn sort_notes(list: &mut [Note]) {
    list.sort_by(|a, b| {
        (a.done_at.is_some(), std::cmp::Reverse(a.done_at.unwrap_or(a.created_at)), &a.note_id).cmp(&(b.done_at.is_some(), std::cmp::Reverse(b.done_at.unwrap_or(b.created_at)), &b.note_id))
    });
}

impl App {
    /// The host has notes (protocol 9).
    pub fn host_has_notes(&self) -> bool {
        self.info.as_ref().is_some_and(|i| i.protocol_version >= 9)
    }

    pub fn note(&self, id: &str) -> Option<&Note> {
        self.notes.iter().find(|n| n.note_id == id)
    }

    /// The note the editor shows, unless a new one is being written.
    pub fn selected_note(&self) -> Option<&Note> {
        if self.new_note_ws.is_some() {
            return None;
        }
        self.sel_note.as_deref().and_then(|id| self.note(id))
    }

    /// The editor has text that isn't saved.
    pub fn note_dirty(&self) -> bool {
        let text = clean_note(&self.note_edit.text);
        match (&self.new_note_ws, self.selected_note()) {
            (Some(_), _) => !title_of(text).is_empty(),
            (None, Some(_)) => text != self.note_base && !title_of(text).is_empty(),
            (None, None) => false,
        }
    }

    /// The workspace a new note goes in by default: the shown note's, else the last one
    /// picked for a session, else the first folder.
    fn default_note_ws(&self) -> Option<String> {
        let live = |id: &String| self.workspace(id).is_some();
        self.selected_note()
            .map(|n| n.workspace_id.clone())
            .filter(live)
            .or_else(|| self.new_note_ws.clone().filter(live))
            .or_else(|| self.new_ws.clone().filter(live))
            .or_else(|| self.workspaces.iter().find(|w| w.kind != WorkspaceKind::Scratch).map(|w| w.workspace_id.clone()))
            .or_else(|| self.workspaces.first().map(|w| w.workspace_id.clone()))
    }

    /// Open the Notes screen: on the note shown last, else the first open one; with no
    /// notes, writing a new one.
    pub fn open_notes(&mut self) {
        self.close_overlays();
        self.session_rename = None;
        self.screen = Screen::Notes;
        self.refresh(Refresh::Notes);
        if self.new_note_ws.is_some() {
            return;
        }
        let shown = self.selected_note().map(|n| n.note_id.clone()).or_else(|| self.notes.iter().find(|n| n.done_at.is_none()).map(|n| n.note_id.clone()));
        match shown {
            Some(id) => self.select_note(&id),
            None => self.new_note(None),
        }
    }

    /// Show a note in the editor, saving the one there first.
    pub fn select_note(&mut self, id: &str) {
        if self.sel_note.as_deref() == Some(id) && self.new_note_ws.is_none() {
            return;
        }
        self.save_note();
        let Some(text) = self.note(id).map(|n| n.text.clone()) else { return };
        self.sel_note = Some(id.to_string());
        self.new_note_ws = None;
        self.note_edit = TextEdit::new(text.clone());
        self.note_base = text;
    }

    /// Start writing a new note in `ws` (else the default workspace), saving the one shown.
    pub fn new_note(&mut self, ws: Option<&str>) {
        self.save_note();
        let Some(ws) = ws.map(str::to_string).or_else(|| self.default_note_ws()) else { return };
        self.screen = Screen::Notes;
        self.close_overlays();
        self.new_note_ws = Some(ws);
        self.note_gen += 1;
        self.note_edit = TextEdit::new("");
        self.note_edit.focused = true;
        self.note_base.clear();
    }

    /// A workspace's "New Note Here".
    pub fn new_note_in(&mut self, ws: &str) {
        self.refresh(Refresh::Notes);
        self.new_note(Some(ws));
    }

    /// Move the new note to another workspace before it's saved.
    pub fn pick_note_ws(&mut self, ws: &str) {
        self.menu = None;
        if self.new_note_ws.is_some() {
            self.new_note_ws = Some(ws.to_string());
        }
    }

    /// Save the editor's text if it changed (a blank editor saves nothing).
    pub fn save_note(&mut self) {
        if !self.note_dirty() {
            return;
        }
        let text = clean_note(&self.note_edit.text).to_string();
        if let Some(ws) = &self.new_note_ws {
            // Once: a second Save before the answer would make it twice.
            if self.note_saving {
                return;
            }
            self.note_saving = true;
            self.call(Tag::NoteSaved { new_gen: Some(self.note_gen) }, m::NOTE_CREATE, json!({"workspace_id": ws, "text": text}));
        } else if let Some(n) = self.selected_note() {
            let id = n.note_id.clone();
            // Shown at once; the host's answer replaces it.
            if let Some(n) = self.notes.iter_mut().find(|n| n.note_id == id) {
                n.text = text.clone();
            }
            self.note_base = text.clone();
            self.note_saving = true;
            self.call(Tag::NoteSaved { new_gen: None }, m::NOTE_UPDATE, json!({"note_id": id, "text": text}));
        }
    }

    /// Mark a note done, or open it again.
    pub fn set_note_done(&mut self, id: &str, done: bool) {
        if self.sel_note.as_deref() == Some(id) {
            self.save_note();
        }
        if let Some(n) = self.notes.iter_mut().find(|n| n.note_id == id) {
            n.done_at = if done { Some(n.done_at.unwrap_or_else(crate::view::now_ms)) } else { None };
        }
        sort_notes(&mut self.notes);
        let what = if done { "Mark note done" } else { "Reopen note" };
        self.call(Tag::NoteAction { what: what.into() }, m::NOTE_UPDATE, json!({"note_id": id, "done": done}));
    }

    /// Delete…: asks first, then deletes. The editor moves to the next open note.
    pub fn delete_note(&mut self, id: &str) {
        if self.confirm_delete_note.as_deref() != Some(id) {
            self.menu = None;
            self.confirm_delete_note = Some(id.to_string());
            return;
        }
        self.confirm_delete_note = None;
        self.notes.retain(|n| n.note_id != id);
        if self.sel_note.as_deref() == Some(id) {
            self.sel_note = None;
            self.note_edit = TextEdit::new("");
            self.note_base.clear();
            if let Some(next) = self.notes.iter().find(|n| n.done_at.is_none()).map(|n| n.note_id.clone()) {
                self.select_note(&next);
            }
        }
        self.call(Tag::NoteAction { what: "Delete note".into() }, m::NOTE_DELETE, json!({"note_id": id}));
    }

    /// Start Session: the New Session sheet in the note's workspace; the session opens with
    /// the note's text in its composer, to edit before sending.
    pub fn start_from_note(&mut self, id: &str) {
        if self.sel_note.as_deref() == Some(id) {
            self.save_note();
        }
        let Some(n) = self.note(id).cloned() else { return };
        let text = if self.sel_note.as_deref() == Some(id) && self.new_note_ws.is_none() { clean_note(&self.note_edit.text).to_string() } else { n.text.clone() };
        self.new_ws = Some(n.workspace_id.clone());
        self.open_new_session(false);
        self.start_note = Some((n.note_id, n.workspace_id, text));
    }

    /// The editor follows the shown note when the host changes it, unless it holds unsaved
    /// text; a deleted note leaves the editor.
    fn sync_note_editor(&mut self) {
        if self.new_note_ws.is_some() {
            return;
        }
        let Some(id) = self.sel_note.clone() else { return };
        match self.note(&id).map(|n| n.text.clone()) {
            Some(text) if text != self.note_base => {
                // Unsaved edits win; Save writes them over the host's.
                if clean_note(&self.note_edit.text) == self.note_base {
                    let focused = self.note_edit.focused;
                    self.note_edit = TextEdit::new(text.clone());
                    self.note_edit.focused = focused;
                    self.note_base = text;
                }
            }
            Some(_) => {}
            None => {
                self.sel_note = None;
                self.note_edit = TextEdit::new("");
                self.note_base.clear();
            }
        }
    }

    fn upsert_note(&mut self, n: Note) {
        match self.notes.iter_mut().find(|x| x.note_id == n.note_id) {
            Some(x) => *x = n,
            None => self.notes.push(n),
        }
        sort_notes(&mut self.notes);
    }

    fn on_note_changed(&mut self, ch: NoteChanged) {
        match ch.note {
            Some(n) if !ch.removed => self.upsert_note(n),
            _ => {
                self.notes.retain(|n| n.note_id != ch.note_id);
                if self.confirm_delete_note.as_deref() == Some(ch.note_id.as_str()) {
                    self.confirm_delete_note = None;
                }
            }
        }
        self.sync_note_editor();
    }
}
