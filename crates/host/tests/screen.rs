//! Screen sharing: the policy gate, tickets, and the pipe between a
//! screen-relay engine and a phone, through the dev relay, against a fake
//! engine that speaks screen-relay's `nsl1` protocol.

mod common;

use agenthost_cli::remote::{self, DeviceState};
use agenthost_host::relay_dev::DevRelay;
use agenthost_proto::jsonrpc::codes;
use agenthost_proto::methods as m;
use agenthost_proto::screen::{self as wire, Input, ServerMessage};
use agenthost_proto::*;
use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message as WsMsg;

const T: Duration = Duration::from_secs(20);

/// What the fake engine saw.
#[derive(Default)]
struct Seen {
    paths: Vec<String>,
    acks: Vec<u32>,
    inputs: Vec<[u8; wire::INPUT_SIZE]>,
}

fn frame(generation: u32, sequence: u32, keyframe: bool) -> Vec<u8> {
    let mut p = vec![];
    p.extend_from_slice(&640u16.to_be_bytes());
    p.extend_from_slice(&480u16.to_be_bytes());
    p.extend_from_slice(&123u64.to_be_bytes());
    // Big enough to need several Noise frames.
    p.extend_from_slice(&[0, 0, 0, 1, 0x65]);
    p.extend(std::iter::repeat_n(0xAB, 150_000));
    let mut m = vec![];
    m.extend_from_slice(wire::MAGIC);
    m.push(wire::VERSION);
    m.push(wire::T_FRAME);
    m.extend_from_slice(&(keyframe as u16).to_be_bytes());
    m.extend_from_slice(&generation.to_be_bytes());
    m.extend_from_slice(&sequence.to_be_bytes());
    m.extend_from_slice(&(p.len() as u32).to_be_bytes());
    m.extend_from_slice(&p);
    m
}

/// A screen-relay engine stand-in: one stream at a time, three frames each.
async fn fake_engine() -> (String, Arc<Mutex<Seen>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Seen::default()));
    let s = seen.clone();
    tokio::spawn(async move {
        let mut generation = 0;
        while let Ok((tcp, _)) = listener.accept().await {
            generation += 1;
            let s = s.clone();
            tokio::spawn(async move {
                let ws = tokio_tungstenite::accept_hdr_async(tcp, |req: &Request, mut resp: Response| {
                    s.lock().unwrap().paths.push(req.uri().to_string());
                    let offered = req.headers().get("Sec-WebSocket-Protocol").and_then(|v| v.to_str().ok()).unwrap_or("");
                    assert!(offered.contains(wire::SUBPROTOCOL), "the host must offer nsl1");
                    resp.headers_mut().insert("Sec-WebSocket-Protocol", wire::SUBPROTOCOL.parse().unwrap());
                    Ok(resp)
                })
                .await
                .unwrap();
                let (mut sink, mut stream) = ws.split();
                for seq in 1..=3 {
                    sink.send(WsMsg::Binary(frame(generation, seq, seq == 1).into())).await.unwrap();
                }
                while let Some(Ok(WsMsg::Binary(b))) = stream.next().await {
                    let h = wire::header(&b).unwrap();
                    assert_eq!(h.generation, generation);
                    let mut seen = s.lock().unwrap();
                    match h.kind {
                        wire::T_ACK => seen.acks.push(h.sequence),
                        wire::T_INPUT => seen.inputs.push(b[wire::HEADER_SIZE..].try_into().unwrap()),
                        k => panic!("unexpected message type {k}"),
                    }
                }
            });
        }
    });
    (url, seen)
}

async fn setup() -> (TestHost, DevRelay, DeviceState, Arc<Mutex<Seen>>) {
    setup_with(None).await
}

async fn setup_with(max_view: Option<Duration>) -> (TestHost, DevRelay, DeviceState, Arc<Mutex<Seen>>) {
    let (engine, seen) = fake_engine().await;
    let relay = DevRelay::start("127.0.0.1:0").await.unwrap();
    let url = relay.url();
    let th = TestHost::start_with(move |c| {
        c.relay_url = Some(url);
        c.screen_engine_url = Some(engine);
        c.screen_idle_stop = Some(Duration::from_millis(300));
        c.screen_max_view = max_view;
    })
    .await;
    let state = pair_device(&th).await;
    (th, relay, state, seen)
}

/// Pair a device through the relay, confirming on the desktop.
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

async fn next_frame(s: &mut remote::ScreenStream) -> wire::Frame {
    loop {
        let msg = tokio::time::timeout(T, s.incoming.recv()).await.unwrap().expect("stream ended");
        if let ServerMessage::Frame(f) = wire::decode(&msg).unwrap() {
            return f;
        }
    }
}

async fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn off_until_the_desktop_allows_it_and_phones_cannot_turn_it_on() {
    let (th, _relay, state, _) = setup().await;
    let (phone, _, _) = remote::connect(&state).await.unwrap();
    let st: ScreenStatus = phone.call(m::SCREEN_STATUS, json!({})).await.unwrap();
    assert!(!st.policy.enabled);
    assert!(st.modes.iter().any(|m| m.mode == ScreenMode::Desktop && m.available));

    let e = phone.request(m::SCREEN_START, json!({"mode": "desktop"})).await.unwrap_err();
    assert_eq!(e.code, codes::CAPABILITY_DENIED);
    let e = phone
        .request(m::SCREEN_SET_POLICY, json!({"enabled": true, "allow_control": true}))
        .await
        .unwrap_err();
    assert_eq!(e.code, codes::CAPABILITY_DENIED);

    // Watching only: asking for control is refused.
    let (desk, _) = th.client().await;
    let _: ScreenStatus = desk.call(m::SCREEN_SET_POLICY, json!({"enabled": true})).await.unwrap();
    let e = phone.request(m::SCREEN_START, json!({"mode": "desktop", "control": true})).await.unwrap_err();
    assert_eq!(e.code, codes::CAPABILITY_DENIED);
    // And attach only works as the first message of a tunnel of its own.
    let e = phone.request(m::SCREEN_ATTACH, json!({"ticket": "x"})).await.unwrap_err();
    assert_eq!(e.code, codes::INVALID_PARAMS);
}

#[tokio::test]
async fn view_only_streams_frames_and_acks_but_drops_input() {
    let (th, _relay, state, seen) = setup().await;
    let (desk, _) = th.client().await;
    let _: ScreenStatus = desk.call(m::SCREEN_SET_POLICY, json!({"enabled": true})).await.unwrap();
    let (phone, _, _) = remote::connect(&state).await.unwrap();
    // An engine you run yourself shows what it shows: no monitors to pick.
    let st: ScreenStatus = phone.call(m::SCREEN_STATUS, json!({})).await.unwrap();
    assert!(st.outputs.is_empty());
    let e = phone.request(m::SCREEN_START, json!({"mode": "desktop", "output": "DP-2"})).await.unwrap_err();
    assert_eq!(e.code, codes::INVALID_PARAMS, "{e:?}");
    let start: ScreenStartResult = phone.call(m::SCREEN_START, json!({"mode": "desktop"})).await.unwrap();
    assert!(!start.control);
    assert_eq!(start.output, None);

    let mut s = remote::open_screen(&state, &start.ticket).await.unwrap();
    let f = next_frame(&mut s).await;
    assert!(f.keyframe);
    assert_eq!((f.width, f.height, f.sequence), (640, 480, 1));
    assert_eq!(f.data.len(), 150_005, "a frame larger than one Noise message arrives whole");
    s.send(wire::ack(f.generation, f.sequence, 0)).await.unwrap();
    s.send(Input::Button { button: 1, down: true, x: 1, y: 1 }.message(f.generation)).await.unwrap();
    s.send(b"not a screen-relay message".to_vec()).await.unwrap();
    let f2 = next_frame(&mut s).await;
    s.send(wire::ack(f2.generation, f2.sequence, 0)).await.unwrap();

    wait_for("both ACKs", || seen.lock().unwrap().acks == vec![1, 2]).await;
    let seen = seen.lock().unwrap();
    assert!(seen.inputs.is_empty(), "a view-only viewer must not reach the desktop");
    assert_eq!(seen.paths, vec!["/stream?view=1"]);
    drop(seen);

    // A ticket works once.
    let e = remote::open_screen(&state, &start.ticket).await.err().unwrap();
    assert!(e.to_string().contains("unknown or expired"), "{e}");
}

#[tokio::test]
async fn control_reaches_the_engine_and_a_new_viewer_takes_over() {
    let (th, _relay, state, seen) = setup().await;
    let (desk, mut dn) = th.client().await;
    let _: ScreenStatus = desk
        .call(m::SCREEN_SET_POLICY, json!({"enabled": true, "allow_control": true}))
        .await
        .unwrap();
    let (phone, _, _) = remote::connect(&state).await.unwrap();
    let start: ScreenStartResult =
        phone.call(m::SCREEN_START, json!({"mode": "desktop", "control": true})).await.unwrap();
    let mut s1 = remote::open_screen(&state, &start.ticket).await.unwrap();
    let f = next_frame(&mut s1).await;
    let click = Input::Button { button: 1, down: true, x: 32768, y: 100 };
    s1.send(click.message(f.generation)).await.unwrap();
    wait_for("the click", || seen.lock().unwrap().inputs.len() == 1).await;
    assert_eq!(seen.lock().unwrap().inputs[0], click.record());
    assert_eq!(seen.lock().unwrap().paths, vec!["/stream"]);

    // The desktop hears about it.
    let running = loop {
        let n = tokio::time::timeout(T, dn.recv()).await.unwrap().unwrap();
        if n.method == m::N_SCREEN_CHANGED {
            let st: ScreenStatus = serde_json::from_value(n.params).unwrap();
            if let Some(r) = st.running.filter(|r| r.viewers == 1) {
                break r;
            }
        }
    };
    assert!(running.control);
    assert_eq!(running.started_by, "Test phone");

    // A second viewer (the phone reconnecting) replaces the first.
    let again: ScreenStartResult =
        phone.call(m::SCREEN_START, json!({"mode": "desktop", "control": true})).await.unwrap();
    let mut s2 = remote::open_screen(&state, &again.ticket).await.unwrap();
    assert_eq!(next_frame(&mut s2).await.sequence, 1);
    let mut closed = s1.closed.clone();
    tokio::time::timeout(T, closed.wait_for(|r| r.is_some())).await.unwrap().unwrap();

    // Turning sharing off ends the stream at once.
    let _: ScreenStatus = desk.call(m::SCREEN_SET_POLICY, json!({"enabled": false})).await.unwrap();
    let mut closed = s2.closed.clone();
    tokio::time::timeout(T, closed.wait_for(|r| r.is_some())).await.unwrap().unwrap();
    let st: ScreenStatus = desk.call(m::SCREEN_STATUS, json!({})).await.unwrap();
    assert!(st.running.is_none());
}

#[tokio::test]
async fn an_unused_engine_stops_on_its_own() {
    let (th, _relay, state, _) = setup().await;
    let (desk, _) = th.client().await;
    let _: ScreenStatus = desk.call(m::SCREEN_SET_POLICY, json!({"enabled": true})).await.unwrap();
    let (phone, _, _) = remote::connect(&state).await.unwrap();
    let _: ScreenStartResult = phone.call(m::SCREEN_START, json!({"mode": "desktop"})).await.unwrap();
    let st: ScreenStatus = desk.call(m::SCREEN_STATUS, json!({})).await.unwrap();
    assert!(st.running.is_some());
    tokio::time::sleep(Duration::from_millis(800)).await;
    let st: ScreenStatus = desk.call(m::SCREEN_STATUS, json!({})).await.unwrap();
    assert!(st.running.is_none(), "no viewer came, so the engine was stopped");
}

#[tokio::test]
async fn a_forgotten_viewer_is_ended_after_the_time_limit() {
    let (th, _relay, state, _) = setup_with(Some(Duration::from_millis(500))).await;
    let (desk, _) = th.client().await;
    let _: ScreenStatus = desk.call(m::SCREEN_SET_POLICY, json!({"enabled": true})).await.unwrap();
    let (phone, _, _) = remote::connect(&state).await.unwrap();
    let start: ScreenStartResult = phone.call(m::SCREEN_START, json!({"mode": "desktop"})).await.unwrap();
    let mut s = remote::open_screen(&state, &start.ticket).await.unwrap();
    next_frame(&mut s).await;
    let mut closed = s.closed.clone();
    tokio::time::timeout(T, closed.wait_for(|r| r.is_some())).await.unwrap().unwrap();
    // Then the engine stops, as after any viewer leaves.
    wait_for("the engine to stop", || {
        let st = th.host.screen.status();
        st.running.is_none()
    })
    .await;
}
