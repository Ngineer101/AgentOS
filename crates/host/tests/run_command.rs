//! Running any command-line program as the user, through the host: only
//! after the user approves the exact command, or for a prefix they let run
//! without asking; never a shell, an interpreter or a build tool without
//! asking.

mod common;

use agenthost_proto::methods as m;
use agenthost_proto::*;
use common::*;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

/// A fake program that logs its arguments, its standard input and the
/// folder it ran in.
fn fake_program(dir: &Path, name: &str) -> PathBuf {
    let bin = dir.join("programs");
    std::fs::create_dir_all(&bin).unwrap();
    let log = dir.join(format!("{name}-log.txt"));
    let script = format!(
        "#!/bin/sh\necho \"args: $@\" >> '{0}'\necho \"cwd: $(pwd)\" >> '{0}'\ncat >> '{0}'\necho 'https://github.com/o/r/pull/6'\n",
        log.display()
    );
    let p = bin.join(name);
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    log
}

#[tokio::test]
async fn commands_run_only_after_approval_or_for_a_prefix_the_user_allowed() {
    let dir = short_tempdir();
    let gh_log = fake_program(dir.path(), "gh");
    let bash_log = fake_program(dir.path(), "bash");
    let mut cfg = test_config(dir.path());
    cfg.install_bin_dir = Some(dir.path().join("programs"));
    let th = TestHost::start_in(dir, cfg).await;

    let (c, mut n) = th.client().await;
    let list: Vec<BuiltinTool> = c.call(m::BUILTIN_LIST, json!({})).await.unwrap();
    let rc = list.iter().find(|t| t.name == "run_command").unwrap();
    assert_eq!((rc.policy, rc.can_auto, rc.allowed.len()), (ToolPolicy::Ask, false, 0));

    let ws = th.workspace_id(&c).await;
    let sid = c.create_session(&ws, AgentKind::Fake).await.unwrap().session_id;
    wait_state(&c, &sid, SessionState::Idle, T).await;
    c.subscribe(&sid, 0).await.unwrap();

    // Asks, showing the exact command and its input; denied runs nothing.
    let ask = r#"mcp agenthost__run_command {"command":["gh","pr","edit","6","--body-file","-"],"stdin":"New description.","reason":"Update the PR text"}"#;
    c.prompt(&sid, ask).await.unwrap();
    let (ev, _) = collect_until(&mut n, T, |e| e.kind == EventType::ApprovalRequested).await;
    let req = ev.last().unwrap().payload.clone();
    assert_eq!(req["tool"], "run_command");
    assert_eq!(req["summary"], "Run gh pr edit on this computer");
    assert_eq!(req["arguments"]["command"], "gh pr edit 6 --body-file -");
    assert_eq!(req["arguments"]["details"], "New description.");
    assert_eq!(req["arguments"]["allow_rule"], json!(["gh", "pr", "edit"]));
    c.request(m::APPROVAL_RESPOND, json!({"approval_id": req["approval_id"], "decision": "deny"})).await.unwrap();
    collect_until(&mut n, T, turn_end).await;
    assert!(!gh_log.exists(), "gh ran without approval");

    // Approved: runs exactly that, in the session's folder, with the input.
    c.prompt(&sid, ask).await.unwrap();
    let (ev, _) = collect_until(&mut n, T, |e| e.kind == EventType::ApprovalRequested).await;
    let id = ev.last().unwrap().payload["approval_id"].clone();
    c.request(m::APPROVAL_RESPOND, json!({"approval_id": id, "decision": "approve"})).await.unwrap();
    let (ev, _) = collect_until(&mut n, T, turn_end).await;
    let result = ev.iter().find(|e| e.kind == EventType::ToolResult && e.payload["name"] == "run_command").unwrap();
    assert_eq!(result.payload["status"], "completed", "{}", result.payload["output"]);
    assert!(result.payload["output"].as_str().unwrap().contains("pull/6"));
    let logged = std::fs::read_to_string(&gh_log).unwrap();
    assert!(logged.contains("args: pr edit 6 --body-file -"), "{logged}");
    assert!(logged.contains("New description."), "{logged}");
    let session: Session = c.call(m::SESSION_GET, json!({"session_id": sid})).await.unwrap();
    let folder = session.worktree.map(|w| w.path).unwrap_or_else(|| th.workspace.display().to_string());
    let folder = std::fs::canonicalize(&folder).unwrap();
    assert!(logged.contains(&format!("cwd: {}", folder.display())), "{logged}");

    // "Don't ask again" for gh pr edit: that prefix runs straight away, others still ask.
    let l: Vec<BuiltinTool> = c
        .call(m::BUILTIN_SET_POLICY, json!({"name": "run_command", "policy": "auto", "command": ["gh", "pr", "edit"]}))
        .await
        .unwrap();
    assert_eq!(l.iter().find(|t| t.name == "run_command").unwrap().allowed, vec![vec!["gh", "pr", "edit"]]);
    c.prompt(&sid, r#"mcp agenthost__run_command {"command":["gh","pr","edit","7","--title","T"],"reason":"x"}"#).await.unwrap();
    let (ev, _) = collect_until(&mut n, T, turn_end).await;
    assert!(!ev.iter().any(|e| e.kind == EventType::ApprovalRequested));
    assert!(std::fs::read_to_string(&gh_log).unwrap().contains("args: pr edit 7 --title T"));
    // Still asks when it names a file outside the session's folder.
    c.prompt(&sid, r#"mcp agenthost__run_command {"command":["gh","pr","edit","7","--body-file","/etc/passwd"],"reason":"x"}"#).await.unwrap();
    let (ev, _) = collect_until(&mut n, T, |e| e.kind == EventType::ApprovalRequested).await;
    let req = ev.last().unwrap().payload.clone();
    assert_eq!(req["arguments"]["outside_file"], "/etc/passwd");
    c.request(m::APPROVAL_RESPOND, json!({"approval_id": req["approval_id"], "decision": "deny"})).await.unwrap();
    collect_until(&mut n, T, turn_end).await;
    assert!(!std::fs::read_to_string(&gh_log).unwrap().contains("/etc/passwd"));
    c.prompt(&sid, r#"mcp agenthost__run_command {"command":["gh","pr","merge","7"],"reason":"x"}"#).await.unwrap();
    let (ev, _) = collect_until(&mut n, T, |e| e.kind == EventType::ApprovalRequested).await;
    let id = ev.last().unwrap().payload["approval_id"].clone();
    c.request(m::APPROVAL_RESPOND, json!({"approval_id": id, "decision": "deny"})).await.unwrap();
    collect_until(&mut n, T, turn_end).await;

    // Shells are never offered "don't ask again", and can't be allowed.
    c.prompt(&sid, r#"mcp agenthost__run_command {"command":["bash","deploy"],"reason":"x"}"#).await.unwrap();
    let (ev, _) = collect_until(&mut n, T, |e| e.kind == EventType::ApprovalRequested).await;
    let req = ev.last().unwrap().payload.clone();
    assert!(req["arguments"]["allow_rule"].is_null());
    c.request(m::APPROVAL_RESPOND, json!({"approval_id": req["approval_id"], "decision": "deny"})).await.unwrap();
    collect_until(&mut n, T, turn_end).await;
    assert!(!bash_log.exists());
    for rule in [json!(["bash", "deploy"]), json!(["gh"]), json!(["gh", "pr", "edit", "6"])] {
        assert!(c
            .request(m::BUILTIN_SET_POLICY, json!({"name": "run_command", "policy": "auto", "command": rule}))
            .await
            .is_err());
    }

    // Paths and unknown programs are refused before anything is asked.
    c.prompt(&sid, r#"mcp agenthost__run_command {"command":["/bin/sh","-c","id"],"reason":"x"}"#).await.unwrap();
    let (ev, _) = collect_until(&mut n, T, turn_end).await;
    assert!(!ev.iter().any(|e| e.kind == EventType::ApprovalRequested));
    assert!(ev.iter().any(|e| e.kind == EventType::ToolResult && e.payload["name"] == "run_command" && e.payload["status"] == "failed"));

    // Asking again for gh pr edit.
    let l: Vec<BuiltinTool> = c
        .call(m::BUILTIN_SET_POLICY, json!({"name": "run_command", "policy": "ask", "command": ["gh", "pr", "edit"]}))
        .await
        .unwrap();
    assert!(l.iter().find(|t| t.name == "run_command").unwrap().allowed.is_empty());

    let audit = std::fs::read_to_string(th.host.ctx.cfg.logs_dir().join("audit.jsonl")).unwrap();
    assert!(audit.contains("command.decision") && audit.contains("builtin.command_policy"));
}
