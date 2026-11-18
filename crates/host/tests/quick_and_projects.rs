//! Quick tasks (sessions without a project folder) and projects created
//! inside the project folders, as a phone would.

mod common;

use agenthost_host::api::{serve_connection, Principal};
use agenthost_proto::methods::{self as m, Capability};
use agenthost_proto::*;
use common::*;
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

#[tokio::test]
async fn quick_tasks_get_a_folder_each_and_open_web() {
    let th = TestHost::start().await;
    let (c, _n) = th.client().await;
    let list: Vec<Workspace> = c.call(m::WORKSPACE_LIST, json!({})).await.unwrap();
    let quick = list
        .iter()
        .find(|w| w.workspace_id == QUICK_WORKSPACE_ID)
        .expect("Quick tasks is built in");
    assert_eq!(quick.kind, WorkspaceKind::Scratch);
    assert_eq!(quick.network.presets, vec![NetworkPreset::Web]);

    // Two at once (a project workspace allows only one live session).
    let a = c.create_session(QUICK_WORKSPACE_ID, AgentKind::Fake).await.unwrap();
    let b = c.create_session(QUICK_WORKSPACE_ID, AgentKind::Fake).await.unwrap();
    wait_state(&c, &a.session_id, SessionState::Idle, T).await;
    wait_state(&c, &b.session_id, SessionState::Idle, T).await;
    let scratch = th.host.ctx.cfg.scratch_dir();
    for s in [&a, &b] {
        let dir = scratch.join(&s.session_id);
        assert!(dir.is_dir(), "{} has no folder", s.session_id);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(dir.metadata().unwrap().permissions().mode() & 0o777, 0o700);
    }
    // The scratch folder is outside the data folder, which sandboxes can't read.
    assert!(!scratch.starts_with(&th.host.ctx.cfg.data_dir));

    // Built in: can't be removed, but open web can be switched off.
    assert!(c
        .request(m::WORKSPACE_REMOVE, json!({"workspace_id": QUICK_WORKSPACE_ID}))
        .await
        .is_err());
    let w: Workspace = c
        .call(
            m::WORKSPACE_UPDATE,
            json!({"workspace_id": QUICK_WORKSPACE_ID, "network": {"presets": []}}),
        )
        .await
        .unwrap();
    assert!(w.network.presets.is_empty());
    // And it stays off across a restart.
    drop(c);
    let (dir, cfg) = th.stop().await;
    let th = TestHost::start_in(dir, cfg).await;
    let (c, _n) = th.client().await;
    let list: Vec<Workspace> = c.call(m::WORKSPACE_LIST, json!({})).await.unwrap();
    let quick = list.iter().find(|w| w.workspace_id == QUICK_WORKSPACE_ID).unwrap();
    assert!(quick.network.presets.is_empty());
    // A folder can't be a workspace inside the quick tasks' folder.
    std::fs::create_dir_all(th.host.ctx.cfg.scratch_dir().join("x")).unwrap();
    assert!(c
        .add_workspace(th.host.ctx.cfg.scratch_dir().join("x").to_str().unwrap(), None)
        .await
        .is_err());
}

#[tokio::test]
async fn projects_stay_inside_the_project_folders() {
    let th = TestHost::start().await;
    let (c, _n) = th.client().await;
    let home = th.host.ctx.cfg.home_dir.clone();
    let code = home.join("Code");

    // Default: ~/Code, listed even before it exists.
    let b: ProjectBrowse = c.call(m::PROJECT_BROWSE, json!({})).await.unwrap();
    assert_eq!(b.roots, vec![code.display().to_string()]);
    let b: ProjectBrowse = c
        .call(m::PROJECT_BROWSE, json!({"path": code}))
        .await
        .unwrap();
    assert!(b.folders.is_empty());

    // A new project in a project folder that doesn't exist yet creates both.
    let w: Workspace = c
        .call(m::PROJECT_CREATE, json!({"parent": code, "name": "first"}))
        .await
        .unwrap();
    let first = std::fs::canonicalize(code.join("first")).unwrap();
    assert_eq!(w.path, first.display().to_string());
    // Asking again for the same folder returns the workspace.
    let again: Workspace = c
        .call(m::PROJECT_CREATE, json!({"path": first}))
        .await
        .unwrap();
    assert_eq!(again.workspace_id, w.workspace_id);

    // Browsing: folders only, hidden ones left out, workspaces marked, no
    // symlink out of the project folder.
    let real_code = std::fs::canonicalize(&code).unwrap();
    std::fs::create_dir_all(real_code.join("second/inner")).unwrap();
    std::fs::create_dir_all(real_code.join(".hidden")).unwrap();
    std::fs::write(real_code.join("notes.txt"), "x").unwrap();
    std::fs::create_dir_all(home.join("Private")).unwrap();
    std::os::unix::fs::symlink(home.join("Private"), real_code.join("escape")).unwrap();
    let b: ProjectBrowse = c
        .call(m::PROJECT_BROWSE, json!({"path": code}))
        .await
        .unwrap();
    let names: Vec<&str> = b.folders.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["first", "second"]);
    assert_eq!(b.folders[0].workspace_id.as_deref(), Some(w.workspace_id.as_str()));
    assert_eq!(b.parent, None);
    let b: ProjectBrowse = c
        .call(m::PROJECT_BROWSE, json!({"path": real_code.join("second")}))
        .await
        .unwrap();
    assert_eq!(b.parent.as_deref(), Some(real_code.display().to_string().as_str()));

    // Outside the project folders, the project folder itself, odd names, existing folders.
    let refused = [
        json!({"path": home.join("Private")}),
        json!({"path": real_code.join("escape")}),
        json!({"path": code}),
        json!({"parent": home, "name": "x"}),
        json!({"parent": code, "name": "../x"}),
        json!({"parent": code, "name": ".x"}),
        json!({"parent": code, "name": "second"}),
    ];
    for p in refused {
        assert!(c.request(m::PROJECT_CREATE, p.clone()).await.is_err(), "{p}");
    }
    assert!(c
        .request(m::PROJECT_BROWSE, json!({"path": home.join("Private")}))
        .await
        .is_err());

    // Project folders: set on the desktop only; never home, host data or secrets.
    for bad in [json!([home]), json!(["/"]), json!([th.host.ctx.cfg.data_dir]), json!(["~/.ssh"])] {
        assert!(c
            .request(m::PROJECT_SET_ROOTS, json!({"roots": bad}))
            .await
            .is_err(), "{bad}");
    }
    let roots: Vec<String> = c
        .call(m::PROJECT_SET_ROOTS, json!({"roots": ["~/Code", "~/Work"]}))
        .await
        .unwrap();
    assert_eq!(roots.len(), 2);
    assert!(roots[1].ends_with("/Work"));
}

#[tokio::test]
async fn a_phone_can_create_projects_but_not_choose_where() {
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
    let (c, _n) = agenthost_cli::Client::over(tx, orx, "phone-test").await.unwrap();
    let code = th.host.ctx.cfg.home_dir.join("Code");
    let w: Workspace = c
        .call(m::PROJECT_CREATE, json!({"parent": code, "name": "from-phone"}))
        .await
        .unwrap();
    assert!(w.path.ends_with("/Code/from-phone"));
    assert!(c
        .request(m::PROJECT_SET_ROOTS, json!({"roots": ["/tmp"]}))
        .await
        .is_err());
    assert!(c
        .request(m::WORKSPACE_ADD, json!({"path": "/tmp"}))
        .await
        .is_err());
    // Quick tasks need no admin either.
    let s = c.create_session(QUICK_WORKSPACE_ID, AgentKind::Fake).await.unwrap();
    wait_state(&c, &s.session_id, SessionState::Idle, T).await;
}
