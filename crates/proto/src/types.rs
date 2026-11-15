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
    /// Scripted ACP agent used for tests. Only offered when the host enables it.
    Fake,
}

impl AgentKind {
    pub const REAL: [AgentKind; 3] = [AgentKind::Codex, AgentKind::Claude, AgentKind::Opencode];

    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::Codex => "codex",
            AgentKind::Claude => "claude",
            AgentKind::Opencode => "opencode",
            AgentKind::Fake => "fake",
        }
    }

    pub fn parse(s: &str) -> Option<AgentKind> {
        Some(match s {
            "codex" => AgentKind::Codex,
            "claude" | "claude-code" | "claude_code" => AgentKind::Claude,
            "opencode" => AgentKind::Opencode,
            "fake" => AgentKind::Fake,
            _ => return None,
        })
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            AgentKind::Codex => "Codex",
            AgentKind::Claude => "Claude Code",
            AgentKind::Opencode => "OpenCode",
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPreset {
    /// npm, PyPI, crates.io and Go module registries.
    Packages,
    /// github.com and its API and content hosts.
    Github,
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
    /// `agent.message` or `agent.thought`.
    pub kind: EventType,
    pub text: String,
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
        alias = "device_id"
    )]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SessionListParams {
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Keep sending `session.changed` notifications after the snapshot.
    #[serde(default)]
    pub watch: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionCreateParams {
    pub workspace_id: String,
    pub agent: AgentKind,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionPromptParams {
    pub session_id: String,
    /// Client-generated id; the same id twice produces one prompt.
    pub prompt_id: String,
    pub text: String,
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
}

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

#[cfg(test)]
mod tests {
    use super::*;

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
