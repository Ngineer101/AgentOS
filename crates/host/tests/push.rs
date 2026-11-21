//! Push notifications: a phone hands over the APNs key and registers, and
//! the host notifies it through an APNs stand-in (HTTP/2) when a session
//! needs it, with the content sealed to the phone.

mod common;

use agenthost_cli::remote::{self, DeviceState};
use agenthost_host::push;
use agenthost_host::relay_dev::DevRelay;
use agenthost_proto::jsonrpc::codes;
use agenthost_proto::methods as m;
use agenthost_proto::*;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use common::*;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::pkcs8::{EncodePrivateKey, LineEnding};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const T: Duration = Duration::from_secs(20);
const TOKEN: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
/// The stand-in knows this token only in the sandbox environment.
const SANDBOX_TOKEN: &str = "5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b";
/// The stand-in says this app was deleted.
const GONE_TOKEN: &str = "dead0000dead0000dead0000dead0000dead0000dead0000dead0000dead0000";

#[derive(Debug, Clone)]
struct Sent {
    env: String,
    token: String,
    authorization: String,
    topic: String,
    push_type: String,
    collapse_id: Option<String>,
    body: Value,
}

/// An APNs stand-in: HTTP/2 without TLS, answers like APNs does.
async fn fake_apns() -> (String, Arc<Mutex<Vec<Sent>>>) {
    use hyper::server::conn::http2;
    use hyper::service::service_fn;
    use hyper_util::rt::{TokioExecutor, TokioIo};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let sent = Arc::new(Mutex::new(Vec::<Sent>::new()));
    let s = sent.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let s = s.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let s = s.clone();
                    async move {
                        let h = |n: &str| req.headers().get(n).map(|v| v.to_str().unwrap().to_string());
                        let path = req.uri().path().to_string();
                        let mut parts = path.trim_start_matches('/').split('/');
                        let env = parts.next().unwrap().to_string();
                        assert_eq!(parts.next(), Some("3"));
                        assert_eq!(parts.next(), Some("device"));
                        let token = parts.next().unwrap().to_string();
                        let sent = Sent {
                            env: env.clone(),
                            token: token.clone(),
                            authorization: h("authorization").unwrap_or_default(),
                            topic: h("apns-topic").unwrap_or_default(),
                            push_type: h("apns-push-type").unwrap_or_default(),
                            collapse_id: h("apns-collapse-id"),
                            body: Value::Null,
                        };
                        assert_eq!(req.version(), hyper::Version::HTTP_2);
                        let body = req.into_body().collect().await.unwrap().to_bytes();
                        let sent = Sent { body: serde_json::from_slice(&body).unwrap(), ..sent };
                        let (status, reason) = if token == GONE_TOKEN {
                            (410, "Unregistered")
                        } else if token == SANDBOX_TOKEN && env != "sandbox" {
                            (400, "BadDeviceToken")
                        } else {
                            (200, "")
                        };
                        if status == 200 {
                            s.lock().unwrap().push(sent);
                        }
                        let body = if reason.is_empty() {
                            Bytes::new()
                        } else {
                            Bytes::from(json!({"reason": reason}).to_string())
                        };
                        Ok::<_, std::convert::Infallible>(
                            hyper::Response::builder().status(status).body(Full::new(body)).unwrap(),
                        )
                    }
                });
                let _ = http2::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(tcp), svc)
                    .await;
            });
        }
    });
    (url, sent)
}

/// A fake `npm`, so the built-in install tool asks for approval.
fn fake_npm(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("managers");
    std::fs::create_dir_all(&bin).unwrap();
    let p = bin.join("npm");
    std::fs::write(&p, "#!/bin/sh\necho ok\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

async fn pair_device(th: &TestHost) -> DeviceState {
    let (desk, mut dn) = th.client().await;
    for _ in 0..100 {
        let info: HostInfo = desk.call(m::HOST_INFO, json!({})).await.unwrap();
        if info.relay.connected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let start: PairStartResult = desk.call(m::DEVICE_PAIR_START, json!({})).await.unwrap();
    let qr = start.qr_payload.clone();
    let phone = tokio::spawn(async move { remote::pair(&qr, "Test phone", |_| {}).await });
    loop {
        let n = tokio::time::timeout(T, dn.recv()).await.unwrap().unwrap();
        if n.method == m::N_PAIRING_PENDING {
            break;
        }
    }
    desk.request(m::DEVICE_PAIR_CONFIRM, json!({"pairing_id": start.pairing_id, "accept": true}))
        .await
        .unwrap();
    phone.await.unwrap().unwrap()
}

async fn wait_sent(sent: &Arc<Mutex<Vec<Sent>>>, n: usize) -> Vec<Sent> {
    for _ in 0..400 {
        let v = sent.lock().unwrap().clone();
        if v.len() >= n {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("APNs got {} notifications, expected {n}", sent.lock().unwrap().len());
}

fn content(key: &[u8], s: &Sent) -> PushContent {
    assert_eq!(s.body["aps"]["alert"]["title"], push::GENERIC_TITLE);
    assert_eq!(s.body["aps"]["alert"]["body"], push::GENERIC_BODY);
    assert_eq!(s.body["aps"]["mutable-content"], 1);
    push::open(key, s.body["e"].as_str().unwrap()).unwrap()
}

fn check_provider_token(auth: &str, vk: &VerifyingKey) {
    let jwt = auth.strip_prefix("Bearer ").expect("bearer token");
    let parts: Vec<&str> = jwt.split('.').collect();
    let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
    assert_eq!(header["kid"], "KEY1234567");
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(claims["iss"], "TEAM123456");
    let sig = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
    vk.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
        .expect("signed with the APNs key");
}

#[tokio::test]
async fn a_phone_sets_up_notifications_and_gets_sealed_ones_when_a_session_needs_it() {
    let (apns, sent) = fake_apns().await;
    let relay = DevRelay::start("127.0.0.1:0").await.unwrap();
    let dir = short_tempdir();
    fake_npm(dir.path());
    let mut cfg = test_config(dir.path());
    cfg.install_bin_dir = Some(dir.path().join("managers"));
    cfg.relay_url = Some(relay.url());
    cfg.apns_url = Some(apns);
    let th = TestHost::start_in(dir, cfg).await;
    let state = pair_device(&th).await;
    let (phone, _, _) = remote::connect(&state).await.unwrap();
    let (desk, mut dn) = th.client().await;

    // Nothing set up yet.
    let st: PushStatus = phone.call(m::PUSH_STATUS, json!({})).await.unwrap();
    assert!(!st.configured && st.this_device.is_none());
    let e = phone.request(m::PUSH_TEST, json!({})).await.unwrap_err();
    assert_eq!(e.code, codes::UNAVAILABLE);

    // The phone hands over the APNs key while pairing. Garbage is refused.
    let e = phone
        .request(m::PUSH_SET_KEY, json!({"key_p8": "hello", "key_id": "KEY1234567", "team_id": "TEAM123456"}))
        .await
        .unwrap_err();
    assert_eq!(e.code, codes::INVALID_PARAMS);
    let sk = SigningKey::random(&mut rand::rngs::OsRng);
    let pem = sk.to_pkcs8_pem(LineEnding::LF).unwrap().to_string();
    let e = phone
        .request(m::PUSH_SET_KEY, json!({"key_p8": pem, "key_id": "short", "team_id": "TEAM123456"}))
        .await
        .unwrap_err();
    assert_eq!(e.code, codes::INVALID_PARAMS);
    let st: PushStatus = phone
        .call(m::PUSH_SET_KEY, json!({"key_p8": pem, "key_id": "key1234567", "team_id": "TEAM123456"}))
        .await
        .unwrap();
    assert!(st.configured);
    assert_eq!(st.key_id.as_deref(), Some("KEY1234567"));
    // Only the desktop removes it.
    let e = phone.request(m::PUSH_CLEAR_KEY, json!({})).await.unwrap_err();
    assert_eq!(e.code, codes::CAPABILITY_DENIED);

    // Registering is for phones.
    let content_key = [42u8; 32];
    let reg = json!({
        "token": TOKEN,
        "topic": "dev.agenthost.ios",
        "environment": "production",
        "key": URL_SAFE_NO_PAD.encode(content_key),
    });
    let e = desk.request(m::PUSH_REGISTER, reg.clone()).await.unwrap_err();
    assert_eq!(e.code, codes::INVALID_PARAMS);
    let e = phone
        .request(m::PUSH_REGISTER, json!({"token": TOKEN, "topic": "dev.agenthost.ios", "environment": "production", "key": "c2hvcnQ"}))
        .await
        .unwrap_err();
    assert_eq!(e.code, codes::INVALID_PARAMS);
    let st: PushStatus = phone.call(m::PUSH_REGISTER, reg.clone()).await.unwrap();
    let me = st.this_device.unwrap();
    assert_eq!(me.prefs, PushPrefs { approvals: true, turn_done: false, failed: false, automations: true });
    let all: PushStatus = desk.call(m::PUSH_STATUS, json!({})).await.unwrap();
    assert_eq!(all.devices.len(), 1);
    assert_eq!(all.devices[0].name, "Test phone");

    // A test notification, signed with the key and sealed to the phone.
    let r: PushTestResult = phone.call(m::PUSH_TEST, json!({})).await.unwrap();
    assert_eq!((r.sent, r.errors.len()), (1, 0));
    let s = wait_sent(&sent, 1).await;
    let vk = VerifyingKey::from(&sk);
    check_provider_token(&s[0].authorization, &vk);
    assert_eq!((s[0].env.as_str(), s[0].token.as_str()), ("production", TOKEN));
    assert_eq!((s[0].topic.as_str(), s[0].push_type.as_str()), ("dev.agenthost.ios", "alert"));
    assert_eq!(content(&content_key, &s[0]).kind, "test");

    // A tool call waits for approval: the phone hears about it.
    let ws = th.workspace_id(&desk).await;
    let sid = desk.create_session(&ws, AgentKind::Fake).await.unwrap().session_id;
    wait_state(&desk, &sid, SessionState::Idle, T).await;
    desk.subscribe(&sid, 0).await.unwrap();
    desk.prompt(
        &sid,
        r#"mcp agenthost__install_tool {"manager":"npm","package":"agent-desktop","version":"1.3.1","reason":"You asked"}"#,
    )
    .await
    .unwrap();
    let (ev, _) = collect_until(&mut dn, T, |e| e.kind == EventType::ApprovalRequested).await;
    let approval_id = ev.last().unwrap().payload["approval_id"].as_str().unwrap().to_string();
    let s = wait_sent(&sent, 2).await;
    let c = content(&content_key, &s[1]);
    assert_eq!(c.kind, "approval");
    assert_eq!(c.session_id.as_deref(), Some(sid.as_str()));
    assert_eq!(c.approval_id.as_deref(), Some(approval_id.as_str()));
    assert_eq!(c.body, "Fake agent needs your approval: Install agent-desktop 1.3.1 with npm, on this computer");
    assert_eq!(s[1].collapse_id.as_deref(), Some(approval_id.as_str()));
    assert!(!s[1].body.to_string().contains("agent-desktop"), "Apple must not see the content");

    // Finished turns are off by default.
    desk.request(m::APPROVAL_RESPOND, json!({"approval_id": approval_id, "decision": "deny"}))
        .await
        .unwrap();
    collect_until(&mut dn, T, turn_end).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(sent.lock().unwrap().len(), 2);

    // Turned on: the reply's first words come along.
    let st: PushStatus = phone
        .call(m::PUSH_SET_PREFS, json!({"prefs": {"approvals": true, "turn_done": true}}))
        .await
        .unwrap();
    assert!(st.this_device.unwrap().prefs.turn_done);
    desk.prompt(&sid, "hello there").await.unwrap();
    collect_until(&mut dn, T, turn_end).await;
    let s = wait_sent(&sent, 3).await;
    let c = content(&content_key, &s[2]);
    assert_eq!(c.kind, "turn_done");
    assert_eq!(c.body, "Fake agent: echo: hello there");
    assert_eq!(s[2].collapse_id, Some(format!("turn-{sid}")));

    // A token from the other environment: the host finds the right one and remembers it.
    let st: PushStatus = phone
        .call(m::PUSH_REGISTER, json!({"token": SANDBOX_TOKEN, "topic": "dev.agenthost.ios", "environment": "production", "key": URL_SAFE_NO_PAD.encode(content_key)}))
        .await
        .unwrap();
    assert_eq!(st.this_device.unwrap().prefs, PushPrefs::default(), "registering again sets the prefs it sends");
    let r: PushTestResult = phone.call(m::PUSH_TEST, json!({})).await.unwrap();
    assert_eq!(r.sent, 1, "{:?}", r.errors);
    let s = wait_sent(&sent, 4).await;
    assert_eq!(s[3].env, "sandbox");
    let st: PushStatus = phone.call(m::PUSH_STATUS, json!({})).await.unwrap();
    assert_eq!(st.this_device.unwrap().environment, PushEnvironment::Sandbox);

    // APNs says the app is gone: the registration goes.
    let _: PushStatus = phone
        .call(m::PUSH_REGISTER, json!({"token": GONE_TOKEN, "topic": "dev.agenthost.ios", "environment": "production", "key": URL_SAFE_NO_PAD.encode(content_key)}))
        .await
        .unwrap();
    let r: PushTestResult = phone.call(m::PUSH_TEST, json!({})).await.unwrap();
    assert_eq!(r.sent, 0);
    assert_eq!(r.errors, vec!["Test phone: Unregistered".to_string()]);
    let st: PushStatus = phone.call(m::PUSH_STATUS, json!({})).await.unwrap();
    assert!(st.this_device.is_none());

    // Unregister, and revoking removes a registration too.
    let _: PushStatus = phone.call(m::PUSH_REGISTER, reg.clone()).await.unwrap();
    let st: PushStatus = phone.call(m::PUSH_UNREGISTER, json!({})).await.unwrap();
    assert!(st.this_device.is_none());
    let _: PushStatus = phone.call(m::PUSH_REGISTER, reg).await.unwrap();
    desk.request(m::DEVICE_REVOKE, json!({"device_id": state.device_id}))
        .await
        .unwrap();
    let all: PushStatus = desk.call(m::PUSH_STATUS, json!({})).await.unwrap();
    assert!(all.devices.is_empty());
    assert!(all.configured);
    let all: PushStatus = desk.call(m::PUSH_CLEAR_KEY, json!({})).await.unwrap();
    assert!(!all.configured && all.key_id.is_none());
}
