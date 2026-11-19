//! Milestone 1: host core against the scripted fake agent.
//! SQLite event log, state machine, client protocol, resume, idempotency.

mod common;

use agenthost_cli::{as_event, Client};
use agenthost_host::api::{serve_connection, Principal};
use agenthost_proto::jsonrpc::codes;
use agenthost_proto::methods::{self as m, Capability};
use agenthost_proto::*;
use common::*;
use serde_json::json;
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(20);

async fn new_session(th: &TestHost, c: &Client) -> String {
    let ws = th.workspace_id(c).await;
    let s = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    wait_state(c, &s.session_id, SessionState::Idle, T).await;
    s.session_id
}

#[tokio::test]
async fn full_session_streams_reply() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let t0 = Instant::now();
    let sid = new_session(&th, &c).await;
    assert!(
        t0.elapsed() < Duration::from_secs(10),
        "idle took {:?}",
        t0.elapsed()
    );

    c.subscribe(&sid, 0).await.unwrap();
    c.prompt(&sid, "hello there").await.unwrap();
    let (events, all) = collect_until(&mut n, T, turn_end).await;

    let deltas: String = all
        .iter()
        .filter(|n| n.method == m::N_SESSION_DELTA && n.params["kind"] == "agent.message")
        .map(|n| n.params["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(deltas, "echo: hello there");
    let msg = events
        .iter()
        .find(|e| e.kind == EventType::AgentMessage)
        .unwrap();
    assert_eq!(msg.payload["text"], "echo: hello there");
    assert!(events.iter().any(|e| e.kind == EventType::AgentThought));
    assert!(events
        .iter()
        .any(|e| e.kind == EventType::Prompt && e.payload["text"] == "hello there"));
    let seqs: Vec<i64> = events.iter().map(|e| e.seq).collect();
    assert!(
        seqs.windows(2).all(|w| w[1] == w[0] + 1),
        "seqs not contiguous: {seqs:?}"
    );
    wait_state(&c, &sid, SessionState::Idle, T).await;
}

/// A reply the agent sends on its own after its turn ended (a background task
/// finished) is written out by itself, before the next prompt, not held back
/// until that prompt's turn and shown below it.
#[tokio::test]
async fn a_reply_between_turns_stays_before_the_next_prompt() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let sid = new_session(&th, &c).await;
    c.subscribe(&sid, 0).await.unwrap();
    c.prompt(&sid, "later 0.2 the build finished").await.unwrap();
    let (mut all, _) = collect_until(&mut n, T, turn_end).await;
    let (between, _) = collect_until(&mut n, T, |e| e.kind == EventType::AgentMessage).await;
    all.extend(between);
    c.prompt(&sid, "hello").await.unwrap();
    all.extend(collect_until(&mut n, T, turn_end).await.0);
    all.sort_by_key(|e| e.seq);
    let order: Vec<String> = all
        .iter()
        .filter(|e| matches!(e.kind, EventType::Prompt | EventType::AgentMessage))
        .map(|e| e.payload["text"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(order, ["later 0.2 the build finished", "started it", "the build finished", "hello", "echo: hello"]);
}

#[tokio::test]
async fn long_replies_are_complete_before_turn_end() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let sid = new_session(&th, &c).await;
    c.subscribe(&sid, 0).await.unwrap();
    let long = "word ".repeat(400);
    for _ in 0..3 {
        c.prompt(&sid, long.trim()).await.unwrap();
        let (events, _) = collect_until(&mut n, T, turn_end).await;
        let msg = events
            .iter()
            .rev()
            .find(|e| e.kind == EventType::AgentMessage)
            .unwrap();
        assert_eq!(
            msg.payload["text"].as_str().unwrap(),
            format!("echo: {}", long.trim())
        );
        // Nothing of this turn arrives after turn.end.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let all = th.host.ctx.store.events_after(&sid, 0, 1000).unwrap();
    let msgs = all
        .iter()
        .filter(|e| e.kind == EventType::AgentMessage)
        .count();
    assert_eq!(msgs, 3);
}

#[tokio::test]
async fn tool_calls_run_without_approval() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let sid = new_session(&th, &c).await;
    c.subscribe(&sid, 0).await.unwrap();
    c.prompt(&sid, "sh echo hi > out.txt && cat out.txt")
        .await
        .unwrap();
    let (events, _) = collect_until(&mut n, T, turn_end).await;
    let call = events
        .iter()
        .find(|e| e.kind == EventType::ToolCall)
        .expect("tool.call");
    assert_eq!(call.payload["source"], "sandbox");
    let res = events
        .iter()
        .find(|e| e.kind == EventType::ToolResult)
        .expect("tool.result");
    assert_eq!(res.payload["status"], "completed");
    assert!(res.payload["output"].as_str().unwrap().contains("hi"));
    assert_eq!(
        std::fs::read_to_string(th.workspace.join("out.txt")).unwrap(),
        "hi\n"
    );
    assert!(!events
        .iter()
        .any(|e| e.kind == EventType::ApprovalRequested));
}

#[tokio::test]
async fn prompt_ids_are_idempotent() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let sid = new_session(&th, &c).await;
    c.subscribe(&sid, 0).await.unwrap();
    let a = c.prompt_with_id(&sid, "p-1", "once").await.unwrap();
    let b = c.prompt_with_id(&sid, "p-1", "once").await.unwrap();
    assert!(a.accepted && !b.accepted);
    assert_eq!(a.seq, b.seq);
    collect_until(&mut n, T, turn_end).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let all = th.host.ctx.store.events_after(&sid, 0, 1000).unwrap();
    assert_eq!(
        all.iter().filter(|e| e.kind == EventType::Prompt).count(),
        1
    );
    assert_eq!(
        all.iter().filter(|e| e.kind == EventType::TurnEnd).count(),
        1
    );
}

#[tokio::test]
async fn cancel_returns_to_idle_quickly() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let sid = new_session(&th, &c).await;
    c.subscribe(&sid, 0).await.unwrap();
    c.prompt(&sid, "sleep 30").await.unwrap();
    wait_state(&c, &sid, SessionState::Running, T).await;
    let t0 = Instant::now();
    c.request(m::SESSION_CANCEL, json!({"session_id": sid}))
        .await
        .unwrap();
    let (events, _) = collect_until(&mut n, T, turn_end).await;
    wait_state(&c, &sid, SessionState::Idle, T).await;
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "cancel took {:?}",
        t0.elapsed()
    );
    assert_eq!(events.last().unwrap().payload["stop_reason"], "cancelled");
}

#[tokio::test]
async fn stuck_agent_is_restarted_on_cancel() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let sid = new_session(&th, &c).await;
    c.subscribe(&sid, 0).await.unwrap();
    c.prompt(&sid, "hang 60").await.unwrap();
    wait_state(&c, &sid, SessionState::Running, T).await;
    let t0 = Instant::now();
    c.request(m::SESSION_CANCEL, json!({"session_id": sid}))
        .await
        .unwrap();
    let (events, _) = collect_until(&mut n, T, turn_end).await;
    wait_state(&c, &sid, SessionState::Idle, T).await;
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "forced cancel took {:?}",
        t0.elapsed()
    );
    assert_eq!(events.last().unwrap().payload["forced"], true);
    // The restarted agent still works.
    c.prompt(&sid, "after").await.unwrap();
    let (events, _) = collect_until(&mut n, T, turn_end).await;
    assert!(events
        .iter()
        .any(|e| e.kind == EventType::AgentMessage && e.payload["text"] == "echo: after"));
}

#[tokio::test]
async fn repeated_failing_tool_call_stops_the_turn() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let sid = new_session(&th, &c).await;
    c.subscribe(&sid, 0).await.unwrap();
    c.prompt(&sid, "fail-loop").await.unwrap();
    let (events, _) = collect_until(&mut n, T, turn_end).await;
    let end = events.last().unwrap();
    assert_eq!(end.payload["stop_reason"], "cancelled");
    assert!(
        end.payload["host_reason"]
            .as_str()
            .unwrap()
            .contains("failed 3 times"),
        "{end:?}"
    );
    let failed = events
        .iter()
        .filter(|e| e.kind == EventType::ToolResult && e.payload["status"] == "failed")
        .count();
    assert!((3..=5).contains(&failed), "failed results: {failed}");
}

#[tokio::test]
async fn resume_after_host_restart_keeps_history() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let sid = new_session(&th, &c).await;
    c.subscribe(&sid, 0).await.unwrap();
    c.prompt(&sid, "first").await.unwrap();
    collect_until(&mut n, T, turn_end).await;
    drop(c);
    let before = th.host.ctx.store.events_after(&sid, 0, 1000).unwrap();

    let (dir, cfg) = th.stop().await;
    let th = TestHost::start_in(dir, cfg).await;
    let (c, mut n) = th.client().await;
    wait_state(&c, &sid, SessionState::Idle, T).await;
    let after = th.host.ctx.store.events_after(&sid, 0, 1000).unwrap();
    assert_eq!(&after[..before.len()], &before[..], "history changed");
    // The load replay was not recorded again.
    assert_eq!(
        after
            .iter()
            .filter(|e| e.kind == EventType::AgentMessage)
            .count(),
        1
    );
    let resumed = after
        .iter()
        .any(|e| e.kind == EventType::SessionState && e.payload["reason"] == "resuming");
    assert!(resumed);

    let s = c.get_session(&sid).await.unwrap();
    c.subscribe(&sid, s.last_seq).await.unwrap();
    c.prompt(&sid, "second").await.unwrap();
    let (events, _) = collect_until(&mut n, T, turn_end).await;
    assert!(events
        .iter()
        .any(|e| e.kind == EventType::AgentMessage && e.payload["text"] == "echo: second"));
}

#[tokio::test]
async fn reconnect_replays_without_gaps_or_duplicates() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let sid = new_session(&th, &c).await;
    c.subscribe(&sid, 0).await.unwrap();
    c.prompt(&sid, "sleep 1").await.unwrap();
    // Read a few events, then drop the connection mid-turn.
    let (first, _) = collect_until(&mut n, T, |e| {
        e.kind == EventType::SessionState && e.payload["state"] == "running"
    })
    .await;
    let last_seen = first.last().unwrap().seq;
    drop(c);
    drop(n);

    let (c, mut n) = th.client().await;
    c.subscribe(&sid, last_seen).await.unwrap();
    let (rest, _) = collect_until(&mut n, T, turn_end).await;
    let mut all: Vec<i64> = first.iter().map(|e| e.seq).collect();
    all.extend(rest.iter().map(|e| e.seq));
    let stored: Vec<i64> = th
        .host
        .ctx
        .store
        .events_after(&sid, 0, 1000)
        .unwrap()
        .iter()
        .map(|e| e.seq)
        .take(all.len())
        .collect();
    assert_eq!(all, stored);
    assert!(rest
        .iter()
        .any(|e| e.kind == EventType::AgentMessage && e.payload["text"] == "slept 1s"));
}

#[tokio::test]
async fn logged_out_agent_fails_with_fix() {
    let th = TestHost::start_with(|c| {
        c.agent_env_overrides
            .push(("FAKE_AGENT_LOGGED_OUT".into(), "1".into()))
    })
    .await;
    let (c, _n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let s = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    let s = wait_state(&c, &s.session_id, SessionState::Failed, T).await;
    assert_eq!(s.state_reason.as_deref(), Some("not logged in"));
    let evs = th
        .host
        .ctx
        .store
        .events_after(&s.session_id, 0, 100)
        .unwrap();
    let failed = evs.iter().find(|e| e.payload["state"] == "failed").unwrap();
    assert_eq!(failed.payload["fix"], "fake-agent login");
}

#[tokio::test]
async fn protocol_mismatch_fails_the_session() {
    let th = TestHost::start_with(|c| {
        c.agent_env_overrides
            .push(("FAKE_AGENT_PROTOCOL_VERSION".into(), "2".into()))
    })
    .await;
    let (c, _n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let s = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    let s = wait_state(&c, &s.session_id, SessionState::Failed, T).await;
    assert!(s
        .state_reason
        .unwrap()
        .contains("protocol version mismatch"));
}

#[tokio::test]
async fn agent_crash_fails_the_session() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let sid = new_session(&th, &c).await;
    c.subscribe(&sid, 0).await.unwrap();
    c.prompt(&sid, "crash").await.unwrap();
    collect_until(&mut n, T, turn_end).await;
    let s = wait_state(&c, &sid, SessionState::Failed, T).await;
    assert!(s.state_reason.unwrap().contains("exited unexpectedly"));
    // Prompts to a failed session are refused.
    let e = c.prompt(&sid, "again").await.unwrap_err().to_string();
    assert!(e.contains("failed"), "{e}");
}

#[tokio::test]
async fn two_sessions_run_at_once() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let ws1 = th.workspace_id(&c).await;
    let other = th.dir.path().join("work2");
    std::fs::create_dir_all(&other).unwrap();
    let ws2 = c
        .add_workspace(other.to_str().unwrap(), None)
        .await
        .unwrap()
        .workspace_id;
    let a = c
        .create_session(&ws1, AgentKind::Fake)
        .await
        .unwrap()
        .session_id;
    let b = c
        .create_session(&ws2, AgentKind::Fake)
        .await
        .unwrap()
        .session_id;
    wait_state(&c, &a, SessionState::Idle, T).await;
    wait_state(&c, &b, SessionState::Idle, T).await;
    c.subscribe(&a, 0).await.unwrap();
    c.subscribe(&b, 0).await.unwrap();
    c.prompt(&a, "sleep 1").await.unwrap();
    c.prompt(&b, "sleep 1").await.unwrap();
    let t0 = Instant::now();
    let mut ended = std::collections::HashSet::new();
    while ended.len() < 2 {
        let (ev, _) = collect_until(&mut n, T, turn_end).await;
        ended.insert(ev.last().unwrap().session_id.clone());
    }
    assert!(
        t0.elapsed() < Duration::from_millis(1900),
        "ran serially: {:?}",
        t0.elapsed()
    );
}

#[tokio::test]
async fn session_end_and_list_watch() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let list: Vec<Session> = c
        .call(m::SESSION_LIST, json!({"watch": true}))
        .await
        .unwrap();
    assert!(list.is_empty());
    let sid = c
        .create_session(&ws, AgentKind::Fake)
        .await
        .unwrap()
        .session_id;
    wait_state(&c, &sid, SessionState::Idle, T).await;
    c.request(m::SESSION_END, json!({"session_id": sid}))
        .await
        .unwrap();
    wait_state(&c, &sid, SessionState::Ended, T).await;
    let mut states = vec![];
    while let Ok(Some(note)) = tokio::time::timeout(Duration::from_millis(500), n.recv()).await {
        if note.method == m::N_SESSION_CHANGED {
            states.push(note.params["state"].as_str().unwrap().to_string());
        }
    }
    assert_eq!(states, vec!["created", "starting", "idle", "ended"]);
}

#[tokio::test]
async fn hello_rules() {
    let th = TestHost::start().await;
    // Raw connection: a request before hello is refused; an old version is refused.
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    let (otx, mut orx) = tokio::sync::mpsc::channel(16);
    let host = th.host.clone();
    tokio::spawn(async move { serve_connection(host, Principal::local(), rx, otx).await });
    tx.send(json!({"jsonrpc":"2.0","id":1,"method":"session.list"}).to_string())
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&orx.recv().await.unwrap()).unwrap();
    assert_eq!(r["error"]["code"], codes::HELLO_REQUIRED);
    tx.send(
        json!({"jsonrpc":"2.0","id":2,"method":"hello","params":{"protocol_version":0}})
            .to_string(),
    )
    .await
    .unwrap();
    let r: serde_json::Value = serde_json::from_str(&orx.recv().await.unwrap()).unwrap();
    assert_eq!(r["error"]["code"], codes::UPDATE_REQUIRED);
    assert!(r["error"]["message"]
        .as_str()
        .unwrap()
        .contains("update required"));
}

#[tokio::test]
async fn phone_capabilities_are_enforced_and_logged() {
    let th = TestHost::start().await;
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    let (otx, orx) = tokio::sync::mpsc::channel(16);
    let host = th.host.clone();
    let phone = Principal {
        device_id: Some("dev_test".into()),
        capabilities: Capability::phone(),
        label: "test phone".into(),
    };
    tokio::spawn(async move { serve_connection(host, phone, rx, otx).await });
    let (c, _n) = Client::over(tx, orx, "phone-test").await.unwrap();
    assert!(!c.hello.capabilities.contains(&Capability::Admin));
    let e = c
        .request(m::WORKSPACE_ADD, json!({"path": th.workspace}))
        .await
        .unwrap_err();
    assert_eq!(e.code, codes::CAPABILITY_DENIED);
    let e = c
        .request(m::DEVICE_PAIR_START, json!({}))
        .await
        .unwrap_err();
    assert_eq!(e.code, codes::CAPABILITY_DENIED);
    // Allowed: reading and sessions.
    c.request(m::WORKSPACE_LIST, json!({})).await.unwrap();
    c.request(m::SESSION_LIST, json!({})).await.unwrap();
    let audit = std::fs::read_to_string(th.host.ctx.cfg.logs_dir().join("audit.jsonl")).unwrap();
    assert!(
        audit.contains("capability.denied")
            && audit.contains("workspace.add")
            && audit.contains("dev_test")
    );
}

#[tokio::test]
async fn workspace_rules() {
    let th = TestHost::start().await;
    let (c, _n) = th.client().await;
    // Home and / are refused; relative paths are refused; duplicates are refused.
    assert!(c
        .request(m::WORKSPACE_ADD, json!({"path": "/"}))
        .await
        .is_err());
    assert!(c
        .request(m::WORKSPACE_ADD, json!({"path": th.host.ctx.cfg.home_dir}))
        .await
        .is_err());
    assert!(c
        .request(m::WORKSPACE_ADD, json!({"path": "relative/dir"}))
        .await
        .is_err());
    assert!(c
        .request(m::WORKSPACE_ADD, json!({"path": th.host.ctx.cfg.data_dir}))
        .await
        .is_err());
    let w = c
        .add_workspace(th.workspace.to_str().unwrap(), None)
        .await
        .unwrap();
    assert_eq!(w.kind, WorkspaceKind::Folder);
    assert!(c
        .add_workspace(th.workspace.to_str().unwrap(), None)
        .await
        .is_err());
    let w: Workspace = c
        .call(
            m::WORKSPACE_UPDATE,
            json!({"workspace_id": w.workspace_id, "add_custom_host": "Example.org"}),
        )
        .await
        .unwrap();
    assert_eq!(w.network.custom_hosts, vec!["example.org"]);
    assert!(c
        .request(
            m::WORKSPACE_UPDATE,
            json!({"workspace_id": w.workspace_id, "add_custom_host": "*"})
        )
        .await
        .is_err());
    // A repo is detected.
    let repo = th.dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let r = c.add_workspace(repo.to_str().unwrap(), None).await.unwrap();
    assert_eq!(r.kind, WorkspaceKind::Repo);
    // A workspace with a live session cannot be removed.
    let sid = c
        .create_session(&r.workspace_id, AgentKind::Fake)
        .await
        .unwrap()
        .session_id;
    wait_state(&c, &sid, SessionState::Idle, T).await;
    assert!(c
        .request(m::WORKSPACE_REMOVE, json!({"workspace_id": r.workspace_id}))
        .await
        .is_err());
    c.request(m::SESSION_END, json!({"session_id": sid}))
        .await
        .unwrap();
    wait_state(&c, &sid, SessionState::Ended, T).await;
    c.request(m::WORKSPACE_REMOVE, json!({"workspace_id": r.workspace_id}))
        .await
        .unwrap();
}

#[tokio::test]
async fn socket_and_data_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let th = TestHost::start().await;
    let cfg = &th.host.ctx.cfg;
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&cfg.socket_path), 0o600);
    assert_eq!(mode(&cfg.data_dir), 0o700);
    assert_eq!(mode(&cfg.db_path()), 0o600);
}

#[tokio::test]
async fn events_never_change() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let sid = new_session(&th, &c).await;
    c.subscribe(&sid, 0).await.unwrap();
    c.prompt(&sid, "x").await.unwrap();
    let (_, all) = collect_until(&mut n, T, turn_end).await;
    assert!(all.iter().filter_map(as_event).count() > 3);
    let db = rusqlite_open(&th.host.ctx.cfg.db_path());
    assert!(db
        .execute("UPDATE events SET payload = '{}' WHERE seq = 1", [])
        .is_err());
    assert!(db.execute("DELETE FROM events", []).is_err());
}

fn rusqlite_open(p: &std::path::Path) -> rusqlite::Connection {
    rusqlite::Connection::open(p).unwrap()
}

#[tokio::test]
async fn attachments_reach_the_agent() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let sid = new_session(&th, &c).await;
    c.subscribe(&sid, 0).await.unwrap();
    // Two pieces, so the upload really is chunked.
    let body = vec![b'x'; UPLOAD_CHUNK_BYTES + 10];
    let a = c
        .upload("notes/../big log.txt", None, &body, |_, _| {})
        .await
        .unwrap();
    assert_eq!(a.name, "big log.txt");
    assert_eq!(a.mime, "text/plain");
    assert_eq!(a.size, body.len() as u64);
    let r = c
        .prompt_with_attachments(&sid, "p1", "look", &[a.attachment_id.clone()])
        .await
        .unwrap();
    assert!(r.accepted);
    let (events, _) = collect_until(&mut n, T, turn_end).await;
    let prompt = events.iter().find(|e| e.kind == EventType::Prompt).unwrap();
    assert_eq!(prompt.payload["attachments"][0]["name"], "big log.txt");
    let reply = events
        .iter()
        .find(|e| e.kind == EventType::AgentMessage)
        .unwrap()
        .payload["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(reply.starts_with("echo: look\n\nAttached files"), "{reply}");
    let path = reply
        .lines()
        .find_map(|l| l.strip_prefix("- "))
        .and_then(|l| l.split(" (").next())
        .unwrap();
    assert!(path.ends_with("/attachments/big log.txt"), "{path}");
    assert_eq!(std::fs::read(path).unwrap(), body);
    // The host's copy is handed over; a retry of the same prompt is a no-op.
    assert!(th.host.ctx.uploads.get(&a.attachment_id).is_err());
    let again = c
        .prompt_with_attachments(&sid, "p1", "look", &[a.attachment_id.clone()])
        .await
        .unwrap();
    assert!(!again.accepted);
    // Unknown attachments are refused.
    assert!(c
        .prompt_with_attachments(&sid, "p2", "x", &["att_nope".into()])
        .await
        .is_err());
    // Attachments alone are a valid prompt.
    let b = c.upload("a.png", None, b"\x89PNG", |_, _| {}).await.unwrap();
    assert_eq!(b.mime, "image/png");
    c.prompt_with_attachments(&sid, "p3", "", &[b.attachment_id])
        .await
        .unwrap();
    let (events, _) = collect_until(&mut n, T, turn_end).await;
    let reply = events
        .iter()
        .find(|e| e.kind == EventType::AgentMessage)
        .unwrap();
    assert!(reply.payload["text"]
        .as_str()
        .unwrap()
        .contains("/attachments/a.png (image/png, 4 bytes)"));
}

#[tokio::test]
async fn voice_reports_a_missing_model() {
    let th = TestHost::start().await;
    let (c, _n) = th.client().await;
    let st: VoiceStatus = c.call(methods::VOICE_STATUS, serde_json::json!({})).await.unwrap();
    assert!(!st.ready);
    assert!(st.download_bytes > 100_000_000);
    let err = c.transcribe_wav(b"RIFF", None).await.unwrap_err();
    assert!(format!("{err:#}").contains("not installed"), "{err:#}");
}
