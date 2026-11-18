//! Several live sessions in one workspace: in a git repository each gets a
//! worktree and a branch of its own; elsewhere they share the folder.

mod common;

use agenthost_cli::Client;
use agenthost_proto::methods as m;
use agenthost_proto::*;
use common::*;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository with one commit in the test workspace folder.
fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("README.md"), "hello\n").unwrap();
    std::fs::write(dir.join("sub/notes.txt"), "notes\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "first"]);
}

/// Run a shell command in a session and return the tool's output.
async fn sh(c: &Client, n: &mut tokio::sync::mpsc::Receiver<agenthost_proto::jsonrpc::Notification>, sid: &str, cmd: &str) -> String {
    c.prompt(sid, &format!("sh {cmd}")).await.unwrap();
    let (ev, _) = collect_until(n, T, |e| e.kind == EventType::TurnEnd && e.session_id == sid).await;
    ev.iter()
        .find(|e| e.kind == EventType::ToolResult && e.session_id == sid)
        .map(|e| e.payload["output"].as_str().unwrap_or("").to_string())
        .unwrap_or_default()
}

fn real(p: &str) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}

#[tokio::test]
async fn git_workspace_sessions_get_their_own_worktree_and_branch() {
    let th = TestHost::start().await;
    init_repo(&th.workspace);
    let (c, mut n) = th.client().await;
    let ws = th.workspace_id(&c).await;

    let a = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    let b = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    wait_state(&c, &a.session_id, SessionState::Idle, T).await;
    wait_state(&c, &b.session_id, SessionState::Idle, T).await;
    let (wa, wb) = (a.worktree.clone().unwrap(), b.worktree.clone().unwrap());
    assert_ne!(wa.path, wb.path);
    assert_ne!(wa.branch, wb.branch);
    assert!(wa.branch.starts_with("agenthost/"), "{}", wa.branch);
    let worktrees = real(th.host.ctx.cfg.worktrees_dir().to_str().unwrap());
    for w in [&wa, &wb] {
        assert!(Path::new(&w.path).starts_with(&worktrees), "{}", w.path);
        assert!(Path::new(&w.path).join("README.md").is_file());
    }
    // Both branches exist in the repository, both worktrees are listed.
    let branches = git(&th.workspace, &["branch", "--list", "agenthost/*"]);
    assert!(branches.contains(&wa.branch) && branches.contains(&wb.branch), "{branches}");
    // Stored, so a later session.get (and a restart) still has them.
    assert_eq!(c.get_session(&a.session_id).await.unwrap().worktree, Some(wa.clone()));

    // Each works in its own worktree: a file one writes is not in the other's
    // or in your checkout.
    c.subscribe(&a.session_id, 0).await.unwrap();
    c.subscribe(&b.session_id, 0).await.unwrap();
    let pwd = sh(&c, &mut n, &a.session_id, "pwd").await;
    assert!(pwd.starts_with(&wa.path), "{pwd}");
    sh(&c, &mut n, &a.session_id, "echo from-a > a.txt").await;
    assert!(Path::new(&wa.path).join("a.txt").is_file());
    assert!(!Path::new(&wb.path).join("a.txt").exists());
    assert!(!th.workspace.join("a.txt").exists());
    // Committing in a worktree lands on its branch only.
    sh(&c, &mut n, &a.session_id, "git add a.txt && git -c user.name=A -c user.email=a@example.com commit -q -m from-a").await;
    assert_eq!(git(&th.workspace, &["log", "-1", "--format=%s", &wa.branch]), "from-a");
    assert_eq!(git(&th.workspace, &["log", "-1", "--format=%s", "main"]), "first");

    // A workspace that is a folder inside the repository: the session works
    // in the same folder of its worktree.
    let sub = c
        .add_workspace(th.workspace.join("sub").to_str().unwrap(), None)
        .await
        .unwrap();
    let s = c.create_session(&sub.workspace_id, AgentKind::Fake).await.unwrap();
    wait_state(&c, &s.session_id, SessionState::Idle, T).await;
    c.subscribe(&s.session_id, 0).await.unwrap();
    let pwd = sh(&c, &mut n, &s.session_id, "pwd && ls").await;
    let wt = s.worktree.unwrap();
    assert!(pwd.starts_with(&format!("{}/sub", wt.path)), "{pwd}");
    assert!(pwd.contains("notes.txt"), "{pwd}");
}

#[tokio::test]
async fn archiving_removes_a_clean_worktree_and_keeps_a_dirty_one() {
    let th = TestHost::start().await;
    init_repo(&th.workspace);
    let (c, mut n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let clean = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    let dirty = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    wait_state(&c, &clean.session_id, SessionState::Idle, T).await;
    wait_state(&c, &dirty.session_id, SessionState::Idle, T).await;
    c.subscribe(&dirty.session_id, 0).await.unwrap();
    sh(&c, &mut n, &dirty.session_id, "echo wip > wip.txt").await;
    let (wc, wd) = (clean.worktree.unwrap(), dirty.worktree.unwrap());

    for s in [&clean.session_id, &dirty.session_id] {
        let a: Session = c.call(m::SESSION_ARCHIVE, json!({"session_id": s})).await.unwrap();
        assert_eq!(a.state, SessionState::Ended);
    }
    // Clean: gone, but its branch stays. Dirty: kept as it was.
    assert!(!Path::new(&wc.path).exists());
    assert!(git(&th.workspace, &["branch", "--list", &wc.branch]).contains(&wc.branch));
    assert!(Path::new(&wd.path).join("wip.txt").is_file());
    assert!(!git(&th.workspace, &["worktree", "list"]).contains(&wc.path));

    // Unarchiving brings it back from its branch.
    let u: Session = c
        .call(m::SESSION_UNARCHIVE, json!({"session_id": clean.session_id}))
        .await
        .unwrap();
    assert_eq!(u.worktree, Some(wc.clone()));
    assert!(Path::new(&wc.path).join("README.md").is_file());
    assert_eq!(git(Path::new(&wc.path), &["rev-parse", "--abbrev-ref", "HEAD"]), wc.branch);
}

#[tokio::test]
async fn a_live_session_whose_worktree_is_gone_gets_it_back_on_resume() {
    let th = TestHost::start().await;
    init_repo(&th.workspace);
    let (c, _n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let s = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    wait_state(&c, &s.session_id, SessionState::Idle, T).await;
    let wt = s.worktree.unwrap();
    drop(c);
    let (dir, cfg) = th.stop().await;
    // Removed behind the host's back while it was stopped.
    git(&dir.path().join("work"), &["worktree", "remove", "--force", &wt.path]);
    let th = TestHost::start_in(dir, cfg).await;
    let (c, _n) = th.client().await;
    wait_state(&c, &s.session_id, SessionState::Idle, T).await;
    assert!(Path::new(&wt.path).join("README.md").is_file());
}

#[tokio::test]
async fn sessions_share_a_folder_outside_git() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let a = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    let b = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    wait_state(&c, &a.session_id, SessionState::Idle, T).await;
    wait_state(&c, &b.session_id, SessionState::Idle, T).await;
    assert_eq!((a.worktree, b.worktree), (None, None));
    // The same folder: what one writes the other sees.
    c.subscribe(&a.session_id, 0).await.unwrap();
    c.subscribe(&b.session_id, 0).await.unwrap();
    sh(&c, &mut n, &a.session_id, "echo shared > s.txt").await;
    let out = sh(&c, &mut n, &b.session_id, "cat s.txt").await;
    assert!(out.contains("shared"), "{out}");
    // Ending one leaves the other running.
    c.request(m::SESSION_END, json!({"session_id": a.session_id})).await.unwrap();
    wait_state(&c, &a.session_id, SessionState::Ended, T).await;
    let out = sh(&c, &mut n, &b.session_id, "cat s.txt").await;
    assert!(out.contains("shared"), "{out}");
}

#[tokio::test]
async fn a_repository_without_commits_is_shared_like_a_folder() {
    let th = TestHost::start().await;
    git(&th.workspace, &["init", "-q"]);
    let (c, _n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let a = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    let b = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    wait_state(&c, &a.session_id, SessionState::Idle, T).await;
    wait_state(&c, &b.session_id, SessionState::Idle, T).await;
    assert_eq!((a.worktree, b.worktree), (None, None));
}
