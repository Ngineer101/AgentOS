//! Domain types and method parameters shared by the host and clients.

use crate::methods::Capability;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Agents
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Codex,
    Claude,
    Opencode,
    Cursor,
    /// Google's Antigravity CLI (`agy`). Hosts before it offered Gemini CLI
    /// as `gemini`, which now names this agent.
    #[serde(alias = "gemini")]
    Antigravity,
    /// Scripted ACP agent used for tests. Only offered when the host enables it.
    Fake,
}

impl AgentKind {
    pub const REAL: [AgentKind; 5] = [
        AgentKind::Codex,
        AgentKind::Claude,
        AgentKind::Opencode,
        AgentKind::Cursor,
        AgentKind::Antigravity,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::Codex => "codex",
            AgentKind::Claude => "claude",
            AgentKind::Opencode => "opencode",
            AgentKind::Cursor => "cursor",
            AgentKind::Antigravity => "antigravity",
            AgentKind::Fake => "fake",
        }
    }

    pub fn parse(s: &str) -> Option<AgentKind> {
        Some(match s {
            "codex" => AgentKind::Codex,
            "claude" | "claude-code" | "claude_code" => AgentKind::Claude,
            "opencode" => AgentKind::Opencode,
            "cursor" | "cursor-agent" | "cursor_agent" => AgentKind::Cursor,
            "antigravity" | "antigravity-cli" | "antigravity_cli" | "agy" => AgentKind::Antigravity,
            // Gemini CLI, which Antigravity replaced (stored sessions and profiles).
            "gemini" | "gemini-cli" | "gemini_cli" => AgentKind::Antigravity,
            "fake" => AgentKind::Fake,
            _ => return None,
        })
    }

    /// The setting that points the agent at a profile's config folder, and
    /// what the folder becomes. `None`: profiles are not possible for this
    /// agent on this system.
    pub fn profile_env(&self) -> Option<&'static str> {
        Some(match self {
            AgentKind::Claude => "CLAUDE_CONFIG_DIR",
            AgentKind::Codex => "CODEX_HOME",
            // Antigravity's folder is set by a flag (`--gemini-dir`), not a
            // setting; not offered until a separate login there is checked.
            AgentKind::Antigravity => return None,
            // Login and history in `<folder>/opencode`.
            AgentKind::Opencode => "XDG_DATA_HOME",
            // Linux: login in `<folder>/cursor/auth.json`. On macOS Cursor
            // keeps its login under the home folder, which no setting moves.
            AgentKind::Cursor if cfg!(target_os = "macos") => return None,
            AgentKind::Cursor => "XDG_CONFIG_HOME",
            AgentKind::Fake => "FAKE_AGENT_HOME",
        })
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            AgentKind::Codex => "Codex",
            AgentKind::Claude => "Claude Code",
            AgentKind::Opencode => "OpenCode",
            AgentKind::Cursor => "Cursor",
            AgentKind::Antigravity => "Antigravity CLI",
            AgentKind::Fake => "Fake agent",
        }
    }
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentStatus {
    pub agent: AgentKind,
    pub display_name: String,
    pub installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub logged_in: bool,
    /// Installed and logged in.
    pub available: bool,
    /// What is wrong, when not available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// The command that fixes `problem`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// Which account of this agent the entry is about: the agent's name
    /// (`"claude"`) for its default login, or an [`AgentProfile`] id. Hosts
    /// before profiles leave it out (one entry per agent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    /// The profile's name ("Work"); `None` for the default login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_name: Option<String>,
    /// The profile's config folder; `None` for the default login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,
}

impl AgentStatus {
    /// "Claude Code" for the default login, "Claude Code · Work" for a profile.
    pub fn label(&self) -> String {
        match &self.profile_name {
            Some(n) => format!("{} · {n}", self.display_name),
            None => self.display_name.clone(),
        }
    }
}

/// A second (third, …) login of an agent: its own config folder, so each
/// subscription keeps its own credentials and history. The folder is what the
/// agent's own setting points at (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, …; see
/// [`AgentKind::profile_env`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentProfile {
    pub profile_id: String,
    pub agent: AgentKind,
    pub name: String,
    /// Absolute, symlinks resolved.
    pub config_dir: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentProfileAddParams {
    pub agent: AgentKind,
    pub name: String,
    /// The agent's config folder; `~/` is expanded. Must exist.
    pub config_dir: String,
}

// ---------------------------------------------------------------------------
// Host
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Prerequisite {
    pub name: String,
    pub present: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SandboxHealth {
    /// `bubblewrap` or `seatbelt`.
    pub backend: String,
    pub ok: bool,
    pub prerequisites: Vec<Prerequisite>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RelayStatus {
    pub configured: bool,
    pub connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostInfo {
    pub host_id: String,
    pub version: String,
    pub os: String,
    pub protocol_version: u32,
    pub agents: Vec<AgentStatus>,
    pub sandbox: SandboxHealth,
    pub relay: RelayStatus,
}

// ---------------------------------------------------------------------------
// Workspaces
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    Repo,
    Folder,
    /// The built-in "Quick tasks" workspace ([`QUICK_WORKSPACE_ID`]): no
    /// folder of yours. Each session gets a fresh empty folder of its own, so
    /// any number can run at once.
    Scratch,
}

/// The built-in workspace for tasks that need no project folder.
pub const QUICK_WORKSPACE_ID: &str = "ws_quick";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPreset {
    /// npm, PyPI, crates.io and Go module registries.
    Packages,
    /// github.com and its API and content hosts.
    Github,
    /// Any public website. Local and private addresses stay refused, and
    /// every connection still goes through (and is logged by) the egress proxy.
    Web,
}

/// Hosts a workspace adds on top of the agent's own hosts.
/// No presets and no custom hosts is the `agent-only` profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkProfile {
    #[serde(default)]
    pub presets: Vec<NetworkPreset>,
    #[serde(default)]
    pub custom_hosts: Vec<String>,
}

impl NetworkProfile {
    pub fn agent_only() -> Self {
        Self::default()
    }
    pub fn label(&self) -> String {
        if self.presets.is_empty() && self.custom_hosts.is_empty() {
            return "agent-only".into();
        }
        let mut parts: Vec<String> = self
            .presets
            .iter()
            .map(|p| match p {
                NetworkPreset::Packages => "packages".to_string(),
                NetworkPreset::Github => "github".to_string(),
                NetworkPreset::Web => "web".to_string(),
            })
            .collect();
        if !self.custom_hosts.is_empty() {
            parts.push(format!("custom({})", self.custom_hosts.len()));
        }
        parts.join("+")
    }
    /// Whether one of the custom hosts names `host:port` (see [`host_entry_matches`]).
    pub fn names(&self, host: &str, port: u16) -> bool {
        self.custom_hosts.iter().any(|e| host_entry_matches(e, host, port))
    }
}

/// Whether an allow-list entry names `host:port`. An entry is `name`, or `*.name` for every
/// subdomain of `name` however deep (but not `name` itself), either one with `:port` to
/// name that port alone. Case and a trailing dot on the host don't matter. This is the rule
/// the host's egress proxy enforces; apps use it to say what a workspace already allows.
pub fn host_entry_matches(entry: &str, host: &str, port: u16) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let entry = entry.trim().to_ascii_lowercase();
    let pat = match entry.rsplit_once(':') {
        Some((name, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) && !name.contains(':') => {
            // A port that isn't one names nothing, rather than every port.
            if p.parse::<u16>().ok() != Some(port) {
                return false;
            }
            name
        }
        _ => entry.as_str(),
    };
    match pat.strip_prefix("*.") {
        // The dot before the suffix must be there: `*.example.com` is not `badexample.com`.
        Some(suffix) => !suffix.is_empty() && host.len() > suffix.len() && host.ends_with(suffix) && host.as_bytes()[host.len() - suffix.len() - 1] == b'.',
        None => !pat.is_empty() && host == pat,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Workspace {
    pub workspace_id: String,
    pub name: String,
    pub path: String,
    pub kind: WorkspaceKind,
    pub network: NetworkProfile,
    /// Connector ids enabled for sessions in this workspace.
    #[serde(default)]
    pub connectors: Vec<String>,
    pub created_at: i64,
    /// The login new sessions here start with: set by the first session
    /// started in the workspace, changed with `workspace.set_default_agent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<AgentKind>,
    /// With `default_agent`: its profile, `None` for the agent's default login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_profile_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Sessions and events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Created,
    Starting,
    Idle,
    Running,
    /// Sub-state of `running`: an MCP write call waits for a decision.
    WaitingApproval,
    Ended,
    Failed,
}

impl SessionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionState::Created => "created",
            SessionState::Starting => "starting",
            SessionState::Idle => "idle",
            SessionState::Running => "running",
            SessionState::WaitingApproval => "waiting_approval",
            SessionState::Ended => "ended",
            SessionState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<SessionState> {
        Some(match s {
            "created" => SessionState::Created,
            "starting" => SessionState::Starting,
            "idle" => SessionState::Idle,
            "running" => SessionState::Running,
            "waiting_approval" => SessionState::WaitingApproval,
            "ended" => SessionState::Ended,
            "failed" => SessionState::Failed,
            _ => return None,
        })
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, SessionState::Ended | SessionState::Failed)
    }

    /// Whether the state machine allows `self -> next`.
    ///
    /// `created -> starting -> idle <-> running -> ended`, with
    /// `waiting_approval` inside `running` and `failed` reachable from any
    /// non-terminal state. A resumed session goes back through `starting`.
    pub fn can_transition_to(&self, next: SessionState) -> bool {
        use SessionState::*;
        if self.is_terminal() {
            return false;
        }
        if next == Failed {
            return true;
        }
        matches!(
            (self, next),
            (Created, Starting)
                | (Starting, Idle)
                | (Idle, Running)
                | (Running, Idle)
                | (Running, WaitingApproval)
                | (WaitingApproval, Running)
                | (WaitingApproval, Idle)
                | (Idle, Ended)
                | (Running, Ended)
                | (WaitingApproval, Ended)
                | (Created, Ended)
                | (Starting, Ended)
                // Resume after a host restart.
                | (Idle, Starting)
                | (Running, Starting)
                | (WaitingApproval, Starting)
        )
    }
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub session_id: String,
    pub workspace_id: String,
    pub agent: AgentKind,
    /// The [`AgentProfile`] the session runs as; `None` for the agent's default login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    pub state: SessionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_session_id: Option<String>,
    /// The session's name: the user's own (`session.rename`), else the
    /// host's (the agent's title for it, or its first message until the
    /// agent gives one). `None` only before the first message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Sequence number of the newest event (0 when there are none).
    pub last_seq: i64,
    /// When the user archived it: hidden from `session.list` unless asked
    /// for (`archived: true`). Archived sessions have ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<i64>,
    /// The session's own git worktree, for sessions in a git workspace:
    /// each session works on a branch of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<SessionWorktree>,
    /// The agent's settings for this session (model, effort, …) as it last
    /// reported them; empty when it reports none. Change one with
    /// `session.set_config`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_options: Vec<SessionConfigOption>,
    /// The agent can answer side questions (`session.side_ask`, the `/btw`
    /// command) in a fork of the conversation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub side_questions: bool,
    /// The pull request the session opened (its newest, when it opened
    /// more than one). Clients show its number ("#48") in the session list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<SessionPullRequest>,
    /// The automation this session is a run of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    /// When the user pinned it (`session.pin`, protocol 8): clients list
    /// pinned sessions first, in the order they were pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<i64>,
}

/// A pull request a session opened.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionPullRequest {
    pub number: u64,
    pub url: String,
    /// Whether it is still open, as the host last saw it on GitHub (it
    /// checks open ones every few minutes). Unknown until the first check,
    /// and from hosts before this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<PullRequestState>,
}

/// Where a pull request stands. A draft is open.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestState {
    Open,
    Merged,
    /// Closed without merging.
    Closed,
}

impl PullRequestState {
    /// gh's `state` ("OPEN", "MERGED", "CLOSED").
    pub fn from_gh(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "OPEN" => Some(Self::Open),
            "MERGED" => Some(Self::Merged),
            "CLOSED" => Some(Self::Closed),
            _ => None,
        }
    }
}

impl SessionPullRequest {
    /// The last pull request link in some text: `gh pr create` prints the
    /// new one's ("https://github.com/o/r/pull/48"), and names the existing
    /// one when the branch already has one.
    pub fn find_in(text: &str) -> Option<Self> {
        text.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '(' | ')'))
            .rev()
            .find_map(|w| {
                let w = w.trim_end_matches(['.', ',', ':', ';']);
                let rest = w.strip_prefix("https://")?;
                // host/owner/repo/pull/N, nothing after N.
                let parts: Vec<&str> = rest.split('/').collect();
                let [host, owner, repo, "pull", n] = parts[..] else { return None };
                if [host, owner, repo].iter().any(|p| p.is_empty()) || n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                Some(Self { number: n.parse().ok()?, url: w.to_string(), state: None })
            })
    }
}

/// Where a session's worktree is and which branch it is on. Named when the
/// session is created; the folder and the branch are there once it has
/// started (a session that is `created` or `starting` may not have them yet).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionWorktree {
    pub path: String,
    pub branch: String,
}

/// One of an agent's session settings (ACP `configOptions`, `select` kind).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionConfigOption {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// ACP's category: `model`, `thought_level` (effort), `mode`, … Clients
    /// show `model` and `thought_level` in the composer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub current_value: String,
    pub options: Vec<SessionConfigValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionConfigValue {
    pub value: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionRenameParams {
    pub session_id: String,
    /// The new title; empty or omitted clears it (the host's title shows again).
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionPinParams {
    pub session_id: String,
    /// Pin it (`true`) or unpin it (`false`).
    pub pinned: bool,
}

/// Longest session title, in characters.
pub const MAX_SESSION_TITLE: usize = 120;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionSetConfigParams {
    pub session_id: String,
    pub config_id: String,
    pub value: String,
}

/// A side question (`/btw`): asked in a fork of the conversation, so the
/// session itself is neither interrupted nor changed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionSideAskParams {
    pub session_id: String,
    /// Continue this side conversation; omitted to start a new one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_id: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionSideAskResult {
    pub side_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventType {
    #[serde(rename = "session.state")]
    SessionState,
    #[serde(rename = "prompt")]
    Prompt,
    #[serde(rename = "agent.message")]
    AgentMessage,
    #[serde(rename = "agent.thought")]
    AgentThought,
    /// `{tool_call_id, name, kind, arguments, source, locations}`, and
    /// optionally `tool_name` (the agent's own name: `Agent`, `Bash`, …),
    /// `parent_tool_call_id` (the subagent call it was made inside) and
    /// `subagent` `{description, agent_type, prompt}` on a call that starts
    /// a subagent.
    #[serde(rename = "tool.call")]
    ToolCall,
    #[serde(rename = "tool.result")]
    ToolResult,
    #[serde(rename = "approval.requested")]
    ApprovalRequested,
    #[serde(rename = "approval.resolved")]
    ApprovalResolved,
    #[serde(rename = "net.denied")]
    NetDenied,
    #[serde(rename = "turn.end")]
    TurnEnd,
    /// `{side_id, text}`: a side question was asked.
    #[serde(rename = "side.question")]
    SideQuestion,
    /// `{side_id, text}`: the answer (streamed as `session.delta` with `side_id`).
    #[serde(rename = "side.message")]
    SideMessage,
    /// `{side_id, stop_reason, error?}`: the side answer is complete.
    #[serde(rename = "side.end")]
    SideEnd,
    /// `{parent_tool_call_id, text}`: what a subagent said, kept out of the
    /// main reply. Its tool calls are ordinary `tool.call` / `tool.result`
    /// events whose `parent_tool_call_id` names the subagent's call.
    #[serde(rename = "subagent.message")]
    SubagentMessage,
    /// A file the agent sent you ([`AgentFile`]): a screenshot, a report, a
    /// recording of its browser tab. Read its bytes with `attachment.read`.
    /// Clients older than protocol 13 skip it.
    #[serde(rename = "agent.file")]
    AgentFile,
}


impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventType::SessionState => "session.state",
            EventType::Prompt => "prompt",
            EventType::AgentMessage => "agent.message",
            EventType::AgentThought => "agent.thought",
            EventType::ToolCall => "tool.call",
            EventType::ToolResult => "tool.result",
            EventType::ApprovalRequested => "approval.requested",
            EventType::ApprovalResolved => "approval.resolved",
            EventType::NetDenied => "net.denied",
            EventType::TurnEnd => "turn.end",
            EventType::SideQuestion => "side.question",
            EventType::SideMessage => "side.message",
            EventType::SideEnd => "side.end",
            EventType::SubagentMessage => "subagent.message",
            EventType::AgentFile => "agent.file",
        }
    }

    pub fn parse(s: &str) -> Option<EventType> {
        serde_json::from_value(Value::String(s.to_string())).ok()
    }
}

/// One immutable fact about a session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub session_id: String,
    pub seq: i64,
    #[serde(rename = "type")]
    pub kind: EventType,
    pub payload: Value,
    /// Milliseconds since the Unix epoch.
    pub ts: i64,
}

/// Ephemeral streaming chunk (`session.delta`), superseded by the coalesced event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionDelta {
    pub session_id: String,
    /// `agent.message`, `agent.thought` or `side.message`.
    pub kind: EventType,
    pub text: String,
    /// The side conversation a `side.message` chunk belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Device {
    pub device_id: String,
    pub name: String,
    pub capabilities: Vec<Capability>,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<i64>,
}

// ---------------------------------------------------------------------------
// Connectors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorAuth {
    None,
    /// A static bearer token, stored in the keychain.
    Bearer,
    /// OAuth 2.1 per the MCP authorization spec; tokens in the keychain.
    Oauth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicy {
    Auto,
    Ask,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectorTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub kind: ToolKind,
    /// Effective policy after per-tool and per-connector overrides.
    pub policy: ToolPolicy,
    /// Per-tool override, if one is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_override: Option<ToolPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Connector {
    pub connector_id: String,
    pub name: String,
    pub url: String,
    pub auth: ConnectorAuth,
    pub enabled: bool,
    /// `ok`, `needs_sign_in`, `error`, `unknown`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_detail: Option<String>,
    /// Policy for write tools without a per-tool override (`ask` by default).
    pub write_policy: ToolPolicy,
    /// Policy for read tools without a per-tool override (`auto` by default).
    pub read_policy: ToolPolicy,
    pub tools: Vec<ConnectorTool>,
    pub created_at: i64,
    /// The service's icon as a `data:image/png;base64,…` URL, fetched by
    /// the host when the connector was added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectorSearchParams {
    pub query: String,
}

/// An MCP server found in the integrations.sh catalog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectorSuggestion {
    /// "linear.app".
    pub domain: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The MCP endpoint, for `connector.add`.
    pub url: String,
    /// How it signs in: `oauth`, `none`, or what integrations.sh says
    /// (`api_key`, …; those can't be added here yet).
    pub auth: String,
    /// The icon as a `data:` URL, when the host could fetch it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// The prefix a connector's tools get when agents see them
/// (`<slug>__<tool>`): its name, lowercased, with runs of anything else as `_`.
/// A device as people read it: its name. Older events say "iPhone (dev_…)";
/// the id in brackets is left out.
pub fn device_display_name(label: &str) -> &str {
    match label.rfind(" (dev_") {
        Some(i) if label.ends_with(')') => &label[..i],
        _ => label,
    }
}

pub fn connector_slug(name: &str) -> String {
    let mut s = String::new();
    for c in name.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('_') {
            s.push('_');
        }
    }
    let s: String = s.trim_matches('_').chars().take(24).collect();
    let s = s.trim_end_matches('_').to_string();
    if s.is_empty() {
        "mcp".into()
    } else {
        s
    }
}

/// A name for a new connector at `host`: the service's name ("Sentry" for
/// `mcp.sentry.dev`), numbered when one by that name exists ("Sentry 2").
pub fn default_connector_name(host: &str, existing: &[&str]) -> String {
    let part = host
        .split('.')
        .find(|p| !matches!(*p, "mcp" | "www" | "api" | "app"))
        .unwrap_or("mcp");
    let mut base: String = part.to_string();
    if let Some(f) = base.get_mut(0..1) {
        f.make_ascii_uppercase();
    }
    let taken = |n: &str| existing.iter().any(|e| e.eq_ignore_ascii_case(n));
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|i| format!("{base} {i}"))
        .find(|n| !taken(n))
        .unwrap()
}

// ---------------------------------------------------------------------------
// Method params and results
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HelloParams {
    pub protocol_version: u32,
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(default)]
    pub client_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HelloResult {
    pub host_id: String,
    pub host_version: String,
    pub protocol_version: u32,
    pub capabilities: Vec<Capability>,
    /// Set when the connection comes from a paired device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Projects: new workspaces inside the folders the desktop allows
// ---------------------------------------------------------------------------

/// `project.browse`: the subfolders of `path`, which must be one of the
/// project folders or inside one. Without `path`: the project folders themselves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProjectBrowseParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectFolder {
    pub name: String,
    pub path: String,
    /// Set when the folder is already a workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectBrowse {
    /// The folders projects may be created in (absolute).
    pub roots: Vec<String>,
    /// The folder listed; `None` when listing the roots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// One level up, while that is still a root or inside one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Subfolders, by name; hidden folders are left out.
    pub folders: Vec<ProjectFolder>,
}

/// `project.create`: make `path` (inside a project folder) a workspace, or
/// first create the new folder `name` in `parent` (a project folder or inside one).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProjectCreateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `project.set_roots`: the folders phones may create projects in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectRootsParams {
    pub roots: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceAddParams {
    pub path: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Detected from the folder (a `.git` entry means `repo`) when omitted.
    #[serde(default)]
    pub kind: Option<WorkspaceKind>,
    #[serde(default)]
    pub network: Option<NetworkProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WorkspaceUpdateParams {
    pub workspace_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub network: Option<NetworkProfile>,
    /// Replace the enabled connector list.
    #[serde(default)]
    pub connectors: Option<Vec<String>>,
    /// Convenience: add one host to the custom profile.
    #[serde(default)]
    pub add_custom_host: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IdParams {
    #[serde(
        alias = "workspace_id",
        alias = "session_id",
        alias = "connector_id",
        alias = "device_id",
        alias = "profile_id"
    )]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SessionListParams {
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Keep sending `session.changed` notifications after the snapshot
    /// (archived sessions included, so clients can drop them).
    #[serde(default)]
    pub watch: bool,
    /// List the archived sessions instead of the others.
    #[serde(default)]
    pub archived: bool,
}

/// `workspace.set_default_agent`: the login new sessions in the workspace
/// start with. `agent: null` clears it (the next session sets it again).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceDefaultAgentParams {
    pub workspace_id: String,
    #[serde(default)]
    pub agent: Option<AgentKind>,
    /// A profile of `agent`; omitted or the agent's own name for its default login.
    #[serde(default)]
    pub profile_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionCreateParams {
    pub workspace_id: String,
    pub agent: AgentKind,
    /// Run as this [`AgentProfile`] of `agent` (from `host.info`'s
    /// `agents[].profile_id`). Omitted, or the agent's own name: the default login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// Start it from this [`crate::Note`] (protocol 9), in the note's
    /// workspace: the note lists the session, opens again if it was done, and
    /// is marked done when the session is archived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionPromptParams {
    pub session_id: String,
    /// Client-generated id; the same id twice produces one prompt.
    pub prompt_id: String,
    /// May be empty when there are attachments.
    pub text: String,
    /// Attachment ids from `upload.finish`, given to the agent with the text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionPromptResult {
    pub seq: i64,
    /// False when this prompt id had already been accepted.
    pub accepted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionRef {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventsSubscribeParams {
    pub session_id: String,
    /// Send events with `seq` greater than this, then stream live.
    #[serde(default)]
    pub after_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventsSubscribeResult {
    pub subscription_id: String,
    /// Number of stored events sent as replay before live streaming.
    pub replayed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovalRespondParams {
    pub approval_id: String,
    pub decision: ApprovalDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingApproval {
    pub approval_id: String,
    pub session_id: String,
    pub connector_id: String,
    pub connector_name: String,
    pub tool: String,
    pub arguments: Value,
    pub requested_at: i64,
    pub expires_at: i64,
    /// One line for the approval card, when the host has a clearer one than
    /// connector + tool ("Install agent-desktop 1.3.1 with npm, on this computer").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// One of the host's own tools (pushing, pull requests, installing,
/// running commands), and
/// whether it asks before it runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuiltinTool {
    /// "create_pull_request", "git_push", "install_tool", "run_command".
    pub name: String,
    pub title: String,
    pub description: String,
    /// `ask` or `auto`.
    pub policy: ToolPolicy,
    /// False for a tool that always asks (installing; a force-push always
    /// asks whatever this says).
    pub can_auto: bool,
    /// `run_command` only: the command prefixes that run without asking
    /// (`["gh", "pr", "edit"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<Vec<String>>,
}

/// `builtin.set_policy`. For `run_command`, `command` names a prefix
/// (`["gh", "pr", "edit"]`, as an approval's `allow_rule` offers it):
/// `auto` lets it run without asking, `ask` asks again.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuiltinPolicyParams {
    pub name: String,
    pub policy: ToolPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
}

/// The `connector_id` of approvals for the host's own tools (installing a
/// command-line tool on the user's computer), not a configured connector.
pub const BUILTIN_CONNECTOR_ID: &str = "agenthost";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectorAddParams {
    pub url: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub auth: Option<ConnectorAuth>,
    /// For `bearer` auth. Goes straight to the keychain.
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ConnectorUpdateParams {
    pub connector_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub write_policy: Option<ToolPolicy>,
    #[serde(default)]
    pub read_policy: Option<ToolPolicy>,
    /// Per-tool overrides; `null` clears an override.
    #[serde(default)]
    pub tool_policies: Option<std::collections::BTreeMap<String, Option<ToolPolicy>>>,
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectorSignInResult {
    /// Open this in a browser to finish OAuth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorize_url: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PairStartResult {
    pub pairing_id: String,
    /// The text encoded in the QR code.
    pub qr_payload: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PairingPending {
    pub pairing_id: String,
    pub device_name: String,
    /// Shown on both screens; the user confirms they match.
    pub short_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PairConfirmParams {
    pub pairing_id: String,
    pub accept: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceRenameParams {
    pub device_id: String,
    pub name: String,
}

// ---------------------------------------------------------------------------
// Attachments and voice
// ---------------------------------------------------------------------------

/// Largest file one upload may carry.
pub const MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;
/// Most attachments on one prompt.
pub const MAX_ATTACHMENTS_PER_PROMPT: usize = 10;
/// Largest raw piece in one `upload.append` (before base64).
pub const UPLOAD_CHUNK_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UploadBeginParams {
    /// File name as the user sees it; the host keeps only the last path part.
    pub name: String,
    /// Total size in bytes.
    pub size: u64,
    /// MIME type if the client knows it; otherwise the host guesses from the name.
    #[serde(default)]
    pub mime: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UploadBeginResult {
    pub upload_id: String,
    /// Send pieces of at most this many bytes (before base64).
    pub chunk_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UploadAppendParams {
    pub upload_id: String,
    /// Byte offset of this piece; must equal the bytes received so far.
    pub offset: u64,
    /// The piece, standard base64.
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UploadAppendResult {
    pub received: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UploadRef {
    pub upload_id: String,
}

/// A file the host holds for a prompt or a transcription.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attachment {
    pub attachment_id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

impl Attachment {
    pub fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }
}

/// What an agent sent you: the payload of an `agent.file` event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentFile {
    /// The file; its bytes come from `attachment.read` with the event's
    /// session.
    pub attachment: Attachment,
    #[serde(default)]
    pub kind: AgentFileKind,
    /// What the agent said about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    /// A still (JPEG) to show for a video before it plays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poster: Option<Attachment>,
    /// A video's length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// A picture's or video's size in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// The gateway call that sent it (`tool.call` / `tool.result`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentFileKind {
    /// A file from the agent's folder (`agenthost__send_file`).
    #[default]
    File,
    /// A recording of the agent's browser tab (MP4, H.264).
    Recording,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttachmentReadParams {
    pub session_id: String,
    pub attachment_id: String,
    /// Where this piece starts.
    #[serde(default)]
    pub offset: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttachmentReadResult {
    pub name: String,
    pub mime: String,
    /// The whole file's size.
    pub size: u64,
    pub offset: u64,
    /// Up to [`UPLOAD_CHUNK_BYTES`] from `offset`, standard base64. Empty at
    /// the end.
    pub data: String,
}

/// MIME type from a file name, for clients and the host alike.
pub fn guess_mime(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "pdf" => "application/pdf",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "json" => "application/json",
        "zip" => "application/zip",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "md" | "markdown" => "text/markdown",
        "txt" | "log" | "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "go" | "java" | "kt"
        | "swift" | "c" | "h" | "cc" | "cpp" | "hpp" | "rb" | "php" | "sh" | "toml" | "yaml"
        | "yml" | "xml" | "sql" | "css" | "scss" | "ini" | "cfg" | "conf" | "env" | "diff"
        | "patch" => "text/plain",
        _ => "application/octet-stream",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceTranscribeParams {
    /// A WAV recording from `upload.finish`.
    pub attachment_id: String,
    /// Language hint such as "en"; the model detects it when absent.
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceTranscribeResult {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceStreamBeginParams {
    /// Rate of the audio the client will send, in Hz.
    pub sample_rate: u32,
    /// Language hint such as "en"; the model detects it when absent.
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceStreamBeginResult {
    pub stream_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceStreamAppendParams {
    pub stream_id: String,
    /// Mono 16-bit little-endian PCM at the stream's rate, standard base64.
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceStreamFinishParams {
    pub stream_id: String,
    /// The last audio, as in `voice.stream_append`.
    #[serde(default)]
    pub data: Option<String>,
    /// Throw the stream away; the result's text is empty.
    #[serde(default)]
    pub cancel: bool,
}

/// `voice.stream_append` and `voice.stream_finish`: everything heard so far
/// (the whole text, not only the new words).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceStreamText {
    pub text: String,
}

/// Largest decoded piece in one `voice.stream_append`.
pub const VOICE_STREAM_CHUNK_BYTES: usize = 4 * 1024 * 1024;

/// The local speech model, as clients show it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct VoiceStatus {
    /// e.g. "NVIDIA Nemotron 3.5 ASR".
    pub model: String,
    /// Download size, for the "download?" prompt.
    pub download_bytes: u64,
    /// Installed and usable.
    pub ready: bool,
    /// Takes live dictation (`voice.stream_*`). Hosts before it leave this out.
    #[serde(default)]
    pub streaming: bool,
    /// Present while downloading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<VoiceProgress>,
    /// Why the last download or load failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceProgress {
    pub received: u64,
    pub total: u64,
}

// ---------------------------------------------------------------------------
// Screen sharing (screen-relay)
// ---------------------------------------------------------------------------

/// What a phone can look at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenMode {
    /// The display you are logged in to: a Mac's main display, an X11
    /// session, or a Wayland compositor with the wlroots capture protocols.
    Desktop,
    /// A separate desktop on the host with only a browser in it (Linux).
    /// Works with no monitor, a locked screen, and on GNOME and KDE.
    Browser,
}

/// What the desktop allows. Only an admin (the desktop) changes it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ScreenPolicy {
    /// Phones may start screen sharing at all.
    #[serde(default)]
    pub enabled: bool,
    /// Phones may click and type, not only watch.
    #[serde(default)]
    pub allow_control: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScreenModeInfo {
    pub mode: ScreenMode,
    pub available: bool,
    /// What it will show ("Main display", "X11 :0", "Hyprland (wayland-1)")
    /// or why it is not available.
    pub detail: String,
}

/// A running screen-relay engine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScreenRunning {
    pub mode: ScreenMode,
    pub control: bool,
    pub started_at: i64,
    /// Who started it (a device's name, or "local").
    pub started_by: String,
    /// Open viewers.
    pub viewers: u32,
    /// The monitor being shown ([`ScreenOutput::name`]), when the host knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// One monitor of the desktop, for showing a particular one in
/// [`ScreenMode::Desktop`]. Listed on Hyprland (Omarchy).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScreenOutput {
    /// The compositor's name for it, which `screen.start` takes: "DP-1".
    pub name: String,
    /// Make and model when the monitor reports them: "Dell Inc. DELL U2720Q".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// What the stream will be, in pixels.
    pub width: u32,
    pub height: u32,
    /// Has the keyboard focus on the host now.
    #[serde(default)]
    pub focused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScreenStatus {
    pub policy: ScreenPolicy,
    /// screen-relay was found on the host.
    pub installed: bool,
    pub modes: Vec<ScreenModeInfo>,
    /// The monitors [`ScreenMode::Desktop`] can show, first one the default.
    /// Empty when the host can't list them (one is shown then).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<ScreenOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running: Option<ScreenRunning>,
    /// Why the last start failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScreenStartParams {
    pub mode: ScreenMode,
    /// Ask to click and type; refused unless the policy allows control.
    #[serde(default)]
    pub control: bool,
    /// The monitor to show in [`ScreenMode::Desktop`] (a
    /// [`ScreenOutput::name`]); the first one when absent. Asking for
    /// another one than the running engine shows restarts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// A one-time ticket for `screen.attach` on a new tunnel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScreenStartResult {
    pub ticket: String,
    pub expires_in_ms: i64,
    pub mode: ScreenMode,
    pub control: bool,
    /// The monitor it will show, when the host knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScreenAttachParams {
    pub ticket: String,
}

// ---------------------------------------------------------------------------
// Push notifications (APNs)
// ---------------------------------------------------------------------------

/// Which events a phone wants a notification for.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct PushPrefs {
    /// A tool call waits for approval.
    #[serde(default = "yes")]
    pub approvals: bool,
    /// The agent finished its turn (the session is waiting for you).
    #[serde(default)]
    pub turn_done: bool,
    /// A session failed.
    #[serde(default)]
    pub failed: bool,
    /// An automation's run finished or failed.
    #[serde(default = "yes")]
    pub automations: bool,
}

fn yes() -> bool {
    true
}

impl Default for PushPrefs {
    fn default() -> Self {
        Self {
            approvals: true,
            turn_done: false,
            failed: false,
            automations: true,
        }
    }
}

/// APNs environment a device token belongs to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PushEnvironment {
    /// Development-signed builds (`api.sandbox.push.apple.com`).
    Sandbox,
    /// TestFlight and App Store builds (`api.push.apple.com`).
    Production,
}

impl PushEnvironment {
    pub fn as_str(&self) -> &'static str {
        match self {
            PushEnvironment::Sandbox => "sandbox",
            PushEnvironment::Production => "production",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "sandbox" => PushEnvironment::Sandbox,
            "production" => PushEnvironment::Production,
            _ => return None,
        })
    }
    pub fn other(&self) -> Self {
        match self {
            PushEnvironment::Sandbox => PushEnvironment::Production,
            PushEnvironment::Production => PushEnvironment::Sandbox,
        }
    }
}

/// `push.set_key`: the host's APNs auth key (from the Apple Developer
/// account the iOS app is signed with). Stored in the host's secret store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PushSetKeyParams {
    /// The `.p8` file's contents (PEM, PKCS#8, P-256).
    pub key_p8: String,
    /// The key's 10-character id (in the file name: `AuthKey_<KEY_ID>.p8`).
    pub key_id: String,
    /// The 10-character Apple team id.
    pub team_id: String,
}

/// A relay that sends notifications to Apple for the host (`POST
/// /v1/push/register` on it gave the phone `handle`). Protocol 6.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PushGateway {
    /// The relay's base URL (`https://…`).
    pub url: String,
    /// The phone's APNs token, sealed by that relay for this host only.
    pub handle: String,
}

/// `push.register`: this device wants notifications. Paired devices only.
/// Either `gateway` (the relay sends to Apple; the host needs no APNs key)
/// or `token` (the host sends to Apple itself with its own key).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PushRegisterParams {
    /// The APNs device token, hex. Empty with `gateway`.
    #[serde(default)]
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<PushGateway>,
    /// The app's bundle id (the APNs topic).
    pub topic: String,
    pub environment: PushEnvironment,
    /// 32 random bytes (base64url) the phone made: the host encrypts each
    /// notification's content with it (ChaCha20-Poly1305), so Apple sees
    /// only a generic alert.
    pub key: String,
    #[serde(default)]
    pub prefs: PushPrefs,
}

/// `push.set_prefs`: change what this device is notified about.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PushSetPrefsParams {
    pub prefs: PushPrefs,
}

/// `push.test`: send a test notification. A device tests itself; the desktop
/// names one (`device_id`) or, leaving it out, every registered device.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PushTestParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PushTestResult {
    /// Devices that got it.
    pub sent: u32,
    /// Why it did not reach the others: `<device name>: <push_error_text>`,
    /// which ends with APNs' or the relay's reason in brackets (hosts before
    /// this change sent the bare reason).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// One device that gets notifications.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PushDevice {
    pub device_id: String,
    pub name: String,
    pub environment: PushEnvironment,
    pub prefs: PushPrefs,
    pub registered_at: i64,
    /// The last time APNs refused a notification for it, and why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// `last_error` in words ([`push_error_text`]), for clients to show as is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_text: Option<String>,
    /// The relay that sends its notifications (protocol 6); none: the host
    /// sends them with its own APNs key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
}

/// Why a notification wasn't sent, in words: `reason` is what APNs, the
/// relay or the host said (`InvalidProviderToken`, `BadHandle`,
/// `NoAPNsKey`, …); `via_relay` says whose APNs key Apple saw (the relay's
/// Worker secrets, or the host's own key).
pub fn push_error_text(reason: &str, via_relay: bool) -> String {
    let key = if via_relay {
        "the relay's APNs key (its APNS_KEY, APNS_KEY_ID and APNS_TEAM_ID secrets)"
    } else {
        "the host's APNs key (`agenthost push status` shows its key and team id)"
    };
    match reason {
        "InvalidProviderToken" => format!(
            "Apple doesn't accept {key} (InvalidProviderToken). Check, in this order: the key is \
             still listed under Certificates, IDs & Profiles → Keys on developer.apple.com (a \
             revoked key disappears from that list, but its .p8 file still looks fine); the key \
             id is that key's; the team id is the team the key belongs to; the key's environment \
             is Sandbox & Production."
        ),
        "BadEnvironmentKeyInToken" | "BadCertificateEnvironment" => format!(
            "{key} only works in the other APNs environment ({reason}). Builds from Xcode use the \
             sandbox, TestFlight and App Store builds production. Use a key for Sandbox & Production."
        ),
        "ExpiredProviderToken" | "TooManyProviderTokenUpdates" | "MissingProviderToken" => {
            format!("Apple refused the sign-in with {key} ({reason}). Try again in a minute.")
        }
        "TopicDisallowed" | "DeviceTokenNotForTopic" | "BadTopic" => format!(
            "{key} isn't for this app ({reason}): it must belong to the team that signs the app, \
             and a topic-specific key must list the app's bundle id."
        ),
        "BadDeviceToken" => "Apple doesn't recognise this iPhone's token in either environment \
            (BadDeviceToken). Turn notifications off and on again in the app; if that doesn't \
            help, reinstall it."
            .into(),
        "Unregistered" => "The app was removed from the iPhone, or its notifications turned off \
            (Unregistered), so the host forgot it. Turn notifications on again in the app."
            .into(),
        "NoAPNsKey" => "Nothing was sent: no relay sends this iPhone's notifications, and the host \
            has no APNs key (NoAPNsKey). Add the key on the iPhone (Settings → Notifications) or \
            with `agenthost push setup`."
            .into(),
        "RelayKeyInvalid" => "The relay's APNS_KEY secret isn't a .p8 key (RelayKeyInvalid). Paste \
            the whole AuthKey_….p8 file into it, including the BEGIN and END lines."
            .into(),
        "BadSignature" => "The relay didn't accept the host's signature (BadSignature). Check that \
            the host's clock is right."
            .into(),
        "BadHandle" | "PushUnavailable" => format!(
            "The relay no longer takes this iPhone's registration ({reason}). Open the app again \
             so it registers anew."
        ),
        "TooManyRequests" => "Too many notifications in a short time (TooManyRequests); some were \
            held back."
            .into(),
        "ApnsUnreachable" | "InternalServerError" | "ServiceUnavailable" | "Shutdown" => {
            format!("Apple's push service didn't answer ({reason}). Try again later.")
        }
        "BadPayload" | "BadCollapseId" | "PayloadTooLarge" => format!(
            "The relay refused the notification ({reason}), which is a bug in AgentHost."
        ),
        // Already a sentence (e.g. "could not reach the relay …").
        r if r.contains(' ') => r.to_string(),
        r => format!("Not sent ({r})."),
    }
}

/// `push.status`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PushStatus {
    /// The host has an APNs key of its own. Phones registered through a
    /// relay (`PushDevice::gateway`) don't need one.
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    /// The calling device's registration (phones only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub this_device: Option<PushDevice>,
    /// Every registered device (the desktop only; empty for phones).
    #[serde(default)]
    pub devices: Vec<PushDevice>,
}

/// The decrypted content of a notification (the `e` field of the APNs
/// payload, sealed with the device's key).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PushContent {
    /// `approval`, `turn_done`, `failed` or `test`.
    pub kind: String,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_errors_say_whose_key_and_what_to_check() {
        let relay = push_error_text("InvalidProviderToken", true);
        assert!(relay.contains("relay's APNs key") && relay.contains("Keys on developer.apple.com"), "{relay}");
        assert!(push_error_text("InvalidProviderToken", false).contains("host's APNs key"));
        assert_eq!(push_error_text("could not reach the relay: timeout", true), "could not reach the relay: timeout");
        assert_eq!(push_error_text("Weird", true), "Not sent (Weird).");
        assert!(!push_error_text("BadDeviceToken", true).contains("  "), "no stray spaces from line breaks");
    }

    /// Gemini CLI sessions and profiles, stored and sent as `gemini`, load as Antigravity.
    #[test]
    fn gemini_is_antigravity() {
        assert_eq!(AgentKind::parse("gemini"), Some(AgentKind::Antigravity));
        assert_eq!(AgentKind::parse("agy"), Some(AgentKind::Antigravity));
        let k: AgentKind = serde_json::from_str("\"gemini\"").unwrap();
        assert_eq!(k, AgentKind::Antigravity);
        assert_eq!(serde_json::to_string(&k).unwrap(), "\"antigravity\"");
        assert_eq!(AgentKind::parse(k.as_str()), Some(k));
    }

    #[test]
    fn device_names_leave_out_the_id() {
        assert_eq!(device_display_name("iPhone (dev_1ff8d2a306a682421fd0)"), "iPhone");
        assert_eq!(device_display_name("Nico's iPhone"), "Nico's iPhone");
        assert_eq!(device_display_name("local"), "local");
    }

    #[test]
    fn connector_names_and_slugs() {
        assert_eq!(default_connector_name("mcp.sentry.dev", &[]), "Sentry");
        assert_eq!(
            default_connector_name("mcp.sentry.dev", &["sentry"]),
            "Sentry 2"
        );
        assert_eq!(
            default_connector_name("mcp.sentry.dev", &["Sentry", "Sentry 2"]),
            "Sentry 3"
        );
        assert_eq!(connector_slug("Sentry (work)"), "sentry_work");
        assert_eq!(connector_slug("GitHub Issues!"), "github_issues");
        assert_eq!(connector_slug("--"), "mcp");
    }

    #[test]
    fn state_machine() {
        use SessionState::*;
        assert!(Created.can_transition_to(Starting));
        assert!(Starting.can_transition_to(Idle));
        assert!(Idle.can_transition_to(Running));
        assert!(Running.can_transition_to(WaitingApproval));
        assert!(WaitingApproval.can_transition_to(Running));
        assert!(Running.can_transition_to(Idle));
        assert!(Idle.can_transition_to(Ended));
        for s in [Created, Starting, Idle, Running, WaitingApproval] {
            assert!(s.can_transition_to(Failed));
        }
        assert!(!Ended.can_transition_to(Idle));
        assert!(!Failed.can_transition_to(Starting));
        assert!(!Created.can_transition_to(Running));
        assert!(!Idle.can_transition_to(WaitingApproval));
    }

    #[test]
    fn event_type_names() {
        assert_eq!(EventType::parse("net.denied"), Some(EventType::NetDenied));
        assert_eq!(EventType::parse("agent.file"), Some(EventType::AgentFile));
        assert_eq!(EventType::AgentFile.as_str(), "agent.file");
        assert_eq!(
            serde_json::to_value(EventType::TurnEnd).unwrap(),
            Value::String("turn.end".into())
        );
        assert_eq!(EventType::ToolResult.as_str(), "tool.result");
    }

    #[test]
    fn finds_pull_request_links() {
        let pr = |t: &str| SessionPullRequest::find_in(t).map(|p| (p.number, p.url));
        assert_eq!(pr("https://github.com/o/r/pull/48\n"), Some((48, "https://github.com/o/r/pull/48".into())));
        assert_eq!(
            pr("Creating pull request for a into main in o/r\n\nhttps://ghe.example.com/o/r/pull/48483"),
            Some((48483, "https://ghe.example.com/o/r/pull/48483".into()))
        );
        assert_eq!(
            pr("a pull request for branch \"x\" into branch \"main\" already exists:\nhttps://github.com/o/r/pull/7."),
            Some((7, "https://github.com/o/r/pull/7".into()))
        );
        assert_eq!(pr("see https://github.com/o/r/pull/1 then https://github.com/o/r/pull/2"), Some((2, "https://github.com/o/r/pull/2".into())));
        assert_eq!(pr("https://github.com/o/r/pull/48/files"), None);
        assert_eq!(pr("https://github.com/o/r/issues/48"), None);
        assert_eq!(pr("http://github.com/o/r/pull/48"), None);
        assert_eq!(pr("https://github.com/o/r/pull/abc"), None);
        assert_eq!(pr("error: no commits between main and x"), None);
    }
}
