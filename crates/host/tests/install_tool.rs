//! The host's own MCP tool: installing a command-line tool on the user's
//! computer, only after the user approves, and only an exact package command.

mod common;

use agenthost_proto::methods as m;
use agenthost_proto::*;
use common::*;
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

/// A fake `npm` that records its arguments instead of installing anything.
fn fake_npm(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("managers");
    std::fs::create_dir_all(&bin).unwrap();
    let log = dir.join("npm-args.txt");
    let script = format!(
        "#!/bin/sh\necho \"$@\" >> '{}'\necho 'added 1 package in 2s'\n",
        log.display()
    );
    let p = bin.join("npm");
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    log
}

#[tokio::test]
async fn installs_run_only_after_approval_and_exactly_as_shown() {
    let dir = short_tempdir();
    let log = fake_npm(dir.path());
    let mut cfg = test_config(dir.path());
    cfg.install_bin_dir = Some(dir.path().join("managers"));
    let th = TestHost::start_in(dir, cfg).await;
    let (c, mut n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let sid = c.create_session(&ws, AgentKind::Fake).await.unwrap().session_id;
    wait_state(&c, &sid, SessionState::Idle, T).await;
    c.subscribe(&sid, 0).await.unwrap();

    // Listed, with only the managers this computer has.
    c.prompt(&sid, "mcp-list").await.unwrap();
    let (ev, _) = collect_until(&mut n, T, turn_end).await;
    let listed = ev
        .iter()
        .filter(|e| e.kind == EventType::AgentMessage)
        .map(|e| e.payload["text"].as_str().unwrap_or_default().to_string())
        .collect::<String>();
    assert!(listed.contains("agenthost__install_tool"), "{listed}");
    assert!(listed.contains("\\\"enum\\\":[\\\"npm\\\"]") || listed.contains(r#""enum":["npm"]"#), "{listed}");

    // A request that isn't a plain package never reaches the user or npm.
    c.prompt(
        &sid,
        r#"mcp agenthost__install_tool {"manager":"npm","package":"--registry=https://evil.example","reason":"x"}"#,
    )
    .await
    .unwrap();
    let (ev, _) = collect_until(&mut n, T, turn_end).await;
    assert!(!ev.iter().any(|e| e.kind == EventType::ApprovalRequested));
    assert!(ev.iter().any(|e| e.kind == EventType::ToolResult && e.payload["status"] == "failed"));

    // Denied: nothing runs.
    c.prompt(
        &sid,
        r#"mcp agenthost__install_tool {"manager":"npm","package":"agent-desktop","version":"1.3.1","reason":"You asked for it"}"#,
    )
    .await
    .unwrap();
    let (ev, _) = collect_until(&mut n, T, |e| e.kind == EventType::ApprovalRequested).await;
    let req = ev.last().unwrap().payload.clone();
    assert_eq!(req["connector_id"], BUILTIN_CONNECTOR_ID);
    assert_eq!(req["summary"], "Install agent-desktop 1.3.1 with npm, on this computer");
    assert_eq!(req["arguments"]["command"], "npm install --global agent-desktop@1.3.1");
    assert_eq!(req["arguments"]["reason"], "You asked for it");
    let pending: Vec<PendingApproval> = c.call(m::APPROVAL_LIST, json!({})).await.unwrap();
    assert_eq!(pending[0].summary.as_deref(), Some("Install agent-desktop 1.3.1 with npm, on this computer"));
    c.request(m::APPROVAL_RESPOND, json!({"approval_id": req["approval_id"], "decision": "deny"}))
        .await
        .unwrap();
    let (ev, _) = collect_until(&mut n, T, turn_end).await;
    assert!(ev.iter().any(|e| e.kind == EventType::ToolResult && e.payload["status"] == "denied"));
    assert!(!log.exists(), "npm ran without approval");

    // Approved: runs once, with exactly the arguments shown.
    c.prompt(
        &sid,
        r#"mcp agenthost__install_tool {"manager":"npm","package":"agent-desktop","version":"1.3.1","reason":"You asked for it"}"#,
    )
    .await
    .unwrap();
    let (ev, _) = collect_until(&mut n, T, |e| e.kind == EventType::ApprovalRequested).await;
    let id = ev.last().unwrap().payload["approval_id"].clone();
    c.request(m::APPROVAL_RESPOND, json!({"approval_id": id, "decision": "approve"}))
        .await
        .unwrap();
    let (ev, _) = collect_until(&mut n, T, turn_end).await;
    let result = ev
        .iter()
        .find(|e| e.kind == EventType::ToolResult && e.payload["name"] == "install_tool")
        .unwrap();
    assert_eq!(result.payload["status"], "completed");
    assert!(result.payload["output"].as_str().unwrap().contains("added 1 package"));
    assert_eq!(
        std::fs::read_to_string(&log).unwrap().trim(),
        "install --global agent-desktop@1.3.1"
    );
    // Both decisions are in the audit log.
    let audit = std::fs::read_to_string(th.host.ctx.cfg.logs_dir().join("audit.jsonl")).unwrap();
    assert!(audit.contains("install.decision") && audit.contains("install.done"));
}

#[tokio::test]
async fn without_package_managers_the_tool_is_not_offered() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let sid = c.create_session(&ws, AgentKind::Fake).await.unwrap().session_id;
    wait_state(&c, &sid, SessionState::Idle, T).await;
    c.subscribe(&sid, 0).await.unwrap();
    c.prompt(&sid, "mcp-list").await.unwrap();
    let (ev, _) = collect_until(&mut n, T, turn_end).await;
    assert!(!ev
        .iter()
        .any(|e| e.kind == EventType::AgentMessage
            && e.payload["text"].as_str().unwrap_or_default().contains("install_tool")));
}
