//! The agent's session settings (model, effort) and side questions (`/btw`).

mod common;

use agenthost_proto::jsonrpc::codes;
use agenthost_proto::methods as m;
use agenthost_proto::*;
use common::*;
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

fn option<'a>(s: &'a Session, id: &str) -> &'a SessionConfigOption {
    s.config_options
        .iter()
        .find(|o| o.id == id)
        .unwrap_or_else(|| panic!("no {id} in {:?}", s.config_options))
}

#[tokio::test]
async fn settings_round_trip_and_the_last_choice_carries_over() {
    let th = TestHost::start().await;
    let (c, _n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let a = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    let a = wait_state(&c, &a.session_id, SessionState::Idle, T).await;

    // Select options only (grouped choices flattened); the mode is the host's.
    let ids: Vec<&str> = a.config_options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, vec!["model", "thought_level"]);
    let model = option(&a, "model");
    assert_eq!(model.category.as_deref(), Some("model"));
    assert_eq!(model.current_value, "fake-fast");
    let values: Vec<&str> = model.options.iter().map(|v| v.value.as_str()).collect();
    assert_eq!(values, vec!["fake-fast", "fake-smart"]);
    assert_eq!(model.options[0].description.as_deref(), Some("Quick answers"));

    // Change it: the result and session.get agree.
    let s: Session = c
        .call(
            m::SESSION_SET_CONFIG,
            json!({"session_id": a.session_id, "config_id": "model", "value": "fake-smart"}),
        )
        .await
        .unwrap();
    assert_eq!(option(&s, "model").current_value, "fake-smart");
    let s: Session = c
        .call(
            m::SESSION_SET_CONFIG,
            json!({"session_id": a.session_id, "config_id": "thought_level", "value": "high"}),
        )
        .await
        .unwrap();
    assert_eq!(option(&s, "thought_level").current_value, "high");
    let got = c.get_session(&a.session_id).await.unwrap();
    assert_eq!(option(&got, "model").current_value, "fake-smart");

    // Values that aren't offered, and settings that don't exist.
    for (id, v) in [("model", "gpt-9"), ("nope", "x"), ("mode", "default")] {
        let e = c
            .request(
                m::SESSION_SET_CONFIG,
                json!({"session_id": a.session_id, "config_id": id, "value": v}),
            )
            .await
            .unwrap_err();
        assert_eq!(e.code, codes::INVALID_PARAMS, "{id}={v}: {}", e.message);
    }

    // The next session with the same login starts with the last choices.
    let b = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    let b = wait_state(&c, &b.session_id, SessionState::Idle, T).await;
    assert_eq!(option(&b, "model").current_value, "fake-smart");
    assert_eq!(option(&b, "thought_level").current_value, "high");

    // An ended session keeps showing its settings, and can't change them.
    c.request(m::SESSION_END, json!({"session_id": a.session_id}))
        .await
        .unwrap();
    let ended = wait_state(&c, &a.session_id, SessionState::Ended, T).await;
    assert_eq!(option(&ended, "model").current_value, "fake-smart");
    let e = c
        .request(
            m::SESSION_SET_CONFIG,
            json!({"session_id": a.session_id, "config_id": "model", "value": "fake-fast"}),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, codes::INVALID_STATE, "{}", e.message);

    // Settings survive a restart.
    drop(c);
    let (dir, cfg) = th.stop().await;
    let th = TestHost::start_in(dir, cfg).await;
    let (c, _n) = th.client().await;
    let ended = c.get_session(&a.session_id).await.unwrap();
    assert_eq!(option(&ended, "thought_level").current_value, "high");
}

#[tokio::test]
async fn side_questions_answer_while_the_turn_runs_and_stay_out_of_it() {
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let s = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    let s = wait_state(&c, &s.session_id, SessionState::Idle, T).await;
    assert!(s.side_questions);
    let sid = s.session_id.clone();
    c.subscribe(&sid, 0).await.unwrap();

    c.prompt(&sid, "sleep 2").await.unwrap();
    wait_state(&c, &sid, SessionState::Running, T).await;
    let r: SessionSideAskResult = c
        .call(m::SESSION_SIDE_ASK, json!({"session_id": sid, "text": "what is 2+2?"}))
        .await
        .unwrap();
    assert!(r.side_id.starts_with("side_"), "{}", r.side_id);
    let (ev, notes) = collect_until(&mut n, T, |e| e.kind == EventType::SideEnd).await;
    // Answered before the main turn ended.
    assert!(!ev.iter().any(|e| e.kind == EventType::TurnEnd), "{ev:#?}");
    let q = ev.iter().find(|e| e.kind == EventType::SideQuestion).unwrap();
    assert_eq!(q.payload, json!({"side_id": r.side_id, "text": "what is 2+2?"}));
    let a = ev.iter().find(|e| e.kind == EventType::SideMessage).unwrap();
    assert_eq!(a.payload, json!({"side_id": r.side_id, "text": "side answer: what is 2+2?"}));
    let end = ev.last().unwrap();
    assert_eq!(end.payload["side_id"], r.side_id.as_str());
    assert_eq!(end.payload["stop_reason"], "end_turn");
    // Streamed as side deltas with the side id.
    let deltas: Vec<SessionDelta> = notes
        .iter()
        .filter(|n| n.method == m::N_SESSION_DELTA)
        .map(|n| serde_json::from_value(n.params.clone()).unwrap())
        .collect();
    assert!(deltas.iter().any(|d| d.kind == EventType::SideMessage && d.side_id.as_deref() == Some(r.side_id.as_str())));
    assert!(!deltas.iter().any(|d| d.kind != EventType::SideMessage && d.text.contains("side answer")));
    assert_eq!(c.get_session(&sid).await.unwrap().state, SessionState::Running);

    // The main turn goes on and ends normally, with nothing of the fork in it.
    let (rest, _) = collect_until(&mut n, T, turn_end).await;
    let all: Vec<&Event> = ev.iter().chain(rest.iter()).collect();
    assert!(all.iter().any(|e| e.kind == EventType::AgentMessage && e.payload["text"] == "slept 2s"));
    assert!(!all.iter().any(|e| e.kind == EventType::AgentMessage && e.payload["text"].as_str().unwrap_or("").contains("side answer")));
    assert!(!all.iter().any(|e| matches!(e.kind, EventType::ToolCall | EventType::ToolResult)), "the fork's tool call leaked");

    // A follow-up in the same side conversation.
    let r2: SessionSideAskResult = c
        .call(
            m::SESSION_SIDE_ASK,
            json!({"session_id": sid, "side_id": r.side_id, "text": "and 3+3?"}),
        )
        .await
        .unwrap();
    assert_eq!(r2.side_id, r.side_id);
    let (ev, _) = collect_until(&mut n, T, |e| e.kind == EventType::SideEnd).await;
    assert!(ev.iter().any(|e| e.kind == EventType::SideMessage && e.payload["text"] == "side answer: and 3+3?"));

    // Unknown side ids (after a restart, say) and empty questions.
    let e = c
        .request(
            m::SESSION_SIDE_ASK,
            json!({"session_id": sid, "side_id": "side_gone", "text": "hi"}),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, codes::INVALID_STATE);
    assert!(e.message.contains("host restarted"), "{}", e.message);
    let e = c
        .request(m::SESSION_SIDE_ASK, json!({"session_id": sid, "text": "  "}))
        .await
        .unwrap_err();
    assert_eq!(e.code, codes::INVALID_PARAMS);

    // Not on an ended session.
    c.request(m::SESSION_END, json!({"session_id": sid})).await.unwrap();
    wait_state(&c, &sid, SessionState::Ended, T).await;
    let e = c
        .request(m::SESSION_SIDE_ASK, json!({"session_id": sid, "text": "hi"}))
        .await
        .unwrap_err();
    assert_eq!(e.code, codes::INVALID_STATE);
}

#[tokio::test]
async fn agents_that_cannot_fork_refuse_side_questions() {
    let th = TestHost::start_with(|c| {
        c.agent_env_overrides
            .push(("FAKE_AGENT_NO_FORK".into(), "1".into()))
    })
    .await;
    let (c, _n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let s = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    let s = wait_state(&c, &s.session_id, SessionState::Idle, T).await;
    assert!(!s.side_questions);
    let e = c
        .request(
            m::SESSION_SIDE_ASK,
            json!({"session_id": s.session_id, "text": "hi"}),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, codes::INVALID_STATE);
}
