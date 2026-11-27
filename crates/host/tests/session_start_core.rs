//! A new session, driven in process without the host's socket: it is there
//! at once, and its worktree is checked out as it starts, from the remote's
//! main branch.

mod common;

use agenthost_host::Host;
use agenthost_proto::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

fn tempdir() -> tempfile::TempDir {
    let root = std::env::var_os("AGENTHOST_TEST_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    tempfile::Builder::new().prefix("ah").tempdir_in(root).unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, file: &str, message: &str) -> String {
    std::fs::write(dir.join(file), message).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

/// A host that isn't serving, with one workspace: your clone (`work`) of an
/// `origin` that has a commit you haven't pulled. Returns the workspace's
/// id and folder, and that commit.
fn open(root: &Path) -> (Arc<Host>, String, PathBuf, String) {
    let (origin, work, theirs) = (root.join("origin.git"), root.join("work"), root.join("theirs"));
    std::fs::create_dir(&theirs).unwrap();
    git(&theirs, &["init", "-q", "-b", "main"]);
    commit(&theirs, "README.md", "one");
    git(root, &["clone", "-q", "--bare", theirs.to_str().unwrap(), origin.to_str().unwrap()]);
    git(&theirs, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(root, &["clone", "-q", origin.to_str().unwrap(), work.to_str().unwrap()]);
    let two = commit(&theirs, "new.txt", "two");
    git(&theirs, &["push", "-q", "origin", "main"]);

    let host = Host::open(common::test_config(root)).unwrap();
    let ws = Workspace {
        workspace_id: "ws_test".into(),
        name: "work".into(),
        path: work.display().to_string(),
        kind: WorkspaceKind::Folder,
        network: NetworkProfile::default(),
        connectors: vec![],
        created_at: 0,
        default_agent: None,
        default_profile_id: None,
    };
    host.ctx.store.workspace_insert(&ws).unwrap();
    (host, ws.workspace_id, work, two)
}

fn new_session(ws: &str) -> SessionCreateParams {
    SessionCreateParams { workspace_id: ws.into(), agent: AgentKind::Fake, profile_id: None, title: None, note_id: None }
}

/// Wait until the session is `state`, or panic with where it got to.
async fn wait_state(host: &Host, sid: &str, state: SessionState) -> Session {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let s = host.ctx.store.session_get(sid).unwrap().unwrap();
        if s.state == state {
            return s;
        }
        assert!(tokio::time::Instant::now() < deadline, "session is {} ({:?}), wanted {state}", s.state, s.state_reason);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Creating a session doesn't wait for its checkout (seconds, in a large
/// repository): the session comes back with its worktree and branch named,
/// and the worktree is made as it starts, from what the remote has now.
#[tokio::test]
async fn a_session_is_there_at_once_and_its_worktree_comes_as_it_starts() {
    let dir = tempdir();
    let (host, ws, work, two) = open(dir.path());
    let s = host.sessions.create(new_session(&ws), Some("test")).await.unwrap();
    let wt = s.worktree.clone().expect("named when the session is created");
    assert_eq!(s.state, SessionState::Created);
    assert!(wt.branch.starts_with("agenthost/"), "{}", wt.branch);
    // Nothing of the session has run yet (one thread, and no await since).
    assert!(!Path::new(&wt.path).exists(), "the checkout ran before the session was returned");
    assert_eq!(git(&work, &["branch", "--list", &wt.branch]), "");

    let idle = wait_state(&host, &s.session_id, SessionState::Idle).await;
    assert_eq!(idle.worktree, Some(wt.clone()));
    // From the remote's main branch as it is now, not your `main`.
    assert_eq!(git(Path::new(&wt.path), &["rev-parse", "HEAD"]), two);
    assert_eq!(git(Path::new(&wt.path), &["rev-parse", "--abbrev-ref", "HEAD"]), wt.branch);
    assert_ne!(git(&work, &["rev-parse", "main"]), two);
    // Nothing changed yet, measured against that same branch.
    let c = agenthost_host::files::changes(&host.ctx, &s.session_id).await.unwrap();
    assert_eq!((c.patch.as_str(), c.against.as_deref(), c.base.as_str()), ("", Some("origin/main"), two.as_str()));
    host.sessions.end(&s.session_id).await.unwrap();
}

/// A worktree git refuses to make no longer fails `session.create`: the
/// session is there, and its start fails with git's reason.
#[tokio::test]
async fn a_worktree_that_cannot_be_made_fails_the_start_with_the_reason() {
    let dir = tempdir();
    let (host, ws, work, _) = open(dir.path());
    // Where git keeps its worktrees is a file: no worktree can be added.
    std::fs::write(work.join(".git/worktrees"), "").unwrap();
    let s = host.sessions.create(new_session(&ws), Some("test")).await.unwrap();
    let failed = wait_state(&host, &s.session_id, SessionState::Failed).await;
    let reason = failed.state_reason.unwrap_or_default();
    assert!(reason.contains("could not create a worktree") && reason.contains("git worktree list"), "{reason}");
    assert!(!Path::new(&s.worktree.unwrap().path).exists());
}

/// While a session's worktree is still being checked out, what the Changes
/// and Files views ask for is answered from what it starts from, not with
/// "its folder and branch are gone".
#[tokio::test]
async fn changes_and_files_of_a_session_that_is_still_starting() {
    let dir = tempdir();
    let (host, ws, work, _) = open(dir.path());
    // A session as `session.create` leaves it, with nothing started.
    let wt = SessionWorktree { path: dir.path().join("not-yet").display().to_string(), branch: "agenthost/notyet".into() };
    let s = Session {
        session_id: "ses_notyet".into(),
        workspace_id: ws,
        agent: AgentKind::Fake,
        profile_id: None,
        archived_at: None,
        worktree: Some(wt.clone()),
        config_options: vec![],
        side_questions: false,
        pull_request: None,
        pinned_at: None,
        automation_id: None,
        state: SessionState::Created,
        state_reason: None,
        acp_session_id: None,
        title: None,
        created_at: 1,
        updated_at: 1,
        last_seq: 0,
    };
    host.ctx.store.session_insert(&s).unwrap();
    host.ctx.store.session_set_worktree(&s.session_id, &wt, "").unwrap();
    let one = git(&work, &["rev-parse", "refs/remotes/origin/main"]);

    let c = agenthost_host::files::changes(&host.ctx, &s.session_id).await.unwrap();
    assert_eq!((c.branch.as_str(), c.patch.as_str(), c.against.as_deref(), c.base.as_str()), ("agenthost/notyet", "", Some("origin/main"), one.as_str()));
    let f = agenthost_host::files::list(&host.ctx, &s.session_id).await.unwrap();
    assert_eq!(f.files, vec!["README.md"]);
    let r = agenthost_host::files::read(&host.ctx, SessionFileReadParams { session_id: s.session_id.clone(), path: "README.md".into() }).await.unwrap();
    assert_eq!(r.text.as_deref(), Some("one"));
}
