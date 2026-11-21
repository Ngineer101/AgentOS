//! Sessions name themselves: the agent's own title, else one it writes when
//! asked, else the first message; the user's title always wins.

mod common;

use agenthost_proto::methods as m;
use agenthost_proto::*;
use common::*;
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

async fn wait_title(c: &agenthost_cli::Client, sid: &str, want: &str) -> Session {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let s = c.get_session(sid).await.unwrap();
        if s.title.as_deref() == Some(want) {
            return s;
        }
        assert!(tokio::time::Instant::now() < deadline, "title is {:?}, wanted {want:?}", s.title);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn a_session_is_named_by_the_agent_when_asked() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let s = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    assert_eq!(s.title, None);
    wait_state(&c, &s.session_id, SessionState::Idle, T).await;
    c.subscribe(&s.session_id, 0).await.unwrap();
    c.prompt(&s.session_id, "tidy up the build scripts please").await.unwrap();
    let (events, _) = collect_until(&mut n, T, turn_end).await;
    // The asking happens out of sight: only the session's own reply is in the transcript.
    let said: Vec<_> = events.iter().filter(|e| e.kind == EventType::AgentMessage).collect();
    assert_eq!(said.len(), 1, "{said:#?}");
    assert!(!said[0].payload["text"].as_str().unwrap().contains("Title:"));
    wait_title(&c, &s.session_id, "About tidy up the build").await;

    // Later messages don't rename it.
    c.prompt(&s.session_id, "now the docs").await.unwrap();
    collect_until(&mut n, T, turn_end).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let s = c.get_session(&s.session_id).await.unwrap();
    assert_eq!(s.title.as_deref(), Some("About tidy up the build"));
}

#[tokio::test]
async fn the_agents_own_title_wins_and_the_users_beats_both() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let s = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    wait_state(&c, &s.session_id, SessionState::Idle, T).await;
    c.subscribe(&s.session_id, 0).await.unwrap();
    c.prompt(&s.session_id, "retitle Agent picked this").await.unwrap();
    collect_until(&mut n, T, turn_end).await;
    wait_title(&c, &s.session_id, "Agent picked this").await;
    // The asked-for title, whenever it comes, doesn't replace the agent's.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(c.get_session(&s.session_id).await.unwrap().title.as_deref(), Some("Agent picked this"));

    let r: Session = c
        .call(m::SESSION_RENAME, json!({"session_id": s.session_id, "title": "Mine"}))
        .await
        .unwrap();
    assert_eq!(r.title.as_deref(), Some("Mine"));
    c.prompt(&s.session_id, "retitle Agent again").await.unwrap();
    collect_until(&mut n, T, turn_end).await;
    assert_eq!(c.get_session(&s.session_id).await.unwrap().title.as_deref(), Some("Mine"));
    // Clearing the user's title shows the agent's again.
    let r: Session = c
        .call(m::SESSION_RENAME, json!({"session_id": s.session_id, "title": ""}))
        .await
        .unwrap();
    assert_eq!(r.title.as_deref(), Some("Agent again"));
}

#[tokio::test]
async fn a_named_session_is_not_retitled() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let s = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    wait_state(&c, &s.session_id, SessionState::Idle, T).await;
    let _: Session = c
        .call(m::SESSION_RENAME, json!({"session_id": s.session_id, "title": "Release prep"}))
        .await
        .unwrap();
    c.subscribe(&s.session_id, 0).await.unwrap();
    c.prompt(&s.session_id, "bump the version").await.unwrap();
    collect_until(&mut n, T, turn_end).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(c.get_session(&s.session_id).await.unwrap().title.as_deref(), Some("Release prep"));
    // No title was asked for, so clearing it leaves the first message's.
    let r: Session = c
        .call(m::SESSION_RENAME, json!({"session_id": s.session_id, "title": ""}))
        .await
        .unwrap();
    assert_eq!(r.title.as_deref(), Some("Bump the version"));
}
