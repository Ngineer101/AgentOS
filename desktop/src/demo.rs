//! Named scenes with sample data, for headless snapshots of every screen and state
//! (`agenthost-desktop --snap out.png --scene session`). Each renders in the look
//! `AGENTHOST_DESKTOP_LOOK` picks (macos or gnome).

use crate::model::{App, Conn, Menu, Pairing, Screen, Tab};
use agenthost_proto::*;
use serde_json::{json, Value};

/// Small service icons, as the host sends them (`data:image/png;base64,…`).
pub const ICON_LINEAR: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAeklEQVR42u2XwQkAIQwEU6kFWO415P3EE/GiyarIBvwpM/hYsiLVhPgk5JHeoOFdiVXwpsRq+EdiFzxLUGDmUTmWO1MCnvBhAW/4kAACrhZAwVUCSPivABquFrAKmgQ8fodBxCBiEDGI7gsi7oTXdINzqtkR5XRXPX8BH+WScw5ywDcAAAAASUVORK5CYII=";
pub const ICON_SENTRY: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAg0lEQVR42mNgQANmupH/aYkZ8AFaW47XEfSyHKsj6G05iiMGynK4I0YdQI4mXIDmDiAW0MQBpAKqOoBcQBUHEGs4uY4g2QHUVs9Aqu/JCTGqOYDcBDvqgOHjALonwgHPhoOiIBrwonhQVEaDojoeFA2S0TbhsOsbDJ6u2aDonA5U9xwA6lYz8RidE1wAAAAASUVORK5CYII=";
pub const ICON_STRIPE: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAaElEQVR42u3Xyw0AIQhFUaqwQCuxc105iWRkfuEzwE1cc3bmAaBKq53zARX3cRIhdfwUIX18QWgdPxAJSMCvAbOYAJwoYFcMwFW+AXdjATzNF+BtPgBfy+84AfYB6sPExDQzMU615vkABXZ0G2LNeaEAAAAASUVORK5CYII=";

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// An event `at` ms after the session's first one (two minutes ago).
fn ev(sid: &str, seq: i64, at: i64, kind: EventType, payload: Value) -> Event {
    Event { session_id: sid.into(), seq, kind, payload, ts: now() - 120_000 + at }
}

fn base(s: &mut App) {
    s.conn = Conn::Connected;
    s.hostname = "workstation".into();
    let t = now();
    s.project_roots = vec!["/home/nico/Code".into()];
    s.workspaces = vec![
        Workspace {
            workspace_id: QUICK_WORKSPACE_ID.into(),
            name: "Quick tasks".into(),
            path: "/home/nico/.local/share/agenthost-scratch".into(),
            kind: WorkspaceKind::Scratch,
            network: NetworkProfile { presets: vec![NetworkPreset::Web], custom_hosts: vec![] },
            connectors: vec!["con_gh".into()],
            created_at: 0,
            default_agent: None,
            default_profile_id: None,
        },
        Workspace {
            workspace_id: "ws_relay".into(),
            name: "relay".into(),
            path: "/home/nico/Code/relay".into(),
            kind: WorkspaceKind::Repo,
            network: NetworkProfile { presets: vec![NetworkPreset::Packages], custom_hosts: vec!["docs.rs".into()] },
            connectors: vec!["con_gh".into()],
            created_at: t,
            default_agent: Some(AgentKind::Claude),
            default_profile_id: Some("prof_work".into()),
        },
        Workspace {
            workspace_id: "ws_notes".into(),
            name: "notes".into(),
            path: "/home/nico/notes".into(),
            kind: WorkspaceKind::Folder,
            network: NetworkProfile::default(),
            connectors: vec![],
            created_at: t,
            default_agent: None,
            default_profile_id: None,
        },
        Workspace {
            workspace_id: "ws_demo".into(),
            name: "demo".into(),
            path: "/home/nico/Code/demo".into(),
            kind: WorkspaceKind::Folder,
            network: NetworkProfile::default(),
            connectors: vec![],
            created_at: t,
            default_agent: None,
            default_profile_id: None,
        },
    ];
    let sess = |id: &str, ws: &str, agent, profile: Option<&str>, state, title: &str, ago_ms: i64| Session {
        session_id: id.into(),
        workspace_id: ws.into(),
        agent,
        profile_id: profile.map(String::from),
        state,
        state_reason: None,
        acp_session_id: Some("x".into()),
        title: Some(title.into()),
        created_at: t - ago_ms,
        updated_at: t - ago_ms,
        last_seq: 0,
        archived_at: None,
        worktree: None,
        config_options: vec![],
        side_questions: false,
    };
    s.sessions = vec![
        sess("ses_q", QUICK_WORKSPACE_ID, AgentKind::Claude, Some("prof_personal"), SessionState::Idle, "What changed in Rust 1.92?", 180_000),
        sess("ses_1", "ws_relay", AgentKind::Claude, Some("prof_work"), SessionState::WaitingApproval, "Fix the flaky resume test", 40_000),
        sess("ses_2", "ws_relay", AgentKind::Codex, None, SessionState::Running, "Add OAuth refresh", 300_000),
        sess("ses_3", "ws_relay", AgentKind::Opencode, None, SessionState::Idle, "Review the relay worker", 7_200_000),
        sess("ses_5", "ws_notes", AgentKind::Codex, None, SessionState::Failed, "Tidy the reading list", 1_500_000),
        sess("ses_4", "ws_notes", AgentKind::Claude, None, SessionState::Ended, "Weekly summary", 90_000_000),
    ];
    if let Some(f) = s.sessions.iter_mut().find(|x| x.session_id == "ses_5") {
        f.state_reason = Some("codex exited with status 1: model rate limit reached".into());
    }
    // Several sessions in one git workspace, each on a branch of its own.
    for (id, branch) in [("ses_1", "agenthost/fix-flaky-resume"), ("ses_2", "agenthost/oauth-refresh"), ("ses_3", "agenthost/review-relay-worker")] {
        if let Some(x) = s.sessions.iter_mut().find(|x| x.session_id == id) {
            x.worktree = Some(SessionWorktree { path: format!("/home/nico/.local/share/agenthost/worktrees/{id}"), branch: branch.into() });
        }
    }
    if let Some(x) = s.sessions.iter_mut().find(|x| x.session_id == "ses_1") {
        x.side_questions = true;
        x.config_options = config_options();
    }
    let archived = |id: &str, ws: &str, agent, profile: Option<&str>, title: &str, ago_ms: i64| Session {
        state: SessionState::Ended,
        archived_at: Some(t - ago_ms),
        ..sess(id, ws, agent, profile, SessionState::Ended, title, ago_ms + 3_600_000)
    };
    s.archived = vec![
        archived("ses_a1", "ws_relay", AgentKind::Claude, Some("prof_work"), "Port the CLI to clap 5", 7_200_000),
        archived("ses_a2", QUICK_WORKSPACE_ID, AgentKind::Claude, Some("prof_personal"), "Summarise the Rust 2027 RFC", 86_400_000 * 2),
        archived("ses_a3", "ws_notes", AgentKind::Codex, None, "Draft the release notes", 86_400_000 * 9),
    ];
    s.connectors = vec![Connector {
        connector_id: "con_gh".into(),
        name: "GitHub".into(),
        url: "https://api.githubcopilot.com/mcp/".into(),
        auth: ConnectorAuth::Oauth,
        enabled: true,
        status: "ok".into(),
        status_detail: None,
        write_policy: ToolPolicy::Ask,
        read_policy: ToolPolicy::Auto,
        tools: vec![
            ConnectorTool { name: "search_issues".into(), description: Some("Search issues and pull requests".into()), kind: ToolKind::Read, policy: ToolPolicy::Auto, policy_override: None },
            ConnectorTool { name: "get_file_contents".into(), description: Some("Read a file from a repository".into()), kind: ToolKind::Read, policy: ToolPolicy::Auto, policy_override: None },
            ConnectorTool { name: "create_issue".into(), description: Some("Open a new issue".into()), kind: ToolKind::Write, policy: ToolPolicy::Ask, policy_override: None },
            ConnectorTool { name: "create_pull_request".into(), description: Some("Open a pull request".into()), kind: ToolKind::Write, policy: ToolPolicy::Auto, policy_override: Some(ToolPolicy::Auto) },
            ConnectorTool { name: "merge_pull_request".into(), description: Some("Merge a pull request".into()), kind: ToolKind::Write, policy: ToolPolicy::Disabled, policy_override: Some(ToolPolicy::Disabled) },
        ],
        created_at: t,
        icon: None,
    }];
    let sentry = |id: &str, name: &str, status: &str| Connector {
        connector_id: id.into(),
        name: name.into(),
        url: "https://mcp.sentry.dev/mcp".into(),
        auth: ConnectorAuth::Oauth,
        enabled: true,
        status: status.into(),
        status_detail: None,
        write_policy: ToolPolicy::Ask,
        read_policy: ToolPolicy::Auto,
        tools: if status == "ok" {
            vec![
                ConnectorTool { name: "search_issues".into(), description: Some("Search issues in an organization".into()), kind: ToolKind::Read, policy: ToolPolicy::Auto, policy_override: None },
                ConnectorTool { name: "update_issue".into(), description: Some("Resolve, assign or ignore an issue".into()), kind: ToolKind::Write, policy: ToolPolicy::Ask, policy_override: None },
            ]
        } else {
            vec![]
        },
        created_at: t,
        icon: Some(ICON_SENTRY.into()),
    };
    s.connectors.push(sentry("con_sw", "Sentry (work)", "ok"));
    s.connectors.push(sentry("con_sp", "Sentry (personal)", "needs_sign_in"));
    s.connectors.push(Connector {
        connector_id: "con_dw".into(),
        name: "DeepWiki".into(),
        url: "https://mcp.deepwiki.com/mcp".into(),
        auth: ConnectorAuth::None,
        enabled: true,
        status: "ok".into(),
        status_detail: None,
        write_policy: ToolPolicy::Ask,
        read_policy: ToolPolicy::Auto,
        tools: ["read_wiki_structure", "read_wiki_contents", "ask_question"]
            .iter()
            .map(|n| ConnectorTool { name: n.to_string(), description: None, kind: ToolKind::Read, policy: ToolPolicy::Auto, policy_override: None })
            .collect(),
        created_at: t,
        icon: None,
    });
    s.builtins = vec![
        BuiltinTool { name: "create_pull_request".into(), title: "Open pull requests".into(), description: "Opens a GitHub pull request with your GitHub CLI login.".into(), policy: ToolPolicy::Ask, can_auto: true, allowed: vec![] },
        BuiltinTool { name: "git_push".into(), title: "Push branches".into(), description: "Pushes with your git credentials. A force-push always asks.".into(), policy: ToolPolicy::Auto, can_auto: true, allowed: vec![] },
        BuiltinTool { name: "install_tool".into(), title: "Install tools on this computer".into(), description: "Runs a package manager as you, outside the sandbox. Always asks.".into(), policy: ToolPolicy::Ask, can_auto: false, allowed: vec![] },
        BuiltinTool {
            name: "run_command".into(),
            title: "Run commands as you".into(),
            description: "Runs a program as you, outside the sandbox, with your logins. Asks, except for the commands you let run.".into(),
            policy: ToolPolicy::Ask,
            can_auto: false,
            allowed: vec![vec!["gh".into(), "pr".into(), "comment".into()]],
        },
    ];
    s.devices = vec![Device {
        device_id: "dev_1".into(),
        name: "Nico's iPhone".into(),
        capabilities: vec![],
        created_at: t,
        last_seen_at: Some(t - 120_000),
        revoked_at: None,
    }];
    s.info = Some(HostInfo {
        host_id: "host_3f9a1c".into(),
        version: "0.1.0".into(),
        os: "linux".into(),
        protocol_version: 2,
        agents: vec![
            agent(AgentKind::Claude, "Claude Code", "2.1.323", Some("claude"), None, None, true),
            agent(AgentKind::Claude, "Claude Code", "2.1.323", Some("prof_work"), Some("Work"), Some("/home/nico/.claude-work"), true),
            agent(AgentKind::Claude, "Claude Code", "2.1.323", Some("prof_personal"), Some("Personal"), Some("/home/nico/.claude-p"), true),
            agent(AgentKind::Codex, "Codex", "2.6.0", Some("codex"), None, None, true),
            agent(AgentKind::Opencode, "OpenCode", "2.3.0", Some("opencode"), None, None, true),
            AgentStatus {
                agent: AgentKind::Gemini,
                display_name: "Gemini CLI".into(),
                installed: true,
                version: Some("0.9.1".into()),
                logged_in: false,
                available: false,
                problem: Some("not logged in".into()),
                fix: Some("gemini auth login".into()),
                profile_id: Some("gemini".into()),
                profile_name: None,
                config_dir: None,
            },
        ],
        sandbox: SandboxHealth {
            backend: "bubblewrap".into(),
            ok: true,
            prerequisites: ["bwrap", "socat", "rg", "node", "sandbox-runtime"].iter().map(|n| Prerequisite { name: n.to_string(), present: true, install: None }).collect(),
            runtime_version: Some("0.0.83".into()),
            problem: None,
        },
        relay: RelayStatus { configured: true, connected: true, url: Some("wss://relay.example.workers.dev".into()), error: None },
    });
    s.voice = Some(VoiceStatus { model: "NVIDIA Nemotron 3.5 ASR".into(), download_bytes: 475_271_763, ready: true, streaming: true, progress: None, error: None });
}

/// Claude Code's settings as ACP reports them: a model, an effort and fast mode.
fn config_options() -> Vec<SessionConfigOption> {
    let v = |value: &str, name: &str, d: Option<&str>| SessionConfigValue { value: value.into(), name: name.into(), description: d.map(String::from) };
    vec![
        SessionConfigOption {
            id: "model".into(),
            name: "Model".into(),
            description: None,
            category: Some("model".into()),
            current_value: "opus".into(),
            options: vec![v("default", "Default (recommended)", Some("Opus 4.5 for hard problems, Sonnet 4.5 for the rest")), v("opus", "Opus 4.5", Some("The most capable")), v("sonnet", "Sonnet 4.5", Some("Fast, for everyday tasks")), v("haiku", "Haiku 4.5", Some("The fastest"))],
        },
        SessionConfigOption {
            id: "effort".into(),
            name: "Effort".into(),
            description: Some("How long the model thinks".into()),
            category: Some("thought_level".into()),
            current_value: "high".into(),
            options: vec![v("low", "Low", None), v("medium", "Medium", None), v("high", "High", None), v("max", "Max", None)],
        },
        SessionConfigOption {
            id: "fast".into(),
            name: "Fast mode".into(),
            description: Some("Faster output at a higher cost".into()),
            category: None,
            current_value: "on".into(),
            options: vec![v("on", "On", None), v("off", "Off", None)],
        },
        SessionConfigOption { id: "mode".into(), name: "Mode".into(), description: None, category: Some("mode".into()), current_value: "default".into(), options: vec![v("default", "Ask", None), v("plan", "Plan", None)] },
    ]
}

/// Two side conversations in ses_1: one answered, one still streaming.
fn side(s: &mut App) {
    let sid = "ses_1";
    let t = s.transcripts.entry(sid.into()).or_default();
    let n = t.last_seq;
    let more = vec![
        ev(sid, n + 1, 40_000, EventType::SideQuestion, json!({"side_id": "side_a", "text": "Why does wait_state poll instead of subscribing to events?"})),
        ev(sid, n + 2, 46_000, EventType::SideMessage, json!({"side_id": "side_a", "text": "It predates `events.subscribe`: the helper was written when the host only had `session.get`. Polling every 50 ms is simple but racy, because the state can change and change back between polls.\n\nSubscribing would make the test **deterministic**:\n\n```rust\nlet mut sub = client.subscribe(&sid, last_seq).await?;\nwhile let Some(e) = sub.next().await { /* … */ }\n```"})),
        ev(sid, n + 3, 46_500, EventType::SideEnd, json!({"side_id": "side_a", "stop_reason": "end_turn"})),
        ev(sid, n + 4, 50_000, EventType::SideQuestion, json!({"side_id": "side_b", "text": "Is the same race in the iOS client's reconnect?"})),
    ];
    t.events.extend(more);
    t.last_seq = n + 4;
    t.side_live.insert("side_b".into(), "Probably not. The iOS client resubscribes from its last `seq` on every reconnect, so a state change that happens".into());
    s.side_open = true;
    s.side_tab.insert(sid.into(), "side_b".into());
}

fn suggestions() -> Vec<ConnectorSuggestion> {
    let sug = |domain: &str, name: &str, d: &str, url: &str, auth: &str, icon: Option<&str>| ConnectorSuggestion {
        domain: domain.into(),
        name: name.into(),
        description: Some(d.into()),
        url: url.into(),
        auth: auth.into(),
        icon: icon.map(String::from),
    };
    vec![
        sug("linear.app", "Linear", "Find, create and update issues, projects and comments", "https://mcp.linear.app/mcp", "oauth", Some(ICON_LINEAR)),
        sug("sentry.io", "Sentry", "Search issues and events, and look into errors", "https://mcp.sentry.dev/mcp", "oauth", Some(ICON_SENTRY)),
        sug("stripe.com", "Stripe", "Payments, customers and subscriptions", "https://mcp.stripe.com", "api_key", Some(ICON_STRIPE)),
        sug("deepwiki.com", "DeepWiki", "Ask questions about any public GitHub repository", "https://mcp.deepwiki.com/mcp", "none", None),
    ]
}

fn agent(kind: AgentKind, name: &str, version: &str, id: Option<&str>, profile: Option<&str>, dir: Option<&str>, ok: bool) -> AgentStatus {
    AgentStatus {
        agent: kind,
        display_name: name.into(),
        installed: true,
        version: Some(version.into()),
        logged_in: ok,
        available: ok,
        problem: None,
        fix: None,
        profile_id: id.map(String::from),
        profile_name: profile.map(String::from),
        config_dir: dir.map(String::from),
    }
}

fn transcript(s: &mut App) {
    let sid = "ses_1";
    // (time, kind, payload); seqs follow the order.
    let rows = vec![
        (0, EventType::SessionState, json!({"state": "created"})),
        (1_000, EventType::Prompt, json!({"text": "The resume test fails about one run in ten on CI. Find out why and fix it.", "device": "iPhone"})),
        (2_000, EventType::AgentThought, json!({"text": "Look at the test first, then the actor's restart path."})),
        (5_000, EventType::ToolCall, json!({"tool_call_id": "t0", "name": "Read crates/host/tests/resume.rs", "kind": "read", "arguments": {"path": "crates/host/tests/resume.rs"}, "source": "sandbox"})),
        (5_100, EventType::ToolResult, json!({"tool_call_id": "t0", "status": "completed"})),
        (7_000, EventType::ToolCall, json!({"tool_call_id": "tg", "name": "grep -rn \"fn wait_state\" crates/host/tests", "kind": "execute", "arguments": {"command": "grep -rn \"fn wait_state\" crates/host/tests"}, "source": "sandbox"})),
        (7_300, EventType::ToolResult, json!({"tool_call_id": "tg", "status": "completed", "output": "crates/host/tests/common/mod.rs:88:pub async fn wait_state("})),
        (14_000, EventType::ToolCall, json!({"tool_call_id": "t1", "name": "cargo test -p agenthost-host resume", "kind": "execute", "arguments": {"command": "cargo test -p agenthost-host resume"}, "source": "sandbox"})),
        (22_400, EventType::ToolResult, json!({"tool_call_id": "t1", "status": "completed", "output": "running 1 test\ntest resume_after_host_restart_keeps_history ... ok\n\ntest result: ok. 1 passed; 0 failed; finished in 3.21s"})),
        (23_000, EventType::NetDenied, json!({"host": "index.crates.io", "port": 443, "reason": "host is not on the allow list"})),
        (30_000, EventType::AgentMessage, json!({"text": "The test passes locally. The flake comes from a race: `wait_state` polls before the resumed session writes its first event, so a slow CI runner sees the old state. I made the helper wait for the event instead and opened an issue to track the timeout."})),
        (31_000, EventType::ToolCall, json!({"tool_call_id": "t2", "name": "Edited crates/host/tests/common/mod.rs", "kind": "edit", "arguments": {"path": "crates/host/tests/common/mod.rs"}, "source": "sandbox"})),
        (31_600, EventType::ToolResult, json!({"tool_call_id": "t2", "status": "completed", "output": "1 file changed, 12 insertions(+), 3 deletions(-)"})),
        (33_000, EventType::ApprovalRequested, json!({"approval_id": "apr_1", "connector_name": "GitHub", "tool": "create_issue", "arguments": {"repo": "nico/relay", "title": "Resume test: wait for the first event, not the state", "body": "Flake seen in 1 of 10 CI runs …"}})),
    ];
    let events: Vec<Event> = rows.into_iter().enumerate().map(|(i, (at, kind, p))| ev(sid, i as i64 + 1, at, kind, p)).collect();
    let t = s.transcripts.entry(sid.into()).or_default();
    t.last_seq = events.len() as i64;
    t.events = events;
    s.approvals = vec![PendingApproval {
        approval_id: "apr_1".into(),
        session_id: sid.into(),
        connector_id: "con_gh".into(),
        connector_name: "GitHub".into(),
        tool: "create_issue".into(),
        arguments: json!({"repo": "nico/relay"}),
        requested_at: now(),
        expires_at: now() + 570_000,
        summary: None,
    }];
    s.expanded.insert("act-3".into());
    s.expanded.insert("t1".into());
}

fn chip(s: &mut App) {
    use crate::model::{FileState, PendingFile};
    s.files.insert(
        "ses_1".into(),
        vec![PendingFile { local_id: 1, name: "ci-run-4812.log".into(), state: FileState::Ready(Attachment { attachment_id: "att_9".into(), name: "ci-run-4812.log".into(), mime: "text/plain".into(), size: 49_152 }) }],
    );
}

/// The session's turn ended: it waits for you.
fn idle(s: &mut App, id: &str) {
    if let Some(x) = s.sessions.iter_mut().find(|x| x.session_id == id) {
        x.state = SessionState::Idle;
    }
}

fn session(s: &mut App) {
    base(s);
    transcript(s);
    s.screen = Screen::Session("ses_1".into());
}

fn pairing(s: &mut App, confirm: bool) {
    base(s);
    s.settings = Some(Tab::Devices);
    s.pairing = Some(Pairing {
        pairing_id: "pair_1".into(),
        qr: "agenthost://pair?v=1&r=wss%3A%2F%2Frelay.example.workers.dev&id=ab7dbd0000000000000000000000000&hk=AAAA&s=BBBB&n=box".into(),
        expires_at: now() + 120_000,
        pending: confirm.then(|| PairingPending { pairing_id: "pair_1".into(), device_name: "Nico's iPhone".into(), short_code: "197 372".into() }),
        done: None,
    });
}

/// "Audit the relay worker": two subagents started together, one done with a report, one
/// still running (4 steps, the last under way, and something it said).
/// The open session's approval as one of the host's own: a pull request or a push.
fn builtin_approval(s: &mut App, tool: &str, force: bool) {
    session(s);
    let sid = s.approvals.first().map(|a| a.session_id.clone()).unwrap_or_default();
    let a = &mut s.approvals[0];
    a.connector_id = BUILTIN_CONNECTOR_ID.into();
    a.connector_name = "AgentHost".into();
    a.tool = tool.into();
    if tool == "run_command" {
        a.summary = Some("Run gh pr edit on this computer".into());
        a.arguments = json!({
            "command": "gh pr edit 6 --body-file -",
            "argv": ["gh", "pr", "edit", "6", "--body-file", "-"],
            "details": "Archiving a session no longer asks first; an Undo toast brings it back for five seconds.",
            "details_label": "Input",
            "reason": "You asked me to update the pull request's description to match the change.",
            "approve_label": "Run", "ask_title": "Run a Command?",
            "note": "Runs with your logins and your access to files, in this session's folder. Run only what you expected.",
            "allow_rule": ["gh", "pr", "edit"], "allow_rule_text": "gh pr edit",
        });
    } else if tool == "create_pull_request" {
        a.summary = Some("Open a pull request \u{201c}Fix the flaky resume test\u{201d} from agenthost/fix-flaky-resume into main".into());
        a.arguments = json!({
            "command": "gh pr create --base main --head agenthost/fix-flaky-resume --title 'Fix the flaky resume test'",
            "title": "Fix the flaky resume test", "base": "main", "head": "agenthost/fix-flaky-resume", "draft": false,
            "details": "The resume test raced the subscription: it could resume before the host had sent the backlog. It now waits for the backlog first.\n\ncargo test -p agenthost-host resume passes 200 runs in a row.",
            "reason": "The fix is pushed; opening the pull request you asked for.",
            "approve_label": "Open Pull Request", "ask_title": "Open a Pull Request?",
            "note": "Posts to GitHub as you, with your GitHub CLI login. Push the branch first.",
        });
    } else {
        let flag = if force { " --force-with-lease" } else { "" };
        a.summary = Some(format!("{} agenthost/fix-flaky-resume to origin", if force { "Force-push" } else { "Push" }));
        a.arguments = json!({
            "command": format!("git push{flag} origin agenthost/fix-flaky-resume"),
            "remote": "origin", "branch": "agenthost/fix-flaky-resume", "force": force,
            "reason": if force { "I rebased the branch on main." } else { "Pushing the fix so I can open the pull request." },
            "approve_label": if force { "Force-Push" } else { "Push" }, "ask_title": "Push?",
            "note": if force { "Replaces the remote branch if nobody else pushed to it since your last fetch. Uses your git credentials. Nothing from the repository runs (its hooks are skipped)." } else { "Uses your git credentials. Nothing from the repository runs (its hooks are skipped)." },
        });
    }
    let aid = a.approval_id.clone();
    if let Some(t) = s.transcripts.get_mut(&sid) {
        for e in t.events.iter_mut() {
            if e.kind == EventType::ApprovalRequested && e.payload["approval_id"] == aid {
                e.payload["connector_id"] = json!(BUILTIN_CONNECTOR_ID);
                e.payload["connector_name"] = json!("AgentHost");
                e.payload["tool"] = json!(tool);
                e.payload["arguments"] = Value::Null;
            }
        }
    }
}

fn subagents(s: &mut App) {
    base(s);
    let sid = "ses_6";
    let t0 = now();
    s.sessions.insert(
        0,
        Session {
            session_id: sid.into(),
            workspace_id: "ws_relay".into(),
            agent: AgentKind::Claude,
            profile_id: Some("prof_work".into()),
            state: SessionState::Running,
            state_reason: None,
            acp_session_id: Some("x".into()),
            title: Some("Audit the relay worker".into()),
            created_at: t0 - 120_000,
            updated_at: t0 - 5_000,
            last_seq: 0,
            archived_at: None,
            worktree: Some(SessionWorktree { path: "/home/nico/.local/share/agenthost/worktrees/ses_6".into(), branch: "agenthost/audit-relay".into() }),
            config_options: config_options(),
            side_questions: true,
        },
    );
    let call = |id: &str, parent: &str, name: &str, kind: &str, args: Value| json!({"tool_call_id": id, "name": name, "kind": kind, "arguments": args, "source": "sandbox", "tool_name": if kind == "execute" { "Bash" } else { "Read" }, "parent_tool_call_id": parent});
    let done = |id: &str, parent: &str, out: &str| json!({"tool_call_id": id, "status": "completed", "output": out, "parent_tool_call_id": parent});
    let read = |p: &str| json!({"file_path": p});
    let report = "Found **2 issues**: the pairing token is compared with `==`, and revoked devices keep their socket for 30 s.\n\n## Findings\n\n1. **Timing-unsafe compare** in `handlers/pair.rs:88`: `if token == expected` leaks the match length. Use `subtle::ConstantTimeEq`.\n2. **Revocation lag** in `handlers/tunnel.rs`: a revoked device's socket stays open until its next 30 s heartbeat. Close it when `device.revoke` lands.\n\nNothing else stood out: every handler checks the session token before it reads the body.";
    let mut evs: Vec<(i64, EventType, Value)> = vec![
        (0, EventType::SessionState, json!({"state": "created"})),
        (1_000, EventType::Prompt, json!({"text": "Check the worker for auth bugs and slow paths."})),
        (4_000, EventType::AgentMessage, json!({"text": "I'll split this up: one agent reviews auth, another profiles the hot paths."})),
        (5_000, EventType::ToolCall, json!({"tool_call_id": "ag1", "name": "Review auth handling", "kind": "other", "tool_name": "Agent", "source": "sandbox", "arguments": {"description": "Review auth handling", "subagent_type": "Explore"}, "subagent": {"description": "Review auth handling", "agent_type": "Explore", "prompt": "Review how relay-worker authenticates devices and sessions: pairing tokens, session tokens, revocation. Report concrete bugs with file and line, and say what you checked that was fine."}})),
        (5_200, EventType::ToolCall, json!({"tool_call_id": "ag2", "name": "Profile the hot paths", "kind": "other", "tool_name": "Agent", "source": "sandbox", "arguments": {"description": "Profile the hot paths", "subagent_type": "general-purpose"}, "subagent": {"description": "Profile the hot paths", "agent_type": "general-purpose", "prompt": "Measure request latency in relay-worker and find the slowest handler."}})),
    ];
    // The auth review: nine steps, then its report.
    let a1 = [
        ("s1", "Read handlers/pair.rs", "read", read("relay-worker/src/handlers/pair.rs")),
        ("s2", "Read handlers/tunnel.rs", "read", read("relay-worker/src/handlers/tunnel.rs")),
        ("s3", "rg -n \"token\" src", "execute", json!({"command": "rg -n \"token\" src"})),
        ("s4", "Read auth.rs", "read", read("relay-worker/src/auth.rs")),
        ("s5", "Read handlers/devices.rs", "read", read("relay-worker/src/handlers/devices.rs")),
        ("s6", "rg -n \"revoke\" src", "execute", json!({"command": "rg -n \"revoke\" src"})),
        ("s7", "Read heartbeat.rs", "read", read("relay-worker/src/heartbeat.rs")),
        ("s8", "Read session.rs", "read", read("relay-worker/src/session.rs")),
        ("s9", "cargo test -p relay-worker auth", "execute", json!({"command": "cargo test -p relay-worker auth"})),
    ];
    for (i, (id, name, kind, args)) in a1.into_iter().enumerate() {
        let at = 7_000 + i as i64 * 7_000;
        evs.push((at, EventType::ToolCall, call(id, "ag1", name, kind, args)));
        evs.push((at + 2_500, EventType::ToolResult, done(id, "ag1", if kind == "execute" { "ok" } else { "(file contents)" })));
        if i == 3 {
            evs.push((at + 3_000, EventType::SubagentMessage, json!({"parent_tool_call_id": "ag1", "text": "The pairing check compares the token with ==. Looking at revocation next."})));
        }
    }
    evs.push((77_000, EventType::ToolResult, json!({"tool_call_id": "ag1", "status": "completed", "output": report})));
    // The profiler: three steps done, a benchmark under way.
    let a2 = [
        ("p1", "Read handlers/tunnel.rs", "read", read("relay-worker/src/handlers/tunnel.rs"), Some("(file contents)")),
        ("p2", "Read handlers/pair.rs", "read", read("relay-worker/src/handlers/pair.rs"), Some("(file contents)")),
        ("p3", "rg -n \"await\" src/handlers", "execute", json!({"command": "rg -n \"await\" src/handlers"}), Some("src/handlers/tunnel.rs:41:    let peer = registry.lookup(&id).await?;\nsrc/handlers/tunnel.rs:57:    sink.send(frame).await?;\nsrc/handlers/pair.rs:30:    let row = db.get(&code).await?;")),
        ("p4", "cargo bench -p relay-worker", "execute", json!({"command": "cargo bench -p relay-worker"}), None),
    ];
    for (i, (id, name, kind, args, out)) in a2.into_iter().enumerate() {
        let at = 80_000 + i as i64 * 6_000;
        evs.push((at, EventType::ToolCall, call(id, "ag2", name, kind, args)));
        if let Some(o) = out {
            evs.push((at + 2_000, EventType::ToolResult, done(id, "ag2", o)));
        }
        if i == 2 {
            evs.push((at + 3_000, EventType::SubagentMessage, json!({"parent_tool_call_id": "ag2", "text": "tunnel.rs awaits the registry lookup on every frame. Benchmarking to see how much it costs."})));
        }
    }
    evs.sort_by_key(|e| e.0);
    let events: Vec<Event> = evs.into_iter().enumerate().map(|(i, (at, kind, p))| ev(sid, i as i64 + 1, at, kind, p)).collect();
    let tr = s.transcripts.entry(sid.into()).or_default();
    tr.last_seq = events.len() as i64;
    tr.events = events;
    s.screen = Screen::Session(sid.into());
}

pub const SCENES: &[(&str, fn(&mut App))] = &[
    ("session", |s| {
        session(s);
        chip(s);
    }),
    ("session-compose", |s| {
        use crate::model::{FileState, PendingFile};
        session(s);
        s.approvals.clear();
        s.voice.as_mut().unwrap().ready = false;
        s.ask_voice_download = true;
        s.prompt = rune::TextEdit::new("Here is the crash from this morning");
        s.files.insert(
            "ses_1".into(),
            vec![
                PendingFile { local_id: 1, name: "crash-report.txt".into(), state: FileState::Ready(Attachment { attachment_id: "att_9".into(), name: "crash-report.txt".into(), mime: "text/plain".into(), size: 18_400 }) },
                PendingFile { local_id: 2, name: "recording of the bug.mov".into(), state: FileState::Uploading },
            ],
        );
    }),
    ("session-voice-download", |s| {
        session(s);
        s.approvals.clear();
        let v = s.voice.as_mut().unwrap();
        v.ready = false;
        v.progress = Some(VoiceProgress { received: 190_000_000, total: 475_271_763 });
        s.mic = crate::model::Mic::Transcribing;
    }),
    ("session-no-sidebar", |s| {
        session(s);
        s.sidebar_open = false;
    }),
    ("session-menu", |s| {
        session(s);
        s.menu = Some(Menu::Session);
    }),
    ("main-menu", |s| {
        session(s);
        s.menu = Some(Menu::Main);
    }),
    ("home", |s| {
        base(s);
        s.screen = Screen::Home;
    }),
    ("new", |s| {
        session(s);
        s.new_ws = Some("ws_relay".into());
        s.new_agent = Some("prof_work".into());
        s.new_open = true;
    }),
    ("new-other-login", |s| {
        session(s);
        s.new_ws = Some("ws_relay".into());
        s.new_agent = Some("claude".into());
        s.make_default = true;
        s.new_open = true;
    }),
    ("confirm-archive", |s| {
        session(s);
        s.confirm_archive = Some("ses_1".into());
    }),
    ("row-menu", |s| {
        session(s);
        s.menu = Some(Menu::Row("ses_3".into(), 120.0, 320.0));
    }),
    ("failed", |s| {
        session(s);
        s.screen = Screen::Session("ses_5".into());
    }),
    ("settings-archived", |s| {
        base(s);
        s.settings = Some(Tab::Archived);
    }),
    ("archived-open", |s| {
        base(s);
        let t = s.transcripts.entry("ses_a1".into()).or_default();
        t.events = vec![
            ev("ses_a1", 1, 0, EventType::SessionState, json!({"state": "created"})),
            ev("ses_a1", 2, 1_000, EventType::Prompt, json!({"text": "Port the CLI's argument parsing to clap 5."})),
            ev("ses_a1", 3, 40_000, EventType::AgentMessage, json!({"text": "Done: the derive macros moved to `#[command]` and `#[arg]`, and `cargo test -p agenthost-cli` passes."})),
            ev("ses_a1", 4, 41_000, EventType::TurnEnd, json!({"stop_reason": "end_turn"})),
            ev("ses_a1", 5, 50_000, EventType::SessionState, json!({"state": "ended", "reason": "archived"})),
        ];
        t.last_seq = 5;
        s.screen = Screen::Session("ses_a1".into());
    }),
    ("new-login-menu", |s| {
        session(s);
        s.new_ws = Some("ws_relay".into());
        s.new_agent = Some("prof_work".into());
        s.new_open = true;
        s.menu = Some(Menu::Login);
    }),
    ("settings-general", |s| {
        base(s);
        s.settings = Some(Tab::General);
    }),
    ("settings-agents", |s| {
        base(s);
        s.settings = Some(Tab::Agents);
        s.sel_agent = Some("prof_work".into());
    }),
    ("settings-agents-add", |s| {
        base(s);
        s.settings = Some(Tab::Agents);
        s.adding_profile = true;
        s.profile_agent = Some(AgentKind::Claude);
    }),
    ("settings-workspaces", |s| {
        base(s);
        s.settings = Some(Tab::Workspaces);
        s.sel_ws = Some("ws_relay".into());
    }),
    ("settings-connectors", |s| {
        base(s);
        s.settings = Some(Tab::Connectors);
        s.sel_conn = Some("con_gh".into());
    }),
    ("install-approval", |s| {
        session(s);
        // Replace the GitHub approval with an install request.
        let sid = s.approvals.first().map(|a| a.session_id.clone()).unwrap_or_default();
        let a = &mut s.approvals[0];
        a.connector_id = BUILTIN_CONNECTOR_ID.into();
        a.connector_name = "AgentHost".into();
        a.tool = "install_tool".into();
        a.summary = Some("Install agent-desktop 1.3.1 with npm, on this computer".into());
        a.arguments = json!({"command": "npm install --global agent-desktop@1.3.1", "manager": "npm", "package": "agent-desktop", "version": "1.3.1", "reason": "You asked me to set up agent-desktop so you can drive desktop apps from the terminal."});
        let aid = a.approval_id.clone();
        if let Some(t) = s.transcripts.get_mut(&sid) {
            for e in t.events.iter_mut() {
                if e.kind == EventType::ApprovalRequested && e.payload["approval_id"] == aid {
                    e.payload["connector_id"] = json!(BUILTIN_CONNECTOR_ID);
                    e.payload["connector_name"] = json!("AgentHost");
                    e.payload["tool"] = json!("install_tool");
                }
            }
        }
    }),
    ("pr-approval", |s| builtin_approval(s, "create_pull_request", false)),
    ("command-approval", |s| builtin_approval(s, "run_command", false)),
    ("push-approval", |s| {
        builtin_approval(s, "git_push", false);
        // Pushes ask in this one, to show the card's offer.
        s.builtins[1].policy = ToolPolicy::Ask;
    }),
    ("force-push-approval", |s| {
        builtin_approval(s, "git_push", true);
        s.builtins[1].policy = ToolPolicy::Ask;
    }),
    ("pr-no-ask-toast", |s| {
        builtin_approval(s, "create_pull_request", false);
        s.approvals.clear();
        s.builtins[0].policy = ToolPolicy::Auto;
        let line = s.no_ask_line("create_pull_request");
        s.push_toast(crate::model::ToastKind::Info, line, Some(crate::model::ToastAction::OpenSettings(Tab::Connectors)), std::time::Instant::now());
    }),
    ("settings-builtins", |s| {
        base(s);
        s.settings = Some(Tab::Connectors);
        s.sel_conn = Some("con_gh".into());
    }),
    ("settings-connectors-tools", |s| {
        base(s);
        s.settings = Some(Tab::Connectors);
        s.sel_conn = Some("con_gh".into());
        s.expanded.insert("tools-con_gh".into());
    }),
    ("settings-connectors-add", |s| {
        base(s);
        s.settings = Some(Tab::Connectors);
        s.adding_connector = true;
        s.signing_in.insert("con_sp".into(), "https://mcp.sentry.dev/oauth/authorize?response_type=code&client_id=…".into());
    }),
    ("settings-devices", |s| {
        base(s);
        s.settings = Some(Tab::Devices);
    }),
    ("settings-screen", |s| {
        base(s);
        s.settings = Some(Tab::Devices);
        s.screen_share = Some(ScreenStatus {
            policy: ScreenPolicy { enabled: true, allow_control: true },
            installed: true,
            modes: vec![
                ScreenModeInfo { mode: ScreenMode::Desktop, available: true, detail: "Hyprland (wayland-1)".into() },
                ScreenModeInfo { mode: ScreenMode::Browser, available: true, detail: "chromium in a separate 1280x800 desktop".into() },
            ],
            outputs: vec![],
            running: Some(ScreenRunning { mode: ScreenMode::Browser, control: true, started_at: now() - 90_000, started_by: "Nico's iPhone".into(), viewers: 1, output: None }),
            error: None,
        });
    }),
    ("pairing", |s| pairing(s, false)),
    ("pairing-confirm", |s| pairing(s, true)),
    ("shortcuts", |s| {
        session(s);
        s.shortcuts_open = true;
    }),
    ("offline", |s| {
        session(s);
        s.conn = Conn::Down("connect to host at /run/user/1000/agenthost/host.sock: No such file or directory".into());
    }),
    // Two subagents started together: one done, one running.
    ("subagents", subagents),
    // The agent panel on the running one, and on the finished one with its report.
    ("subagents-panel-running", |s| {
        subagents(s);
        s.open_agent("ses_6", "ag2");
    }),
    ("subagents-panel-done", |s| {
        subagents(s);
        s.open_agent("ses_6", "ag1");
    }),
    // A step opened in the panel.
    ("subagents-panel-step", |s| {
        subagents(s);
        s.open_agent("ses_6", "ag2");
        s.expanded.insert("p3".into());
    }),
    ("side-panel", |s| {
        session(s);
        s.approvals.clear();
        side(s);
    }),
    ("side-panel-pending", |s| {
        session(s);
        s.approvals.clear();
        side(s);
        s.side_pending.insert("ses_1".into(), "Which tests touch the relay?".into());
    }),
    ("composer-pickers", |s| {
        session(s);
        s.approvals.clear();
        s.prompt = rune::TextEdit::new("Now make the timeout configurable");
    }),
    ("composer-picker-open", |s| {
        session(s);
        s.approvals.clear();
        s.menu = Some(Menu::Model);
    }),
    // The composer at rest: nothing typed, so Send is dimmed.
    ("composer", |s| {
        session(s);
        s.approvals.clear();
        idle(s, "ses_1");
    }),
    // While the agent works an empty composer's button stops the turn.
    ("composer-working", |s| {
        session(s);
        s.approvals.clear();
        if let Some(x) = s.sessions.iter_mut().find(|x| x.session_id == "ses_1") {
            x.state = SessionState::Running;
        }
    }),
    // Just sent, the host not yet answered: the message shows at once, "Working…" under it.
    ("session-sent", |s| {
        session(s);
        s.approvals.clear();
        idle(s, "ses_1");
        s.transcripts.entry("ses_1".into()).or_default().pending.push(crate::model::PendingPrompt {
            prompt_id: "p_demo".into(),
            payload: json!({"text": "Now make the timeout configurable"}),
        });
    }),
    // A session without model settings: no chip, the shortcut hint instead.
    ("composer-no-model", |s| {
        session(s);
        s.screen = Screen::Session("ses_3".into());
    }),
    ("model-popover", |s| {
        session(s);
        s.approvals.clear();
        idle(s, "ses_1");
        s.menu = Some(Menu::Model);
    }),
    // The effort knob mid-drag, on Max (the setting changes on release).
    ("model-popover-drag", |s| {
        session(s);
        s.approvals.clear();
        idle(s, "ses_1");
        s.menu = Some(Menu::Model);
        s.effort_drag = Some(3);
    }),
    ("composer-slash", |s| {
        session(s);
        s.approvals.clear();
        s.prompt = rune::TextEdit::new("/b");
        s.prompt.focused = true;
    }),
    ("composer-paste", |s| {
        use crate::model::{FileState, PendingFile};
        session(s);
        s.approvals.clear();
        s.files.insert(
            "ses_1".into(),
            vec![
                PendingFile { local_id: 1, name: "Pasted image.png".into(), state: FileState::Ready(Attachment { attachment_id: "att_p1".into(), name: "Pasted image.png".into(), mime: "image/png".into(), size: 284_102 }) },
                PendingFile { local_id: 2, name: "Pasted image 2.png".into(), state: FileState::Uploading },
            ],
        );
    }),
    ("connector-search", |s| {
        base(s);
        s.settings = Some(Tab::Connectors);
        s.adding_connector = true;
        s.conn_search = rune::TextEdit::new("issues");
        s.search_query = "issues".into();
        s.suggestions = suggestions();
    }),
    ("connector-search-empty", |s| {
        base(s);
        s.settings = Some(Tab::Connectors);
        s.adding_connector = true;
        s.conn_search.focused = true;
    }),
    ("connector-add-url", |s| {
        base(s);
        s.settings = Some(Tab::Connectors);
        s.adding_connector = true;
        s.add_manual = true;
        s.conn_url = rune::TextEdit::new("https://mcp.example.com/mcp");
        s.conn_name = rune::TextEdit::new("project-x");
    }),
    ("connector-rename", |s| {
        base(s);
        s.settings = Some(Tab::Connectors);
        s.sel_conn = Some("con_sw".into());
        s.renaming = Some("con_sw".into());
        s.rename = rune::TextEdit::new("work");
        s.rename.focused = true;
    }),
    ("connector-rename-error", |s| {
        base(s);
        s.settings = Some(Tab::Connectors);
        s.sel_conn = Some("con_sw".into());
        s.renaming = Some("con_sw".into());
        s.rename = rune::TextEdit::new("GitHub");
        s.rename_error = Some(("con_sw".into(), "Another connector is already called GitHub: its tools would clash.".into()));
    }),
    ("connector-detail", |s| {
        base(s);
        s.settings = Some(Tab::Connectors);
        s.sel_conn = Some("con_sw".into());
    }),
    ("toasts", |s| {
        session(s);
        s.screen = Screen::Session("ses_2".into());
        let now = std::time::Instant::now();
        s.push_toast(crate::model::ToastKind::Info, "Signed in to Sentry (work). 2 tools available.".into(), None, now);
        s.push_toast(crate::model::ToastKind::Error, "Send prompt failed: the session is not running".into(), None, now);
        let a = s.approvals[0].clone();
        let line = s.approval_line(&a);
        s.push_toast(crate::model::ToastKind::Approval, line, Some(crate::model::ToastAction::ShowApproval { session_id: a.session_id, approval_id: a.approval_id }), now);
    }),
    // The header: the session's own title over login · workspace · branch.
    ("header-titled", |s| {
        session(s);
        s.approvals.clear();
        idle(s, "ses_1");
    }),
    // An untitled quick task: "Quick task" over the login.
    ("header-quick-untitled", |s| {
        session(s);
        if let Some(x) = s.sessions.iter_mut().find(|x| x.session_id == "ses_q") {
            x.title = None;
        }
        s.screen = Screen::Session("ses_q".into());
    }),
    // An untitled session in a git workspace: the workspace's name, the branch beside the login.
    ("header-untitled", |s| {
        session(s);
        if let Some(x) = s.sessions.iter_mut().find(|x| x.session_id == "ses_2") {
            x.title = None;
        }
        s.screen = Screen::Session("ses_2".into());
    }),
    // An ended session keeps its workspace and branch in the header.
    ("header-ended", |s| {
        session(s);
        s.approvals.clear();
        if let Some(x) = s.sessions.iter_mut().find(|x| x.session_id == "ses_1") {
            x.state = SessionState::Ended;
        }
    }),
    // Rename Session…: the header's title is a field.
    ("rename-session", |s| {
        session(s);
        s.approvals.clear();
        idle(s, "ses_1");
        s.start_session_rename();
        s.title_edit = rune::TextEdit::new("Fix the flaky resume test on CI");
        s.title_edit.focused = true;
    }),
    // The ⋯ menu with Rename Session… and Copy Branch Name.
    ("session-menu-rename", |s| {
        session(s);
        s.approvals.clear();
        idle(s, "ses_1");
        s.menu = Some(Menu::Session);
    }),
    // A model called "Default (recommended)": the chip says Default.
    ("composer-default-model", |s| {
        session(s);
        s.approvals.clear();
        idle(s, "ses_1");
        if let Some(o) = s.sessions.iter_mut().find(|x| x.session_id == "ses_1").and_then(|x| x.config_options.iter_mut().find(|o| o.id == "model")) {
            o.current_value = "default".into();
        }
    }),
    ("composer-default-model-open", |s| {
        session(s);
        s.approvals.clear();
        idle(s, "ses_1");
        if let Some(o) = s.sessions.iter_mut().find(|x| x.session_id == "ses_1").and_then(|x| x.config_options.iter_mut().find(|o| o.id == "model")) {
            o.current_value = "default".into();
        }
        s.menu = Some(Menu::Model);
    }),
    // Copying the branch from the header.
    ("copied-branch", |s| {
        session(s);
        s.approvals.clear();
        idle(s, "ses_1");
        s.notice = Some("Copied branch name".into());
    }),
    ("offline-home", |s| {
        base(s);
        s.screen = Screen::Home;
        s.conn = Conn::Down("connect to host at /run/user/1000/agenthost/host.sock: No such file or directory".into());
    }),
];
