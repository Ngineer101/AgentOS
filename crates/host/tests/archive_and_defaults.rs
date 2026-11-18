//! Archived sessions, and the login each workspace starts new sessions with.

mod common;

use agenthost_proto::methods as m;
use agenthost_proto::*;
use common::*;
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

async fn list(c: &agenthost_cli::Client, archived: bool) -> Vec<Session> {
    c.call(m::SESSION_LIST, json!({"archived": archived}))
        .await
        .unwrap()
}

async fn workspace(c: &agenthost_cli::Client, id: &str) -> Workspace {
    let l: Vec<Workspace> = c.call(m::WORKSPACE_LIST, json!({})).await.unwrap();
    l.into_iter().find(|w| w.workspace_id == id).unwrap()
}

#[tokio::test]
async fn archiving_ends_and_hides_a_session() {
    let th = TestHost::start().await;
    let (c, _n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let s = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    wait_state(&c, &s.session_id, SessionState::Idle, T).await;

    // Archiving a live session ends it first.
    let a: Session = c
        .call(m::SESSION_ARCHIVE, json!({"session_id": s.session_id}))
        .await
        .unwrap();
    assert_eq!(a.state, SessionState::Ended);
    assert!(a.archived_at.is_some());
    assert!(list(&c, false).await.iter().all(|x| x.session_id != s.session_id));
    assert!(list(&c, true).await.iter().any(|x| x.session_id == s.session_id));
    // Its transcript is still there.
    assert!(!th.host.ctx.store.events_after(&s.session_id, 0, 10).unwrap().is_empty());

    // It stays archived across a restart, and can be brought back.
    drop(c);
    let (dir, cfg) = th.stop().await;
    let th = TestHost::start_in(dir, cfg).await;
    let (c, _n) = th.client().await;
    assert!(list(&c, true).await.iter().any(|x| x.session_id == s.session_id));
    let u: Session = c
        .call(m::SESSION_UNARCHIVE, json!({"session_id": s.session_id}))
        .await
        .unwrap();
    assert_eq!(u.archived_at, None);
    assert!(list(&c, false).await.iter().any(|x| x.session_id == s.session_id));
    assert!(c
        .request(m::SESSION_ARCHIVE, json!({"session_id": "ses_missing"}))
        .await
        .is_err());
}

#[tokio::test]
async fn the_first_login_becomes_the_workspace_default() {
    let th = TestHost::start().await;
    let (c, _n) = th.client().await;
    let dirs = short_tempdir();
    let work = dirs.path().join("fake-work");
    std::fs::create_dir_all(&work).unwrap();
    let p = c
        .add_profile(AgentKind::Fake, "Work", work.to_str().unwrap())
        .await
        .unwrap();
    let ws = th.workspace_id(&c).await;
    assert_eq!(workspace(&c, &ws).await.default_agent, None);

    // The first session sets it ...
    let s = c
        .create_session_as(&ws, AgentKind::Fake, Some(&p.profile_id))
        .await
        .unwrap();
    let w = workspace(&c, &ws).await;
    assert_eq!(w.default_agent, Some(AgentKind::Fake));
    assert_eq!(w.default_profile_id.as_deref(), Some(p.profile_id.as_str()));
    wait_state(&c, &s.session_id, SessionState::Idle, T).await;
    c.request(m::SESSION_END, json!({"session_id": s.session_id}))
        .await
        .unwrap();

    // ... later ones with another login don't change it ...
    let s2 = c.create_session_as(&ws, AgentKind::Fake, None).await.unwrap();
    assert_eq!(
        workspace(&c, &ws).await.default_profile_id.as_deref(),
        Some(p.profile_id.as_str())
    );
    wait_state(&c, &s2.session_id, SessionState::Idle, T).await;

    // ... but setting it does, and clearing it lets the next session set it again.
    let w: Workspace = c
        .call(
            m::WORKSPACE_SET_DEFAULT_AGENT,
            json!({"workspace_id": ws, "agent": "fake", "profile_id": "fake"}),
        )
        .await
        .unwrap();
    assert_eq!((w.default_agent, w.default_profile_id), (Some(AgentKind::Fake), None));
    assert!(c
        .request(
            m::WORKSPACE_SET_DEFAULT_AGENT,
            json!({"workspace_id": ws, "agent": "codex", "profile_id": p.profile_id}),
        )
        .await
        .is_err());
    let w: Workspace = c
        .call(m::WORKSPACE_SET_DEFAULT_AGENT, json!({"workspace_id": ws, "agent": null}))
        .await
        .unwrap();
    assert_eq!(w.default_agent, None);

    // Phones may archive and set defaults.
    for method in [m::SESSION_ARCHIVE, m::SESSION_UNARCHIVE, m::WORKSPACE_SET_DEFAULT_AGENT] {
        assert!(methods::Capability::phone()
            .contains(&methods::required_capability(method).unwrap()));
    }
}

#[tokio::test]
async fn sessions_can_be_renamed_and_the_title_cleared() {
    let th = TestHost::start().await;
    let (c, _n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let s = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    wait_state(&c, &s.session_id, SessionState::Idle, T).await;

    let r: Session = c
        .call(m::SESSION_RENAME, json!({"session_id": s.session_id, "title": "  Fix the flaky test "}))
        .await
        .unwrap();
    assert_eq!(r.title.as_deref(), Some("Fix the flaky test"));
    let again: Session = c
        .call(m::SESSION_GET, json!({"session_id": s.session_id}))
        .await
        .unwrap();
    assert_eq!(again.title.as_deref(), Some("Fix the flaky test"));

    // Blank clears it; too long is refused.
    let r: Session = c
        .call(m::SESSION_RENAME, json!({"session_id": s.session_id, "title": " "}))
        .await
        .unwrap();
    assert_eq!(r.title, None);
    assert!(c
        .request(m::SESSION_RENAME, json!({"session_id": s.session_id, "title": "x".repeat(MAX_SESSION_TITLE + 1)}))
        .await
        .is_err());
}
