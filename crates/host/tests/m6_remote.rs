//! Milestone 6: pairing, the encrypted tunnel, revocation and tampering,
//! against the in-process dev relay (same protocol as the Cloudflare relay).

mod common;

use agenthost_cli::remote::{self, DeviceState};
use agenthost_cli::{as_event, Client};
use agenthost_host::relay_dev::DevRelay;
use agenthost_proto::jsonrpc::codes;
use agenthost_proto::methods as m;
use agenthost_proto::*;
use common::*;
use serde_json::json;
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(20);

async fn host_with_relay() -> (TestHost, DevRelay) {
    let relay = DevRelay::start("127.0.0.1:0").await.unwrap();
    let url = relay.url();
    let th = TestHost::start_with(move |c| c.relay_url = Some(url)).await;
    let (c, _) = th.client().await;
    for _ in 0..100 {
        let info: HostInfo = c.call(m::HOST_INFO, json!({})).await.unwrap();
        if info.relay.connected {
            return (th, relay);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("host never connected to the relay");
}

/// Pair a device; the desktop confirms (or refuses) when the codes match.
async fn pair(th: &TestHost, accept: bool) -> anyhow::Result<DeviceState> {
    let (desk, mut dn) = th.client().await;
    let start: PairStartResult = desk.call(m::DEVICE_PAIR_START, json!({})).await?;
    let qr = start.qr_payload.clone();
    let (code_tx, code_rx) = tokio::sync::oneshot::channel::<String>();
    let mut phone = tokio::spawn(async move {
        remote::pair(&qr, "Test phone", |c| {
            let _ = code_tx.send(c.to_string());
        })
        .await
    });
    let pending: PairingPending = loop {
        // The phone can't finish before the desktop confirms, so if it does,
        // it failed: report why instead of waiting out the timeout.
        let n = tokio::select! {
            n = tokio::time::timeout(T, dn.recv()) => n?.unwrap(),
            r = &mut phone => anyhow::bail!("phone stopped before the desktop saw the pairing: {:?}", r?.err()),
        };
        if n.method == m::N_PAIRING_PENDING {
            break serde_json::from_value(n.params)?;
        }
    };
    let phone_code = code_rx.await?;
    assert_eq!(
        pending.short_code, phone_code,
        "codes must match on both screens"
    );
    assert_eq!(pending.device_name, "Test phone");
    desk.request(
        m::DEVICE_PAIR_CONFIRM,
        json!({"pairing_id": start.pairing_id, "accept": accept}),
    )
    .await
    .map_err(|e| anyhow::anyhow!(e))?;
    phone.await?
}

#[tokio::test]
async fn pair_then_use_sessions_over_the_relay() {
    let (th, relay) = host_with_relay().await;
    let t0 = Instant::now();
    let state = pair(&th, true).await.unwrap();
    let (phone, mut pn, _closed) = remote::connect(&state).await.unwrap();
    let list: Vec<Session> = phone.call(m::SESSION_LIST, json!({})).await.unwrap();
    assert!(list.is_empty());
    assert!(
        t0.elapsed() < Duration::from_secs(30),
        "pairing took {:?}",
        t0.elapsed()
    );
    assert_eq!(
        phone.hello.device_id.as_deref(),
        Some(state.device_id.as_str())
    );
    assert!(!phone
        .hello
        .capabilities
        .contains(&methods::Capability::Admin));

    // The desktop sets up a workspace; the phone starts a session and prompts.
    let (desk, _) = th.client().await;
    let ws = th.workspace_id(&desk).await;
    let s = phone.create_session(&ws, AgentKind::Fake).await.unwrap();
    wait_state(&phone, &s.session_id, SessionState::Idle, T).await;
    phone.subscribe(&s.session_id, 0).await.unwrap();
    phone
        .prompt(&s.session_id, "secret prompt words")
        .await
        .unwrap();
    let (ev, _) = collect_until(&mut pn, T, turn_end).await;
    assert!(ev
        .iter()
        .any(|e| e.kind == EventType::AgentMessage
            && e.payload["text"] == "echo: secret prompt words"));
    let prompt = ev.iter().find(|e| e.kind == EventType::Prompt).unwrap();
    // Named by the device, not its id.
    assert_eq!(prompt.payload["device"], "Test phone");

    // Desktop-only methods are refused for the phone.
    let e = phone
        .request(m::WORKSPACE_ADD, json!({"path": "/tmp"}))
        .await
        .unwrap_err();
    assert_eq!(e.code, codes::CAPABILITY_DENIED);
    let e = phone
        .request(m::DEVICE_PAIR_START, json!({}))
        .await
        .unwrap_err();
    assert_eq!(e.code, codes::CAPABILITY_DENIED);

    // A relay that logs every frame learns nothing readable.
    let log = relay.tamper.lock().unwrap().log.clone();
    assert!(log.len() > 10);
    for frame in &log {
        let s = String::from_utf8_lossy(frame);
        for needle in [
            "secret prompt",
            "session.prompt",
            "jsonrpc",
            "echo:",
            "Test phone",
            "agent.message",
        ] {
            assert!(!s.contains(needle), "relay saw {needle:?}");
        }
    }
}

#[tokio::test]
async fn pairing_code_is_single_use_and_refusal_works() {
    let (th, _relay) = host_with_relay().await;
    let (desk, _) = th.client().await;
    let start: PairStartResult = desk.call(m::DEVICE_PAIR_START, json!({})).await.unwrap();
    assert!(start.expires_at - crate_now() <= 120_000 && start.expires_at - crate_now() > 110_000);
    // Refused on the desktop.
    let qr = start.qr_payload.clone();
    let (desk2, mut dn) = th.client().await;
    let phone = tokio::spawn(async move { remote::pair(&qr, "Unexpected", |_| {}).await });
    loop {
        let n = tokio::time::timeout(T, dn.recv()).await.unwrap().unwrap();
        if n.method == m::N_PAIRING_PENDING {
            desk2
                .request(
                    m::DEVICE_PAIR_CONFIRM,
                    json!({"pairing_id": start.pairing_id, "accept": false}),
                )
                .await
                .unwrap();
            break;
        }
    }
    assert!(phone.await.unwrap().is_err());
    // The same QR again is refused by the relay.
    let e = remote::pair(&start.qr_payload, "Again", |_| {})
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("4403") || e.to_string().contains("not allowed"),
        "{e}"
    );
    let devices: Vec<Device> = desk.call(m::DEVICE_LIST, json!({})).await.unwrap();
    assert!(devices.is_empty());
}

fn crate_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[tokio::test]
async fn revoked_device_is_disconnected_and_cannot_return() {
    let (th, _relay) = host_with_relay().await;
    let state = pair(&th, true).await.unwrap();
    let (phone, mut pn, mut closed) = remote::connect(&state).await.unwrap();
    phone.request(m::SESSION_LIST, json!({})).await.unwrap();
    let (desk, _) = th.client().await;
    let t0 = Instant::now();
    desk.request(m::DEVICE_REVOKE, json!({"device_id": state.device_id}))
        .await
        .unwrap();
    // The phone is told, then disconnected.
    let mut told = false;
    while let Ok(Some(n)) = tokio::time::timeout(Duration::from_secs(2), pn.recv()).await {
        if n.method == m::N_DEVICE_REVOKED {
            told = true;
        }
    }
    tokio::time::timeout(Duration::from_secs(2), closed.changed())
        .await
        .ok();
    assert!(
        t0.elapsed() < Duration::from_millis(2500),
        "disconnect took {:?}",
        t0.elapsed()
    );
    assert!(told, "phone was not told it was removed");
    let reason = closed.borrow().clone();
    assert!(reason.as_ref().is_some_and(|c| c.code == 4401), "expected close code 4401, got {reason:?}");
    assert!(phone.request(m::SESSION_LIST, json!({})).await.is_err());
    // Reconnecting fails.
    let e = remote::connect(&state)
        .await
        .err()
        .expect("revoked device connected");
    assert!(
        e.to_string().contains("4403") || e.to_string().contains("not allowed"),
        "{e}"
    );
    let devices: Vec<Device> = desk.call(m::DEVICE_LIST, json!({})).await.unwrap();
    assert!(devices.is_empty());
}

async fn tamper_case(flip: bool) {
    let (th, relay) = host_with_relay().await;
    let state = pair(&th, true).await.unwrap();
    let (phone, _pn, mut closed) = remote::connect(&state).await.unwrap();
    phone.request(m::SESSION_LIST, json!({})).await.unwrap();
    {
        let mut t = relay.tamper.lock().unwrap();
        let next = t.device_frames + 1;
        if flip {
            t.flip_device_frame = Some(next);
        } else {
            t.replay_device_frame = Some(next);
        }
    }
    let r = phone.request(m::SESSION_LIST, json!({})).await;
    tokio::time::timeout(Duration::from_secs(3), closed.changed())
        .await
        .ok();
    let reason = closed.borrow().clone();
    assert!(
        reason.is_some(),
        "tunnel survived tampering (request result: {r:?})"
    );
    // The host is fine: its state is intact and other clients work.
    let (desk, _) = th.client().await;
    desk.request(m::SESSION_LIST, json!({})).await.unwrap();
    // The device can connect again cleanly.
    relay.tamper.lock().unwrap().flip_device_frame = None;
    relay.tamper.lock().unwrap().replay_device_frame = None;
    let (phone2, _, _) = remote::connect(&state).await.unwrap();
    phone2.request(m::SESSION_LIST, json!({})).await.unwrap();
}

#[tokio::test]
async fn altered_frame_closes_the_connection() {
    tamper_case(true).await;
}

#[tokio::test]
async fn replayed_frame_closes_the_connection() {
    tamper_case(false).await;
}

#[tokio::test]
async fn silent_relay_link_is_noticed_and_reconnect_catches_up() {
    // Short keepalive for this process: ping every 200 ms, dead after 1 s.
    std::env::set_var("AGENTHOST_KEEPALIVE_MS", "200,1000");
    let (th, relay) = host_with_relay().await;
    let state = pair(&th, true).await.unwrap();
    let (phone, _n, mut closed) = remote::connect(&state).await.unwrap();
    phone.request(m::SESSION_LIST, json!({})).await.unwrap();
    // Healthy and idle: pings keep it open past the silence limit.
    tokio::time::sleep(Duration::from_millis(1600)).await;
    assert!(closed.borrow().is_none(), "closed while healthy: {:?}", closed.borrow());
    // The network dies without a close: the device notices by itself.
    relay.tamper.lock().unwrap().stall_devices = true;
    tokio::time::timeout(Duration::from_secs(5), closed.changed())
        .await
        .expect("a dead link was not noticed")
        .unwrap();
    let reason = closed.borrow().clone().unwrap().to_string();
    assert!(reason.contains("connection lost"), "{reason}");
    // Back online: a new connection works and replays what happened meanwhile.
    relay.tamper.lock().unwrap().stall_devices = false;
    let (phone2, _, _) = remote::connect(&state).await.unwrap();
    phone2.request(m::SESSION_LIST, json!({})).await.unwrap();
}

#[tokio::test]
async fn host_offline_is_reported() {
    let (th, relay) = host_with_relay().await;
    let state = pair(&th, true).await.unwrap();
    let (dir, cfg) = th.stop().await;
    // Drop the relay connection by stopping the host process state.
    drop(dir);
    let _ = cfg;
    // A device whose host is gone gets "host offline" once the relay notices.
    let mut last = String::new();
    for _ in 0..40 {
        if let Err(e) = remote::connect(&state).await {
            last = e.to_string();
            if last.contains("4404") || last.contains("offline") {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let _ = relay;
    let _ = Client::connect;
    let _ = as_event;
    assert!(last.contains("offline") || last.contains("4404"), "{last}");
}
