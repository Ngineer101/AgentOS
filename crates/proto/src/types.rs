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
    Gemini,
    /// Scripted ACP agent used for tests. Only offered when the host enables it.
    Fake,
}

impl AgentKind {
    pub const REAL: [AgentKind; 5] = [
        AgentKind::Codex,
        AgentKind::Claude,
        AgentKind::Opencode,
        AgentKind::Cursor,
        AgentKind::Gemini,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::Codex => "codex",
            AgentKind::Claude => "claude",
            AgentKind::Opencode => "opencode",
            AgentKind::Cursor => "cursor",
            AgentKind::Gemini => "gemini",
            AgentKind::Fake => "fake",
        }
    }

    pub fn parse(s: &str) -> Option<AgentKind> {
        Some(match s {
            "codex" => AgentKind::Codex,
            "claude" | "claude-code" | "claude_code" => AgentKind::Claude,
            "opencode" => AgentKind::Opencode,
            "cursor" | "cursor-agent" | "cursor_agent" => AgentKind::Cursor,
            "gemini" | "gemini-cli" | "gemini_cli" => AgentKind::Gemini,
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
            // The folder is Gemini's home: its state goes in `<folder>/.gemini`.
            AgentKind::Gemini => "GEMINI_CLI_HOME",
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
            AgentKind::Gemini => "Gemini CLI",
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
}

/// Where a session's worktree is and which branch it is on.
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
    /// The new title; empty or omitted clears it.
    #[serde(default)]
    pub title: Option<String>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScreenStatus {
    pub policy: ScreenPolicy,
    /// screen-relay was found on the host.
    pub installed: bool,
    pub modes: Vec<ScreenModeInfo>,
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
}

/// A one-time ticket for `screen.attach` on a new tunnel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScreenStartResult {
    pub ticket: String,
    pub expires_in_ms: i64,
    pub mode: ScreenMode,
    pub control: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScreenAttachParams {
    pub ticket: String,
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            serde_json::to_value(EventType::TurnEnd).unwrap(),
            Value::String("turn.end".into())
        );
        assert_eq!(EventType::ToolResult.as_str(), "tool.result");
    }
}
