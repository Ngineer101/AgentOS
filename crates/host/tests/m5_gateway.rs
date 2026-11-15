//! Milestone 5: MCP gateway against a test MCP server, driven by the fake
//! agent through the egress proxy (the same route a sandboxed agent uses).

mod common;
#[path = "common/mcp_server.rs"]
mod mcp_server;

use agenthost_cli::Client;
use agenthost_proto::methods as m;
use agenthost_proto::*;
use common::*;
use serde_json::json;
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(20);
const TOKEN: &str = "s3cret-connector-token-1234";

struct Setup {
    th: TestHost,
    c: Client,
    n: tokio::sync::mpsc::Receiver<agenthost_proto::jsonrpc::Notification>,
    sid: String,
    ws: String,
    conn: Connector,
    server: mcp_server::TestMcp,
}

async fn setup() -> Setup {
    let server = mcp_server::start(Some(TOKEN)).await;
    let th = TestHost::start().await;
    let (c, n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let conn: Connector = c
        .call(
            m::CONNECTOR_ADD,
            json!({"url": server.url, "name": "Test", "auth": "bearer", "token": TOKEN}),
        )
        .await
        .unwrap();
    c.request(
        m::WORKSPACE_UPDATE,
        json!({"workspace_id": ws, "connectors": [conn.connector_id]}),
    )
    .await
    .unwrap();
    let sid = c
        .create_session(&ws, AgentKind::Fake)
        .await
        .unwrap()
        .session_id;
    wait_state(&c, &sid, SessionState::Idle, T).await;
    c.subscribe(&sid, 0).await.unwrap();
    Setup {
        th,
        c,
        n,
        sid,
        ws,
        conn,
        server,
    }
}

#[tokio::test]
async fn connector_tools_are_classified() {
    let s = setup().await;
    assert_eq!(s.conn.status, "ok", "{:?}", s.conn.status_detail);
    let echo = s.conn.tools.iter().find(|t| t.name == "echo").unwrap();
    let note = s
        .conn
        .tools
        .iter()
        .find(|t| t.name == "create_note")
        .unwrap();
    assert_eq!((echo.kind, echo.policy), (ToolKind::Read, ToolPolicy::Auto));
    assert_eq!((note.kind, note.policy), (ToolKind::Write, ToolPolicy::Ask));
    // Wrong token: the connector reports it needs sign-in / errors, never ok.
    let bad: Connector =
        s.c.call(
            m::CONNECTOR_ADD,
            json!({"url": s.server.url, "name": "Bad", "token": "wrong"}),
        )
        .await
        .unwrap();
    assert_ne!(bad.status, "ok");
}

#[tokio::test]
async fn read_tool_runs_without_prompt() {
    let mut s = setup().await;
    s.c.prompt(&s.sid, "mcp-list").await.unwrap();
    let (ev, _) = collect_until(&mut s.n, T, turn_end).await;
    let msg = ev
        .iter()
        .find(|e| e.kind == EventType::AgentMessage)
        .unwrap()
        .payload["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        msg.contains("test__echo") && msg.contains("test__create_note"),
        "{msg}"
    );

    s.c.prompt(&s.sid, r#"mcp test__echo {"text":"hi"}"#)
        .await
        .unwrap();
    let (ev, _) = collect_until(&mut s.n, T, turn_end).await;
    assert!(!ev.iter().any(|e| e.kind == EventType::ApprovalRequested));
    let call = ev
        .iter()
        .find(|e| e.kind == EventType::ToolCall)
        .expect("tool.call");
    assert_eq!(call.payload["source"], "mcp");
    assert_eq!(call.payload["name"], "echo");
    let res = ev
        .iter()
        .find(|e| e.kind == EventType::ToolResult)
        .expect("tool.result");
    assert_eq!(res.payload["status"], "completed");
    assert_eq!(res.payload["output"], "echo: hi");
    // The ACP-side record of the same call is not duplicated.
    assert_eq!(
        ev.iter().filter(|e| e.kind == EventType::ToolCall).count(),
        1
    );
    // The gateway attached the real credential.
    assert!(s
        .server
        .state
        .auth_headers
        .lock()
        .unwrap()
        .iter()
        .all(|h| h == &format!("Bearer {TOKEN}")));
}

#[tokio::test]
async fn write_tool_waits_for_approval_from_any_device() {
    let mut s = setup().await;
    let (other, mut other_n) = s.th.client().await;
    other.subscribe(&s.sid, 0).await.unwrap();
    s.c.prompt(&s.sid, r#"mcp test__create_note {"text":"buy milk"}"#)
        .await
        .unwrap();
    let t0 = Instant::now();
    let (ev, _) = collect_until(&mut other_n, T, |e| e.kind == EventType::ApprovalRequested).await;
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "approval took {:?}",
        t0.elapsed()
    );
    let req = ev.last().unwrap();
    assert_eq!(req.payload["tool"], "create_note");
    assert_eq!(req.payload["arguments"]["text"], "buy milk");
    wait_state(&s.c, &s.sid, SessionState::WaitingApproval, T).await;
    assert!(
        s.server.state.calls.lock().unwrap().is_empty(),
        "called before approval"
    );
    let pending: Vec<PendingApproval> = s.c.call(m::APPROVAL_LIST, json!({})).await.unwrap();
    assert_eq!(pending.len(), 1);

    // Approve from the other client.
    other
        .request(
            m::APPROVAL_RESPOND,
            json!({"approval_id": req.payload["approval_id"], "decision": "approve"}),
        )
        .await
        .unwrap();
    let (ev, _) = collect_until(&mut s.n, T, turn_end).await;
    let resolved = ev
        .iter()
        .find(|e| e.kind == EventType::ApprovalResolved)
        .unwrap();
    assert_eq!(resolved.payload["decision"], "approve");
    assert!(ev.iter().any(
        |e| e.kind == EventType::ToolResult && e.payload["output"] == "created note: buy milk"
    ));
    assert_eq!(s.server.state.calls.lock().unwrap().len(), 1);
    // Answering twice fails: the other device's prompt is gone.
    assert!(s
        .c
        .request(
            m::APPROVAL_RESPOND,
            json!({"approval_id": req.payload["approval_id"], "decision": "deny"})
        )
        .await
        .is_err());
    let pending: Vec<PendingApproval> = s.c.call(m::APPROVAL_LIST, json!({})).await.unwrap();
    assert!(pending.is_empty());
    wait_state(&s.c, &s.sid, SessionState::Idle, T).await;
}

#[tokio::test]
async fn denied_write_never_reaches_the_server() {
    let mut s = setup().await;
    s.c.prompt(&s.sid, r#"mcp test__create_note {"text":"nope"}"#)
        .await
        .unwrap();
    let (ev, _) = collect_until(&mut s.n, T, |e| e.kind == EventType::ApprovalRequested).await;
    let id = ev.last().unwrap().payload["approval_id"].clone();
    s.c.request(
        m::APPROVAL_RESPOND,
        json!({"approval_id": id, "decision": "deny"}),
    )
    .await
    .unwrap();
    let (ev, _) = collect_until(&mut s.n, T, turn_end).await;
    assert!(ev
        .iter()
        .any(|e| e.kind == EventType::ToolResult && e.payload["status"] == "denied"));
    assert!(s.server.state.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn policy_changes_and_disabling_apply_immediately() {
    let mut s = setup().await;
    // Per-tool auto: no approval.
    s.c.request(
        m::CONNECTOR_UPDATE,
        json!({"connector_id": s.conn.connector_id, "tool_policies": {"create_note": "auto"}}),
    )
    .await
    .unwrap();
    s.c.prompt(&s.sid, r#"mcp test__create_note {"text":"auto"}"#)
        .await
        .unwrap();
    let (ev, _) = collect_until(&mut s.n, T, turn_end).await;
    assert!(!ev.iter().any(|e| e.kind == EventType::ApprovalRequested));
    assert_eq!(s.server.state.calls.lock().unwrap().len(), 1);
    // Per-tool disabled: not listed.
    s.c.request(
        m::CONNECTOR_UPDATE,
        json!({"connector_id": s.conn.connector_id, "tool_policies": {"echo": "disabled"}}),
    )
    .await
    .unwrap();
    s.c.prompt(&s.sid, "mcp-list").await.unwrap();
    let (ev, _) = collect_until(&mut s.n, T, turn_end).await;
    let msg = ev
        .iter()
        .find(|e| e.kind == EventType::AgentMessage)
        .unwrap()
        .payload["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        !msg.contains("test__echo") && msg.contains("test__create_note"),
        "{msg}"
    );
    // Connector disabled: no tools at all, at once.
    s.c.request(
        m::CONNECTOR_UPDATE,
        json!({"connector_id": s.conn.connector_id, "enabled": false}),
    )
    .await
    .unwrap();
    s.c.prompt(&s.sid, "mcp-list").await.unwrap();
    let (ev, _) = collect_until(&mut s.n, T, turn_end).await;
    let msg = ev
        .iter()
        .find(|e| e.kind == EventType::AgentMessage)
        .unwrap()
        .payload["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!msg.contains("test__"), "{msg}");
    // Not enabled for another workspace.
    let _ = s.ws;
}

#[tokio::test]
async fn connector_token_never_enters_the_agent() {
    let mut s = setup().await;
    s.c.prompt(&s.sid, "sh env; cat $FAKE_AGENT_HOME/sessions/*.json")
        .await
        .unwrap();
    let (ev, _) = collect_until(&mut s.n, T, turn_end).await;
    let out = ev
        .iter()
        .find(|e| e.kind == EventType::ToolResult)
        .unwrap()
        .payload["output"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!out.contains(TOKEN), "token visible to the agent");
    // Nor anywhere in the event log.
    let all = s.th.host.ctx.store.events_after(&s.sid, 0, 10_000).unwrap();
    assert!(!serde_json::to_string(&all).unwrap().contains(TOKEN));
    // Nor in the database file.
    let db = std::fs::read(s.th.host.ctx.cfg.db_path()).unwrap();
    assert!(!String::from_utf8_lossy(&db).contains(TOKEN));
}
