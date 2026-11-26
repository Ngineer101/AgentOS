//! Files agents send the user (`agenthost__send_file`): kept with the
//! session, in the transcript as `agent.file`, read back with
//! `attachment.read`; only from the session's own folders.

mod common;

use agenthost_proto::methods as m;
use agenthost_proto::*;
use common::*;
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

#[tokio::test]
async fn an_agent_sends_a_file_from_its_folder_and_only_from_there() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    std::fs::create_dir_all(th.workspace.join("out")).unwrap();
    std::fs::write(th.workspace.join("out/report.txt"), b"all 12 checks passed\n").unwrap();
    let secret = th.dir.path().join("secret.txt");
    std::fs::write(&secret, b"not for sending").unwrap();
    std::os::unix::fs::symlink(&secret, th.workspace.join("sneaky.txt")).unwrap();

    // Recording the browser is on by default, and can be set to ask.
    let list: Vec<BuiltinTool> = c.call(m::BUILTIN_LIST, json!({})).await.unwrap();
    let rec = list.iter().find(|t| t.name == "browser_recording").expect("listed");
    assert_eq!((rec.policy, rec.can_auto), (ToolPolicy::Auto, true));

    let sid = c.create_session(&ws, AgentKind::Fake).await.unwrap().session_id;
    wait_state(&c, &sid, SessionState::Idle, T).await;
    c.subscribe(&sid, 0).await.unwrap();

    c.prompt(&sid, r#"mcp agenthost__send_file {"path":"out/report.txt","caption":"  The test report. "}"#).await.unwrap();
    let (ev, _) = collect_until(&mut n, T, turn_end).await;
    let file = ev.iter().find(|e| e.kind == EventType::AgentFile).unwrap_or_else(|| panic!("no agent.file in {ev:#?}"));
    let f: AgentFile = serde_json::from_value(file.payload.clone()).unwrap();
    assert_eq!((f.attachment.name.as_str(), f.attachment.mime.as_str(), f.attachment.size), ("report.txt", "text/plain", 21));
    assert_eq!((f.kind, f.caption.as_deref()), (AgentFileKind::File, Some("The test report.")));
    let result = ev.iter().find(|e| e.kind == EventType::ToolResult && e.payload["name"] == "send_file").expect("its result");
    assert_eq!(result.payload["status"], "completed", "{result:?}");
    assert_eq!(f.tool_call_id.as_deref(), result.payload["tool_call_id"].as_str());

    // Any client reads it back, and it lasts: the agent's copy can go.
    std::fs::remove_file(th.workspace.join("out/report.txt")).unwrap();
    let (a, bytes) = c.read_attachment(&sid, &f.attachment.attachment_id).await.unwrap();
    assert_eq!((a.name.as_str(), bytes.as_slice()), ("report.txt", &b"all 12 checks passed\n"[..]));
    // Not through another session.
    let other = c.create_session(&ws, AgentKind::Fake).await.unwrap().session_id;
    assert!(c.read_attachment(&other, &f.attachment.attachment_id).await.is_err());

    // Nothing from outside its folders, through a link or not.
    for path in [secret.display().to_string(), "sneaky.txt".into(), "../secret.txt".into()] {
        c.prompt(&sid, &format!("mcp agenthost__send_file {}", json!({"path": path}))).await.unwrap();
        let (ev, _) = collect_until(&mut n, T, turn_end).await;
        assert!(!ev.iter().any(|e| e.kind == EventType::AgentFile), "{path} was sent");
        let r = ev.iter().find(|e| e.kind == EventType::ToolResult && e.payload["name"] == "send_file").expect("its result");
        assert_eq!(r.payload["status"], "failed", "{path}");
        assert!(r.payload["output"].as_str().unwrap().contains("outside your session's folders"), "{r:?}");
    }
}
