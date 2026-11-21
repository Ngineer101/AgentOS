//! Session titles, driven in process (no socket): an agent's "title" that
//! only repeats a message is not taken for its own, and a session still
//! named by its first message is titled when it is resumed.

mod common;

use agenthost_host::Host;
use agenthost_proto::*;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

fn tempdir() -> tempfile::TempDir {
    let root = std::env::var_os("AGENTHOST_TEST_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    tempfile::Builder::new().prefix("ah").tempdir_in(root).unwrap()
}

/// A host that isn't serving, with one folder workspace.
fn open(root: &Path) -> Arc<Host> {
    let host = Host::open(common::test_config(root)).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    if host.ctx.store.workspace_get("ws_test").unwrap().is_none() {
        host.ctx
            .store
            .workspace_insert(&Workspace {
                workspace_id: "ws_test".into(),
                name: "work".into(),
                path: work.display().to_string(),
                kind: WorkspaceKind::Folder,
                network: NetworkProfile::default(),
                connectors: vec![],
                created_at: 0,
                default_agent: None,
                default_profile_id: None,
            })
            .unwrap();
    }
    host
}

fn session(host: &Host, sid: &str) -> Session {
    host.ctx.store.session_get(sid).unwrap().unwrap()
}

async fn wait_for(host: &Host, sid: &str, what: &str, ok: impl Fn(&Session) -> bool) -> Session {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let s = session(host, sid);
        if ok(&s) {
            return s;
        }
        assert!(tokio::time::Instant::now() < deadline, "waiting for {what}: {s:#?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_title(host: &Host, sid: &str, want: &str) {
    wait_for(host, sid, want, |s| s.title.as_deref() == Some(want)).await;
}

async fn start(host: &Arc<Host>) -> String {
    let p = SessionCreateParams { workspace_id: "ws_test".into(), agent: AgentKind::Fake, profile_id: None, title: None };
    let s = host.sessions.create(p, None).await.unwrap();
    wait_for(host, &s.session_id, "idle", |s| s.state == SessionState::Idle).await;
    s.session_id
}

async fn say(host: &Host, sid: &str, text: &str) {
    let before = session(host, sid).last_seq;
    let p = SessionPromptParams { session_id: sid.into(), prompt_id: uuid::Uuid::new_v4().to_string(), text: text.into(), attachments: vec![] };
    host.sessions.prompt(p, "test").unwrap();
    let turn_ended = |s: &Session| {
        s.state == SessionState::Idle
            && host.ctx.store.events_after(&s.session_id, before, 1000).unwrap().iter().any(|e| e.kind == EventType::TurnEnd)
    };
    wait_for(host, sid, "the turn to end", turn_ended).await;
}

/// Name a session as a host before this one may have: from its first
/// message (`prompt`), or from the agent echoing it (`agent`).
fn set_stored_title(root: &Path, sid: &str, title: &str, source: &str) {
    let db = rusqlite::Connection::open(root.join("data/host.db")).unwrap();
    db.execute(
        "UPDATE sessions SET auto_title = ?2, auto_title_source = ?3 WHERE session_id = ?1",
        rusqlite::params![sid, title, source],
    )
    .unwrap();
}

async fn stop(host: Arc<Host>) {
    host.sessions.shutdown_and_wait(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn an_agent_title_that_repeats_the_message_is_not_the_agents_own() {
    let dir = tempdir();
    let host = open(dir.path());
    let sid = start(&host).await;
    // The agent "titles" it with the message itself: the title it writes when asked stands.
    say(&host, &sid, "echo-title the session list should show what each session is for").await;
    wait_title(&host, &sid, "About echo-title the session list").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(session(&host, &sid).title.as_deref(), Some("About echo-title the session list"));
    // A title of its own still wins.
    say(&host, &sid, "retitle Session list purpose").await;
    wait_title(&host, &sid, "Session list purpose").await;
    stop(host).await;
}

#[tokio::test]
async fn a_session_named_by_its_first_message_is_titled_when_resumed() {
    let dir = tempdir();
    let host = open(dir.path());
    // One the agent titled itself, one it didn't.
    let titled = start(&host).await;
    say(&host, &titled, "retitle Agent picked this").await;
    wait_title(&host, &titled, "Agent picked this").await;
    let untitled = start(&host).await;
    say(&host, &untitled, "tidy up the build scripts please").await;
    wait_title(&host, &untitled, "About tidy up the build").await;
    stop(host).await;

    // As a host before this one left them: named by the first message, or
    // by the agent repeating it.
    set_stored_title(dir.path(), &titled, "Retitle Agent picked this", "prompt");
    set_stored_title(dir.path(), &untitled, "tidy up the build scripts please", "agent");

    let host = open(dir.path());
    assert_eq!(session(&host, &untitled).title.as_deref(), Some("Tidy up the build scripts please"), "the echo is put back to the first message's");
    assert_eq!(host.sessions.resume_all().unwrap(), 2);
    // The agent's own title, from its list of sessions; for the other, whose
    // listed title is only its first message, the one it writes when asked.
    wait_title(&host, &titled, "Agent picked this").await;
    wait_title(&host, &untitled, "About tidy up the build").await;
    stop(host).await;
}
