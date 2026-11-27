//! Automations: runs by hand and on a schedule (catch-up and skips), a
//! Sentry webhook through the relay (signature, filtering, dedupe), and
//! pre-approved pushes.

mod common;

use agenthost_host::relay_dev::DevRelay;
use agenthost_proto::jsonrpc::codes;
use agenthost_proto::methods as m;
use agenthost_proto::*;
use common::*;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

async fn create(c: &agenthost_cli::Client, p: Value) -> Automation {
    c.call(m::AUTOMATION_CREATE, p).await.unwrap()
}

async fn runs(c: &agenthost_cli::Client, id: &str) -> Vec<AutomationRun> {
    c.call(m::AUTOMATION_RUNS, json!({"automation_id": id})).await.unwrap()
}

/// Wait until the newest run of `id` is done.
async fn finished(c: &agenthost_cli::Client, id: &str, count: usize) -> Vec<AutomationRun> {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let r = runs(c, id).await;
        if r.len() >= count && r.iter().all(|r| r.outcome.is_done()) {
            return r;
        }
        assert!(tokio::time::Instant::now() < deadline, "runs not finished: {r:#?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn runs_by_hand_and_on_schedule() {
    let th = TestHost::start().await;
    let (c, _n) = th.client().await;
    let ws = th.workspace_id(&c).await;

    let bad: Result<Automation, _> = c
        .call(m::AUTOMATION_CREATE, json!({"name": "x", "workspace_id": ws, "agent": "fake", "prompt": "hi", "trigger": {"kind": "schedule", "cron": "* * * * *"}}))
        .await;
    assert!(bad.is_err(), "every minute is too often");

    let a = create(
        &c,
        json!({"name": "Nightly", "workspace_id": ws, "agent": "fake", "prompt": "hello there", "trigger": {"kind": "schedule", "cron": "0 3 * * *"}}),
    )
    .await;
    assert_eq!(a.schedule_text.as_deref(), Some("Every day at 03:00"));
    assert!(a.next_run_at.unwrap() > now_ms());
    let id = a.automation_id.clone();

    // By hand.
    let run: AutomationRun = c.call(m::AUTOMATION_RUN_NOW, json!({"automation_id": id})).await.unwrap();
    assert_eq!(run.trigger, RunTrigger::Manual);
    let first_sid = run.session_id.clone().expect("a session");
    let done = finished(&c, &id, 1).await;
    assert_eq!(done[0].outcome, RunOutcome::Succeeded, "{:?}", done[0]);
    assert!(done[0].summary.as_deref().unwrap_or_default().contains("echo: hello there"), "{:?}", done[0].summary);
    let s = c.get_session(&first_sid).await.unwrap();
    assert_eq!(s.automation_id.as_deref(), Some(id.as_str()));
    assert_eq!(s.title.as_deref(), Some("Nightly"));
    assert_eq!(s.state, SessionState::Idle, "the latest run stays open for follow-ups");

    // Due now: a scheduled run, and the previous run's idle session ends.
    th.host.ctx.store.automation_set_next_run(&id, Some(now_ms() - 1000)).unwrap();
    th.host.automations.tick().await;
    let done = finished(&c, &id, 2).await;
    assert_eq!(done[0].trigger, RunTrigger::Schedule);
    assert_eq!(done[0].outcome, RunOutcome::Succeeded);
    wait_state(&c, &first_sid, SessionState::Ended, T).await;
    let a: Automation = c.call(m::AUTOMATION_GET, json!({"automation_id": id})).await.unwrap();
    assert!(a.next_run_at.unwrap() > now_ms(), "the next time counts from now");

    // Missed by two hours (asleep): one catch-up run.
    th.host.ctx.store.automation_set_next_run(&id, Some(now_ms() - 2 * 3600 * 1000)).unwrap();
    th.host.automations.tick().await;
    let done = finished(&c, &id, 3).await;
    assert_eq!(done[0].trigger, RunTrigger::CatchUp);

    // Missed by more than a day: recorded as skipped, nothing runs.
    th.host.ctx.store.automation_set_next_run(&id, Some(now_ms() - 26 * 3600 * 1000)).unwrap();
    th.host.automations.tick().await;
    let r = runs(&c, &id).await;
    assert_eq!((r.len(), r[0].outcome, r[0].session_id.clone()), (4, RunOutcome::Skipped, None));

    // A run still going makes the next one skip.
    let slow = create(
        &c,
        json!({"name": "Slow", "workspace_id": ws, "agent": "fake", "prompt": "sleep 3", "trigger": {"kind": "schedule", "cron": "@hourly"}}),
    )
    .await;
    let r: AutomationRun = c.call(m::AUTOMATION_RUN_NOW, json!({"automation_id": slow.automation_id})).await.unwrap();
    assert_eq!(r.outcome, RunOutcome::Running);
    let again: Result<AutomationRun, _> = c.call(m::AUTOMATION_RUN_NOW, json!({"automation_id": slow.automation_id})).await;
    assert!(again.is_err(), "run now refuses while a run is going");
    th.host.ctx.store.automation_set_next_run(&slow.automation_id, Some(now_ms() - 1000)).unwrap();
    th.host.automations.tick().await;
    let r = runs(&c, &slow.automation_id).await;
    assert_eq!(r[0].outcome, RunOutcome::Skipped);
    assert_eq!(r[0].detail.as_deref(), Some("The previous run was still going."));

    // Off: no next time; deleting keeps the sessions.
    let a: Automation = c.call(m::AUTOMATION_UPDATE, json!({"automation_id": id, "enabled": false})).await.unwrap();
    assert_eq!(a.next_run_at, None);
    c.request(m::AUTOMATION_DELETE, json!({"automation_id": id})).await.unwrap();
    assert!(c.get_session(&first_sid).await.is_ok());
    let list: Vec<Automation> = c.call(m::AUTOMATION_LIST, json!({})).await.unwrap();
    assert_eq!(list.len(), 1);

    let check: ScheduleCheck = c.call(m::AUTOMATION_CHECK_SCHEDULE, json!({"cron": "0 9 * * 1-5"})).await.unwrap();
    assert_eq!((check.text.as_deref(), check.next.len(), check.error), (Some("Weekdays at 09:00"), 3, None));
}

#[tokio::test]
async fn phones_cannot_add_webhooks_or_pre_approve() {
    let th = TestHost::start().await;
    let (c, _n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let base = AutomationCreateParams {
        name: "x".into(),
        workspace_id: ws,
        agent: AgentKind::Fake,
        profile_id: None,
        prompt: "hi".into(),
        trigger: AutomationTrigger::Manual,
        auto_approve: vec![],
        max_minutes: None,
        enabled: true,
        webhook_secret: None,
    };
    let au = &th.host.automations;
    let hook = AutomationCreateParams {
        trigger: AutomationTrigger::Webhook { source: WebhookSource::Generic, filter: vec![], dedupe_key: None, cooldown_minutes: 0 },
        ..base.clone()
    };
    assert_eq!(au.create(hook.clone(), false).unwrap_err().code, codes::CAPABILITY_DENIED);
    let pre = AutomationCreateParams { auto_approve: vec!["git_push".into()], ..base.clone() };
    assert_eq!(au.create(pre, false).unwrap_err().code, codes::CAPABILITY_DENIED);
    let a = au.create(base.clone(), false).unwrap();
    let up = AutomationUpdateParams { automation_id: a.automation_id.clone(), auto_approve: Some(vec!["create_pull_request".into()]), ..Default::default() };
    assert_eq!(au.update(up.clone(), false).unwrap_err().code, codes::CAPABILITY_DENIED);
    let plain = au.create(base.clone(), false).unwrap();
    assert!(au.update(up, true).is_ok());
    // Once it pre-approves a tool, a phone can't change anything about it,
    // not even the prompt: that would change what the pre-approval authorises.
    let up = AutomationUpdateParams { automation_id: a.automation_id.clone(), prompt: Some("hello".into()), ..Default::default() };
    assert_eq!(au.update(up, false).unwrap_err().code, codes::CAPABILITY_DENIED);
    // A phone may still change the prompt of a plain one.
    let up = AutomationUpdateParams { automation_id: plain.automation_id.clone(), prompt: Some("hello".into()), ..Default::default() };
    assert_eq!(au.update(up, false).unwrap().prompt, "hello");
    // Unknown tools can't be pre-approved by anyone.
    let bad = AutomationCreateParams { auto_approve: vec!["run_command".into()], ..base };
    assert!(au.create(bad, true).is_err());
    // A Sentry webhook needs its client secret.
    let sentry = AutomationCreateParams {
        trigger: AutomationTrigger::Webhook { source: WebhookSource::Sentry, filter: vec![], dedupe_key: None, cooldown_minutes: 60 },
        ..hook
    };
    assert!(au.create(sentry, true).is_err());
}

fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = <Hmac<sha2::Sha256> as Mac>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

const EVENT_ALERT: &str = r#"{"action":"triggered","actor":{"id":"sentry","name":"Sentry","type":"application"},"data":{"event":{"issue_id":"1117540176","title":"ReferenceError: heck is not defined","web_url":"https://sentry.io/organizations/test-org/issues/1117540176/events/e4874d664c3540c1a32eab185f12c5ab/"},"triggered_rule":"Very Important Alert!"},"installation":{"uuid":"a8e5d37a"}}"#;

#[tokio::test]
async fn a_sentry_alert_through_the_relay_starts_one_run() {
    let relay = DevRelay::start("127.0.0.1:0").await.unwrap();
    let url = relay.url();
    let th = TestHost::start_with(move |c| c.relay_url = Some(url)).await;
    let (c, _n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let a = create(
        &c,
        json!({
            "name": "Investigate Sentry alerts", "workspace_id": ws, "agent": "fake", "prompt": SENTRY_PROMPT,
            "trigger": {"kind": "webhook", "source": "sentry"}, "webhook_secret": "s3cret"
        }),
    )
    .await;
    let id = a.automation_id.clone();
    let hook_url = a.webhook.as_ref().and_then(|w| w.url.clone()).expect("a webhook URL");
    assert!(a.webhook.as_ref().unwrap().has_secret);
    let parts: Vec<&str> = hook_url.rsplit('/').take(2).collect();
    let (hook_id, relay_id) = (parts[0].to_string(), parts[1].to_string());
    assert!(hook_url.starts_with(&format!("http://{}/v1/hook/", relay.addr)), "{hook_url}");

    let headers = |sig: &str, resource: &str| {
        vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Sentry-Hook-Resource".to_string(), resource.to_string()),
            ("Sentry-Hook-Signature".to_string(), sig.to_string()),
            ("CF-Connecting-IP".to_string(), "203.0.113.9".to_string()),
        ]
    };
    // Wait for the host to register the hook with the relay; a forged
    // signature is refused.
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let (status, _) = relay.post_hook(&relay_id, &hook_id, &headers("00", "event_alert"), EVENT_ALERT.as_bytes());
        if status == 202 {
            break;
        }
        assert_eq!(status, 404);
        assert!(tokio::time::Instant::now() < deadline, "hook never registered");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let a: Automation = c.call(m::AUTOMATION_GET, json!({"automation_id": id})).await.unwrap();
        if let Some(d) = a.webhook.unwrap().last_delivery {
            assert_eq!(d.result, "refused", "{d:?}");
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(runs(&c, &id).await.is_empty());

    // The real thing, over HTTP: one run, prompted with the alert's fields.
    let resp = reqwest::Client::new()
        .post(&hook_url)
        .header("content-type", "application/json")
        .header("sentry-hook-resource", "event_alert")
        .header("sentry-hook-signature", sign("s3cret", EVENT_ALERT.as_bytes()))
        .body(EVENT_ALERT)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 202);
    let done = finished(&c, &id, 1).await;
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].trigger, RunTrigger::Webhook);
    assert_eq!(done[0].outcome, RunOutcome::Succeeded, "{:?}", done[0]);
    assert_eq!(done[0].detail.as_deref(), Some("Sentry: ReferenceError: heck is not defined"));
    let sid = done[0].session_id.clone().unwrap();
    let evs = th.host.ctx.store.events_after(&sid, 0, 1000).unwrap();
    let prompt = evs.iter().find(|e| e.kind == EventType::Prompt).expect("a prompt");
    let text = prompt.payload["text"].as_str().unwrap();
    assert!(text.starts_with("A Sentry alert fired: ReferenceError: heck is not defined\nRule: Very Important Alert!\nIn Sentry: https://sentry.io/"), "{text}");
    assert!(text.contains("untrusted data from the internet"), "{text}");
    assert_eq!(prompt.payload["attachments"][0]["name"], "webhook.json");

    // The same issue again: deduped. A resolved metric alert: ignored.
    let (status, _) = relay.post_hook(&relay_id, &hook_id, &headers(&sign("s3cret", EVENT_ALERT.as_bytes()), "event_alert"), EVENT_ALERT.as_bytes());
    assert_eq!(status, 202);
    let resolved = br#"{"action":"resolved","data":{"description_title":"Resolved: Too many errors","web_url":"https://sentry.io/x","metric_alert":{"id":"4","alert_rule":{"name":"Too many errors"}}}}"#;
    let (status, _) = relay.post_hook(&relay_id, &hook_id, &headers(&sign("s3cret", resolved), "metric_alert"), resolved);
    assert_eq!(status, 202);
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let a: Automation = c.call(m::AUTOMATION_GET, json!({"automation_id": id})).await.unwrap();
        let d = a.webhook.unwrap().last_delivery.unwrap();
        if d.detail == "A metric alert resolved." {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{d:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(runs(&c, &id).await.len(), 1, "no second run for the same issue");
    let audit = std::fs::read_to_string(th.host.ctx.cfg.logs_dir().join("audit.jsonl")).unwrap();
    assert!(audit.contains("A run for issue:1117540176 started within the last 1 day."), "{audit}");
    // Everything was acked.
    assert_eq!(relay.queued_hooks(&relay_id), 0);

    // A new URL: the old one stops working.
    let a: Automation = c.call(m::AUTOMATION_UPDATE, json!({"automation_id": id, "rotate_webhook": true})).await.unwrap();
    assert_ne!(a.webhook.unwrap().url.unwrap(), hook_url);
    let deadline = tokio::time::Instant::now() + T;
    while relay.post_hook(&relay_id, &hook_id, &[], b"{}").0 != 404 {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

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
async fn pre_approved_pushes_skip_the_question_for_the_run_branch_only() {
    let th = TestHost::start().await;
    let remote = th.workspace.parent().unwrap().join("remote.git");
    git(th.workspace.parent().unwrap(), &["init", "--bare", "-q", "-b", "main", remote.to_str().unwrap()]);
    git(&th.workspace, &["init", "-q", "-b", "main"]);
    std::fs::write(th.workspace.join("README.md"), "hello\n").unwrap();
    git(&th.workspace, &["add", "."]);
    git(&th.workspace, &["commit", "-q", "-m", "first"]);
    git(&th.workspace, &["remote", "add", "origin", remote.to_str().unwrap()]);
    let (c, _n) = th.client().await;
    let ws = th.workspace_id(&c).await;

    let push = r#"mcp agenthost__git_push {"set_upstream":true,"reason":"Publish the upgrade"}"#;
    let a = create(
        &c,
        json!({"name": "Upgrades", "workspace_id": ws, "agent": "fake", "prompt": push, "trigger": {"kind": "manual"}, "auto_approve": ["git_push"]}),
    )
    .await;
    let run: AutomationRun = c.call(m::AUTOMATION_RUN_NOW, json!({"automation_id": a.automation_id})).await.unwrap();
    let done = finished(&c, &a.automation_id, 1).await;
    assert_eq!(done[0].outcome, RunOutcome::Succeeded);
    let sid = run.session_id.unwrap();
    let branch = c.get_session(&sid).await.unwrap().worktree.unwrap().branch;
    let evs = th.host.ctx.store.events_after(&sid, 0, 1000).unwrap();
    assert!(!evs.iter().any(|e| e.kind == EventType::ApprovalRequested), "asked anyway");
    let result = evs.iter().find(|e| e.kind == EventType::ToolResult && e.payload["name"] == "git_push").unwrap();
    assert_eq!(result.payload["status"], "completed", "{}", result.payload["output"]);
    assert!(!git(&remote, &["branch", "--list", &branch]).is_empty(), "not pushed");

    // Another branch still asks, and an automation's question waits 12 hours.
    git(&th.workspace, &["branch", "other"]);
    let other = r#"mcp agenthost__git_push {"branch":"other","reason":"x"}"#;
    let b = create(
        &c,
        json!({"name": "Other", "workspace_id": ws, "agent": "fake", "prompt": other, "trigger": {"kind": "manual"}, "auto_approve": ["git_push"]}),
    )
    .await;
    c.request(m::AUTOMATION_RUN_NOW, json!({"automation_id": b.automation_id})).await.unwrap();
    let deadline = tokio::time::Instant::now() + T;
    let pending = loop {
        let p: Vec<PendingApproval> = c.call(m::APPROVAL_LIST, json!({})).await.unwrap();
        if let Some(p) = p.into_iter().next() {
            break p;
        }
        assert!(tokio::time::Instant::now() < deadline, "no approval asked");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(pending.expires_at - pending.requested_at >= 11 * 3600 * 1000, "{pending:?}");
    c.request(m::APPROVAL_RESPOND, json!({"approval_id": pending.approval_id, "decision": "deny"})).await.unwrap();
    finished(&c, &b.automation_id, 1).await;
    let audit = std::fs::read_to_string(th.host.ctx.cfg.logs_dir().join("audit.jsonl")).unwrap();
    assert!(audit.contains(&format!("\"automation_id\":\"{}\"", a.automation_id)), "{audit}");
}
