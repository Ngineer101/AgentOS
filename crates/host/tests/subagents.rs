//! Subagents (Claude Code's `Agent` tool), as the Claude ACP adapter reports
//! them without the `subagents` client capability: their steps carry the
//! parent call's id, and their words stay out of the main reply.

mod common;

use agenthost_proto::methods as m;
use agenthost_proto::*;
use common::*;
use serde_json::Value;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

#[tokio::test]
async fn subagent_steps_and_words_are_attributed_to_their_call() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let s = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    let sid = s.session_id;
    wait_state(&c, &sid, SessionState::Idle, T).await;
    c.subscribe(&sid, 0).await.unwrap();
    c.prompt(&sid, "subagents").await.unwrap();
    let (events, all) = collect_until(&mut n, T, turn_end).await;
    let of = |k: EventType| events.iter().filter(move |e| e.kind == k);
    let (a, b) = ("toolu_sub_a", "toolu_sub_b");

    // The main reply is the main agent's alone, in the transcript and live.
    let main: Vec<&str> = of(EventType::AgentMessage)
        .map(|e| e.payload["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        main,
        vec![
            "I'll split this between two subagents.",
            "Both subagents are done."
        ]
    );
    let deltas: String = all
        .iter()
        .filter(|n| n.method == m::N_SESSION_DELTA && n.params["kind"] == "agent.message")
        .map(|n| n.params["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        deltas,
        "I'll split this between two subagents.Both subagents are done."
    );
    // A subagent's thinking is dropped.
    assert_eq!(of(EventType::AgentThought).count(), 0);

    // The Agent calls: their tool name and what the subagent was asked.
    let call = |id: &str| -> &Event {
        of(EventType::ToolCall)
            .find(|e| e.payload["tool_call_id"] == id)
            .unwrap_or_else(|| panic!("no tool.call {id}"))
    };
    let pa = &call(a).payload;
    assert_eq!(pa["tool_name"], "Agent");
    assert_eq!(pa["name"], "Review the parser");
    assert_eq!(pa["subagent"]["description"], "Review the parser");
    assert_eq!(pa["subagent"]["agent_type"], "general-purpose");
    assert_eq!(
        pa["subagent"]["prompt"],
        "Read src/parser.rs and report problems."
    );
    assert!(pa.get("parent_tool_call_id").is_none());
    let pb = &call(b).payload;
    assert_eq!(pb["tool_name"], "Agent");
    assert_eq!(pb["subagent"]["agent_type"], "Explore");

    // Each child call names its subagent, and comes after the subagent's call.
    let pos = |pred: &dyn Fn(&Event) -> bool| events.iter().position(|e| pred(e)).unwrap();
    for (child, parent) in [
        ("toolu_a1", a),
        ("toolu_a2", a),
        ("toolu_a3", a),
        ("toolu_b1", b),
        ("toolu_b2", b),
    ] {
        let p = &call(child).payload;
        assert_eq!(p["parent_tool_call_id"], parent, "{child}");
        assert_eq!(p["tool_name"], "Bash", "{child}");
        assert!(p.get("subagent").is_none());
        let result = of(EventType::ToolResult)
            .find(|e| e.payload["tool_call_id"] == child)
            .unwrap_or_else(|| panic!("no tool.result {child}"));
        // toolu_a1's result update lost the stamp; the host remembers it.
        assert_eq!(result.payload["parent_tool_call_id"], parent, "{child}");
        let at = |id: &str| {
            pos(&|e: &Event| e.kind == EventType::ToolCall && e.payload["tool_call_id"] == id)
        };
        assert!(at(parent) < at(child), "{child} before its subagent");
    }

    // What the subagents said, coalesced between their steps.
    let said: Vec<(String, String)> = of(EventType::SubagentMessage)
        .map(|e| {
            (
                e.payload["parent_tool_call_id"]
                    .as_str()
                    .unwrap()
                    .to_string(),
                e.payload["text"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let of_parent = |p: &str| -> Vec<&str> {
        said.iter()
            .filter(|(q, _)| q == p)
            .map(|(_, t)| t.as_str())
            .collect()
    };
    assert_eq!(
        of_parent(a),
        vec!["Looking at the parser.", "Parser is fine."]
    );
    assert_eq!(of_parent(b), vec!["Checking the lexer.", "Lexer is fine."]);
    assert_eq!(said.len(), 4);
    // "Looking at the parser." comes before the subagent's first step, and
    // its last words before its report.
    let sub_at = |text: &str| {
        pos(&|e: &Event| e.kind == EventType::SubagentMessage && e.payload["text"] == text)
    };
    let call_at = |id: &str| {
        pos(&|e: &Event| e.kind == EventType::ToolCall && e.payload["tool_call_id"] == id)
    };
    let result_at = |id: &str| {
        pos(&|e: &Event| e.kind == EventType::ToolResult && e.payload["tool_call_id"] == id)
    };
    assert!(sub_at("Looking at the parser.") < call_at("toolu_a1"));
    assert!(sub_at("Checking the lexer.") < call_at("toolu_b1"));
    assert!(sub_at("Parser is fine.") < result_at(a));
    assert!(sub_at("Lexer is fine.") < result_at(b));

    // The Agent calls finish with the subagents' reports.
    for (id, report) in [
        (a, "Report A: the parser has no problems."),
        (b, "Report B: the lexer has no problems."),
    ] {
        let r = &events[result_at(id)].payload;
        assert_eq!(r["status"], "completed");
        assert_eq!(r["output"], report);
        assert!(r.get("parent_tool_call_id").is_none());
    }

    // `false` failed four times, but twice in each subagent: not a repeat.
    let end = events.last().unwrap();
    assert_eq!(end.payload["stop_reason"], "end_turn", "{:?}", end.payload);
    assert_eq!(end.payload.get("host_reason"), None::<&Value>);
}
