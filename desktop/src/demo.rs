//! Named scenes with sample data, for headless snapshots of every screen and state
//! (`agenthost-desktop --snap out.png --scene session`). Each renders in the look
//! `AGENTHOST_DESKTOP_LOOK` picks (macos or gnome).

use crate::model::{App, Conn, Menu, Pairing, Screen, Tab};
use agenthost_proto::*;
use serde_json::{json, Value};

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
    });
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
    s.voice = Some(VoiceStatus { model: "NVIDIA Nemotron 3.5 ASR".into(), download_bytes: 475_271_763, ready: true, progress: None, error: None });
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
    let events = vec![
        ev(sid, 1, 0, EventType::SessionState, json!({"state": "created"})),
        ev(sid, 2, 1_000, EventType::Prompt, json!({"text": "The resume test fails about one run in ten on CI. Find out why and fix it.", "device": "iPhone"})),
        ev(sid, 3, 2_000, EventType::AgentThought, json!({"text": "Look at the test first, then the actor's restart path."})),
        ev(sid, 4, 14_000, EventType::ToolCall, json!({"tool_call_id": "t1", "name": "cargo test -p agenthost-host resume", "kind": "execute", "arguments": {"command": "cargo test -p agenthost-host resume"}, "source": "sandbox"})),
        ev(sid, 5, 22_400, EventType::ToolResult, json!({"tool_call_id": "t1", "status": "completed", "output": "running 1 test\ntest resume_after_host_restart_keeps_history ... ok\n\ntest result: ok. 1 passed; 0 failed; finished in 3.21s"})),
        ev(sid, 6, 23_000, EventType::NetDenied, json!({"host": "index.crates.io", "port": 443, "reason": "host is not on the allow list"})),
        ev(sid, 7, 30_000, EventType::AgentMessage, json!({"text": "The test passes locally. The flake comes from a race: `wait_state` polls before the resumed session writes its first event, so a slow CI runner sees the old state. I made the helper wait for the event instead and opened an issue to track the timeout."})),
        ev(sid, 8, 31_000, EventType::ToolCall, json!({"tool_call_id": "t2", "name": "Edited crates/host/tests/common/mod.rs", "kind": "edit", "arguments": {"path": "crates/host/tests/common/mod.rs"}, "source": "sandbox"})),
        ev(sid, 9, 31_600, EventType::ToolResult, json!({"tool_call_id": "t2", "status": "completed", "output": "1 file changed, 12 insertions(+), 3 deletions(-)"})),
        ev(sid, 10, 33_000, EventType::ApprovalRequested, json!({"approval_id": "apr_1", "connector_name": "GitHub", "tool": "create_issue", "arguments": {"repo": "nico/relay", "title": "Resume test: wait for the first event, not the state", "body": "Flake seen in 1 of 10 CI runs …"}})),
    ];
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
    s.expanded.insert("t1".into());
}

fn chip(s: &mut App) {
    use crate::model::{FileState, PendingFile};
    s.files.insert(
        "ses_1".into(),
        vec![PendingFile { local_id: 1, name: "ci-run-4812.log".into(), state: FileState::Ready(Attachment { attachment_id: "att_9".into(), name: "ci-run-4812.log".into(), mime: "text/plain".into(), size: 49_152 }) }],
    );
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
    ("offline-home", |s| {
        base(s);
        s.screen = Screen::Home;
        s.conn = Conn::Down("connect to host at /run/user/1000/agenthost/host.sock: No such file or directory".into());
    }),
];
