//! Automations driven in process, without the host's socket or the relay:
//! runs with the fake agent, the schedule's catch-up rules, and webhook
//! deliveries sealed here the way the relay seals them.

mod common;

use agenthost_host::Host;
use agenthost_proto::hookseal::{self, Envelope};
use agenthost_proto::tunnel::b64;
use agenthost_proto::*;
use hmac::{Hmac, Mac};
use std::sync::Arc;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

/// A host that isn't serving: no socket, no gateway, no relay.
fn open(root: &std::path::Path) -> (Arc<Host>, String) {
    let cfg = common::test_config(root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let host = Host::open(cfg).unwrap();
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
    (host, ws.workspace_id)
}

fn tempdir() -> tempfile::TempDir {
    let root = std::env::var_os("AGENTHOST_TEST_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    tempfile::Builder::new().prefix("ah").tempdir_in(root).unwrap()
}

fn params(ws: &str, prompt: &str, trigger: AutomationTrigger) -> AutomationCreateParams {
    AutomationCreateParams {
        name: "Test".into(),
        workspace_id: ws.into(),
        agent: AgentKind::Fake,
        profile_id: None,
        prompt: prompt.into(),
        trigger,
        auto_approve: vec![],
        max_minutes: None,
        enabled: true,
        webhook_secret: None,
    }
}

async fn finished(host: &Host, id: &str, count: usize) -> Vec<AutomationRun> {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let r = host.automations.runs(&AutomationRunsParams { automation_id: id.into(), limit: None, before: None }).unwrap();
        if r.len() >= count && r.iter().all(|r| r.outcome.is_done()) {
            return r;
        }
        assert!(tokio::time::Instant::now() < deadline, "runs not finished: {r:#?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn runs_and_the_schedule() {
    let dir = tempdir();
    let (host, ws) = open(dir.path());
    let au = &host.automations;
    let a = au
        .create(params(&ws, "hello there", AutomationTrigger::Schedule { cron: "0 3 * * *".into() }), false)
        .unwrap();
    assert_eq!(a.schedule_text.as_deref(), Some("Every day at 03:00"));
    let id = a.automation_id.clone();

    let run = au.run_now(&id).await.unwrap();
    let sid = run.session_id.clone().unwrap();
    let done = finished(&host, &id, 1).await;
    assert_eq!(done[0].outcome, RunOutcome::Succeeded, "{:?}", done[0]);
    assert!(done[0].summary.as_deref().unwrap_or_default().contains("echo: hello there"), "{:?}", done[0]);
    let s = host.ctx.store.session_get(&sid).unwrap().unwrap();
    assert_eq!((s.automation_id.as_deref(), s.title.as_deref(), s.state), (Some(id.as_str()), Some("Test"), SessionState::Idle));
    let ev = host.ctx.store.events_after(&sid, 0, 100).unwrap();
    let prompt = ev.iter().find(|e| e.kind == EventType::Prompt).unwrap();
    assert_eq!((prompt.payload["device"].as_str(), prompt.payload["prompt_id"].as_str()), (Some("automation"), Some(run.run_id.as_str())));
    assert!(au.run_now(&id).await.is_ok(), "the first run is done");
    finished(&host, &id, 2).await;
    // The older run's idle session was ended when the newer one started.
    assert_eq!(host.ctx.store.session_get(&sid).unwrap().unwrap().state, SessionState::Ended);

    // Due: a scheduled run. Two hours late: a catch-up. A day late: skipped.
    for (late, trigger, outcome, n) in [
        (1_000, RunTrigger::Schedule, RunOutcome::Succeeded, 3),
        (2 * 3_600_000, RunTrigger::CatchUp, RunOutcome::Succeeded, 4),
        (26 * 3_600_000, RunTrigger::CatchUp, RunOutcome::Skipped, 5),
    ] {
        host.ctx.store.automation_set_next_run(&id, Some(now_ms() - late)).unwrap();
        let next = au.tick().await.unwrap();
        assert!(next > now_ms());
        let r = finished(&host, &id, n).await;
        assert_eq!((r[0].trigger, r[0].outcome), (trigger, outcome), "{:?}", r[0]);
        assert_eq!(au.get(&id).unwrap().next_run_at, Some(next));
    }

    // Busy: the scheduled run is skipped.
    let slow = au.create(params(&ws, "sleep 3", AutomationTrigger::Schedule { cron: "@hourly".into() }), false).unwrap();
    au.run_now(&slow.automation_id).await.unwrap();
    assert!(au.run_now(&slow.automation_id).await.is_err());
    host.ctx.store.automation_set_next_run(&slow.automation_id, Some(now_ms() - 1000)).unwrap();
    au.tick().await;
    let r = au.runs(&AutomationRunsParams { automation_id: slow.automation_id.clone(), limit: None, before: None }).unwrap();
    assert_eq!((r[0].outcome, r[0].detail.as_deref()), (RunOutcome::Skipped, Some("The previous run was still going.")));
    let r = finished(&host, &slow.automation_id, 2).await;
    assert_eq!(r[1].outcome, RunOutcome::Succeeded);

    // A host restart marks runs that were going as failed.
    let stuck = au.create(params(&ws, "sleep 30", AutomationTrigger::Manual), false).unwrap();
    au.run_now(&stuck.automation_id).await.unwrap();
    au.spawn();
    let r = au.runs(&AutomationRunsParams { automation_id: stuck.automation_id.clone(), limit: None, before: None }).unwrap();
    assert_eq!(r[0].outcome, RunOutcome::Failed);

    // Turning it off clears the next time; deleting removes the history.
    let off = au.update(AutomationUpdateParams { automation_id: id.clone(), enabled: Some(false), ..Default::default() }, false).unwrap();
    assert_eq!(off.next_run_at, None);
    au.delete(&id).unwrap();
    assert!(au.get(&id).is_err());
    assert!(host.ctx.store.session_get(&sid).unwrap().is_some(), "sessions stay");
    host.sessions.shutdown();
}

fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = <Hmac<sha2::Sha256> as Mac>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// Seal a delivery as the relay does.
fn sealed(host: &Host, hook_id: &str, id: &str, headers: &[(&str, &str)], body: &[u8]) -> String {
    let env = Envelope {
        v: 1,
        hook_id: hook_id.into(),
        delivery_id: id.into(),
        received_at: now_ms(),
        headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        body: b64(body),
    };
    b64(&hookseal::seal(&host.automations.seal_key(), id, &serde_json::to_vec(&env).unwrap()))
}

const EVENT_ALERT: &str = r#"{"action":"triggered","data":{"event":{"issue_id":"1117540176","title":"ReferenceError: heck is not defined","web_url":"https://sentry.io/organizations/test-org/issues/1117540176/events/e48/"},"triggered_rule":"Very Important Alert!"}}"#;

#[tokio::test]
async fn sentry_deliveries() {
    let dir = tempdir();
    let (host, ws) = open(dir.path());
    let au = &host.automations;
    let hook = AutomationTrigger::Webhook { source: WebhookSource::Sentry, filter: vec![], dedupe_key: None, cooldown_minutes: 60 };
    let p = AutomationCreateParams { webhook_secret: Some("s3cret".into()), ..params(&ws, SENTRY_PROMPT, hook) };
    let a = au.create(p, true).unwrap();
    let id = a.automation_id.clone();
    assert_eq!(a.webhook.as_ref().unwrap().url, None, "no relay configured");
    let hook_id = host.ctx.store.automation_get(&id).unwrap().unwrap().hook_id.unwrap();
    let rh = au.relay_hooks();
    assert_eq!(rh["hooks"][0], b64(&agenthost_proto::tunnel::sha256(hook_id.as_bytes())));
    assert_eq!(rh["seal_key"], b64(&au.seal_key()));
    let last = |host: &Host| host.automations.get(&id).unwrap().webhook.unwrap().last_delivery.unwrap();

    // A forged signature, then the real one.
    let body = EVENT_ALERT.as_bytes();
    au.deliver("d1", &sealed(&host, &hook_id, "d1", &[("sentry-hook-resource", "event_alert"), ("sentry-hook-signature", "00")], body)).await;
    assert_eq!(last(&host).result, "refused");
    let sig = sign("s3cret", body);
    let good = [("sentry-hook-resource", "event_alert"), ("sentry-hook-signature", sig.as_str())];
    au.deliver("d2", &sealed(&host, &hook_id, "d2", &good, body)).await;
    assert_eq!(last(&host).result, "started", "{:?}", last(&host));
    let r = finished(&host, &id, 1).await;
    assert_eq!((r[0].trigger, r[0].outcome), (RunTrigger::Webhook, RunOutcome::Succeeded), "{:?}", r[0]);
    let sid = r[0].session_id.clone().unwrap();
    let ev = host.ctx.store.events_after(&sid, 0, 100).unwrap();
    let prompt = ev.iter().find(|e| e.kind == EventType::Prompt).unwrap();
    let text = prompt.payload["text"].as_str().unwrap();
    assert!(text.starts_with("A Sentry alert fired: ReferenceError: heck is not defined\nRule: Very Important Alert!"), "{text}");
    assert!(text.contains("untrusted data from the internet"));
    assert_eq!(prompt.payload["attachments"][0]["name"], "webhook.json");

    // The same delivery again (the relay retries): nothing. The same issue
    // in a new delivery: deduped. A sealed blob for another id: dropped.
    au.deliver("d2", &sealed(&host, &hook_id, "d2", &good, body)).await;
    au.deliver("d3", &sealed(&host, &hook_id, "d3", &good, body)).await;
    assert_eq!(last(&host).result, "ignored");
    assert!(last(&host).detail.contains("issue:1117540176"), "{:?}", last(&host));
    au.deliver("d4", &sealed(&host, &hook_id, "d5", &good, body)).await;
    assert_eq!(finished(&host, &id, 1).await.len(), 1);

    // A resolved metric alert starts nothing; a critical one does.
    let metric = br#"{"action":"resolved","data":{"description_title":"Too many errors","web_url":"https://sentry.io/a/1/","metric_alert":{"id":"4","alert_rule":{"name":"Too many errors"}}}}"#;
    let sig = sign("s3cret", metric);
    let h = [("sentry-hook-resource", "metric_alert"), ("sentry-hook-signature", sig.as_str())];
    au.deliver("d6", &sealed(&host, &hook_id, "d6", &h, metric)).await;
    assert_eq!(last(&host).detail, "A metric alert resolved.");
    let critical = String::from_utf8_lossy(metric).replace("resolved", "critical");
    let sig = sign("s3cret", critical.as_bytes());
    let h = [("sentry-hook-resource", "metric_alert"), ("sentry-hook-signature", sig.as_str())];
    au.deliver("d7", &sealed(&host, &hook_id, "d7", &h, critical.as_bytes())).await;
    assert_eq!(last(&host).result, "started", "{:?}", last(&host));
    assert_eq!(finished(&host, &id, 2).await.len(), 2);

    // Turned off: the relay is told, and a delivery that still arrives is ignored.
    au.update(AutomationUpdateParams { automation_id: id.clone(), enabled: Some(false), ..Default::default() }, true).unwrap();
    assert_eq!(au.relay_hooks()["hooks"].as_array().unwrap().len(), 0);
    let sig = sign("s3cret", body);
    let h = [("sentry-hook-resource", "event_alert"), ("sentry-hook-signature", sig.as_str())];
    au.deliver("d8", &sealed(&host, &hook_id, "d8", &h, body)).await;
    assert_eq!(last(&host).detail, "The automation is off.");
    host.sessions.shutdown();
}

#[tokio::test]
async fn a_burst_of_one_alert_starts_one_run() {
    let dir = tempdir();
    let (host, ws) = open(dir.path());
    let au = &host.automations;
    let hook = AutomationTrigger::Webhook { source: WebhookSource::Sentry, filter: vec![], dedupe_key: None, cooldown_minutes: 60 };
    let a = au.create(AutomationCreateParams { webhook_secret: Some("s3cret".into()), ..params(&ws, SENTRY_PROMPT, hook) }, true).unwrap();
    let hook_id = host.ctx.store.automation_get(&a.automation_id).unwrap().unwrap().hook_id.unwrap();
    let body = EVENT_ALERT.as_bytes();
    let sig = sign("s3cret", body);
    let h = [("sentry-hook-resource", "event_alert"), ("sentry-hook-signature", sig.as_str())];
    let blobs: Vec<(String, String)> = (0..5).map(|i| {
        let id = format!("burst{i}");
        let b = sealed(&host, &hook_id, &id, &h, body);
        (id, b)
    }).collect();
    futures::future::join_all(blobs.iter().map(|(id, b)| au.deliver(id, b))).await;
    assert_eq!(finished(&host, &a.automation_id, 1).await.len(), 1);
    host.sessions.shutdown();
}
