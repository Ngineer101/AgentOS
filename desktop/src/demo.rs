//! Named scenes with sample data, for headless snapshots of every screen
//! (`agenthost-desktop --snap out.png --scene session`).

use crate::model::{App, Conn, Pairing, Screen};
use agenthost_proto::*;
use serde_json::{json, Value};

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn ev(sid: &str, seq: i64, kind: EventType, payload: Value) -> Event {
    Event { session_id: sid.into(), seq, kind, payload, ts: now() - 60_000 + seq * 1000 }
}

fn base(s: &mut App) {
    s.conn = Conn::Connected;
    let t = now();
    s.workspaces = vec![
        Workspace {
            workspace_id: "ws_relay".into(),
            name: "relay".into(),
            path: "/home/nico/Code/relay".into(),
            kind: WorkspaceKind::Repo,
            network: NetworkProfile { presets: vec![NetworkPreset::Packages], custom_hosts: vec!["docs.rs".into()] },
            connectors: vec!["con_gh".into()],
            created_at: t,
        },
        Workspace {
            workspace_id: "ws_notes".into(),
            name: "notes".into(),
            path: "/home/nico/notes".into(),
            kind: WorkspaceKind::Folder,
            network: NetworkProfile::default(),
            connectors: vec![],
            created_at: t,
        },
    ];
    let sess = |id: &str, ws: &str, agent, state, title: &str, ago_ms: i64| Session {
        session_id: id.into(),
        workspace_id: ws.into(),
        agent,
        state,
        state_reason: None,
        acp_session_id: Some("x".into()),
        title: Some(title.into()),
        created_at: t - ago_ms,
        updated_at: t - ago_ms,
        last_seq: 0,
    };
    s.sessions = vec![
        sess("ses_1", "ws_relay", AgentKind::Claude, SessionState::WaitingApproval, "Fix the flaky resume test", 40_000),
        sess("ses_2", "ws_relay", AgentKind::Codex, SessionState::Running, "Add OAuth refresh", 300_000),
        sess("ses_3", "ws_relay", AgentKind::Opencode, SessionState::Idle, "Review the relay worker", 7_200_000),
        sess("ses_4", "ws_notes", AgentKind::Claude, SessionState::Ended, "Weekly summary", 90_000_000),
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
    s.signing_in.insert("con_sp".into(), "https://mcp.sentry.dev/oauth/authorize?response_type=code&client_id=…".into());
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
            AgentStatus { agent: AgentKind::Codex, display_name: "Codex".into(), installed: true, version: Some("2.6.0".into()), logged_in: true, available: true, problem: None, fix: None },
            AgentStatus { agent: AgentKind::Claude, display_name: "Claude Code".into(), installed: true, version: Some("0.107.0".into()), logged_in: true, available: true, problem: None, fix: None },
            AgentStatus {
                agent: AgentKind::Opencode,
                display_name: "OpenCode".into(),
                installed: true,
                version: Some("2.3.0".into()),
                logged_in: false,
                available: false,
                problem: Some("not logged in".into()),
                fix: Some("opencode auth login".into()),
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

fn transcript(s: &mut App) {
    let sid = "ses_1";
    let events = vec![
        ev(sid, 1, EventType::SessionState, json!({"state": "created"})),
        ev(sid, 2, EventType::Prompt, json!({"text": "The resume test fails about one run in ten. Find out why and fix it, then open an issue describing the root cause.", "attachments": [{"attachment_id": "att_1", "name": "ci-failure.log", "mime": "text/plain", "size": 48_213}, {"attachment_id": "att_2", "name": "screenshot.png", "mime": "image/png", "size": 1_204_311}]})),
        ev(sid, 3, EventType::AgentThought, json!({"text": "Look at the test first, then the actor's restart path."})),
        ev(sid, 4, EventType::ToolCall, json!({"tool_call_id": "t1", "name": "cargo test -p agenthost-host resume", "arguments": {"command": "cargo test -p agenthost-host resume"}, "source": "sandbox"})),
        ev(sid, 5, EventType::ToolResult, json!({"tool_call_id": "t1", "status": "completed", "output": "running 1 test\ntest resume_after_host_restart_keeps_history ... ok\n\ntest result: ok. 1 passed"})),
        ev(sid, 6, EventType::NetDenied, json!({"host": "index.crates.io", "port": 443, "reason": "host is not on the allow list"})),
        ev(sid, 7, EventType::AgentMessage, json!({"text": "The test passes locally. The flake comes from the session list being read **before** the actor has written the resumed state:\n\n- `wait_state` polls every 25 ms\n- the restart can take longer under load\n\nI'll make the wait follow the event instead:\n\n```rust\nlet ev = bus.next_matching(|e| e.kind == EventType::State).await?;\n```\n\nThen I'll file the issue."})),
        ev(sid, 8, EventType::ToolCall, json!({"tool_call_id": "t2", "name": "edit crates/host/tests/common/mod.rs", "arguments": {"path": "crates/host/tests/common/mod.rs"}, "source": "sandbox"})),
        ev(sid, 9, EventType::ToolResult, json!({"tool_call_id": "t2", "status": "completed", "output": "1 file changed"})),
        ev(sid, 10, EventType::ToolCall, json!({"tool_call_id": "t3", "name": "create_issue", "connector_name": "GitHub", "arguments": {"repo": "nico/relay", "title": "Resume test reads state before the actor writes it"}, "source": "mcp"})),
        ev(sid, 11, EventType::ApprovalRequested, json!({"approval_id": "apr_1", "connector_name": "GitHub", "tool": "create_issue", "arguments": {"repo": "nico/relay", "title": "Resume test reads state before the actor writes it"}})),
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
        expires_at: now() + 600_000,
    }];
    s.expanded.insert("t1".into());
}

pub const SCENES: &[(&str, fn(&mut App))] = &[
    ("session", |s| {
        base(s);
        transcript(s);
        s.screen = Screen::Session("ses_1".into());
    }),
    ("session-compose", |s| {
        use crate::model::{FileState, PendingFile};
        base(s);
        transcript(s);
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
        s.screen = Screen::Session("ses_1".into());
    }),
    ("session-voice-download", |s| {
        base(s);
        transcript(s);
        s.approvals.clear();
        let v = s.voice.as_mut().unwrap();
        v.ready = false;
        v.progress = Some(VoiceProgress { received: 190_000_000, total: 475_271_763 });
        s.mic = crate::model::Mic::Transcribing;
        s.screen = Screen::Session("ses_1".into());
    }),
    ("new", |s| {
        base(s);
        s.screen = Screen::NewSession;
        s.new_ws = Some("ws_relay".into());
        s.new_agent = Some(AgentKind::Claude);
    }),
    ("workspaces", |s| {
        base(s);
        s.screen = Screen::Workspaces;
    }),
    ("connectors", |s| {
        base(s);
        s.screen = Screen::Connectors;
    }),
    ("devices", |s| {
        base(s);
        s.screen = Screen::Devices;
    }),
    ("pairing", |s| {
        base(s);
        s.screen = Screen::Devices;
        s.pairing = Some(Pairing {
            pairing_id: "pair_1".into(),
            qr: "agenthost://pair?v=1&r=wss%3A%2F%2Frelay.example.workers.dev&id=ab7dbd0000000000000000000000000&hk=AAAA&s=BBBB&n=box".into(),
            expires_at: now() + 120_000,
            pending: None,
            done: None,
        });
    }),
    ("pairing-confirm", |s| {
        base(s);
        s.screen = Screen::Devices;
        s.pairing = Some(Pairing {
            pairing_id: "pair_1".into(),
            pending: Some(PairingPending { pairing_id: "pair_1".into(), device_name: "Nico's iPhone".into(), short_code: "197 372".into() }),
            ..Default::default()
        });
    }),
    ("host", |s| {
        base(s);
        s.screen = Screen::Host;
    }),
    ("offline", |s| {
        base(s);
        transcript(s);
        s.conn = Conn::Down("connect to host at /run/user/1000/agenthost/host.sock: No such file or directory".into());
        s.screen = Screen::Session("ses_1".into());
    }),
];
