//! Agent profiles: more than one login of an agent, each with its own
//! config folder, chosen when a session starts.

mod common;

use agenthost_proto::methods as m;
use agenthost_proto::*;
use common::*;
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

/// Profile folders live outside the test host's root (which holds its socket).
fn folders() -> tempfile::TempDir {
    short_tempdir()
}

#[tokio::test]
async fn sessions_run_as_the_chosen_profile() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let dirs = folders();
    let work = dirs.path().join("fake-work");
    std::fs::create_dir_all(&work).unwrap();

    let p = c
        .add_profile(AgentKind::Fake, "Work", work.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(p.agent, AgentKind::Fake);
    assert_eq!(p.name, "Work");
    assert_eq!(
        p.config_dir,
        std::fs::canonicalize(&work).unwrap().display().to_string()
    );

    // host.info: the default login, then the profile.
    let info: HostInfo = c.call(m::HOST_INFO, json!({})).await.unwrap();
    let fakes: Vec<&AgentStatus> = info
        .agents
        .iter()
        .filter(|a| a.agent == AgentKind::Fake)
        .collect();
    assert_eq!(fakes.len(), 2);
    assert_eq!(fakes[0].profile_id.as_deref(), Some("fake"));
    assert_eq!(fakes[0].profile_name, None);
    assert_eq!(fakes[1].profile_id.as_deref(), Some(p.profile_id.as_str()));
    assert_eq!(fakes[1].label(), "Fake agent · Work");
    assert_eq!(fakes[1].config_dir.as_deref(), Some(p.config_dir.as_str()));

    // A session as the profile keeps its state in the profile's folder.
    let ws = th.workspace_id(&c).await;
    let s = c
        .create_session_as(&ws, AgentKind::Fake, Some(&p.profile_id))
        .await
        .unwrap();
    assert_eq!(s.profile_id.as_deref(), Some(p.profile_id.as_str()));
    wait_state(&c, &s.session_id, SessionState::Idle, T).await;
    c.subscribe(&s.session_id, 0).await.unwrap();
    c.prompt(&s.session_id, "hi").await.unwrap();
    collect_until(&mut n, T, turn_end).await;
    let s = c.get_session(&s.session_id).await.unwrap();
    let acp = s.acp_session_id.clone().unwrap();
    assert!(
        work.join("sessions").join(format!("{acp}.json")).is_file(),
        "history not in the profile folder"
    );
    assert!(!th
        .host
        .ctx
        .cfg
        .home_dir
        .join(".fake-agent/sessions")
        .join(format!("{acp}.json"))
        .exists());

    // In use: can't be removed.
    assert!(c.remove_profile(&p.profile_id).await.is_err());
    c.request(m::SESSION_END, json!({"session_id": s.session_id}))
        .await
        .unwrap();
    wait_state(&c, &s.session_id, SessionState::Ended, T).await;

    // The default login still works, and its state stays in the default place.
    let d = c
        .create_session_as(&ws, AgentKind::Fake, Some("fake"))
        .await
        .unwrap();
    assert_eq!(d.profile_id, None);
    wait_state(&c, &d.session_id, SessionState::Idle, T).await;
    c.request(m::SESSION_END, json!({"session_id": d.session_id}))
        .await
        .unwrap();
    wait_state(&c, &d.session_id, SessionState::Ended, T).await;

    c.remove_profile(&p.profile_id).await.unwrap();
    let list: Vec<AgentProfile> = c.call(m::AGENT_PROFILE_LIST, json!({})).await.unwrap();
    assert!(list.is_empty());
}

#[tokio::test]
async fn profile_rules() {
    let th = TestHost::start().await;
    let (c, _n) = th.client().await;
    let dirs = folders();
    let a = dirs.path().join("a");
    std::fs::create_dir_all(a.join("inner")).unwrap();
    let add = |name: &str, dir: String| {
        let c = &c;
        let name = name.to_string();
        async move {
            c.request(
                m::AGENT_PROFILE_ADD,
                json!({"agent": "fake", "name": name, "config_dir": dir}),
            )
            .await
        }
    };
    let s = |p: &std::path::Path| p.display().to_string();

    // Missing, relative, home, the host's data, a default login.
    assert!(add("x", s(&dirs.path().join("missing"))).await.is_err());
    assert!(add("x", "relative/dir".into()).await.is_err());
    assert!(add("x", s(&th.host.ctx.cfg.home_dir)).await.is_err());
    assert!(add("x", s(&th.host.ctx.cfg.data_dir)).await.is_err());
    let default_login = th.host.ctx.cfg.home_dir.join(".fake-agent");
    std::fs::create_dir_all(&default_login).unwrap();
    assert!(add("x", s(&default_login)).await.is_err());
    // The default login's name is reserved; names are 1–40 characters.
    assert!(add("Default", s(&a)).await.is_err());
    assert!(add("  ", s(&a)).await.is_err());
    assert!(add(&"n".repeat(41), s(&a)).await.is_err());

    let first = add("First", s(&a)).await.unwrap();
    // Same name (any case), same folder, or a folder inside another profile's.
    let b = dirs.path().join("b");
    std::fs::create_dir_all(&b).unwrap();
    assert!(add("first", s(&b)).await.is_err());
    assert!(add("Second", s(&a)).await.is_err());
    assert!(add("Second", s(&a.join("inner"))).await.is_err());
    assert!(add("Second", s(&b)).await.is_ok());

    // A profile of another agent can't be used for this one.
    let ws = th.workspace_id(&c).await;
    assert!(c
        .create_session_as(&ws, AgentKind::Codex, Some(first["profile_id"].as_str().unwrap()))
        .await
        .is_err());
    assert!(c
        .create_session_as(&ws, AgentKind::Fake, Some("prof_missing"))
        .await
        .is_err());

    // A workspace can't overlap a profile, nor a profile a workspace.
    assert!(c.add_workspace(&s(&a.join("inner")), None).await.is_err());
    let proj = dirs.path().join("proj");
    std::fs::create_dir_all(proj.join("login")).unwrap();
    c.add_workspace(&s(&proj), None).await.unwrap();
    assert!(add("Third", s(&proj.join("login"))).await.is_err());

    // Phones can't add profiles.
    assert!(!methods::Capability::phone()
        .contains(&methods::required_capability(m::AGENT_PROFILE_ADD).unwrap()));
}

#[tokio::test]
async fn a_removed_profile_fails_its_session_on_resume() {
    let th = TestHost::start().await;
    let (c, _n) = th.client().await;
    let dirs = folders();
    let work = dirs.path().join("w");
    std::fs::create_dir_all(&work).unwrap();
    let p = c
        .add_profile(AgentKind::Fake, "W", work.to_str().unwrap())
        .await
        .unwrap();
    let ws = th.workspace_id(&c).await;
    let s = c
        .create_session_as(&ws, AgentKind::Fake, Some(&p.profile_id))
        .await
        .unwrap();
    wait_state(&c, &s.session_id, SessionState::Idle, T).await;
    drop(c);
    let (dir, cfg) = th.stop().await;
    // Removed behind the host's back (the API refuses while a session is live).
    let store = agenthost_host::store::Store::open(&cfg.db_path()).unwrap();
    assert!(store.profile_remove(&p.profile_id).unwrap());
    drop(store);
    let th = TestHost::start_in(dir, cfg).await;
    let (c, _n) = th.client().await;
    let s = wait_state(&c, &s.session_id, SessionState::Failed, T).await;
    assert!(
        s.state_reason.as_deref().unwrap_or_default().contains("removed"),
        "{:?}",
        s.state_reason
    );
}
